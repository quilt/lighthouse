mod attestation_id;

use attestation_id::AttestationId;
use parking_lot::RwLock;
use std::collections::{hash_map, HashMap};
use std::marker::PhantomData;
use types::{BeaconState, ChainSpec, EthSpec, ShardAttestation, ShardSlot, ShardSpec, ShardState};

#[derive(Default, Debug)]
pub struct OperationPool<T: ShardSpec + Default> {
    attestations: RwLock<HashMap<AttestationId, Vec<ShardAttestation>>>,
    body: RwLock<Vec<u8>>,
    _phantom: PhantomData<T>,
}

impl<T: ShardSpec> OperationPool<T> {
    /// Create a new operation pool.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert an attestation into the pool, aggregating it with existing attestations if possible.
    pub fn insert_attestation<U: EthSpec>(
        &self,
        attestation: ShardAttestation,
        beacon_state: &BeaconState<U>,
        spec: &ChainSpec,
    ) -> () {
        let id = AttestationId::from_data(&attestation.data, beacon_state, spec);

        // Take a write lock on the attestations map.
        let mut attestations = self.attestations.write();

        let existing_attestations = match attestations.entry(id) {
            hash_map::Entry::Vacant(entry) => {
                entry.insert(vec![attestation]);
                return ();
            }
            hash_map::Entry::Occupied(entry) => entry.into_mut(),
        };

        let mut aggregated = false;
        for existing_attestation in existing_attestations.iter_mut() {
            if existing_attestation.signers_disjoint_from(&attestation) {
                existing_attestation.aggregate(&attestation);
                aggregated = true;
            } else if *existing_attestation == attestation {
                aggregated = true;
            }
        }

        if !aggregated {
            existing_attestations.push(attestation);
        }

        ()
    }

    /// Total number of attestations in the pool, including attestations for the same data.
    pub fn num_attestations(&self) -> usize {
        self.attestations.read().values().map(Vec::len).sum()
    }

    /// Get attestation with most attesters for inclusion in a block
    pub fn get_attestation<U: EthSpec>(
        &self,
        state: &ShardState<T>,
        beacon_state: &BeaconState<U>,
        spec: &ChainSpec,
    ) -> Vec<ShardAttestation> {
        let attesting_slot = ShardSlot::from(state.slot - 1);
        let epoch = attesting_slot.epoch(spec.slots_per_epoch, spec.shard_slots_per_beacon_slot);
        let domain_bytes =
            AttestationId::compute_domain_bytes(epoch, attesting_slot, beacon_state, spec);
        let reader = self.attestations.read();

        let mut attestations: Vec<ShardAttestation> = reader
            .iter()
            .filter(|(key, _)| key.domain_bytes_match(&domain_bytes))
            .flat_map(|(_, attestations)| attestations)
            .cloned()
            .collect();

        attestations.sort_by(|a, b| {
            b.aggregation_bitfield
                .num_set_bits()
                .cmp(&a.aggregation_bitfield.num_set_bits())
        });

        let mut attestation = vec![];
        if !attestations.is_empty() {
            attestation.push((&attestations[0]).clone());
        }

        attestation
    }

    pub fn prune_attestations(&self, finalized_state: &ShardState<T>) {
        self.attestations.write().retain(|_, attestations| {
            attestations
                .first()
                .map_or(false, |att| finalized_state.slot <= att.data.target_slot)
        });
    }

    // This is temporary and should not be here at all - this would actually be defined within
    // the validator client and its own communication with the relay network. We will put it here for now
    // as it is the most simple. As the simulation advances, this should be removed
    pub fn insert_body(&self, body: Vec<u8>) -> () {
        *self.body.write() = body;
    }

    pub fn get_body(&self) -> Vec<u8> {
        let body = self.body.read().clone();
        // quite hacky to reset it - but this does not belong here in the first place
        *self.body.write() = vec![];
        body
    }
}

impl<T: ShardSpec + Default> PartialEq for OperationPool<T> {
    fn eq(&self, other: &Self) -> bool {
        *self.attestations.read() == *other.attestations.read()
    }
}
