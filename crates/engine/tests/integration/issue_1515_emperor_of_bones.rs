//! Issue #1515 — Emperor of Bones must grant haste to, and later sacrifice,
//! the creature returned from its linked exile set.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ChoiceType, ChosenAttribute,
    ContinuousModification, DelayedTriggerCondition, Effect, QuantityExpr, QuantityRef,
    ReplacementDefinition, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, ExileLink, ExileLinkKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

const EMPEROR_COUNTER_TRIGGER_EFFECT: &str = "put a creature card exiled with this creature onto \
the battlefield under your control with a finality counter on it. it gains haste. sacrifice it at \
the beginning of the next end step.";
const EMPEROR_ORACLE: &str =
    "At the beginning of combat on your turn, exile up to one target card from a graveyard.\n\
{1}{B}: Adapt 2.\n\
Whenever one or more +1/+1 counters are put on this creature, put a creature card exiled with this \
creature onto the battlefield under your control with a finality counter on it. It gains haste. \
Sacrifice it at the beginning of the next end step.";
const PUT_COUNTER_ORACLE: &str = "Put a +1/+1 counter on target creature.";
const YAWGMOTHS_VILE_OFFERING_ORACLE: &str = "Put up to one target creature or planeswalker card from a graveyard onto the battlefield under your control. Destroy up to one target creature or planeswalker. Exile Yawgmoth's Vile Offering.";

const ANOINTED_PEACEKEEPER: &str = "Vigilance\n\
As this creature enters, look at an opponent's hand, then choose any card name.\n\
Spells your opponents cast with the chosen name cost {2} more to cast.\n\
Activated abilities of sources with the chosen name cost {2} more to activate unless they're mana abilities.";

const P1: PlayerId = PlayerId(1);
const NAMED_CARD: &str = "Llanowar Elves";

fn creature_has_haste_from_transient_effects(
    state: &engine::types::game_state::GameState,
    creature: ObjectId,
) -> bool {
    state.transient_continuous_effects.iter().any(|effect| {
        effect.affected == TargetFilter::SpecificObject { id: creature }
            && effect.modifications.iter().any(|modification| {
                matches!(
                    modification,
                    ContinuousModification::AddKeyword {
                        keyword: Keyword::Haste
                    }
                )
            })
    })
}

#[test]
fn issue_1515_emperor_of_bones_binds_haste_and_delayed_sacrifice_to_returned_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let emperor = scenario.add_creature(P0, "Emperor of Bones", 2, 2).id();
    let returned = scenario
        .add_creature_to_exile(P0, "Linked Gravebeast", 3, 3)
        .id();

    let mut runner = scenario.build();
    runner.state_mut().exile_links.push(ExileLink {
        exiled_id: returned,
        source_id: emperor,
        kind: ExileLinkKind::TrackedBySource,
    });

    let def = parse_effect_chain(EMPEROR_COUNTER_TRIGGER_EFFECT, AbilityKind::Spell);
    let ability = build_resolved_from_def(&def, emperor, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("Emperor of Bones counter-trigger effect must resolve");

    let state = runner.state();
    assert_eq!(
        state.objects[&returned].zone,
        Zone::Battlefield,
        "linked creature card must be returned to the battlefield"
    );
    assert_eq!(
        state.objects[&emperor].zone,
        Zone::Battlefield,
        "Emperor must remain on the battlefield after returning the linked creature"
    );
    assert_eq!(
        state.objects[&returned]
            .counters
            .get(&CounterType::Finality)
            .copied()
            .unwrap_or(0),
        1,
        "returned creature must enter with a finality counter"
    );
    assert!(
        creature_has_haste_from_transient_effects(state, returned),
        "haste grant must bind to the returned creature, not Emperor"
    );
    assert!(
        !creature_has_haste_from_transient_effects(state, emperor),
        "Emperor itself must not receive the returned creature's haste grant"
    );
    assert_eq!(
        state.delayed_triggers.len(),
        1,
        "resolution must install exactly one delayed sacrifice trigger"
    );
    assert!(matches!(
        state.delayed_triggers[0].condition,
        DelayedTriggerCondition::AtNextPhase { phase: Phase::End }
    ));
    assert_eq!(
        state.delayed_triggers[0].ability.targets,
        vec![engine::types::ability::TargetRef::Object(returned)],
        "delayed sacrifice trigger must snapshot the returned creature"
    );
    assert!(
        matches!(
            &state.delayed_triggers[0].ability.effect,
            Effect::Sacrifice {
                target: TargetFilter::ParentTarget,
                ..
            }
        ),
        "delayed trigger effect must sacrifice the snapshotted returned creature"
    );

    let mut guard = 0;
    while !runner.state().delayed_triggers.is_empty() || !runner.state().stack.is_empty() {
        guard += 1;
        assert!(
            guard < 256,
            "delayed sacrifice trigger never fired; phase = {:?}, waiting_for = {:?}, \
             delayed_triggers = {}, stack = {}",
            runner.state().phase,
            runner.state().waiting_for,
            runner.state().delayed_triggers.len(),
            runner.state().stack.len(),
        );
        match runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => runner
                .act(engine::types::actions::GameAction::DeclareAttackers {
                    attacks: vec![],
                    bands: vec![],
                })
                .expect("declare no attackers while advancing to end step"),
            WaitingFor::DeclareBlockers { .. } => runner
                .act(engine::types::actions::GameAction::DeclareBlockers {
                    assignments: vec![],
                })
                .expect("declare no blockers while advancing to end step"),
            _ => runner
                .act(engine::types::actions::GameAction::PassPriority)
                .expect("priority pass while waiting for delayed sacrifice"),
        };
    }

    assert_eq!(
        runner.state().objects[&returned].zone,
        Zone::Exile,
        "returned creature must be sacrificed at the beginning of the next end step; \
         its finality counter sends it to exile"
    );
    assert_eq!(
        runner.state().objects[&emperor].zone,
        Zone::Battlefield,
        "the delayed sacrifice must not sacrifice Emperor"
    );
}

/// CR 122.1 + CR 603.2 + CR 608.2c: Drive the printed counter trigger through
/// the reducer so the returned creature, rather than the trigger source, owns
/// both anaphoric riders.
#[test]
fn emperor_of_bones_counter_trigger_uses_returned_creature_in_cast_pipeline() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let emperor = scenario
        .add_creature_from_oracle(P0, "Emperor of Bones", 2, 2, EMPEROR_ORACLE)
        .id();
    let returned = scenario
        .add_creature_to_exile(P0, "Linked Gravebeast", 3, 3)
        .id();
    let counter_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Counter Placement", false, PUT_COUNTER_ORACLE)
        .id();

    let mut runner = scenario.build();
    runner.state_mut().exile_links.push(ExileLink {
        exiled_id: returned,
        source_id: emperor,
        kind: ExileLinkKind::TrackedBySource,
    });

    runner.cast(counter_spell).target_object(emperor).resolve();

    let state = runner.state();
    assert_eq!(
        state.objects[&returned].zone,
        Zone::Battlefield,
        "the counter trigger must return the linked creature through apply()"
    );
    assert_eq!(
        state.objects[&returned]
            .counters
            .get(&CounterType::Finality)
            .copied()
            .unwrap_or(0),
        1,
        "the returned creature must receive Emperor's finality entry modifier"
    );
    assert_eq!(
        state.objects[&emperor].zone,
        Zone::Battlefield,
        "the counter trigger must not sacrifice Emperor while resolving"
    );
    assert!(
        creature_has_haste_from_transient_effects(state, returned),
        "the printed haste rider must bind to the returned creature"
    );
    assert!(
        !creature_has_haste_from_transient_effects(state, emperor),
        "the printed haste rider must not bind to Emperor"
    );
    assert_eq!(state.delayed_triggers.len(), 1);
    assert_eq!(
        state.delayed_triggers[0].ability.targets,
        vec![engine::types::ability::TargetRef::Object(returned)],
        "the delayed sacrifice must snapshot the returned creature"
    );
}

#[test]
fn emperor_of_bones_adapt_pipeline_binds_delayed_sacrifice_to_returned_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let emperor = scenario
        .add_creature_from_oracle(P0, "Emperor of Bones", 2, 2, EMPEROR_ORACLE)
        .id();
    let returned = scenario
        .add_creature_to_exile(P0, "Linked Gravebeast", 3, 3)
        .id();
    let swamp_a = scenario.add_basic_land(P0, engine::types::mana::ManaColor::Black);
    let swamp_b = scenario.add_basic_land(P0, engine::types::mana::ManaColor::Black);

    let mut runner = scenario.build();
    runner.state_mut().exile_links.push(ExileLink {
        exiled_id: returned,
        source_id: emperor,
        kind: ExileLinkKind::TrackedBySource,
    });

    runner
        .activate(emperor, 0)
        .pay_with(&[swamp_a, swamp_b])
        .resolve();

    let state = runner.state();
    assert_eq!(
        state.objects[&returned].zone,
        Zone::Battlefield,
        "Adapt must resolve Emperor's counter trigger and return the linked creature"
    );
    assert_eq!(
        state.delayed_triggers.len(),
        1,
        "the counter trigger must install one delayed sacrifice"
    );
    assert_eq!(
        state.delayed_triggers[0].ability.targets,
        vec![engine::types::ability::TargetRef::Object(returned)],
        "the Adapt-triggered delayed sacrifice must snapshot the returned creature"
    );
    assert_eq!(
        state.objects[&emperor].zone,
        Zone::Battlefield,
        "Emperor must remain on the battlefield until its own ability is removed"
    );
}

#[test]
fn emperor_of_bones_adapt_without_linked_exile_has_no_riders_to_apply() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let emperor = scenario
        .add_creature_from_oracle(P0, "Emperor of Bones", 2, 2, EMPEROR_ORACLE)
        .id();
    let swamp_a = scenario.add_basic_land(P0, engine::types::mana::ManaColor::Black);
    let swamp_b = scenario.add_basic_land(P0, engine::types::mana::ManaColor::Black);

    let mut runner = scenario.build();
    runner
        .activate(emperor, 0)
        .pay_with(&[swamp_a, swamp_b])
        .resolve();

    let state = runner.state();
    assert_eq!(
        state.objects[&emperor]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        2,
        "Adapt must still put its counters on Emperor"
    );
    assert_eq!(
        state.delayed_triggers.len(),
        0,
        "no returned creature means Emperor's haste and delayed Sacrifice riders must not run"
    );
    assert!(
        !creature_has_haste_from_transient_effects(state, emperor),
        "Emperor must not receive the returned creature's haste rider"
    );
    assert_eq!(
        state.objects[&emperor].zone,
        Zone::Battlefield,
        "Emperor must remain on the battlefield when no linked creature was exiled"
    );
}

#[test]
fn empty_forward_result_preserves_independent_sequential_siblings() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let graveyard_creature = scenario
        .add_creature_to_graveyard(P0, "Unreturned Creature", 2, 2)
        .id();
    let destroy_target = scenario.add_creature(P1, "Destroy Target", 2, 2).id();
    let offering = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Yawgmoth's Vile Offering",
            true,
            YAWGMOTHS_VILE_OFFERING_ORACLE,
        )
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&offering].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: offering,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Yawgmoth's Vile Offering must be castable for the regression");

    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { selection, .. } => {
                let target = if selection.current_slot == 0 {
                    Some(engine::types::ability::TargetRef::Object(
                        graveyard_creature,
                    ))
                } else {
                    Some(engine::types::ability::TargetRef::Object(destroy_target))
                };
                runner
                    .act(GameAction::ChooseTarget { target })
                    .expect("target choice must be accepted");
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.pass_both_players();
            }
            _ => break,
        }
    }

    // CR 608.2b: Make the first selected target illegal between announcement
    // and resolution, so its forward-result move returns no object while the
    // independently targeted Destroy sibling remains legal.
    engine::game::zones::move_to_zone(
        runner.state_mut(),
        graveyard_creature,
        Zone::Battlefield,
        &mut Vec::new(),
    );
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&graveyard_creature].zone,
        Zone::Battlefield,
        "the pre-resolution move must invalidate the selected reanimation target"
    );
    assert_ne!(
        runner.state().objects[&destroy_target].zone,
        Zone::Battlefield,
        "the independent Destroy sibling must still resolve"
    );
    assert_eq!(
        runner.state().objects[&offering].zone,
        Zone::Exile,
        "the later self-exile sibling must still resolve"
    );
}

/// CR 614.12a + CR 400.7j: An as-enters choice on the returned permanent must
/// complete without losing later instructions that refer to that permanent.
#[test]
fn emperor_of_bones_resumes_riders_after_anointed_peacekeepers_as_enters_choices() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let emperor = scenario.add_creature(P0, "Emperor of Bones", 2, 2).id();
    let _opponent_card = scenario.add_card_to_hand(P1, "Opponent Secret");
    let peacekeeper = {
        let mut builder = scenario.add_creature_to_exile(P0, "Anointed Peacekeeper", 3, 3);
        builder.from_oracle_text(ANOINTED_PEACEKEEPER);
        builder.id()
    };

    let mut runner = scenario.build();
    runner.state_mut().all_card_names = std::sync::Arc::from([NAMED_CARD.to_string()]);
    runner.state_mut().exile_links.push(ExileLink {
        exiled_id: peacekeeper,
        source_id: emperor,
        kind: ExileLinkKind::TrackedBySource,
    });

    let definition = parse_effect_chain(EMPEROR_COUNTER_TRIGGER_EFFECT, AbilityKind::Spell);
    let ability = build_resolved_from_def(&definition, emperor, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("Emperor of Bones return must reach Peacekeeper's as-enters choice");

    let WaitingFor::NamedChoice {
        choice_type,
        options,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "Peacekeeper must ask which opponent to look at, got {}",
            runner.waiting_for_kind()
        );
    };
    assert!(matches!(choice_type, ChoiceType::Opponent { .. }));
    assert_eq!(options, vec![P1.0.to_string()]);
    runner
        .act(GameAction::ChooseOption {
            choice: P1.0.to_string(),
        })
        .expect("choose the opponent whose hand Peacekeeper looks at");

    let WaitingFor::NamedChoice { choice_type, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "Peacekeeper must ask for a card name after looking, got {}",
            runner.waiting_for_kind()
        );
    };
    assert!(matches!(choice_type, ChoiceType::CardName));
    runner
        .act(GameAction::ChooseOption {
            choice: NAMED_CARD.to_string(),
        })
        .expect("choose the card name for Peacekeeper");

    let state = runner.state();
    let returned = &state.objects[&peacekeeper];
    assert_eq!(returned.zone, Zone::Battlefield);
    assert!(returned.chosen_attributes.iter().any(
        |attribute| matches!(attribute, ChosenAttribute::CardName(name) if name == NAMED_CARD)
    ));
    assert_eq!(
        returned
            .counters
            .get(&CounterType::Finality)
            .copied()
            .unwrap_or(0),
        1,
        "Peacekeeper must retain Emperor's finality entry modifier"
    );
    assert!(
        creature_has_haste_from_transient_effects(state, peacekeeper),
        "Emperor's forwarded haste rider must resume after both as-enters choices"
    );
    assert_eq!(
        state.delayed_triggers.len(),
        1,
        "Emperor's delayed sacrifice rider must resume after both as-enters choices"
    );
    assert_eq!(
        state.delayed_triggers[0].ability.targets,
        vec![engine::types::ability::TargetRef::Object(peacekeeper)]
    );
}

/// A synthetic as-enters replacement that opens a PayAmountChoice before the
/// returning permanent finishes entering. This mirrors the shape of a printed
/// Moved replacement while keeping the regression independent of card data.
fn pay_amount_choice_replacement() -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(Zone::Battlefield)
        .valid_card(TargetFilter::SelfRef)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PayCost {
                cost: AbilityCost::PayLife {
                    amount: QuantityExpr::Ref {
                        qty: QuantityRef::Variable {
                            name: "X".to_string(),
                        },
                    },
                },
                scale: None,
                payer: TargetFilter::Controller,
            },
        ))
}

/// CR 614.12a + CR 400.7j: A previously unlisted resolution-owned prompt must
/// survive the replacement pause and resume the Emperor continuation.
#[test]
fn emperor_of_bones_preserves_pay_amount_choice_through_replacement_pipeline() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let emperor = scenario.add_creature(P0, "Emperor of Bones", 2, 2).id();
    let peacekeeper = {
        let mut builder = scenario.add_creature_to_exile(P0, "Anointed Peacekeeper", 3, 3);
        builder.from_oracle_text(ANOINTED_PEACEKEEPER);
        builder.id()
    };

    let mut runner = scenario.build();
    runner.state_mut().exile_links.push(ExileLink {
        exiled_id: peacekeeper,
        source_id: emperor,
        kind: ExileLinkKind::TrackedBySource,
    });
    runner
        .state_mut()
        .objects
        .get_mut(&peacekeeper)
        .unwrap()
        .replacement_definitions = vec![pay_amount_choice_replacement()].into();

    let definition = parse_effect_chain(EMPEROR_COUNTER_TRIGGER_EFFECT, AbilityKind::Spell);
    let ability = build_resolved_from_def(&definition, emperor, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("Emperor of Bones return must reach the replacement PayAmountChoice");

    let WaitingFor::PayAmountChoice {
        player, min, max, ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the replacement must preserve its PayAmountChoice, got {}",
            runner.waiting_for_kind()
        );
    };
    assert_eq!(player, P0);
    assert_eq!(min, 0);
    assert!(max > 0);

    runner
        .act(GameAction::SubmitPayAmount { amount: 0 })
        .expect("answer the replacement PayAmountChoice through GameRunner::act");

    let state = runner.state();
    assert_eq!(state.objects[&peacekeeper].zone, Zone::Battlefield);
    assert!(matches!(
        state.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert!(state.stack.is_empty());
    assert_eq!(
        state.delayed_triggers.len(),
        1,
        "the Emperor delayed sacrifice rider must resume after the replacement choice"
    );
}
