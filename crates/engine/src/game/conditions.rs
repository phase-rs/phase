//! Shared game-state predicate helpers used by multiple condition evaluators.
//!
//! Each function here eliminates duplication across two or more of:
//! `layers.rs` (StaticCondition), `triggers.rs` (TriggerCondition),
//! `effects/mod.rs` (AbilityCondition), and `replacement.rs` (ReplacementCondition).

use crate::game::game_object::GameObject;
use crate::types::counter::CounterMatch;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// CR 122.1: True when `obj` has at least `minimum` (and at most `maximum` if specified)
/// counters matching `counters`. `CounterMatch::Any` sums all counter types;
/// `CounterMatch::OfType(ct)` matches only that counter type.
/// Used for HasCounters evaluation in StaticCondition and TriggerCondition.
pub(crate) fn counter_condition_matches(
    obj: &GameObject,
    counters: &CounterMatch,
    minimum: u32,
    maximum: Option<u32>,
) -> bool {
    let count: u32 = match counters {
        CounterMatch::Any => obj.counters.values().sum(),
        CounterMatch::OfType(ct) => obj.counters.get(ct).copied().unwrap_or(0),
    };
    count >= minimum && maximum.is_none_or(|max| count <= max)
}

/// CR 110.5b + CR 110.5d: True when the source object is on the battlefield AND tapped.
/// Use in static-condition evaluation where CR 110.5d requires the zone guard
/// (cards not on the battlefield are neither tapped nor untapped).
pub(crate) fn eval_source_is_tapped_on_battlefield(state: &GameState, source_id: ObjectId) -> bool {
    state
        .objects
        .get(&source_id)
        .is_some_and(|obj| obj.zone == Zone::Battlefield && obj.tapped)
}

/// CR 110.5b: True when the source object is tapped, regardless of zone.
/// Use in trigger and ability evaluation where the source's zone is already
/// constrained by the functioning-abilities path.
pub(crate) fn eval_source_is_tapped(state: &GameState, source_id: ObjectId) -> bool {
    state.objects.get(&source_id).is_some_and(|obj| obj.tapped)
}

/// CR 614.12c + CR 607.2d: True when the source object's chosen label matches
/// the given anchor word, case-insensitively.
pub(crate) fn eval_chosen_label_is(state: &GameState, source_id: ObjectId, label: &str) -> bool {
    state
        .objects
        .get(&source_id)
        .and_then(|obj| obj.chosen_label())
        .is_some_and(|chosen| chosen.eq_ignore_ascii_case(label))
}

/// CR 716.2a: True when the source Class enchantment is at or above the given level.
/// Does NOT include a battlefield zone guard — callers that require the source to be
/// on the battlefield (e.g. `replacement.rs`) must apply the guard before calling.
pub(crate) fn eval_class_level_ge(state: &GameState, source_id: ObjectId, level: u8) -> bool {
    state
        .objects
        .get(&source_id)
        .and_then(|obj| obj.class_level)
        .is_some_and(|current| current >= level)
}

/// CR 113.6b: True when the source object is in the specified zone.
pub(crate) fn eval_source_in_zone(state: &GameState, source_id: ObjectId, zone: Zone) -> bool {
    state
        .objects
        .get(&source_id)
        .is_some_and(|obj| obj.zone == zone)
}

/// CR 508.1k: True when the source creature is currently an attacking creature.
pub(crate) fn eval_source_is_attacking(state: &GameState, source_id: ObjectId) -> bool {
    state
        .combat
        .as_ref()
        .is_some_and(|combat| combat.attackers.iter().any(|a| a.object_id == source_id))
}

/// CR 725.1: True when no player holds the monarch designation.
pub(crate) fn eval_no_monarch(state: &GameState) -> bool {
    state.monarch.is_none()
}

/// CR 725.1: True when the given player is the monarch.
pub(crate) fn eval_is_monarch(state: &GameState, controller: PlayerId) -> bool {
    state.monarch == Some(controller)
}

/// CR 702.131a + CR 702.131c: True when the given player has the city's blessing.
pub(crate) fn eval_has_city_blessing(state: &GameState, controller: PlayerId) -> bool {
    state.city_blessing.contains(&controller)
}

/// CR 400.7: True when the source permanent entered the battlefield this turn.
pub(crate) fn eval_source_entered_this_turn(state: &GameState, source_id: ObjectId) -> bool {
    state
        .objects
        .get(&source_id)
        .is_some_and(|obj| obj.entered_battlefield_turn == Some(state.turn_number))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::game_state::GameState;
    use crate::types::player::PlayerId;
    use crate::types::CardId;

    /// CR 110.5d: eval_source_is_tapped_on_battlefield must return false when the
    /// object is tapped but not on the battlefield (e.g. in the graveyard). The
    /// zone-guard-free eval_source_is_tapped must return true for the same object.
    #[test]
    fn tapped_zone_guard_distinguishes_battlefield_vs_graveyard() {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Test".to_string(),
            Zone::Graveyard,
        );
        state.objects.get_mut(&id).unwrap().tapped = true;

        // Zone guard: tapped but NOT on battlefield → false
        assert!(!eval_source_is_tapped_on_battlefield(&state, id));
        // No zone guard: tapped regardless of zone → true
        assert!(eval_source_is_tapped(&state, id));
    }

    #[test]
    fn tapped_on_battlefield_returns_true() {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Test".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&id).unwrap().tapped = true;

        assert!(eval_source_is_tapped_on_battlefield(&state, id));
        assert!(eval_source_is_tapped(&state, id));
    }
}
