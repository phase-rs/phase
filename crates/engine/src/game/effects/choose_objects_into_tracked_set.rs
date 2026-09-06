//! CR 608.2c: Interactive battlefield-object selection into the chain's
//! tracked object set.
//!
//! Resolves `Effect::ChooseObjectsIntoTrackedSet`. The `chooser` field is a
//! `TargetFilter` resolved per-instance (like `Effect::PayCost.payer`) so an
//! "at the beginning of each player's upkeep" trigger prompts the player whose
//! upkeep it is — not a fixed controller. The chosen objects are written into
//! a fresh tracked set so downstream effects ("pay {N} for each ... chosen
//! this way", "untap those creatures") resolve against the exact selection.

use crate::game::effects::counters::counter_removal_blocked;
use crate::game::filter::{matches_target_filter, FilterContext};
use crate::game::targeting::resolve_effect_player_ref;
use crate::types::ability::{
    Effect, EffectError, EffectKind, ObjectSelectionCardinality, ObjectSelectionEligibility,
    ResolvedAbility, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};

/// CR 608.2c: Resolve `Effect::ChooseObjectsIntoTrackedSet` — surface a
/// `WaitingFor::ChooseObjectsSelection` prompt for the affected player.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (chooser_filter, filter, min, max, cardinality, eligibility) = match &ability.effect {
        Effect::ChooseObjectsIntoTrackedSet {
            chooser,
            filter,
            min,
            max,
            cardinality,
            eligibility,
        } => (
            chooser.clone(),
            filter.clone(),
            *min,
            *max,
            *cardinality,
            eligibility.clone(),
        ),
        _ => {
            return Err(EffectError::MissingParam(
                "ChooseObjectsIntoTrackedSet".to_string(),
            ))
        }
    };

    // CR 608.2c: Resolve the chooser to the affected player — the same
    // single-authority player-ref resolver used by `PayCost.payer`. For an
    // "each player's upkeep" trigger this is the upkeep player.
    let Some(chooser) = resolve_effect_player_ref(state, ability, &chooser_filter) else {
        // No resolvable chooser — nothing to select; resolve as a no-op.
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    };

    // Evaluate `filter` against the chooser's battlefield permanents. The
    // filter's "they control" controller constraint resolves against the
    // ability controller, so bind the filter context controller to the
    // chooser (mirrors `pay.rs`'s payer-rebinding pattern).
    let ctx = FilterContext::from_ability_with_controller(ability, chooser);
    let eligible = eligible_targets(state, &filter, &ctx, eligibility.as_ref());

    // CR 609.3: If a resolving effect asks for more objects than are
    // available, the player chooses all that are available. Publish an
    // achievable runtime range at this trust seam so every consumer (engine,
    // AI, and client) sees the same liveness-preserving cardinality.
    let (min, max) = match cardinality {
        // CR 609.3: a mandatory instruction does as much as possible. An
        // optional exact selection is screened before this resolver can run,
        // so retain its exact published cardinality for that decision path.
        Some(ObjectSelectionCardinality::Exactly { count }) if ability.optional => {
            (count, Some(count))
        }
        Some(ObjectSelectionCardinality::Exactly { count }) => {
            let available = u32::try_from(eligible.len()).unwrap_or(u32::MAX);
            let achievable = count.min(available);
            (achievable, Some(achievable))
        }
        None => {
            let available = u32::try_from(eligible.len()).unwrap_or(u32::MAX);
            let max = max.map(|maximum| maximum.min(available));
            (min.min(max.unwrap_or(available)), max)
        }
    };

    // CR 608.2c: Surface the interactive selection. Even with an empty
    // `eligible` set the prompt is raised — the player's act of submitting an
    // empty selection IS a legal resolution-time choice (CR 608.2d: choosing
    // zero of an "up to N" selection while applying the effect), and the
    // downstream `ScaledMana { times: 0 }` payment is a no-op {0}-cost SUCCESS
    // (CR 118.5).
    // CR 608.2: carry the triggering event across the interactive selection
    // pause so the stashed `PayCost { payer: TriggeringPlayer }` continuation
    // resolves the payer correctly. PART 1 has already restored
    // `current_trigger_event`, so this clone captures the real event.
    state.waiting_for = WaitingFor::ChooseObjectsSelection {
        player: chooser,
        eligible,
        min,
        max,
        trigger_event: state.current_trigger_event.clone(),
    };

    Ok(())
}

/// CR 122.1 + CR 101.2: A counter-removal selection may include only objects
/// that hold a removable matching counter. This is shared by the prompt and the
/// optional-effect feasibility gate so they cannot disagree about availability.
fn eligible_targets(
    state: &GameState,
    filter: &crate::types::ability::TargetFilter,
    ctx: &FilterContext,
    eligibility: Option<&ObjectSelectionEligibility>,
) -> Vec<TargetRef> {
    state
        .battlefield
        .iter()
        .filter(|&&obj_id| matches_target_filter(state, obj_id, filter, ctx))
        .filter(|&&obj_id| match eligibility {
            None => true,
            Some(ObjectSelectionEligibility::RemovableCounter { counter_type }) => {
                state.objects.get(&obj_id).is_some_and(|object| {
                    object.counters.iter().any(|(kind, &available)| {
                        counter_type
                            .as_ref()
                            .is_none_or(|expected| expected == kind)
                            && available > 0
                            && !counter_removal_blocked(state, obj_id, kind)
                    })
                })
            }
        })
        .map(|&obj_id| TargetRef::Object(obj_id))
        .collect()
}

/// CR 608.2d: An optional exact selection is unavailable unless its complete
/// selection cardinality can be met from the same eligible set shown by the UI.
pub(crate) fn optional_exact_selection_is_infeasible(
    state: &GameState,
    ability: &ResolvedAbility,
) -> bool {
    let Effect::ChooseObjectsIntoTrackedSet {
        chooser,
        filter,
        cardinality: Some(ObjectSelectionCardinality::Exactly { count }),
        eligibility,
        ..
    } = &ability.effect
    else {
        return false;
    };
    let Some(chooser) = resolve_effect_player_ref(state, ability, chooser) else {
        return true;
    };
    let ctx = FilterContext::from_ability_with_controller(ability, chooser);
    eligible_targets(state, filter, &ctx, eligibility.as_ref()).len() < *count as usize
}

#[cfg(test)]
mod tests {
    use super::optional_exact_selection_is_infeasible;
    use crate::game::effects::resolve_ability_chain;
    use crate::game::scenario::GameScenario;
    use crate::parser::oracle_effect::parse_effect_chain;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, Effect, ObjectSelectionCardinality,
        ObjectSelectionEligibility, ResolvedAbility, TargetFilter, TargetRef, TypedFilter,
    };
    use crate::types::actions::GameAction;
    use crate::types::card_type::CoreType;
    use crate::types::counter::CounterType;
    use crate::types::game_state::WaitingFor;
    use crate::types::identifiers::ObjectId;
    use crate::types::phase::Phase;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    const P0: PlayerId = PlayerId(0);
    const P1: PlayerId = PlayerId(1);

    #[test]
    fn required_choice_clamps_to_the_objects_that_exist() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let required_choice = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::ChooseObjectsIntoTrackedSet {
                chooser: TargetFilter::Controller,
                filter: TargetFilter::Typed(TypedFilter::creature()),
                min: 2,
                max: Some(2),
                cardinality: Some(ObjectSelectionCardinality::Exactly { count: 2 }),
                eligibility: None,
            },
        );
        let host = {
            let mut builder = scenario.add_artifact_from_oracle(P0, "Required Choice Host", "");
            builder.with_ability_definition(required_choice);
            builder.id()
        };
        let only_eligible = scenario
            .add_creature(P0, "Only Eligible Creature", 1, 1)
            .id();

        let mut runner = scenario.build();
        runner
            .act(GameAction::ActivateAbility {
                source_id: host,
                ability_index: 0,
            })
            .expect("activate required-choice ability");
        runner.advance_until_stack_empty();
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseObjectsSelection {
                min: 1,
                max: Some(1),
                ..
            }
        ));
        runner
            .act(GameAction::SelectTargets {
                targets: vec![crate::types::ability::TargetRef::Object(only_eligible)],
            })
            .expect("CR 609.3 permits choosing the sole available object");
    }

    /// CR 608.2d + CR 122.1: an optional exact selection is offerable only
    /// when its complete removable-counter set exists; unlike a legacy range,
    /// one eligible object cannot be silently clamped into a "choose two".
    #[test]
    fn optional_exact_counter_selection_requires_every_object() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let host = scenario
            .add_artifact_from_oracle(P0, "Choice Host", "")
            .id();
        let creatures = [
            scenario.add_creature(P0, "First", 1, 1).id(),
            scenario.add_creature(P0, "Second", 1, 1).id(),
        ];
        let mut state = scenario.build().state().clone();
        let effect = Effect::ChooseObjectsIntoTrackedSet {
            chooser: TargetFilter::Controller,
            filter: TargetFilter::Typed(TypedFilter::creature()),
            min: 2,
            max: Some(2),
            cardinality: Some(ObjectSelectionCardinality::Exactly { count: 2 }),
            eligibility: Some(ObjectSelectionEligibility::RemovableCounter {
                counter_type: Some(CounterType::Plus1Plus1),
            }),
        };
        let ability = ResolvedAbility::new(effect, Vec::new(), host, P0);

        assert!(optional_exact_selection_is_infeasible(&state, &ability));
        state
            .objects
            .get_mut(&creatures[0])
            .expect("first creature exists")
            .counters
            .insert(CounterType::Plus1Plus1, 1);
        assert!(optional_exact_selection_is_infeasible(&state, &ability));
        state
            .objects
            .get_mut(&creatures[1])
            .expect("second creature exists")
            .counters
            .insert(CounterType::Plus1Plus1, 1);
        assert!(
            !optional_exact_selection_is_infeasible(&state, &ability),
            "one removable counter on each creature makes both objects selectable"
        );
    }

    /// CR 608.2c + CR 608.2d: An infeasible optional exact selection takes
    /// the ordinary decline path instead of installing a choice whose minimum
    /// is impossible to submit.
    #[test]
    fn infeasible_optional_exact_counter_selection_auto_declines() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let host = scenario
            .add_artifact_from_oracle(P0, "Choice Host", "")
            .id();
        let mut state = scenario.build().state().clone();
        let mut ability = ResolvedAbility::new(
            Effect::ChooseObjectsIntoTrackedSet {
                chooser: TargetFilter::Controller,
                filter: TargetFilter::Typed(TypedFilter::creature()),
                min: 2,
                max: Some(2),
                cardinality: Some(ObjectSelectionCardinality::Exactly { count: 2 }),
                eligibility: Some(ObjectSelectionEligibility::RemovableCounter {
                    counter_type: Some(CounterType::Plus1Plus1),
                }),
            },
            Vec::new(),
            host,
            P0,
        );
        ability.optional = true;

        resolve_ability_chain(&mut state, &ability, &mut Vec::new(), 0)
            .expect("infeasible optional selection declines");
        assert!(
            !matches!(state.waiting_for, WaitingFor::ChooseObjectsSelection { .. }),
            "an infeasible exact selection must not leave the game waiting"
        );
    }

    /// CR 608.2c + CR 122.1: The exact selection's tracked set feeds the
    /// removal continuation. `RemoveCounter` removes as many as possible from
    /// each selected object, so one counter remains a legal selection for a
    /// two-counter instruction.
    #[test]
    fn exact_counter_selection_allows_partial_removal_from_every_selected_creature() {
        let removal = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::RemoveCounter {
                counter_type: Some(CounterType::Plus1Plus1),
                count: crate::types::ability::QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::TrackedSet {
                    id: crate::types::identifiers::TrackedSetId(0),
                },
            },
        );
        let choice = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::ChooseObjectsIntoTrackedSet {
                chooser: TargetFilter::Controller,
                filter: TargetFilter::Typed(TypedFilter::creature()),
                min: 2,
                max: Some(2),
                cardinality: Some(ObjectSelectionCardinality::Exactly { count: 2 }),
                eligibility: Some(ObjectSelectionEligibility::RemovableCounter {
                    counter_type: Some(CounterType::Plus1Plus1),
                }),
            },
        )
        .sub_ability(removal);
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let host = {
            let mut builder = scenario.add_artifact_from_oracle(P0, "Choice Host", "");
            builder.with_ability_definition(choice);
            builder.id()
        };
        let first = {
            let mut builder = scenario.add_creature(P0, "First", 1, 1);
            builder.with_plus_counters(1);
            builder.id()
        };
        let second = {
            let mut builder = scenario.add_creature(P0, "Second", 1, 1);
            builder.with_plus_counters(1);
            builder.id()
        };
        let mut runner = scenario.build();
        runner
            .act(GameAction::ActivateAbility {
                source_id: host,
                ability_index: 0,
            })
            .expect("activate exact counter-removal choice");
        runner.advance_until_stack_empty();
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(first), TargetRef::Object(second)],
            })
            .expect("select both eligible creatures");
        for creature in [first, second] {
            assert_eq!(
                runner
                    .state()
                    .objects
                    .get(&creature)
                    .and_then(|object| object.counters.get(&CounterType::Plus1Plus1))
                    .copied()
                    .unwrap_or_default(),
                0,
                "selected creature must lose its +1/+1 counter"
            );
        }
    }

    /// CR 608.2c + CR 608.2d + official ruling: The Day of the Doctor IV — "Choose
    /// up to three Doctors. You may exile all other creatures." — when the
    /// controller chooses ZERO Doctors (a legal resolution-time choice for an
    /// "up to N" selection, CR 608.2d), "all other creatures" is EVERY creature
    /// on the battlefield (the chosen set is empty, so nothing is excluded).
    ///
    /// This is the end-to-end runtime proof the `game/filter.rs` unit test
    /// (`not_in_tracked_set_excludes_chosen_and_includes_rest`) cannot give: that
    /// submitting an EMPTY `WaitingFor::ChooseObjectsSelection` really drives
    /// `publish_fresh_tracked_set(state, [])`, which allocates a fresh EMPTY set
    /// and rebinds `chain_tracked_set_id` to it, so the following
    /// `ChangeZoneAll { target: creatures with Not(InTrackedSet(sentinel)) }`
    /// resolves the sentinel to that empty set and exiles ALL creatures — the
    /// controller's, the opponent's, and the (would-be-chosen) Doctors alike.
    #[test]
    fn choosing_zero_doctors_exiles_all_creatures() {
        // Chapter IV, parsed as the real card text and re-hosted as an ACTIVATED
        // ability so the test can fire it on demand. Parse produces:
        //   head:  ChooseObjectsIntoTrackedSet { filter: Doctor, min: 0, max: 3 }
        //   sub0:  ChangeZoneAll -> Exile, target = creatures Not(InTrackedSet(0))
        //          (optional: the "You may exile all other creatures")
        //   sub1:  DealDamage 13 to controller ("If you do, ... 13 damage to you")
        let mut activated = parse_effect_chain(
            "Choose up to three Doctors. You may exile all other creatures. \
             If you do, this Saga deals 13 damage to you.",
            AbilityKind::Activated,
        );
        activated.kind = AbilityKind::Activated;

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);

        // The activation vehicle for the chapter ability. Typed as a creature
        // purely so it can host an activated ability on the battlefield; it stands
        // in for the (non-creature) Saga. Being a creature, it is itself one of the
        // "other creatures" and is exiled too — which does not weaken the "every
        // creature is exiled" assertion.
        let host = {
            let mut b = scenario.add_creature(P0, "The Day of the Doctor", 0, 1);
            b.with_ability_definition(activated);
            b.id()
        };
        // Two Doctors the controller COULD choose but deliberately does not.
        let doctor_a = {
            let mut b = scenario.add_creature(P0, "First Doctor", 2, 2);
            b.with_subtypes(vec!["Doctor"]);
            b.id()
        };
        let doctor_b = {
            let mut b = scenario.add_creature(P0, "Thirteenth Doctor", 3, 3);
            b.with_subtypes(vec!["Doctor"]);
            b.id()
        };
        // A non-Doctor creature under the OPPONENT — proves the mass exile has no
        // controller constraint (all creatures, not just yours).
        let enemy = scenario.add_creature(P1, "Enemy Dalek", 4, 4).id();

        let mut runner = scenario.build();
        let life_before = runner.life(P0);

        // Fire the chapter ability; it resolves to the interactive selection.
        runner
            .act(GameAction::ActivateAbility {
                source_id: host,
                ability_index: 0,
            })
            .expect("activate the chapter IV ability");
        runner.advance_until_stack_empty();
        assert_eq!(
            runner.waiting_for_kind(),
            "ChooseObjectsSelection",
            "the head must pause on the interactive Doctor selection"
        );

        // Choose ZERO Doctors: submit an empty selection (CR 608.2d — choosing
        // zero of an "up to N" selection is a legal resolution-time choice). The
        // eligible set contains both Doctors, but the controller picks none.
        if let WaitingFor::ChooseObjectsSelection { eligible, .. } =
            runner.state().waiting_for.clone()
        {
            assert!(
                eligible.len() >= 2,
                "both Doctors must be eligible to be chosen, got {eligible:?}"
            );
        } else {
            panic!("expected ChooseObjectsSelection");
        }
        runner
            .act(GameAction::SelectTargets { targets: vec![] })
            .expect("submit an empty (zero-Doctor) selection");

        // The "You may exile all other creatures" clause now offers the mass
        // exile; accept it.
        assert_eq!(
            runner.waiting_for_kind(),
            "OptionalEffectChoice",
            "the optional 'You may exile all other creatures' clause must be offered"
        );
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .expect("accept the mass exile");
        runner.advance_until_stack_empty();

        // With an empty chosen set, EVERY creature is "other" and is exiled.
        let zone_of = |id: ObjectId| runner.state().objects[&id].zone;
        for (id, name) in [
            (host, "host"),
            (doctor_a, "First Doctor"),
            (doctor_b, "Thirteenth Doctor"),
            (enemy, "Enemy Dalek"),
        ] {
            assert_eq!(
                zone_of(id),
                Zone::Exile,
                "{name} must be exiled — zero Doctors chosen means none are excluded"
            );
        }
        let creatures_left = runner
            .state()
            .battlefield
            .iter()
            .filter(|&&id| {
                runner.state().objects[&id]
                    .card_types
                    .core_types
                    .contains(&CoreType::Creature)
            })
            .count();
        assert_eq!(
            creatures_left, 0,
            "no creature may remain on the battlefield after exiling all"
        );

        // "If you do, this Saga deals 13 damage to you" — the tail confirms the
        // continuation drained through the whole chapter chain.
        assert_eq!(
            runner.life(P0),
            life_before - 13,
            "the exile-all branch's 13-damage rider must have resolved"
        );
    }
}
