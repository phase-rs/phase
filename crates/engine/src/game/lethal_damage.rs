//! CR 120.6 + CR 704.5g + CR 702.19b: the single authority for what counts as lethal damage on a
//! creature.
//!
//! Readers that ask "is this much damage lethal for this creature?" or "how much more must an
//! attacker assign before it may trample over?" read this module, so their answers cannot drift
//! apart. Today those are the combat assignment minimum, the excess-damage reading
//! (`combat_damage::lethal_damage_needed`) and the combat predictor
//! (`combat_damage::combat_damage_to_defender`). The lethal-damage state-based action
//! (`sba::lethal_damage_candidates`) still compares marked damage with toughness directly.

use crate::game::game_object::GameObject;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;

/// CR 120.6 + CR 704.5g: total marked damage at or above which `obj` has been dealt lethal
/// damage.
///
/// Toughness at or below 0 yields 0; a creature in that state is put into its owner's graveyard
/// by CR 704.5f, which is not a consequence of damage.
pub fn lethal_damage_threshold(_state: &GameState, obj: &GameObject) -> u32 {
    obj.toughness.unwrap_or(0).max(0) as u32
}

/// CR 120.6 + CR 704.5g: `total` damage is lethal for `obj`: it is nonzero ("it has damage marked
/// on it") and at least the lethal threshold.
///
/// This does NOT apply CR 704.5g's "toughness greater than 0" precondition; callers that destroy
/// apply it themselves.
pub fn is_lethal_damage(state: &GameState, obj: &GameObject, total: u32) -> bool {
    total > 0 && total >= lethal_damage_threshold(state, obj)
}

/// CR 702.19b: combat damage other attackers have already committed to a creature earlier in
/// this combat damage step, split by whether the assigning source has deathtouch (CR 702.2c).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AssignedThisStep {
    /// Damage assigned by sources with deathtouch. Any nonzero amount is lethal (CR 702.2c).
    pub from_deathtouch: u32,
    /// Damage assigned by sources without deathtouch.
    pub from_other: u32,
}

/// CR 702.19b + CR 702.2c: lethal damage an attacker still has to assign to `blocker_id` before
/// it may assign excess damage.
///
/// CR 702.19b: "take into account damage already marked on the creature and damage from other
/// creatures that's being assigned during the same combat damage step". A blocker that already
/// has lethal damage, marked or committed, owes nothing, even to a deathtouch attacker; only a
/// blocker that is not yet lethal owes a deathtouch attacker 1 (CR 702.2c).
///
/// CR 510.1e (partial): `assigned` holds only damage committed by attackers whose assignments
/// were made earlier in this step. An earlier attacker cannot assign excess based on damage that
/// a later attacker will assign. For example, two 3/3 tramplers cannot each assign 2 to a shared
/// 2/4 blocker and 1 to the player. Tracked by Command Zone #285.
pub fn combat_lethal_damage_remaining(
    state: &GameState,
    blocker_id: ObjectId,
    source_has_deathtouch: bool,
    assigned: AssignedThisStep,
) -> u32 {
    let Some(obj) = state.objects.get(&blocker_id) else {
        return 1;
    };
    // CR 702.2c: nonzero damage already assigned by a deathtouch source is lethal.
    if assigned.from_deathtouch > 0 {
        return 0;
    }
    let counted = obj.damage_marked.saturating_add(assigned.from_other);
    // CR 702.19b: already lethal (marked plus committed) — nothing more is owed. Checked before
    // the deathtouch rule, which only decides how much more makes a creature lethal.
    if is_lethal_damage(state, obj, counted) {
        return 0;
    }
    // CR 702.2c + CR 702.19b: one damage from a deathtouch source is lethal.
    if source_has_deathtouch {
        return 1;
    }
    lethal_damage_threshold(state, obj).saturating_sub(counted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::combat_damage::lethal_damage_needed;
    use crate::game::scenario::{GameScenario, P1};

    /// A board with one P1 creature of the given toughness and marked damage.
    fn board(toughness: i32, marked: u32) -> (GameState, ObjectId) {
        let mut scenario = GameScenario::new_n_player(2, 42);
        let id = scenario
            .add_creature(P1, "Blocker", 1, toughness)
            .with_damage_marked(marked)
            .id();
        (scenario.build().state().clone(), id)
    }

    fn remaining(
        state: &GameState,
        id: ObjectId,
        deathtouch: bool,
        from_deathtouch: u32,
        from_other: u32,
    ) -> u32 {
        combat_lethal_damage_remaining(
            state,
            id,
            deathtouch,
            AssignedThisStep {
                from_deathtouch,
                from_other,
            },
        )
    }

    /// CR 120.6 + CR 704.5g: a total of 0 is never lethal; otherwise the total must reach
    /// toughness.
    #[test]
    fn is_lethal_damage_needs_a_nonzero_total_at_or_above_toughness() {
        let (state, id) = board(3, 0);
        let obj = &state.objects[&id];
        assert!(!is_lethal_damage(&state, obj, 0));
        assert!(!is_lethal_damage(&state, obj, 2));
        assert!(is_lethal_damage(&state, obj, 3));
        assert!(is_lethal_damage(&state, obj, 4));

        let (state, id) = board(0, 0);
        assert!(
            !is_lethal_damage(&state, &state.objects[&id], 0),
            "CR 704.5g: a creature with no damage marked has not been dealt lethal damage"
        );
    }

    /// CR 120.4a reading kept: `lethal_damage_needed` subtracts marked damage from toughness and
    /// returns 1 for any deathtouch source, even on a creature already carrying lethal damage.
    #[test]
    fn lethal_damage_needed_reads_toughness_minus_marked_damage() {
        let (state, id) = board(5, 2);
        assert_eq!(lethal_damage_needed(&state, id, false), 3);

        let (state, id) = board(2, 2);
        assert_eq!(lethal_damage_needed(&state, id, true), 1);
        assert_eq!(lethal_damage_needed(&state, ObjectId(9_999), false), 1);
    }

    /// CR 702.19b + CR 702.2c: the combat remainder counts committed damage, and an already
    /// lethal blocker owes 0 even to a deathtouch attacker.
    #[test]
    fn combat_lethal_damage_remaining_counts_committed_and_marked_damage() {
        let (state, id) = board(4, 0);
        assert_eq!(remaining(&state, id, false, 0, 3), 1);
        assert_eq!(remaining(&state, id, false, 0, 4), 0);
        assert_eq!(remaining(&state, id, false, 1, 0), 0, "CR 702.2c");
        assert_eq!(
            remaining(&state, id, true, 0, 0),
            1,
            "not yet lethal: a deathtouch source still owes 1"
        );
        assert_eq!(
            remaining(&state, id, false, 0, 0),
            lethal_damage_needed(&state, id, false),
            "nothing committed reproduces the excess reading"
        );

        let (state, id) = board(1, 1);
        assert_eq!(
            remaining(&state, id, true, 0, 0),
            0,
            "lethal damage already marked: a deathtouch attacker owes nothing more"
        );
        assert_eq!(remaining(&state, ObjectId(9_999), false, 0, 0), 1);
    }

    /// CR 702.19b: marked and committed damage add up. A 4-toughness blocker with 2 marked and 2
    /// committed this step already has lethal damage, so a later deathtouch trampler owes it 0,
    /// not the 1 that CR 702.2c would otherwise require.
    #[test]
    fn combat_lethal_damage_remaining_sums_marked_and_committed_damage() {
        let (state, id) = board(4, 2);
        assert_eq!(remaining(&state, id, true, 0, 2), 0);
        assert_eq!(remaining(&state, id, false, 0, 2), 0);
        assert_eq!(
            remaining(&state, id, true, 0, 1),
            1,
            "3 of 4 is not yet lethal: the deathtouch source still owes 1"
        );
    }
}
