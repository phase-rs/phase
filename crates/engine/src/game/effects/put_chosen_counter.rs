//! CR 122.1 + CR 122.6: "put an additional counter of that kind on that
//! permanent" — The Caves of Androzani II/III.
//!
//! Resolves `Effect::PutChosenCounter`. Reads the counter kind the preceding
//! `Effect::ChooseCounterKind` persisted onto the source as
//! `ChosenAttribute::Counter`, then delegates to the single counter-placement
//! authority (`counters::resolve_add`) via a synthetic `Effect::PutCounter` so
//! all counter placement — replacement effects, evolve triggers, distribution —
//! flows through one code path. Mirrors the `ChosenAttribute::Keyword` →
//! `ContinuousModification::RemoveChosenKeyword` consume precedent.
//!
//! No-op when no counter kind was chosen (the `ChooseCounterKind` was skipped
//! because the object had no counters, per CR 608.2d).

use crate::types::ability::{ChosenAttribute, Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 122.1 + CR 122.6: Resolve `Effect::PutChosenCounter`.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (target, count) = match &ability.effect {
        Effect::PutChosenCounter { target, count } => (target.clone(), count.clone()),
        _ => return Err(EffectError::MissingParam("PutChosenCounter".to_string())),
    };

    // Read the most recently chosen counter kind from the source.
    let chosen_kind: Option<CounterType> = state.objects.get(&ability.source_id).and_then(|src| {
        src.chosen_attributes
            .iter()
            .rev()
            .find_map(|attr| match attr {
                ChosenAttribute::Counter(kind) => Some(kind.clone()),
                _ => None,
            })
    });

    let Some(counter_type) = chosen_kind else {
        // CR 608.2d: the counter-kind choice was skipped (no counters on the
        // object) — there is no "that kind", so nothing is added.
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        return Ok(());
    };

    // CR 122.1 + CR 122.6: Delegate to the single counter-placement authority.
    // The synthetic `PutCounter` inherits the resolving ability's targets so a
    // `ParentTarget` resolves to the current `repeat_for` iteration object.
    let mut synthetic = ability.clone();
    synthetic.sub_ability = None;
    synthetic.effect = Effect::PutCounter {
        counter_type,
        count,
        target,
    };
    crate::game::effects::counters::resolve_add(state, &synthetic, events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{QuantityExpr, TargetFilter, TargetRef};
    use crate::types::counter::CounterType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    fn setup() -> (GameState, ObjectId, ObjectId) {
        let mut state = GameState::new_two_player(1);
        let source = crate::game::zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        let target = crate::game::zones::create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Target".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        (state, source, target)
    }

    fn ability(source: ObjectId, target_obj: ObjectId) -> ResolvedAbility {
        let mut a = ResolvedAbility::new(
            Effect::PutChosenCounter {
                target: TargetFilter::ParentTarget,
                count: QuantityExpr::Fixed { value: 1 },
            },
            vec![TargetRef::Object(target_obj)],
            source,
            PlayerId(0),
        );
        a.targets = vec![TargetRef::Object(target_obj)];
        a
    }

    /// CR 122.1 + CR 122.6: The chosen kind is read from the source and one
    /// counter of that kind is added to the (parent-target) object.
    #[test]
    fn adds_one_counter_of_chosen_kind() {
        let (mut state, source, target) = setup();
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .chosen_attributes
            .push(ChosenAttribute::Counter(CounterType::Stun));
        state
            .objects
            .get_mut(&target)
            .unwrap()
            .counters
            .insert(CounterType::Stun, 1);

        let mut events = Vec::new();
        resolve(&mut state, &ability(source, target), &mut events).unwrap();
        assert_eq!(
            state.objects[&target].counters.get(&CounterType::Stun),
            Some(&2),
            "one Stun counter of the chosen kind is added"
        );
    }

    /// CR 608.2d: When no counter kind was chosen (the choose was skipped), the
    /// put is a no-op — no counters are added.
    #[test]
    fn no_chosen_kind_is_noop() {
        let (mut state, source, target) = setup();
        state
            .objects
            .get_mut(&target)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 1);
        let before = state.objects[&target].counters.clone();

        let mut events = Vec::new();
        resolve(&mut state, &ability(source, target), &mut events).unwrap();
        assert_eq!(
            state.objects[&target].counters, before,
            "no chosen kind → no counters added"
        );
    }
}
