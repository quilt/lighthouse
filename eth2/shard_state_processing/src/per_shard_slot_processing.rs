use crate::*;
use types::*;

use process_shard_slot::process_shard_slot;

pub mod errors;
pub mod process_shard_slot;

pub fn per_shard_slot_processing<T: ShardSpec>(
    state: &mut ShardState<T>,
    spec: &ChainSpec,
) -> Result<(), Error> {
    if (state
        .slot
        .epoch(spec.slots_per_epoch, spec.shard_slots_per_beacon_slot)
        + 1)
        % spec.epochs_per_shard_period
        == 0
    {
        // include period processing here :)
    }

    process_shard_slot(state, spec);

    state.slot += 1;

    Ok(())
}
