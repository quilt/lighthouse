mod reduced_tree;

use std::sync::Arc;
use store::Store;
use types::{ShardBlock, EthSpec, Hash256, Slot};

pub use reduced_tree::ThreadSafeReducedTree;

pub type Result<T> = std::result::Result<T, String>;

pub trait LmdGhost<S: Store, E: EthSpec>: Send + Sync {
    /// Create a new instance, with the given `store` and `finalized_root`.
    fn new(store: Arc<S>, finalized_block: &ShardBlock, finalized_root: Hash256) -> Self;

    /// Process an attestation message from some validator that attests to some `block_hash`
    /// representing a block at some `block_slot`.
    fn process_attestation(
        &self,
        validator_index: usize,
        block_hash: Hash256,
        block_slot: Slot,
    ) -> Result<()>;

    /// Process a block that was seen on the network.
    fn process_block(&self, block: &ShardBlock, block_hash: Hash256) -> Result<()>;

    /// Returns the head of the chain, starting the search at `start_block_root` and moving upwards
    /// (in block height).
    fn find_head<F>(
        &self,
        start_block_slot: Slot,
        start_block_root: Hash256,
        weight: F,
    ) -> Result<Hash256>
    where
        F: Fn(usize) -> Option<u64> + Copy;

    /// Provide an indication that the blockchain has been finalized at the given `finalized_block`.
    ///
    /// `finalized_block_root` must be the root of `finalized_block`.
    fn update_finalized_root(
        &self,
        finalized_block: &ShardBlock,
        finalized_block_root: Hash256,
    ) -> Result<()>;
}