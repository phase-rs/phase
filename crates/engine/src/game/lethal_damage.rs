//! CR 120.6 + CR 704.5g + CR 702.19b: the single authority for what counts as lethal damage on a
//! creature.
//!
//! Readers that ask "is this much damage lethal for this creature?" or "how much more must an
//! attacker assign before it may trample over?" read this module, so their answers cannot drift
//! apart: the lethal-damage state-based action (`sba::lethal_damage_candidates`), the combat
//! assignment minimum, the excess-damage reading (`combat_damage::lethal_damage_needed`), the
//! combat predictor (`combat_damage::combat_damage_to_defender`) and the AI's survival estimate
//! (`combat_damage::assess_combat_impact`).
//!
//! The combat predictor also asks these questions of a predicted battlefield: the one left after
//! an earlier combat damage step's state-based actions. `is_destroyed_by_lethal_damage` and
//! `lethal_damage_still_owed` take the objects predicted to have left it, whose static abilities
//! then no longer set the basis (CR 611.3b). Everything else is the live reading.

use crate::game::game_object::GameObject;
use crate::game::static_abilities::{
    carrier_static_applies, check_static_ability, functioning_static_carriers, StaticCheckContext,
};
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::statics::StaticMode;

/// CR 120.6: the characteristic a creature's marked damage is checked against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LethalDamageBasis {
    /// CR 120.6 + CR 704.5g: the default — toughness.
    Toughness,
    /// CR 613.11: a rule-modifying static substitutes power ("Lethal damage dealt to creatures
    /// you control is determined by their power rather than their toughness" — Zilortha,
    /// Strength Incarnate).
    Power,
}

/// CR 604.1 + CR 613.11 + CR 109.5: the lethal-damage basis for `object_id`, read live from the
/// functioning `LethalDamageDeterminedByPower` statics whose `affected` filter matches it. "You"
/// in that filter is the static's current controller, so a control change moves the effect, and
/// it ends as soon as its source stops functioning.
pub fn lethal_damage_basis(state: &GameState, object_id: ObjectId) -> LethalDamageBasis {
    lethal_damage_basis_after(state, object_id, &[])
}

/// CR 611.3b: the lethal-damage basis for `object_id` once the `departed` objects have left the
/// battlefield. A static ability's effect applies only while the permanent generating it is on
/// the battlefield, so a departed carrier's `LethalDamageDeterminedByPower` no longer applies.
///
/// `departed` is a forecast's prediction (the creatures an earlier combat damage step will
/// destroy); with none, this is the live reading. Every other gate is the live static lookup.
fn lethal_damage_basis_after(
    state: &GameState,
    object_id: ObjectId,
    departed: &[ObjectId],
) -> LethalDamageBasis {
    let mode = StaticMode::LethalDamageDeterminedByPower;
    let context = StaticCheckContext {
        target_id: Some(object_id),
        ..Default::default()
    };
    let power_basis = if departed.is_empty() {
        check_static_ability(state, mode, &context)
    } else {
        functioning_static_carriers(state, &mode)
            .into_iter()
            .filter(|carrier| !departed.contains(carrier))
            .any(|carrier| carrier_static_applies(state, carrier, &mode, &context))
    };
    if power_basis {
        LethalDamageBasis::Power
    } else {
        LethalDamageBasis::Toughness
    }
}

/// CR 120.6 + CR 704.5g: total marked damage at or above which `obj` has been dealt lethal
/// damage.
///
/// Lethal damage is "an amount of damage greater than or equal to a creature's toughness"
/// (glossary), with power substituted under the power basis. Either characteristic at or below 0
/// yields 0, so a 0-power creature under the power basis is already owed no lethal damage: a
/// trampler may assign it nothing and excess damage passes it entirely (CR 702.19b, CR 120.4a).
/// Destruction separately needs damage marked; that is `is_lethal_damage`'s nonzero test (CR
/// 704.5g: "it has damage marked on it"). A creature with toughness 0 or less is put into its
/// owner's graveyard by CR 704.5f, which is not a consequence of damage.
pub fn lethal_damage_threshold(state: &GameState, obj: &GameObject) -> u32 {
    lethal_damage_threshold_after(state, obj, &[])
}

/// `lethal_damage_threshold` once the `departed` objects have left the battlefield (CR 611.3b;
/// see `lethal_damage_basis_after`).
fn lethal_damage_threshold_after(
    state: &GameState,
    obj: &GameObject,
    departed: &[ObjectId],
) -> u32 {
    let characteristic = match lethal_damage_basis_after(state, obj.id, departed) {
        LethalDamageBasis::Toughness => obj.toughness,
        LethalDamageBasis::Power => obj.power,
    };
    characteristic.unwrap_or(0).max(0) as u32
}

/// CR 120.6 + CR 704.5g: `total` damage is lethal for `obj`: it is nonzero ("it has damage marked
/// on it") and at least the lethal threshold.
///
/// This does NOT apply CR 704.5g's "toughness greater than 0" precondition; callers that destroy
/// apply it themselves.
pub fn is_lethal_damage(state: &GameState, obj: &GameObject, total: u32) -> bool {
    is_lethal_damage_after(state, obj, total, &[])
}

/// `is_lethal_damage` once the `departed` objects have left the battlefield (CR 611.3b).
fn is_lethal_damage_after(
    state: &GameState,
    obj: &GameObject,
    total: u32,
    departed: &[ObjectId],
) -> bool {
    total > 0 && total >= lethal_damage_threshold_after(state, obj, departed)
}

/// CR 704.5g + CR 704.5h + CR 702.12b: the lethal-damage state-based action destroys `obj`, a
/// creature with `marked` total damage that has (`dealt_deathtouch_damage`) or has not been dealt
/// damage by a deathtouch source since state-based actions were last checked.
///
/// CR 704.5g: toughness greater than 0 and lethal damage marked. CR 704.5h + CR 702.2b:
/// toughness greater than 0 and dealt deathtouch damage. CR 702.12b: an indestructible
/// creature is destroyed by neither. Whether `obj` is a creature is the caller's question.
///
/// `departed` holds the objects a forecast predicts have already left the battlefield, whose
/// static abilities no longer set the lethal-damage basis (CR 611.3b). The state-based action
/// itself passes none: it reads the live battlefield.
pub fn is_destroyed_by_lethal_damage(
    state: &GameState,
    obj: &GameObject,
    marked: u32,
    dealt_deathtouch_damage: bool,
    departed: &[ObjectId],
) -> bool {
    obj.toughness.is_some_and(|toughness| toughness > 0)
        && (dealt_deathtouch_damage || is_lethal_damage_after(state, obj, marked, departed))
        && !obj.has_keyword(&Keyword::Indestructible)
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
/// has lethal damage, marked or committed, owes nothing, even to a deathtouch attacker. If
/// damage is still owed, deathtouch caps the remaining assignment at 1. A zero minimum stays
/// zero.
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
    lethal_damage_still_owed(
        state,
        obj,
        obj.damage_marked.saturating_add(assigned.from_other),
        source_has_deathtouch,
        &[],
    )
}

/// CR 702.19b + CR 702.2c: lethal damage an attacker still has to assign to `obj` before it may
/// assign excess, given the `counted` damage already marked on it or committed to it.
///
/// The core of `combat_lethal_damage_remaining`. A forecast of a later combat damage step calls
/// it directly, with the damage the earlier step marked counted in and the creatures that step's
/// state-based actions destroyed as `departed`: once a departed creature's static stops setting
/// the basis (CR 611.3b), the survivor's lethal damage is its toughness again.
pub fn lethal_damage_still_owed(
    state: &GameState,
    obj: &GameObject,
    counted: u32,
    source_has_deathtouch: bool,
    departed: &[ObjectId],
) -> u32 {
    // CR 702.19b: already lethal (marked plus committed) — nothing more is owed. Checked before
    // the deathtouch rule, which only decides how much more makes a creature lethal.
    if is_lethal_damage_after(state, obj, counted, departed) {
        return 0;
    }
    let remaining = lethal_damage_threshold_after(state, obj, departed).saturating_sub(counted);
    // CR 702.2c + CR 702.19b: one damage from a deathtouch source is lethal. It never raises
    // the requirement above the ordinary remainder, which is 0 for a creature whose lethal
    // damage is 0 (a 0-power creature under the power basis).
    if source_has_deathtouch {
        return remaining.min(1);
    }
    remaining
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::combat_damage::lethal_damage_needed;
    use crate::game::scenario::{GameScenario, P0, P1};

    /// Verbatim Oracle text (MTGJSON): Zilortha, Strength Incarnate.
    const ZILORTHA: &str = "Trample\nLethal damage dealt to creatures you control is determined \
         by their power rather than their toughness.";

    /// A board where P0 controls Zilortha (when `with_zilortha`), one P0 creature `mine` and one
    /// P1 creature `theirs`, both `power`/`toughness` with `marked` damage.
    fn zilortha_board(
        with_zilortha: bool,
        power: i32,
        toughness: i32,
        marked: u32,
    ) -> (GameState, ObjectId, ObjectId) {
        let mut scenario = GameScenario::new_n_player(2, 42);
        if with_zilortha {
            scenario
                .add_creature(P0, "Zilortha, Strength Incarnate", 7, 3)
                .from_oracle_text_with_keywords(&["Trample"], ZILORTHA);
        }
        let mine = scenario
            .add_creature(P0, "Mine", power, toughness)
            .with_damage_marked(marked)
            .id();
        let theirs = scenario
            .add_creature(P1, "Theirs", power, toughness)
            .with_damage_marked(marked)
            .id();
        (scenario.build().state().clone(), mine, theirs)
    }

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

    /// CR 613.11 + CR 109.5: the basis is power only for creatures the static's controller
    /// controls, and toughness without the static.
    #[test]
    fn lethal_damage_basis_reads_the_functioning_static() {
        let (state, mine, theirs) = zilortha_board(false, 1, 5, 0);
        assert_eq!(
            lethal_damage_basis(&state, mine),
            LethalDamageBasis::Toughness
        );
        assert_eq!(
            lethal_damage_basis(&state, theirs),
            LethalDamageBasis::Toughness
        );

        let (state, mine, theirs) = zilortha_board(true, 1, 5, 0);
        assert_eq!(lethal_damage_basis(&state, mine), LethalDamageBasis::Power);
        assert_eq!(
            lethal_damage_basis(&state, theirs),
            LethalDamageBasis::Toughness
        );
    }

    /// Glossary "Lethal Damage" with power substituted: the power threshold floors at 0, like the
    /// toughness arm (0 for toughness 0, CR 704.5f's territory). Destruction still needs damage
    /// marked: `is_lethal_damage` rejects a total of 0 even when the threshold is 0 (CR 704.5g;
    /// a creature with 0 power isn't destroyed unless it has at least 1 damage marked on it).
    #[test]
    fn lethal_damage_threshold_floors_power_at_zero() {
        let (mut state, mine, theirs) = zilortha_board(true, 0, 4, 0);
        assert_eq!(lethal_damage_threshold(&state, &state.objects[&mine]), 0);
        assert!(!is_lethal_damage(&state, &state.objects[&mine], 0));
        assert!(is_lethal_damage(&state, &state.objects[&mine], 1));
        assert_eq!(remaining(&state, mine, false, 0, 0), 0);
        assert_eq!(
            remaining(&state, mine, true, 0, 0),
            0,
            "deathtouch never raises the requirement above the ordinary 0"
        );
        state.objects.get_mut(&mine).unwrap().power = Some(-1);
        assert_eq!(lethal_damage_threshold(&state, &state.objects[&mine]), 0);
        state.objects.get_mut(&mine).unwrap().power = Some(5);
        assert_eq!(lethal_damage_threshold(&state, &state.objects[&mine]), 5);
        assert_eq!(lethal_damage_threshold(&state, &state.objects[&theirs]), 4);
        state.objects.get_mut(&theirs).unwrap().toughness = Some(0);
        assert_eq!(lethal_damage_threshold(&state, &state.objects[&theirs]), 0);
    }

    /// CR 120.4a + CR 702.19b under the power basis: the excess reading and the combat remainder
    /// both measure against power.
    #[test]
    fn excess_and_combat_remainder_read_the_power_basis() {
        let (state, mine, theirs) = zilortha_board(true, 5, 9, 2);
        assert_eq!(lethal_damage_needed(&state, mine, false), 3);
        assert_eq!(lethal_damage_needed(&state, theirs, false), 7);

        let (state, mine, theirs) = zilortha_board(true, 1, 5, 0);
        assert_eq!(remaining(&state, mine, false, 0, 1), 0);
        assert_eq!(remaining(&state, theirs, false, 0, 1), 4);
    }

    /// The board's Zilortha.
    fn zilortha_id(state: &GameState) -> ObjectId {
        *state
            .battlefield
            .iter()
            .find(|id| state.objects[*id].name == "Zilortha, Strength Incarnate")
            .expect("the board has Zilortha")
    }

    /// CR 704.5g + CR 611.3b: once Zilortha is predicted to have left the battlefield, a creature
    /// it made survive by power is checked against its toughness. Deathtouch damage destroys
    /// regardless (CR 704.5h); indestructible survives both (CR 702.12b).
    #[test]
    fn destruction_reads_the_basis_after_departures() {
        let (mut state, mine, theirs) = zilortha_board(true, 9, 3, 0);
        let z = zilortha_id(&state);
        let obj = &state.objects[&mine];
        assert!(!is_destroyed_by_lethal_damage(&state, obj, 5, false, &[]));
        assert!(
            is_destroyed_by_lethal_damage(&state, obj, 5, false, &[z]),
            "5 damage on toughness 3 is lethal once Zilortha is gone"
        );
        assert!(
            !is_destroyed_by_lethal_damage(&state, obj, 0, false, &[z]),
            "CR 704.5g: no damage marked, no destruction"
        );
        assert!(is_destroyed_by_lethal_damage(&state, obj, 1, true, &[]));
        assert!(
            !is_destroyed_by_lethal_damage(&state, obj, 5, false, &[mine]),
            "a departed object that carries no basis static changes nothing"
        );
        let other = &state.objects[&theirs];
        assert!(is_destroyed_by_lethal_damage(&state, other, 3, false, &[]));
        assert!(is_destroyed_by_lethal_damage(&state, other, 3, false, &[z]));

        state
            .objects
            .get_mut(&mine)
            .unwrap()
            .keywords
            .push(Keyword::Indestructible);
        let obj = &state.objects[&mine];
        assert!(!is_destroyed_by_lethal_damage(&state, obj, 5, false, &[z]));
        assert!(!is_destroyed_by_lethal_damage(&state, obj, 5, true, &[z]));
    }

    /// CR 702.19b + CR 611.3b: the lethal damage still owed to a survivor counts its damage and
    /// measures it on the battlefield the departures left: 12/8 with 5 counted owes 7 by power,
    /// 3 by toughness once Zilortha is gone, and a deathtouch source owes at most 1.
    #[test]
    fn still_owed_reads_the_basis_after_departures() {
        let (state, mine, theirs) = zilortha_board(true, 12, 8, 0);
        let z = zilortha_id(&state);
        let obj = &state.objects[&mine];
        assert_eq!(lethal_damage_still_owed(&state, obj, 5, false, &[]), 7);
        assert_eq!(lethal_damage_still_owed(&state, obj, 5, false, &[z]), 3);
        assert_eq!(lethal_damage_still_owed(&state, obj, 5, true, &[z]), 1);
        assert_eq!(
            lethal_damage_still_owed(&state, obj, 8, true, &[z]),
            0,
            "already lethal by toughness: nothing more is owed, even to deathtouch"
        );
        assert_eq!(
            lethal_damage_still_owed(&state, obj, 0, false, &[]),
            combat_lethal_damage_remaining(&state, mine, false, AssignedThisStep::default()),
            "with no departures it is the live combat remainder"
        );
        let other = &state.objects[&theirs];
        assert_eq!(lethal_damage_still_owed(&state, other, 5, false, &[z]), 3);
    }
}
