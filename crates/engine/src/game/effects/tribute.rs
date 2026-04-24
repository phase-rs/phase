//! CR 702.104a: Tribute resolver.
//!
//! `Effect::Tribute { count }` is the second-stage effect in the synthesized
//! Tribute ETB replacement chain (see `database::synthesis::synthesize_tribute_intrinsics`).
//! The preceding `Effect::Choose { Opponent, persist: true }` has already persisted
//! the opponent selection on the source as `ChosenAttribute::Player`.
//!
//! This resolver:
//!   1. Reads the persisted opponent from the source object.
//!   2. Sets `WaitingFor::TributeChoice` — the chosen opponent decides pay or decline.
//!   3. The `DecideOptionalEffect` handler for `TributeChoice` applies the +1/+1
//!      counters on accept and persists the outcome as `ChosenAttribute::TributeOutcome`
//!      so the companion trigger (CR 702.104b) can read it.

use crate::types::ability::{
    ChosenAttribute, Effect, EffectError, EffectKind, ResolvedAbility, TributeOutcome,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};

/// CR 702.104a: Prompt the previously chosen opponent pay-or-decline.
///
/// The opponent must already be persisted on the source object as
/// `ChosenAttribute::Player` (from a preceding `Effect::Choose { Opponent, persist: true }`
/// step in the Tribute ETB replacement chain).
///
/// If no opponent was persisted (e.g., eliminated between the Choose step and this
/// step), records the outcome as `Declined` so the companion "if tribute wasn't
/// paid" trigger (CR 702.104b) still fires correctly.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let count = match &ability.effect {
        Effect::Tribute { count } => *count,
        _ => {
            return Err(EffectError::InvalidParam(
                "expected Tribute effect".to_string(),
            ))
        }
    };

    let source_id = ability.source_id;

    // Read the opponent persisted by the preceding Effect::Choose { Opponent, persist: true }.
    // CR 702.104a: "choose an opponent" — that opponent then decides pay/decline.
    let chosen_opponent = state.objects.get(&source_id).and_then(|obj| {
        obj.chosen_attributes
            .iter()
            .rev() // Most recent choice wins if multiple Tributes share a source.
            .find_map(|attr| match attr {
                ChosenAttribute::Player(pid) => Some(*pid),
                _ => None,
            })
    });

    let Some(opponent) = chosen_opponent else {
        // No chooser available (opponent eliminated, or missing persist step).
        // Record Declined so the trigger (CR 702.104b) evaluates correctly.
        record_outcome(state, source_id, TributeOutcome::Declined);
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::Tribute,
            source_id,
        });
        return Ok(());
    };

    state.waiting_for = WaitingFor::TributeChoice {
        player: opponent,
        source_id,
        count,
    };

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Tribute,
        source_id,
    });

    Ok(())
}

/// Apply the paid outcome: add the +1/+1 counters and persist `TributeOutcome::Paid`.
/// CR 702.104a: "That player may put an additional N +1/+1 counters on it."
pub(crate) fn apply_paid(
    state: &mut GameState,
    source_id: crate::types::identifiers::ObjectId,
    count: u32,
    events: &mut Vec<GameEvent>,
) {
    if count > 0 {
        super::counters::add_counter_with_replacement(
            state,
            source_id,
            crate::types::counter::CounterType::Plus1Plus1,
            count,
            events,
        );
    }
    record_outcome(state, source_id, TributeOutcome::Paid);
}

/// Apply the declined outcome: persist `TributeOutcome::Declined` so the
/// companion "if tribute wasn't paid" trigger (CR 702.104b) evaluates true.
pub(crate) fn apply_declined(
    state: &mut GameState,
    source_id: crate::types::identifiers::ObjectId,
) {
    record_outcome(state, source_id, TributeOutcome::Declined);
}

fn record_outcome(
    state: &mut GameState,
    source_id: crate::types::identifiers::ObjectId,
    outcome: TributeOutcome,
) {
    if let Some(obj) = state.objects.get_mut(&source_id) {
        // CR 702.104b: Only one outcome is meaningful per ETB; replace any prior.
        obj.chosen_attributes
            .retain(|attr| !matches!(attr, ChosenAttribute::TributeOutcome(_)));
        obj.chosen_attributes
            .push(ChosenAttribute::TributeOutcome(outcome));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::game_object::GameObject;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn seed_tribute_source(state: &mut GameState, id: ObjectId) {
        let obj = GameObject::new(
            id,
            CardId(id.0),
            PlayerId(0),
            "Tribute Test".into(),
            Zone::Battlefield,
        );
        state.objects.insert(id, obj);
        state.battlefield.push_back(id);
    }

    #[test]
    fn apply_paid_records_outcome_and_adds_counters() {
        let mut state = GameState::new_two_player(1);
        seed_tribute_source(&mut state, ObjectId(500));

        let mut events = Vec::new();
        apply_paid(&mut state, ObjectId(500), 2, &mut events);

        let obj = state.objects.get(&ObjectId(500)).unwrap();
        assert_eq!(
            obj.counters
                .get(&crate::types::counter::CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            2,
        );
        assert!(obj
            .chosen_attributes
            .iter()
            .any(|a| matches!(a, ChosenAttribute::TributeOutcome(TributeOutcome::Paid))));
    }

    #[test]
    fn apply_declined_records_outcome_without_counters() {
        let mut state = GameState::new_two_player(1);
        seed_tribute_source(&mut state, ObjectId(501));

        apply_declined(&mut state, ObjectId(501));

        let obj = state.objects.get(&ObjectId(501)).unwrap();
        assert_eq!(
            obj.counters
                .get(&crate::types::counter::CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            0,
        );
        assert!(obj
            .chosen_attributes
            .iter()
            .any(|a| matches!(a, ChosenAttribute::TributeOutcome(TributeOutcome::Declined))));
    }
}
