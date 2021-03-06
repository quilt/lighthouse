use crate::test_utils::TestRandom;
use crate::*;
use bls::Signature;

use serde_derive::{Deserialize, Serialize};
use ssz_derive::{Decode, Encode};
use test_random_derive::TestRandom;
use tree_hash::{SignedRoot, TreeHash};
use tree_hash_derive::{CachedTreeHash, SignedRoot, TreeHash};

#[derive(
    Debug,
    PartialEq,
    Clone,
    Serialize,
    Deserialize,
    Encode,
    Decode,
    TreeHash,
    CachedTreeHash,
    TestRandom,
    SignedRoot,
)]
pub struct ShardBlockHeader {
    pub slot: ShardSlot,
    pub shard: u64,
    pub parent_root: Hash256,
    pub beacon_block_root: Hash256,
    pub state_root: Hash256,
    #[test_random(default)]
    pub body: Vec<u8>,
    pub attestation: Vec<ShardAttestation>,
    #[signed_root(skip_hashing)]
    pub signature: Signature,
}

impl ShardBlockHeader {
    pub fn empty(spec: &ChainSpec, shard: u64) -> ShardBlockHeader {
        ShardBlockHeader {
            shard,
            slot: ShardSlot::from(spec.phase_1_fork_slot),
            beacon_block_root: spec.zero_hash,
            parent_root: spec.zero_hash,
            state_root: spec.zero_hash,
            body: vec![],
            attestation: vec![],
            signature: Signature::empty_signature(),
        }
    }

    pub fn canonical_root(&self) -> Hash256 {
        Hash256::from_slice(&self.signed_root()[..])
    }

    pub fn into_block(self) -> ShardBlock {
        // add body logic
        ShardBlock {
            shard: self.shard,
            slot: self.slot,
            beacon_block_root: self.beacon_block_root,
            parent_root: self.parent_root,
            state_root: self.state_root,
            body: self.body,
            attestation: self.attestation,
            signature: self.signature,
        }
    }

    pub fn block(&self) -> ShardBlock {
        ShardBlock {
            shard: self.shard,
            slot: self.slot,
            beacon_block_root: self.beacon_block_root,
            parent_root: self.parent_root,
            state_root: self.state_root,
            body: self.body.clone(),
            attestation: self.attestation.clone(),
            signature: self.signature.clone(),
        }
    }
}
