//! CR 122.1 + CR 122.6: "put an additional counter of that kind on that
//! permanent" — The Caves of Androzani II/III.
//!
//! Resolves `Effect::PutChosenCounter`. Reads the counter kind the preceding
//! `Effect::ChooseCounterKind` retained in resolution state, then delegates to
//! the single counter-placement
//! authority (`counters::resolve_add`) via a synthetic `Effect::PutCounter` so
//! all counter placement — replacement effects, evolve triggers, distribution —
//! flows through one code path.
//!
//! No-op when no counter kind was chosen (the `ChooseCounterKind` was skipped
//! because the object had no counters, per CR 608.2d).

use crate::types::ability::{
    ChosenCounterCountCondition, Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter,
};
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 608.2c + CR 122.1: Evaluate an optional predicate against the resolved
/// target's count of the counter kind chosen earlier in this resolution.
///
/// This is the single authority shared by normal resolution and the optional
/// effect feasibility check, so an impossible optional placement is never
/// offered as a choice under CR 608.2d.
pub(crate) fn target_condition_is_satisfied(
    state: &GameState,
    ability: &ResolvedAbility,
    target: &TargetFilter,
    chosen_kind: &CounterType,
    condition: Option<&ChosenCounterCountCondition>,
) -> bool {
    let Some(condition) = condition else {
        return true;
    };
    let Some(target_id) =
        crate::game::targeting::resolved_object_ids_for_filter(state, ability, target)
            .into_iter()
            .next()
    else {
        return false;
    };
    let count = state
        .objects
        .get(&target_id)
        .and_then(|object| object.counters.get(chosen_kind))
        .copied()
        .map(crate::game::arithmetic::u32_to_i32_saturating)
        .unwrap_or(0);
    let rhs = crate::game::quantity::resolve_quantity_for_ability_condition(
        state,
        &condition.rhs,
        ability,
    );
    condition.comparator.evaluate(count, rhs)
}

/// CR 122.1 + CR 122.6: Resolve `Effect::PutChosenCounter`.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (target, count, target_condition) = match &ability.effect {
        Effect::PutChosenCounter {
            target,
            count,
            target_condition,
        } => (target.clone(), count.clone(), target_condition.as_ref()),
        _ => return Err(EffectError::MissingParam("PutChosenCounter".to_string())),
    };

    // Read the immediately preceding resolution choice. `ChooseCounterKind`
    // clears this slot for its zero-kind branch, so an earlier same-ID source
    // incarnation cannot supply a stale counter kind here.
    let chosen_kind = crate::game::effects::choose_counter_kind::chosen_counter_kind(state);

    let Some(counter_type) = chosen_kind else {
        // CR 608.2d: the counter-kind choice was skipped (no counters on the
        // object) — there is no "that kind", so nothing is added.
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    };

    // CR 608.2c: Later text can condition this instruction on the target's
    // current count of the kind chosen by the preceding instruction (Aven
    // Courier). The condition is checked after the kind is known and before
    // counter placement.
    if !target_condition_is_satisfied(state, ability, &target, &counter_type, target_condition) {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    }

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
    use crate::types::ability::{
        Comparator, QuantityExpr, QuantityModification, ReplacementDefinition, TargetFilter,
        TargetRef,
    };
    use crate::types::counter::CounterType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::replacements::ReplacementEvent;

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
                target_condition: None,
            },
            vec![TargetRef::Object(target_obj)],
            source,
            PlayerId(0),
        );
        a.targets = vec![TargetRef::Object(target_obj)];
        a
    }

    fn absent_chosen_counter_ability(source: ObjectId, target_obj: ObjectId) -> ResolvedAbility {
        let mut a = ability(source, target_obj);
        let Effect::PutChosenCounter {
            target_condition, ..
        } = &mut a.effect
        else {
            unreachable!("helper constructs PutChosenCounter");
        };
        *target_condition = Some(ChosenCounterCountCondition {
            comparator: Comparator::EQ,
            rhs: QuantityExpr::Fixed { value: 0 },
        });
        a
    }

    /// CR 122.1 + CR 122.6: The resolution-scoped chosen kind drives one
    /// counter of that kind is added to the (parent-target) object.
    #[test]
    fn adds_one_counter_of_chosen_kind() {
        let (mut state, source, target) = setup();
        state.chosen_counter_kind_this_resolution = Some(CounterType::Stun);
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

    /// CR 608.2c + CR 122.1: An EQ-zero predicate permits placement when the
    /// resolved target has no counter of the chosen kind.
    #[test]
    fn target_condition_allows_absent_chosen_kind() {
        let (mut state, source, target) = setup();
        state.chosen_counter_kind_this_resolution = Some(CounterType::Stun);

        resolve(
            &mut state,
            &absent_chosen_counter_ability(source, target),
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(
            state.objects[&target].counters.get(&CounterType::Stun),
            Some(&1)
        );
    }

    /// CR 608.2c + CR 122.1: The same predicate suppresses placement when the
    /// resolved target already has a counter of the chosen kind.
    #[test]
    fn target_condition_blocks_present_chosen_kind() {
        let (mut state, source, target) = setup();
        state.chosen_counter_kind_this_resolution = Some(CounterType::Stun);
        state
            .objects
            .get_mut(&target)
            .unwrap()
            .counters
            .insert(CounterType::Stun, 1);

        resolve(
            &mut state,
            &absent_chosen_counter_ability(source, target),
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(
            state.objects[&target].counters.get(&CounterType::Stun),
            Some(&1),
            "a false chosen-counter predicate makes the put a no-op"
        );
    }

    /// CR 608.2c + CR 122.1: an indexed predicate and its counter placement
    /// resolve the same flattened root target slot even when the tail node's
    /// local target list contains only the most-recent slot.
    #[test]
    fn parent_target_slot_condition_and_placement_share_chain_root() {
        use crate::types::game_state::{StackEntry, StackEntryKind};

        let (mut state, source, first) = setup();
        let second = crate::game::zones::create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Second Target".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        state.chosen_counter_kind_this_resolution = Some(CounterType::Stun);
        state
            .objects
            .get_mut(&second)
            .unwrap()
            .counters
            .insert(CounterType::Stun, 1);

        let root = ResolvedAbility::new(
            Effect::TargetOnly {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(first)],
            source,
            PlayerId(0),
        )
        .sub_ability(ResolvedAbility::new(
            Effect::TargetOnly {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(second)],
            source,
            PlayerId(0),
        ));
        state.resolving_stack_entry = Some(StackEntry {
            id: ObjectId(500),
            source_id: source,
            controller: PlayerId(0),
            kind: StackEntryKind::ActivatedAbility {
                source_id: source,
                ability: Box::new(root),
            },
        });

        let tail = ResolvedAbility::new(
            Effect::PutChosenCounter {
                target: TargetFilter::ParentTargetSlot { index: 0 },
                count: QuantityExpr::Fixed { value: 1 },
                target_condition: Some(ChosenCounterCountCondition {
                    comparator: Comparator::EQ,
                    rhs: QuantityExpr::Fixed { value: 0 },
                }),
            },
            vec![TargetRef::Object(second)],
            source,
            PlayerId(0),
        );

        resolve(&mut state, &tail, &mut Vec::new()).unwrap();

        assert_eq!(
            state.objects[&first].counters.get(&CounterType::Stun),
            Some(&1),
            "the absent-kind predicate and placement both resolve root slot 0"
        );
        assert_eq!(
            state.objects[&second].counters.get(&CounterType::Stun),
            Some(&1),
            "the tail-local slot must be neither checked nor modified"
        );
    }

    /// CR 614.1a + CR 122.1: PutChosenCounter delegates to the ordinary
    /// PutCounter authority, so counter-addition replacement effects still
    /// transform the placement.
    #[test]
    fn placement_uses_counter_replacement_pipeline() {
        let (mut state, source, target) = setup();
        state.chosen_counter_kind_this_resolution = Some(CounterType::Stun);
        let doubler = crate::game::zones::create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Counter Doubler".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        let mut replacement = ReplacementDefinition::new(ReplacementEvent::AddCounter);
        replacement.valid_card = Some(TargetFilter::Any);
        replacement.quantity_modification = Some(QuantityModification::DOUBLE);
        state
            .objects
            .get_mut(&doubler)
            .unwrap()
            .replacement_definitions
            .push(replacement);

        resolve(&mut state, &ability(source, target), &mut Vec::new()).unwrap();
        assert_eq!(
            state.objects[&target].counters.get(&CounterType::Stun),
            Some(&2),
            "the synthetic PutCounter must pass through AddCounter replacements"
        );
    }

    /// CR 400.7 + CR 608.2c: A source may retain an older counter choice (or
    /// have left and supplied LKI), but only the current resolution's explicit
    /// result can authorize "that kind".
    #[test]
    fn does_not_read_a_stale_counter_kind_from_the_source() {
        let (mut state, source, target) = setup();
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .chosen_attributes
            .push(crate::types::ability::ChosenAttribute::Counter(
                CounterType::Stun,
            ));
        state
            .objects
            .get_mut(&target)
            .unwrap()
            .counters
            .insert(CounterType::Stun, 1);
        let before = state.objects[&target].counters.clone();

        resolve(&mut state, &ability(source, target), &mut Vec::new()).unwrap();
        assert_eq!(state.objects[&target].counters, before);
    }
}
