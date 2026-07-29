//! CR 608.2d + CR 122.1: Interactive counter-kind selection.
//!
//! Resolves `Effect::ChooseCounterKind` ("choose a counter on it" — The Caves
//! of Androzani II/III). The distinct counter kinds present on the resolved
//! target object are enumerated at resolution time, then:
//!   * 0 kinds → no-op (CR 608.2d: a player can't choose an impossible option),
//!   * 1 kind  → auto-select (bind directly, no prompt),
//!   * 2+ kinds → an interactive `WaitingFor::NamedChoice` reusing the shared
//!     named-choice seam with a `ChoiceType::CounterKind` whose option list is
//!     baked with the concrete kinds.
//!
//! The chosen kind is retained only in resolution-local state (via the single
//! `bind_named_choice` authority) so a following `Effect::PutChosenCounter` can
//! read it without leaking a persistent attribute onto the source.

use crate::types::ability::{
    ChoiceType, Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter,
};
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};

/// CR 608.2d + CR 122.1: Resolve `Effect::ChooseCounterKind`.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let target_filter = match &ability.effect {
        Effect::ChooseCounterKind { target } => target,
        _ => return Err(EffectError::MissingParam("ChooseCounterKind".to_string())),
    };

    let (mut source, persist_player) = crate::game::effects::choose::named_choice_authority(
        state,
        ability,
        false,
        &ChoiceType::CounterKind {
            options: Vec::new(),
        },
    );

    // CR 608.2d: Each (per-`repeat_for`-iteration) choice starts fresh — clear
    // the explicit resolution result before the zero, auto, and interactive
    // branches. This prevents a departed exact source or a previous iteration
    // from supplying a stale "that kind" to the following PutChosenCounter.
    state.chosen_counter_kind_this_resolution = None;
    state.last_named_choice = None;

    // CR 608.2d + CR 122.1: Context references and typed/zone domains share the
    // same distinct-kind authority used by dynamic quantities and repeat loops.
    // Aven Courier's untargeted population therefore ignores its downstream
    // stack target, while an `InZone` domain scans that declared zone exactly.
    let filter_ctx = crate::game::filter::FilterContext::from_ability(ability);
    // CR 608.2d: For an untargeted resolution-time instruction, the object was
    // selected immediately before this resolver ran. Its concrete target slot,
    // not the whole grammatical eligibility domain, defines the legal counter
    // kinds. Context references (The Caves of Androzani) retain their ordinary
    // parent-target resolution.
    let selected_object_filter =
        ability
            .effect_context_object
            .as_ref()
            .map(|selected| TargetFilter::SpecificObject {
                id: selected.object_id,
            });
    let kind_domain =
        if ability.target_choice_timing == crate::types::ability::TargetChoiceTiming::Resolution {
            selected_object_filter.as_ref().unwrap_or(target_filter)
        } else {
            target_filter
        };
    let kinds =
        crate::game::quantity::distinct_counter_kinds_among(state, kind_domain, &filter_ctx);

    let resolved = || GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
        subject: None,
    };

    // CR 608.2d: no counters on the object → nothing to choose → no-op.
    if kinds.is_empty() {
        events.push(resolved());
        return Ok(());
    }

    let choice_type = ChoiceType::CounterKind {
        options: kinds.clone(),
    };

    // CR 608.2d: a single legal option is auto-selected — no interactive prompt.
    if kinds.len() == 1 {
        let only = kinds[0].as_str().into_owned();
        crate::game::effects::choose::bind_named_choice(
            state,
            &choice_type,
            &only,
            source.as_mut(),
            persist_player,
        );
        events.push(resolved());
        return Ok(());
    }

    // CR 608.2d: two or more kinds → surface the shared interactive choice seam.
    let options: Vec<String> = kinds.iter().map(|k| k.as_str().into_owned()).collect();
    state.waiting_for = WaitingFor::NamedChoice {
        player: ability.controller,
        choice_type,
        options,
        source,
        persist_player,
    };
    events.push(resolved());
    Ok(())
}

/// CR 608.2c + CR 122.1: Read the counter kind selected by the immediately
/// preceding counter-kind instruction. This dedicated resolution result is the
/// only authority: a source object, its LKI, and `last_named_choice` may all
/// describe older state once an iteration has advanced or the source left.
pub(crate) fn chosen_counter_kind(state: &GameState) -> Option<CounterType> {
    state.chosen_counter_kind_this_resolution.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{ControllerRef, FilterProp, TargetFilter, TargetRef, TypedFilter};
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;

    fn ability_with_target(target_obj: ObjectId, source: ObjectId) -> ResolvedAbility {
        let mut ability = ResolvedAbility::new(
            Effect::ChooseCounterKind {
                target: TargetFilter::ParentTarget,
            },
            vec![TargetRef::Object(target_obj)],
            source,
            PlayerId(0),
        );
        ability.targets = vec![TargetRef::Object(target_obj)];
        ability
    }

    /// CR 608.2d: An object with two distinct counter kinds surfaces an
    /// interactive `NamedChoice` listing both kinds.
    #[test]
    fn two_kinds_prompt_named_choice() {
        let mut state = GameState::new_two_player(1);
        let obj = crate::game::zones::create_object(
            &mut state,
            crate::types::identifiers::CardId(1),
            PlayerId(0),
            "Counter Test".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        {
            let o = state.objects.get_mut(&obj).unwrap();
            o.counters.insert(CounterType::Plus1Plus1, 1);
            o.counters.insert(CounterType::Stun, 1);
        }
        let ability = ability_with_target(obj, ObjectId(999));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        match &state.waiting_for {
            WaitingFor::NamedChoice {
                choice_type,
                options,
                source,
                ..
            } => {
                assert!(matches!(choice_type, ChoiceType::CounterKind { .. }));
                assert_eq!(options.len(), 2);
                assert!(
                    source.is_none(),
                    "a resolution-local counter choice must not carry a persistent source"
                );
            }
            other => panic!("expected NamedChoice, got {other:?}"),
        }
    }

    /// CR 608.2d + CR 122.1: A typed untargeted domain is enumerated from the
    /// battlefield at resolution. An explicit downstream stack target must not
    /// narrow the population to itself; because it is also a controlled
    /// permanent, its kind remains one member of the complete legal union.
    #[test]
    fn typed_domain_unions_controlled_permanents_despite_downstream_target() {
        let mut state = GameState::new_two_player(1);
        let source = crate::game::zones::create_object(
            &mut state,
            crate::types::identifiers::CardId(1),
            PlayerId(0),
            "Aven".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        let controlled_stun = crate::game::zones::create_object(
            &mut state,
            crate::types::identifiers::CardId(2),
            PlayerId(0),
            "Controlled Stun".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        let controlled_plus = crate::game::zones::create_object(
            &mut state,
            crate::types::identifiers::CardId(3),
            PlayerId(0),
            "Controlled Plus".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        let opponent = crate::game::zones::create_object(
            &mut state,
            crate::types::identifiers::CardId(4),
            PlayerId(1),
            "Opponent".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        let downstream_target = crate::game::zones::create_object(
            &mut state,
            crate::types::identifiers::CardId(5),
            PlayerId(0),
            "Downstream Target".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        for id in [
            source,
            controlled_stun,
            controlled_plus,
            opponent,
            downstream_target,
        ] {
            let object = state.objects.get_mut(&id).unwrap();
            object.card_types.core_types.push(CoreType::Creature);
            object.base_card_types = object.card_types.clone();
        }
        state
            .objects
            .get_mut(&controlled_stun)
            .unwrap()
            .counters
            .insert(CounterType::Stun, 1);
        state
            .objects
            .get_mut(&controlled_plus)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 1);
        state
            .objects
            .get_mut(&opponent)
            .unwrap()
            .counters
            .insert(CounterType::Loyalty, 1);
        state
            .objects
            .get_mut(&downstream_target)
            .unwrap()
            .counters
            .insert(CounterType::Generic("charge".to_string()), 1);

        let ability = ResolvedAbility::new(
            Effect::ChooseCounterKind {
                target: TargetFilter::Typed(
                    TypedFilter::permanent().controller(ControllerRef::You),
                ),
            },
            vec![TargetRef::Object(downstream_target)],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let WaitingFor::NamedChoice { options, .. } = &state.waiting_for else {
            panic!("two controlled counter kinds must produce NamedChoice");
        };
        assert_eq!(
            options,
            &vec![
                CounterType::Plus1Plus1.as_str().into_owned(),
                "charge".to_string(),
                CounterType::Stun.as_str().into_owned(),
            ],
            "all kinds on controlled permanents are legal despite a downstream target"
        );
        assert!(
            !options.contains(&CounterType::Loyalty.as_str().into_owned()),
            "the opponent's kind must be excluded"
        );
    }

    /// CR 122.1: Typed counter-kind domains honor an explicit nonbattlefield
    /// zone and do not accidentally scan the battlefield.
    #[test]
    fn typed_domain_reads_distinct_kinds_from_declared_zone() {
        let mut state = GameState::new_two_player(1);
        let source = crate::game::zones::create_object(
            &mut state,
            crate::types::identifiers::CardId(1),
            PlayerId(0),
            "Source".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        let exiled_stun = crate::game::zones::create_object(
            &mut state,
            crate::types::identifiers::CardId(2),
            PlayerId(0),
            "Exiled Stun".to_string(),
            crate::types::zones::Zone::Exile,
        );
        let exiled_plus = crate::game::zones::create_object(
            &mut state,
            crate::types::identifiers::CardId(3),
            PlayerId(0),
            "Exiled Plus".to_string(),
            crate::types::zones::Zone::Exile,
        );
        let battlefield_lore = crate::game::zones::create_object(
            &mut state,
            crate::types::identifiers::CardId(4),
            PlayerId(0),
            "Battlefield Lore".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&exiled_stun)
            .unwrap()
            .counters
            .insert(CounterType::Stun, 1);
        state
            .objects
            .get_mut(&exiled_plus)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 1);
        state
            .objects
            .get_mut(&battlefield_lore)
            .unwrap()
            .counters
            .insert(CounterType::Lore, 1);

        let ability = ResolvedAbility::new(
            Effect::ChooseCounterKind {
                target: TargetFilter::Typed(TypedFilter {
                    type_filters: Vec::new(),
                    controller: None,
                    properties: vec![FilterProp::InZone {
                        zone: crate::types::zones::Zone::Exile,
                    }],
                }),
            },
            Vec::new(),
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let WaitingFor::NamedChoice { options, .. } = &state.waiting_for else {
            panic!("two exiled counter kinds must produce NamedChoice");
        };
        assert_eq!(
            options,
            &vec![
                CounterType::Plus1Plus1.as_str().into_owned(),
                CounterType::Stun.as_str().into_owned(),
            ],
        );
        assert!(
            !options.contains(&CounterType::Lore.as_str().into_owned()),
            "the battlefield kind must be excluded from an exile domain"
        );
    }

    /// CR 608.2d: A single counter kind is auto-selected — no prompt, and the
    /// kind is retained only for the current resolution.
    #[test]
    fn single_kind_auto_selects_without_prompt() {
        let mut state = GameState::new_two_player(1);
        let source = crate::game::zones::create_object(
            &mut state,
            crate::types::identifiers::CardId(2),
            PlayerId(0),
            "Counter Test".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        let obj = crate::game::zones::create_object(
            &mut state,
            crate::types::identifiers::CardId(1),
            PlayerId(0),
            "Counter Test".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&obj)
            .unwrap()
            .counters
            .insert(CounterType::Stun, 2);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        let ability = ability_with_target(obj, source);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert!(
            !matches!(state.waiting_for, WaitingFor::NamedChoice { .. }),
            "a single kind must auto-select without prompting"
        );
        let attrs = &state.objects.get(&source).unwrap().chosen_attributes;
        assert!(
            attrs.iter().all(|attribute| !matches!(
                attribute,
                crate::types::ability::ChosenAttribute::Counter(_)
            )),
            "auto-selection must not persist the counter kind on the source"
        );
        assert_eq!(
            state.chosen_counter_kind_this_resolution,
            Some(CounterType::Stun),
            "auto-selection records the same explicit resolution result as an interactive answer"
        );
    }

    /// CR 608.2d: An object with no counters is skipped (no prompt, no bind).
    #[test]
    fn zero_kinds_is_noop() {
        let mut state = GameState::new_two_player(1);
        // A prior iteration or departed source may have selected this kind. The
        // zero-kind branch must clear it before the following put can observe it.
        state.chosen_counter_kind_this_resolution = Some(CounterType::Stun);
        let obj = crate::game::zones::create_object(
            &mut state,
            crate::types::identifiers::CardId(1),
            PlayerId(0),
            "Counter Test".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        let ability = ability_with_target(obj, ObjectId(999));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert!(!matches!(state.waiting_for, WaitingFor::NamedChoice { .. }));
        assert!(
            state.chosen_counter_kind_this_resolution.is_none(),
            "zero legal kinds clears the current-resolution counter result"
        );
    }

    /// CR 608.2c + CR 608.2d + CR 115.10a + CR 122.1: Contractual
    /// Safeguard selects the creature whose counter defines the kind during
    /// resolution, then excludes that exact creature from "each other
    /// creature you control." This drives the live parser, cast pipeline,
    /// `ChooseFromZoneChoice` answer, auto-selected kind, continuation, and
    /// counter-placement resolver.
    #[test]
    fn contractual_safeguard_excludes_the_counter_kind_source_creature() {
        use crate::game::scenario::GameScenario;
        use crate::types::actions::GameAction;
        use crate::types::mana::ManaCost;
        use crate::types::phase::Phase;

        const P0: PlayerId = PlayerId(0);
        const P1: PlayerId = PlayerId(1);
        const ORACLE: &str = "Addendum — If you cast this spell during your main phase, put a \
            shield counter on a creature you control. (If it would be dealt damage or destroyed, \
            remove a shield counter from it instead.)\nChoose a kind of counter on a creature you \
            control. Put a counter of that kind on each other creature you control.";

        let mut scenario = GameScenario::new();
        // Outside a main phase, the Addendum instruction is skipped while its
        // independent counter-kind continuation still resolves.
        scenario.at_phase(Phase::BeginCombat);
        let chosen = scenario.add_creature(P0, "Chosen Creature", 2, 2).id();
        scenario.with_counter(chosen, CounterType::Stun, 1);
        scenario.with_counter(chosen, CounterType::Plus1Plus1, 1);
        let other_countered = scenario.add_creature(P0, "Other Countered", 2, 2).id();
        scenario.with_counter(other_countered, CounterType::Plus1Plus1, 1);
        let other_plain = scenario.add_creature(P0, "Other Plain", 2, 2).id();
        let opponent = scenario.add_creature(P1, "Opponent Creature", 2, 2).id();
        let safeguard = scenario
            .add_spell_to_hand_from_oracle(P0, "Contractual Safeguard", true, ORACLE)
            .with_mana_cost(ManaCost::zero())
            .id();

        let mut runner = scenario.build();
        runner.state_mut().debug_mode = true;
        let _paused = runner.cast(safeguard).resolve();

        let offered = match &runner.state().waiting_for {
            WaitingFor::ChooseFromZoneChoice { cards, count, .. } => {
                assert_eq!(*count, 1);
                cards.clone()
            }
            other => panic!("expected counter-source creature choice, got {other:?}"),
        };
        assert!(offered.contains(&chosen));
        assert!(offered.contains(&other_countered));
        assert!(
            !offered.contains(&other_plain),
            "a creature with no counters cannot define a counter kind"
        );

        runner
            .act(GameAction::SelectCards {
                cards: vec![chosen],
            })
            .expect("select the creature whose Stun counter defines the kind");
        match &runner.state().waiting_for {
            WaitingFor::NamedChoice {
                choice_type,
                options,
                ..
            } => {
                assert!(matches!(choice_type, ChoiceType::CounterKind { .. }));
                assert!(options.contains(&CounterType::Stun.as_str().into_owned()));
            }
            other => panic!("expected kind choice on the selected creature, got {other:?}"),
        }
        runner
            .act(GameAction::ChooseOption {
                choice: CounterType::Stun.as_str().into_owned(),
            })
            .expect("choose Stun from the selected creature");
        runner.advance_until_stack_empty();

        let counter_count = |id| {
            runner.state().objects[&id]
                .counters
                .get(&CounterType::Stun)
                .copied()
                .unwrap_or(0)
        };
        assert_eq!(
            counter_count(chosen),
            1,
            "the selected creature is the authority for 'other' and is excluded"
        );
        assert_eq!(counter_count(other_countered), 1);
        assert_eq!(counter_count(other_plain), 1);
        assert_eq!(
            counter_count(opponent),
            0,
            "only creatures controlled by the spell's controller receive the kind"
        );
    }

    /// CR 714.2 + CR 608.2d + CR 122.1 + CR 122.6: Runtime proof of the composed
    /// The Caves of Androzani chapter — the real parsed `repeat_for` +
    /// `ChooseCounterKind` (auto-select) + optional `PutChosenCounter` chain,
    /// re-hosted as an activated ability and driven through the production
    /// pipeline. Asserts the single-kind object receives exactly one ADDITIONAL
    /// counter of the chosen kind (Stun: 1 → 2), proving the choose→persist→put
    /// round-trip resolves correctly inside a member-driven iteration.
    #[test]
    fn caves_chapter_repeat_choose_and_put_round_trip() {
        use crate::game::scenario::GameScenario;
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::AbilityKind;
        use crate::types::actions::GameAction;
        use crate::types::counter::CounterType;
        use crate::types::phase::Phase;

        const P0: PlayerId = PlayerId(0);

        let mut activated = parse_effect_chain(
            "For each non-Saga permanent, choose a counter on it. You may put an \
             additional counter of that kind on that permanent.",
            AbilityKind::Spell,
        );
        activated.kind = AbilityKind::Activated;

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);

        // The Saga subtype excludes the host from the non-Saga repeat filter.
        let host = {
            let mut b = scenario.add_creature(P0, "Caves Host", 0, 1);
            b.with_subtypes(vec!["Saga"]);
            b.with_ability_definition(activated);
            b.id()
        };
        let single = scenario.add_creature(P0, "Single", 2, 2).id();
        scenario.with_counter(single, CounterType::Stun, 1);

        let mut runner = scenario.build();
        runner
            .act(GameAction::ActivateAbility {
                source_id: host,
                ability_index: 0,
            })
            .expect("activate the Caves chapter ability");

        for _ in 0..80 {
            match runner.state().waiting_for.clone() {
                WaitingFor::Priority { .. } => {
                    if runner.state().stack.is_empty() {
                        break;
                    }
                    if runner.act(GameAction::PassPriority).is_err() {
                        break;
                    }
                }
                WaitingFor::OptionalEffectChoice { .. } => {
                    runner
                        .act(GameAction::DecideOptionalEffect { accept: true })
                        .expect("accept the optional put");
                }
                WaitingFor::NamedChoice { options, .. } => {
                    let choice = options.first().cloned().expect("a counter-kind option");
                    runner
                        .act(GameAction::ChooseOption { choice })
                        .expect("answer the counter-kind choice");
                }
                _ => break,
            }
        }

        let stun = runner
            .state()
            .objects
            .get(&single)
            .unwrap()
            .counters
            .get(&CounterType::Stun)
            .copied()
            .unwrap_or(0);
        assert_eq!(
            stun, 2,
            "one additional counter of the chosen (Stun) kind must be added"
        );
    }

    /// CR 714.2 + CR 608.2d + CR 122.1 + CR 122.6: The genuinely risky mid-repeat
    /// continuation — a permanent bearing TWO distinct counter kinds suspends the
    /// `repeat_for` loop on an interactive `NamedChoice`, resumes on
    /// `GameAction::ChooseOption`, re-binds `ParentTarget` to the SAME iteration's
    /// object for the optional `PutChosenCounter`, and then advances to the next
    /// member (a single-kind permanent driven by auto-select). Proves the loop
    /// does not lose its place across an interactive counter-kind choice.
    #[test]
    fn caves_chapter_two_counter_kinds_suspends_resumes_and_advances() {
        use crate::game::scenario::GameScenario;
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::AbilityKind;
        use crate::types::actions::GameAction;
        use crate::types::counter::CounterType;
        use crate::types::phase::Phase;

        const P0: PlayerId = PlayerId(0);

        let mut activated = parse_effect_chain(
            "For each non-Saga permanent, choose a counter on it. You may put an \
             additional counter of that kind on that permanent.",
            AbilityKind::Spell,
        );
        activated.kind = AbilityKind::Activated;

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);

        // The Saga subtype excludes the host from the non-Saga repeat filter.
        let host = {
            let mut b = scenario.add_creature(P0, "Caves Host", 0, 1);
            b.with_subtypes(vec!["Saga"]);
            b.with_ability_definition(activated);
            b.id()
        };
        // Two distinct counter kinds → the interactive 2+-kind NamedChoice branch.
        let two_kinds = scenario.add_creature(P0, "Two Kinds", 2, 2).id();
        scenario.with_counter(two_kinds, CounterType::Stun, 1);
        scenario.with_counter(two_kinds, CounterType::Plus1Plus1, 1);
        // A second member with a single kind → auto-select; proves the loop
        // advances to (and resolves) the next member after the suspend/resume.
        let single = scenario.add_creature(P0, "Single", 3, 3).id();
        scenario.with_counter(single, CounterType::Loyalty, 1);

        let mut runner = scenario.build();
        runner
            .act(GameAction::ActivateAbility {
                source_id: host,
                ability_index: 0,
            })
            .expect("activate the Caves chapter ability");

        let mut chosen_kind: Option<String> = None;
        for _ in 0..80 {
            match runner.state().waiting_for.clone() {
                WaitingFor::Priority { .. } => {
                    if runner.state().stack.is_empty() {
                        break;
                    }
                    if runner.act(GameAction::PassPriority).is_err() {
                        break;
                    }
                }
                WaitingFor::OptionalEffectChoice { .. } => {
                    runner
                        .act(GameAction::DecideOptionalEffect { accept: true })
                        .expect("accept the optional put");
                }
                WaitingFor::NamedChoice { options, .. } => {
                    assert_eq!(
                        options.len(),
                        2,
                        "the two-kind permanent must surface both distinct kinds"
                    );
                    let choice = options.first().cloned().expect("a counter-kind option");
                    chosen_kind = Some(choice.clone());
                    runner
                        .act(GameAction::ChooseOption { choice })
                        .expect("answer the counter-kind choice");
                }
                _ => break,
            }
        }

        let chosen = chosen_kind
            .expect("the two-kind permanent must have suspended the loop on a NamedChoice");

        // The chosen kind receives exactly one additional counter; the other kind
        // on the same permanent is untouched (per-iteration isolation, CR 608.2d).
        let two = &runner.state().objects.get(&two_kinds).unwrap().counters;
        let stun = two.get(&CounterType::Stun).copied().unwrap_or(0);
        let p1p1 = two.get(&CounterType::Plus1Plus1).copied().unwrap_or(0);
        assert_eq!(
            stun + p1p1,
            3,
            "exactly one additional counter must land on the two-kind permanent"
        );
        if chosen == CounterType::Stun.as_str() {
            assert_eq!(stun, 2, "the chosen (Stun) kind gains one counter");
            assert_eq!(p1p1, 1, "the unchosen (+1/+1) kind is untouched");
        } else {
            assert_eq!(p1p1, 2, "the chosen (+1/+1) kind gains one counter");
            assert_eq!(stun, 1, "the unchosen (Stun) kind is untouched");
        }

        // The loop advanced to the next member: its single (auto-selected) kind
        // gains its additional counter too.
        let loyalty = runner
            .state()
            .objects
            .get(&single)
            .unwrap()
            .counters
            .get(&CounterType::Loyalty)
            .copied()
            .unwrap_or(0);
        assert_eq!(
            loyalty, 2,
            "the loop must resume past the interactive choice and resolve the next member"
        );
    }

    /// CR 608.2d + CR 122.6: A counterless permanent in the `repeat_for` (a land,
    /// a fresh creature) has no "that kind" to add, so the per-iteration optional
    /// "you may put an additional counter" is an impossible option and must NOT be
    /// offered — the effect resolves as its defined no-op with no yes/no prompt.
    #[test]
    fn caves_chapter_counterless_permanent_raises_no_impossible_optional() {
        use crate::game::scenario::GameScenario;
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::AbilityKind;
        use crate::types::actions::GameAction;
        use crate::types::phase::Phase;

        const P0: PlayerId = PlayerId(0);

        let mut activated = parse_effect_chain(
            "For each non-Saga permanent, choose a counter on it. You may put an \
             additional counter of that kind on that permanent.",
            AbilityKind::Spell,
        );
        activated.kind = AbilityKind::Activated;

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);

        let host = {
            let mut b = scenario.add_creature(P0, "Caves Host", 0, 1);
            b.with_subtypes(vec!["Saga"]);
            b.with_ability_definition(activated);
            b.id()
        };
        // No counters → 0-kind branch fires → PutChosenCounter can only no-op.
        let bare = scenario.add_creature(P0, "Counterless", 2, 2).id();

        let mut runner = scenario.build();
        runner
            .act(GameAction::ActivateAbility {
                source_id: host,
                ability_index: 0,
            })
            .expect("activate the Caves chapter ability");

        for _ in 0..40 {
            match runner.state().waiting_for.clone() {
                WaitingFor::Priority { .. } => {
                    if runner.state().stack.is_empty() {
                        break;
                    }
                    if runner.act(GameAction::PassPriority).is_err() {
                        break;
                    }
                }
                WaitingFor::OptionalEffectChoice { .. } => {
                    panic!(
                        "a counterless permanent must not raise an impossible \
                         'you may put an additional counter' prompt (CR 608.2d)"
                    );
                }
                WaitingFor::NamedChoice { .. } => {
                    panic!("a counterless permanent has no counter kinds to choose");
                }
                _ => break,
            }
        }

        assert!(
            runner
                .state()
                .objects
                .get(&bare)
                .unwrap()
                .counters
                .is_empty(),
            "no counter is added to a counterless permanent"
        );
    }
}
