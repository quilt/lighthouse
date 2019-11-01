use super::manager::SyncMessage;
use crate::service::NetworkMessage;
use beacon_chain::{
    AttestationProcessingOutcome, BeaconChain, BeaconChainTypes, BlockProcessingOutcome,
};
use eth2_libp2p::rpc::methods::*;
use eth2_libp2p::rpc::{RPCEvent, RPCRequest, RPCResponse, RequestId};
use eth2_libp2p::PeerId;
use slog::{debug, error, info, o, trace, warn};
use ssz::Encode;
use std::sync::Arc;
use store::Store;
use tokio::sync::{mpsc, oneshot};
use tree_hash::SignedRoot;
use types::{Attestation, BeaconBlock, Epoch, EthSpec, Hash256, Slot};

//TODO: Rate limit requests

/// If a block is more than `FUTURE_SLOT_TOLERANCE` slots ahead of our slot clock, we drop it.
/// Otherwise we queue it.
pub(crate) const FUTURE_SLOT_TOLERANCE: u64 = 1;

const SHOULD_FORWARD_GOSSIP_BLOCK: bool = true;
const SHOULD_NOT_FORWARD_GOSSIP_BLOCK: bool = false;

/// Keeps track of syncing information for known connected peers.
#[derive(Clone, Copy, Debug)]
pub struct PeerSyncInfo {
    fork_version: [u8; 4],
    pub finalized_root: Hash256,
    pub finalized_epoch: Epoch,
    pub head_root: Hash256,
    pub head_slot: Slot,
}

impl From<StatusMessage> for PeerSyncInfo {
    fn from(status: StatusMessage) -> PeerSyncInfo {
        PeerSyncInfo {
            fork_version: status.fork_version,
            finalized_root: status.finalized_root,
            finalized_epoch: status.finalized_epoch,
            head_root: status.head_root,
            head_slot: status.head_slot,
        }
    }
}

impl<T: BeaconChainTypes> From<&Arc<BeaconChain<T>>> for PeerSyncInfo {
    fn from(chain: &Arc<BeaconChain<T>>) -> PeerSyncInfo {
        Self::from(status_message(chain))
    }
}

/// Processes validated messages from the network. It relays necessary data to the syncing thread
/// and processes blocks from the pubsub network.
pub struct MessageProcessor<T: BeaconChainTypes> {
    /// A reference to the underlying beacon chain.
    chain: Arc<BeaconChain<T>>,
    /// A channel to the syncing thread.
    sync_send: mpsc::UnboundedSender<SyncMessage<T::EthSpec>>,
    /// A oneshot channel for destroying the sync thread.
    _sync_exit: oneshot::Sender<()>,
    /// A nextwork context to return and handle RPC requests.
    network: NetworkContext,
    /// The `RPCHandler` logger.
    log: slog::Logger,
}

impl<T: BeaconChainTypes> MessageProcessor<T> {
    /// Instantiate a `MessageProcessor` instance
    pub fn new(
        executor: &tokio::runtime::TaskExecutor,
        beacon_chain: Arc<BeaconChain<T>>,
        network_send: mpsc::UnboundedSender<NetworkMessage>,
        log: &slog::Logger,
    ) -> Self {
        let sync_logger = log.new(o!("Service"=> "Sync"));
        let sync_network_context = NetworkContext::new(network_send.clone(), sync_logger.clone());

        // spawn the sync thread
        let (sync_send, _sync_exit) = super::manager::spawn(
            executor,
            Arc::downgrade(&beacon_chain),
            sync_network_context,
            sync_logger,
        );

        MessageProcessor {
            chain: beacon_chain,
            sync_send,
            _sync_exit,
            network: NetworkContext::new(network_send, log.clone()),
            log: log.clone(),
        }
    }

    fn send_to_sync(&mut self, message: SyncMessage<T::EthSpec>) {
        self.sync_send.try_send(message).unwrap_or_else(|_| {
            warn!(
                self.log,
                "Could not send message to the sync service";
            )
        });
    }

    /// Handle a peer disconnect.
    ///
    /// Removes the peer from the manager.
    pub fn on_disconnect(&mut self, peer_id: PeerId) {
        self.send_to_sync(SyncMessage::Disconnect(peer_id));
    }

    /// Handle the connection of a new peer.
    ///
    /// Sends a `Status` message to the peer.
    pub fn on_connect(&mut self, peer_id: PeerId) {
        self.network.send_rpc_request(
            None,
            peer_id,
            RPCRequest::Status(status_message(&self.chain)),
        );
    }

    /// Handle a `Status` request.
    ///
    /// Processes the `Status` from the remote peer and sends back our `Status`.
    pub fn on_status_request(
        &mut self,
        peer_id: PeerId,
        request_id: RequestId,
        status: StatusMessage,
    ) {
        // ignore status responses if we are shutting down
        trace!(self.log, "StatusRequest"; "peer" => format!("{:?}", peer_id));

        // Say status back.
        self.network.send_rpc_response(
            peer_id.clone(),
            request_id,
            RPCResponse::Status(status_message(&self.chain)),
        );

        self.process_status(peer_id, status);
    }

    /// Process a `Status` response from a peer.
    pub fn on_status_response(&mut self, peer_id: PeerId, status: StatusMessage) {
        trace!(self.log, "StatusResponse"; "peer" => format!("{:?}", peer_id));

        // Process the status message, without sending back another status.
        self.process_status(peer_id, status);
    }

    /// Process a `Status` message, requesting new blocks if appropriate.
    ///
    /// Disconnects the peer if required.
    fn process_status(&mut self, peer_id: PeerId, status: StatusMessage) {
        let remote = PeerSyncInfo::from(status);
        let local = PeerSyncInfo::from(&self.chain);

        let start_slot = |epoch: Epoch| epoch.start_slot(T::EthSpec::slots_per_epoch());

        if local.fork_version != remote.fork_version {
            // The node is on a different network/fork, disconnect them.
            debug!(
                self.log, "HandshakeFailure";
                "peer" => format!("{:?}", peer_id),
                "reason" => "network_id"
            );

            self.network
                .disconnect(peer_id.clone(), GoodbyeReason::IrrelevantNetwork);
        } else if remote.finalized_epoch <= local.finalized_epoch
            && remote.finalized_root != Hash256::zero()
            && local.finalized_root != Hash256::zero()
            && (self.chain.root_at_slot(start_slot(remote.finalized_epoch))
                != Some(remote.finalized_root))
        {
            // The remotes finalized epoch is less than or greater than ours, but the block root is
            // different to the one in our chain.
            //
            // Therefore, the node is on a different chain and we should not communicate with them.
            debug!(
                self.log, "HandshakeFailure";
                "peer" => format!("{:?}", peer_id),
                "reason" => "different finalized chain"
            );
            self.network
                .disconnect(peer_id.clone(), GoodbyeReason::IrrelevantNetwork);
        } else if remote.finalized_epoch < local.finalized_epoch {
            // The node has a lower finalized epoch, their chain is not useful to us. There are two
            // cases where a node can have a lower finalized epoch:
            //
            // ## The node is on the same chain
            //
            // If a node is on the same chain but has a lower finalized epoch, their head must be
            // lower than ours. Therefore, we have nothing to request from them.
            //
            // ## The node is on a fork
            //
            // If a node is on a fork that has a lower finalized epoch, switching to that fork would
            // cause us to revert a finalized block. This is not permitted, therefore we have no
            // interest in their blocks.
            debug!(
                self.log,
                "NaivePeer";
                "peer" => format!("{:?}", peer_id),
                "reason" => "lower finalized epoch"
            );
        } else if self
            .chain
            .store
            .exists::<BeaconBlock<T::EthSpec>>(&remote.head_root)
            .unwrap_or_else(|_| false)
        {
            trace!(
                self.log, "Peer with known chain found";
                "peer" => format!("{:?}", peer_id),
                "remote_head_slot" => remote.head_slot,
                "remote_latest_finalized_epoch" => remote.finalized_epoch,
            );

            // If the node's best-block is already known to us and they are close to our current
            // head, treat them as a fully sync'd peer.
            self.send_to_sync(SyncMessage::AddPeer(peer_id, remote));
        } else {
            // The remote node has an equal or great finalized epoch and we don't know it's head.
            //
            // Therefore, there are some blocks between the local finalized epoch and the remote
            // head that are worth downloading.
            debug!(
                self.log, "UsefulPeer";
                "peer" => format!("{:?}", peer_id),
                "local_finalized_epoch" => local.finalized_epoch,
                "remote_latest_finalized_epoch" => remote.finalized_epoch,
            );
            self.send_to_sync(SyncMessage::AddPeer(peer_id, remote));
        }
    }

    /// Handle a `BlocksByRoot` request from the peer.
    pub fn on_blocks_by_root_request(
        &mut self,
        peer_id: PeerId,
        request_id: RequestId,
        request: BlocksByRootRequest,
    ) {
        let mut send_block_count = 0;
        for root in request.block_roots.iter() {
            if let Ok(Some(block)) = self.chain.store.get::<BeaconBlock<T::EthSpec>>(root) {
                self.network.send_rpc_response(
                    peer_id.clone(),
                    request_id,
                    RPCResponse::BlocksByRoot(Some(block.as_ssz_bytes())),
                );
                send_block_count += 1;
            } else {
                debug!(
                    self.log,
                    "Peer requested unknown block";
                    "peer" => format!("{:?}", peer_id),
                    "request_root" => format!("{:}", root),
                );
            }
        }
        debug!(
            self.log,
            "BlocksByRootRequest";
            "peer" => format!("{:?}", peer_id),
            "requested" => request.block_roots.len(),
            "returned" => send_block_count,
        );

        // send stream termination
        self.network
            .send_rpc_response(peer_id, request_id, RPCResponse::BlocksByRoot(None));
    }

    /// Handle a `BlocksByRange` request from the peer.
    pub fn on_blocks_by_range_request(
        &mut self,
        peer_id: PeerId,
        request_id: RequestId,
        req: BlocksByRangeRequest,
    ) {
        debug!(
            self.log,
            "BlocksByRangeRequest";
            "peer" => format!("{:?}", peer_id),
            "count" => req.count,
            "start_slot" => req.start_slot,
        );

        //TODO: Optimize this
        // Currently for skipped slots, the blocks returned could be less than the requested range.
        // In the current implementation we read from the db then filter out out-of-range blocks.
        // Improving the db schema to prevent this would be ideal.

        //TODO: This really needs to be read forward for infinite streams
        // We should be reading the first block from the db, sending, then reading the next... we
        // need a forwards iterator!!

        let mut blocks: Vec<BeaconBlock<T::EthSpec>> = self
            .chain
            .rev_iter_block_roots()
            .filter(|(_root, slot)| {
                req.start_slot <= slot.as_u64() && req.start_slot + req.count > slot.as_u64()
            })
            .take_while(|(_root, slot)| req.start_slot <= slot.as_u64())
            .filter_map(|(root, _slot)| {
                if let Ok(Some(block)) = self.chain.store.get::<BeaconBlock<T::EthSpec>>(&root) {
                    Some(block)
                } else {
                    warn!(
                        self.log,
                        "Block in the chain is not in the store";
                        "request_root" => format!("{:}", root),
                    );

                    None
                }
            })
            .filter(|block| block.slot >= req.start_slot)
            .collect();

        blocks.reverse();
        blocks.dedup_by_key(|brs| brs.slot);

        debug!(
            self.log,
            "BlocksByRangeRequest response";
            "peer" => format!("{:?}", peer_id),
            "msg" => "Failed to return all requested blocks",
            "start_slot" => req.start_slot,
            "current_slot" => self.chain.slot().unwrap_or_else(|_| Slot::from(0_u64)).as_u64(),
            "requested" => req.count,
            "returned" => blocks.len(),
        );

        for block in blocks {
            self.network.send_rpc_response(
                peer_id.clone(),
                request_id,
                RPCResponse::BlocksByRange(Some(block.as_ssz_bytes())),
            );
        }
        // send the stream terminator
        self.network
            .send_rpc_response(peer_id, request_id, RPCResponse::BlocksByRange(None));
    }

    /// Handle a `BlocksByRange` response from the peer.
    /// A `beacon_block` behaves as a stream which is terminated on a `None` response.
    pub fn on_blocks_by_range_response(
        &mut self,
        peer_id: PeerId,
        request_id: RequestId,
        beacon_block: Option<BeaconBlock<T::EthSpec>>,
    ) {
        trace!(
            self.log,
            "BlocksByRangeResponse";
            "peer" => format!("{:?}", peer_id),
        );

        self.send_to_sync(SyncMessage::BlocksByRangeResponse {
            peer_id,
            request_id,
            beacon_block,
        });
    }

    /// Handle a `BlocksByRoot` response from the peer.
    pub fn on_blocks_by_root_response(
        &mut self,
        peer_id: PeerId,
        request_id: RequestId,
        beacon_block: Option<BeaconBlock<T::EthSpec>>,
    ) {
        trace!(
            self.log,
            "BlocksByRootResponse";
            "peer" => format!("{:?}", peer_id),
        );

        self.send_to_sync(SyncMessage::BlocksByRootResponse {
            peer_id,
            request_id,
            beacon_block,
        });
    }

    /// Process a gossip message declaring a new block.
    ///
    /// Attempts to apply to block to the beacon chain. May queue the block for later processing.
    ///
    /// Returns a `bool` which, if `true`, indicates we should forward the block to our peers.
    pub fn on_block_gossip(&mut self, peer_id: PeerId, block: BeaconBlock<T::EthSpec>) -> bool {
        match self.chain.process_block(block.clone()) {
            Ok(outcome) => match outcome {
                BlockProcessingOutcome::Processed { .. } => {
                    trace!(self.log, "Gossipsub block processed";
                            "peer_id" => format!("{:?}",peer_id));
                    SHOULD_FORWARD_GOSSIP_BLOCK
                }
                BlockProcessingOutcome::ParentUnknown { parent: _ } => {
                    // Inform the sync manager to find parents for this block
                    trace!(self.log, "Block with unknown parent received";
                            "peer_id" => format!("{:?}",peer_id));
                    self.send_to_sync(SyncMessage::UnknownBlock(peer_id, block.clone()));
                    SHOULD_FORWARD_GOSSIP_BLOCK
                }
                BlockProcessingOutcome::FutureSlot {
                    present_slot,
                    block_slot,
                } if present_slot + FUTURE_SLOT_TOLERANCE >= block_slot => {
                    //TODO: Decide the logic here
                    SHOULD_FORWARD_GOSSIP_BLOCK
                }
                BlockProcessingOutcome::BlockIsAlreadyKnown => SHOULD_FORWARD_GOSSIP_BLOCK,
                other => {
                    warn!(
                        self.log,
                        "Invalid gossip beacon block";
                        "outcome" => format!("{:?}", other),
                        "block root" => format!("{}", Hash256::from_slice(&block.signed_root()[..])),
                        "block slot" => block.slot
                    );
                    trace!(
                        self.log,
                        "Invalid gossip beacon block ssz";
                        "ssz" => format!("0x{}", hex::encode(block.as_ssz_bytes())),
                    );
                    SHOULD_NOT_FORWARD_GOSSIP_BLOCK //TODO: Decide if we want to forward these
                }
            },
            Err(e) => {
                error!(
                    self.log,
                    "Error processing gossip beacon block";
                    "error" => format!("{:?}", e),
                    "block slot" => block.slot
                );
                trace!(
                    self.log,
                    "Erroneous gossip beacon block ssz";
                    "ssz" => format!("0x{}", hex::encode(block.as_ssz_bytes())),
                );
                SHOULD_NOT_FORWARD_GOSSIP_BLOCK
            }
        }
    }

    /// Process a gossip message declaring a new attestation.
    ///
    /// Not currently implemented.
    pub fn on_attestation_gossip(&mut self, _peer_id: PeerId, msg: Attestation<T::EthSpec>) {
        match self.chain.process_attestation(msg.clone()) {
            Ok(outcome) => {
                info!(
                    self.log,
                    "Processed attestation";
                    "source" => "gossip",
                    "outcome" => format!("{:?}", outcome)
                );

                if outcome != AttestationProcessingOutcome::Processed {
                    trace!(
                        self.log,
                        "Invalid gossip attestation ssz";
                        "ssz" => format!("0x{}", hex::encode(msg.as_ssz_bytes())),
                    );
                }
            }
            Err(e) => {
                trace!(
                    self.log,
                    "Erroneous gossip attestation ssz";
                    "ssz" => format!("0x{}", hex::encode(msg.as_ssz_bytes())),
                );
                error!(self.log, "Invalid gossip attestation"; "error" => format!("{:?}", e));
            }
        }
    }
}

/// Build a `StatusMessage` representing the state of the given `beacon_chain`.
pub(crate) fn status_message<T: BeaconChainTypes>(beacon_chain: &BeaconChain<T>) -> StatusMessage {
    let state = &beacon_chain.head().beacon_state;

    StatusMessage {
        fork_version: state.fork.current_version,
        finalized_root: state.finalized_checkpoint.root,
        finalized_epoch: state.finalized_checkpoint.epoch,
        head_root: beacon_chain.head().beacon_block_root,
        head_slot: state.slot,
    }
}

/// Wraps a Network Channel to employ various RPC/Sync related network functionality.
pub struct NetworkContext {
    /// The network channel to relay messages to the Network service.
    network_send: mpsc::UnboundedSender<NetworkMessage>,
    /// Logger for the `NetworkContext`.
    log: slog::Logger,
}

impl NetworkContext {
    pub fn new(network_send: mpsc::UnboundedSender<NetworkMessage>, log: slog::Logger) -> Self {
        Self { network_send, log }
    }

    pub fn disconnect(&mut self, peer_id: PeerId, reason: GoodbyeReason) {
        warn!(
            &self.log,
            "Disconnecting peer (RPC)";
            "reason" => format!("{:?}", reason),
            "peer_id" => format!("{:?}", peer_id),
        );
        self.send_rpc_request(None, peer_id, RPCRequest::Goodbye(reason))
        // TODO: disconnect peers.
    }

    pub fn send_rpc_request(
        &mut self,
        request_id: Option<RequestId>,
        peer_id: PeerId,
        rpc_request: RPCRequest,
    ) {
        // use 0 as the default request id, when an ID is not required.
        let request_id = request_id.unwrap_or_else(|| 0);
        self.send_rpc_event(peer_id, RPCEvent::Request(request_id, rpc_request));
    }

    //TODO: Handle Error responses
    pub fn send_rpc_response(
        &mut self,
        peer_id: PeerId,
        request_id: RequestId,
        rpc_response: RPCResponse,
    ) {
        self.send_rpc_event(
            peer_id,
            RPCEvent::Response(request_id, RPCErrorResponse::Success(rpc_response)),
        );
    }

    fn send_rpc_event(&mut self, peer_id: PeerId, rpc_event: RPCEvent) {
        self.network_send
            .try_send(NetworkMessage::RPC(peer_id, rpc_event))
            .unwrap_or_else(|_| {
                warn!(
                    self.log,
                    "Could not send RPC message to the network service"
                )
            });
    }
}
