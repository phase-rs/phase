//! CR 701.64: Harness — the keyword action that grants a permanent the
//! harnessed designation.
//!
//! CR 701.64a: "Harness [this permanent]" means "If this permanent isn't
//! harnessed, it becomes harnessed." Like Monstrosity (CR 701.37a), this is an
//! idempotent designation toggle with no other rules effect — there are no
//! counters or sub-choices, so the resolver simply sets the marker on the
//! source permanent.
//!
//! CR 701.64b: Harnessed is a pure marker (neither an ability nor a copiable
//! value) that stays until the permanent leaves the battlefield. The marker is
//! cleared by `GameObject::reset_for_battlefield_exit` (CR 400.7). It is read by
//! the ∞ (Infinity) ability gate (`SourceIsHarnessed`, CR 702.186b).

use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 701.64a: Harness the source permanent — if it isn't already harnessed,
/// it becomes harnessed.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    if !matches!(ability.effect, Effect::Harness) {
        return Ok(());
    }

    let source_id = ability.source_id;

    // CR 701.64a: If already harnessed (or the source has left the battlefield),
    // do nothing. The idempotent guard mirrors Monstrosity (CR 701.37a).
    match state.objects.get(&source_id) {
        Some(obj) if obj.harnessed => return Ok(()),
        Some(_) => {}
        None => return Ok(()),
    }

    // CR 701.64a + CR 701.64b: Set the harnessed designation.
    if let Some(obj) = state.objects.get_mut(&source_id) {
        obj.harnessed = true;
    }

    // CR 701.64a: Emit EffectResolved so any "When ~ becomes harnessed" triggers
    // can fire (no printed card uses this yet, but the marker-set parallels
    // Monstrosity's "becomes monstrous" event).
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Harness,
        source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones;
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn setup_artifact(state: &mut GameState) -> ObjectId {
        let id = zones::create_object(
            state,
            CardId(1),
            PlayerId(0),
            "Test Artifact".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        id
    }

    fn make_harness_ability(source_id: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(Effect::Harness, vec![], source_id, PlayerId(0))
    }

    #[test]
    fn harness_sets_designation() {
        let mut state = GameState::new_two_player(42);
        let id = setup_artifact(&mut state);
        assert!(!state.objects.get(&id).unwrap().harnessed);

        let ability = make_harness_ability(id);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            state.objects.get(&id).unwrap().harnessed,
            "harness must set the harnessed designation"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                GameEvent::EffectResolved {
                    kind: EffectKind::Harness,
                    ..
                }
            )),
            "harness must emit an EffectResolved event"
        );
    }

    #[test]
    fn harness_is_idempotent() {
        // CR 701.64a: harnessing an already-harnessed permanent does nothing
        // (and emits no second EffectResolved).
        let mut state = GameState::new_two_player(42);
        let id = setup_artifact(&mut state);
        state.objects.get_mut(&id).unwrap().harnessed = true;

        let ability = make_harness_ability(id);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.objects.get(&id).unwrap().harnessed);
        assert!(
            events.is_empty(),
            "re-harnessing already-harnessed permanent must not emit an event"
        );
    }
}
