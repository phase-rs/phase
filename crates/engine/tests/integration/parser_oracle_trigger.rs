//! Tests relocated from `src/parser/oracle_trigger_tests.rs` (`parser::oracle_trigger`'s test module).

use engine::game::ability_utils::ability_definition_supported;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::has_unimplemented;
use engine::parser::oracle_trigger::extract_illegal_targets_disposition;
use engine::parser::oracle_trigger::test_helpers::{
    drake_shaped_trigger_execute, GILDED_DRAKE_TEXT,
};
use engine::types::ability::{
    AbilityCondition, AbilityDefinition, ChosenAttribute, ControllerRef, Effect,
    EffectOutcomeSignal, IllegalTargetsDisposition, MultiTargetSpec, QuantityExpr, TargetFilter,
    TypedFilter,
};
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;

#[test]
fn talion_runtime_triggers_only_for_matching_spell_quality() {
    let run = |name: &str, creature_pt: Option<(i32, i32)>, mana_value: u32| {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P0, &["Drawn Card"]);
        let talion = scenario
                .add_creature_from_oracle(
                    P0,
                    "Talion, the Kindly Lord",
                    3,
                    4,
                    "Flying\nAs Talion, the Kindly Lord enters the battlefield, choose a number between 1 and 10.\nWhenever an opponent casts a spell with mana value, power, or toughness equal to the chosen number, that player loses 2 life and you draw a card.",
                )
                .id();
        let spell = if let Some((power, toughness)) = creature_pt {
            scenario
                .add_creature_to_hand_from_oracle(P1, name, power, toughness, "")
                .with_mana_cost(ManaCost::generic(mana_value))
                .id()
        } else {
            scenario
                .add_spell_to_hand_from_oracle(P1, name, true, "")
                .with_mana_cost(ManaCost::generic(mana_value))
                .id()
        };
        scenario.with_mana_pool(
            P1,
            (0..mana_value)
                .map(|_| ManaUnit::new(ManaType::Colorless, talion, false, Vec::new()))
                .collect(),
        );
        let mut runner = scenario.build();
        {
            let state = runner.state_mut();
            state
                .objects
                .get_mut(&talion)
                .expect("Talion source exists")
                .chosen_attributes
                .push(ChosenAttribute::Number(3));
            state.active_player = P1;
            state.priority_player = P1;
            state.waiting_for = WaitingFor::Priority { player: P1 };
        }

        let outcome = runner.cast(spell).resolve();
        (outcome.life_delta(P1), outcome.hand_drawn(P0))
    };

    assert_eq!(run("Three-Mana Instant", None, 3), (-2, 1));
    assert_eq!(run("Three-Power Creature", Some((3, 1)), 1), (-2, 1));
    assert_eq!(run("Three-Toughness Creature", Some((1, 3)), 1), (-2, 1));
    assert_eq!(run("One-One Creature", Some((1, 1)), 1), (0, 0));
}

/// CR 603.8: Endangered Armodon's state trigger fires at runtime when its
/// controller controls a creature with toughness 2 or less, sacrificing
/// Endangered Armodon. Drives a priority pass's state-trigger check →
/// `GameRunner` resolution path (not just the parsed AST): the
/// `ControlsType` condition is evaluated against the live battlefield and
/// the `Sacrifice` effect resolves. Discriminates against reverting the new
/// parser arm — without it the text produces no StateCondition trigger, so
/// the pre-assertion fails and Endangered Armodon is never sacrificed.
#[test]
fn endangered_armodon_state_trigger_fires_and_sacrifices_self() {
    use engine::game::scenario::GameRunner;
    use engine::game::zones::create_object;
    use engine::parser::oracle::parse_oracle_text;
    use engine::types::card_type::CoreType;
    use engine::types::identifiers::CardId;
    use engine::types::phase::Phase;
    use engine::types::zones::Zone;
    use engine::types::PlayerId;
    use std::sync::Arc;

    const ORACLE: &str =
        "When you control a creature with toughness 2 or less, sacrifice this creature.";

    let parsed = parse_oracle_text(
        ORACLE,
        "Endangered Armodon",
        &[],
        &["Creature".to_string()],
        &[],
    );

    // Confirm the state trigger was parsed — shape check before the runtime test.
    assert!(
        parsed
            .triggers
            .iter()
            .any(|t| t.mode == TriggerMode::StateCondition),
        "Endangered Armodon must parse a StateCondition trigger; got {:?}",
        parsed.triggers,
    );

    let mut state = engine::types::game_state::GameState::new_two_player(7);
    state.phase = Phase::PreCombatMain;
    state.turn_number = 2;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);

    // Endangered Armodon (3/3) — its own toughness (3) does NOT satisfy the
    // "toughness 2 or less" filter, so the trigger fires only because of the
    // separate small creature below, not self-reference.
    let armodon_id = create_object(
        &mut state,
        CardId(7),
        PlayerId(0),
        "Endangered Armodon".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&armodon_id).unwrap();
        obj.controller = PlayerId(0);
        obj.power = Some(3);
        obj.toughness = Some(3);
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types.core_types.push(CoreType::Creature);
        obj.base_trigger_definitions = Arc::new(parsed.triggers.clone());
        obj.trigger_definitions = parsed.triggers.clone().into();
    }

    // A 1/1 creature under the same controller satisfies "a creature with
    // toughness 2 or less", making the state condition true.
    let token_id = create_object(
        &mut state,
        CardId(8),
        PlayerId(0),
        "Goblin".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&token_id).unwrap();
        obj.controller = PlayerId(0);
        obj.power = Some(1);
        obj.toughness = Some(1);
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types.core_types.push(CoreType::Creature);
    }

    // CR 603.8 + CR 117.1: a priority pass checks state triggers, so the small
    // creature puts the sacrifice on the stack.
    state.waiting_for = engine::types::game_state::WaitingFor::Priority {
        player: PlayerId(0),
    };
    let mut runner = GameRunner::from_state(state);
    runner
        .act(engine::types::actions::GameAction::PassPriority)
        .expect("P0 passes priority");
    assert!(
        !runner.state().stack.is_empty(),
        "state trigger must be on the stack after a priority pass",
    );

    // Resolve the sacrifice and confirm Endangered Armodon left the battlefield.
    runner.advance_until_stack_empty();
    let state = runner.state();
    assert!(
        !state.battlefield.iter().any(|id| *id == armodon_id),
        "Endangered Armodon must be sacrificed once a toughness-≤2 creature is controlled",
    );
    assert!(
        state.battlefield.iter().any(|id| *id == token_id),
        "the toughness-≤2 creature is unaffected by the sacrifice",
    );
}

/// CR 603.8: Darksteel Reactor's state trigger fires when the reactor reaches
/// 20 charge counters and resolves the WinTheGame effect, ending the game.
/// Discriminates against guard-reversion regressions: reverting the
/// `minimum: 1..` arm causes the pre-assertion (StateCondition trigger exists)
/// to fail because the oracle text no longer produces a StateCondition trigger.
#[test]
fn darksteel_reactor_state_trigger_fires_and_wins_game_at_twenty_counters() {
    use engine::game::scenario::GameRunner;
    use engine::game::zones::create_object;
    use engine::parser::oracle::parse_oracle_text;
    use engine::types::game_state::WaitingFor;
    use engine::types::identifiers::CardId;
    use engine::types::phase::Phase;
    use engine::types::zones::Zone;
    use engine::types::PlayerId;
    use std::sync::Arc;

    const ORACLE: &str = "Indestructible\n\
            At the beginning of your upkeep, you may put a charge counter on Darksteel Reactor.\n\
            When Darksteel Reactor has twenty or more charge counters on it, you win the game.";

    let parsed = parse_oracle_text(
        ORACLE,
        "Darksteel Reactor",
        &[],
        &["Artifact".to_string()],
        &[],
    );

    // Confirm the state trigger was parsed — shape check before the runtime test.
    assert!(
        parsed
            .triggers
            .iter()
            .any(|t| t.mode == TriggerMode::StateCondition),
        "Darksteel Reactor must parse a StateCondition trigger; got {:?}",
        parsed.triggers,
    );

    let mut state = engine::types::game_state::GameState::new_two_player(42);
    state.phase = Phase::PreCombatMain;
    state.turn_number = 2;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);

    // Place Darksteel Reactor on the battlefield for player 0.
    let reactor_id = create_object(
        &mut state,
        CardId(42),
        PlayerId(0),
        "Darksteel Reactor".to_string(),
        Zone::Battlefield,
    );

    // Install the parsed trigger definitions and add 20 charge counters.
    {
        let obj = state.objects.get_mut(&reactor_id).unwrap();
        obj.base_trigger_definitions = Arc::new(parsed.triggers.clone());
        obj.trigger_definitions = parsed.triggers.clone().into();
        obj.controller = PlayerId(0);
        obj.counters
            .insert(CounterType::Generic("charge".to_string()), 20);
    }

    // CR 603.8 + CR 117.1: a priority pass checks state triggers, so the reactor's
    // 20 charge counters put the WinTheGame trigger on the stack.
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };
    let mut runner = GameRunner::from_state(state);
    runner
        .act(engine::types::actions::GameAction::PassPriority)
        .expect("P0 passes priority");
    assert!(
        !runner.state().stack.is_empty(),
        "state trigger must be on the stack after a priority pass",
    );

    // Drain the stack: the WinTheGame effect resolves, eliminating all opponents.
    runner.advance_until_stack_empty();
    let state = runner.state();

    assert!(
        matches!(
            state.waiting_for,
            WaitingFor::GameOver {
                winner: Some(PlayerId(0))
            }
        ),
        "game must end with player 0 as winner after Darksteel Reactor fires; got {:?}",
        state.waiting_for,
    );
}

#[test]
fn high_tide_runtime_bonus_mana_routes_to_triggering_player_and_expires_at_eot() {
    // CR 603.7b + CR 106.12a + CR 605.1a (issue #4673): End-to-end runtime proof
    // for High Tide's real Oracle text. The instant creates an until-end-of-turn
    // multi-fire delayed trigger. Whenever ANY player taps an Island for mana,
    // THAT player (TriggeringPlayer, not the caster) gets an additional {U}.
    // The delayed trigger is purged at end-of-turn cleanup (CR 603.7b).
    use engine::game::scenario::GameScenario;
    use engine::game::triggers::check_delayed_triggers;
    use engine::game::turns::execute_cleanup;
    use engine::types::events::{GameEvent, ManaTapState};
    use engine::types::identifiers::ObjectId;
    use engine::types::phase::Phase;
    use engine::types::player::PlayerId;

    fn blue(runner: &engine::game::scenario::GameRunner, p: PlayerId) -> usize {
        runner
            .state()
            .players
            .iter()
            .find(|ps| ps.id == p)
            .unwrap()
            .mana_pool
            .count_color(ManaType::Blue)
    }

    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let hi = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "High Tide",
            true,
            "Until end of turn, whenever a player taps an Island for mana, that player adds an additional {U}.",
        )
        .id();
    let isl0 = scenario.add_basic_land(P0, ManaColor::Blue);
    let isl1 = scenario.add_basic_land(P1, ManaColor::Blue);
    // Fund P0's {U} to cast High Tide through the real pipeline (registers the
    // delayed trigger authentically).
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Blue, ObjectId(9999), false, vec![])],
    );
    let mut runner = scenario.build();
    runner.cast(hi).resolve();

    // The instant registered exactly one until-end-of-turn delayed trigger.
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "High Tide must register its delayed trigger on resolution"
    );
    // Clear any residual pool mana from the cast so bonus deltas read cleanly.
    for p in [P0, P1] {
        runner
            .state_mut()
            .players
            .iter_mut()
            .find(|ps| ps.id == p)
            .unwrap()
            .mana_pool
            .clear();
    }

    // (1) P0 taps its own Island for mana. Simulate the base {U} the land
    // produced (CR 106.12a) and fire the delayed trigger; P0 must gain the
    // ADDITIONAL {U} on top (net +2 blue in the pool: base + bonus).
    let _ = runner
        .state_mut()
        .add_mana_to_pool(P0, ManaUnit::new(ManaType::Blue, isl0, false, vec![]));
    check_delayed_triggers(
        runner.state_mut(),
        &[GameEvent::TappedForMana {
            player_id: PlayerId(0),
            source_id: isl0,
            produced: vec![ManaType::Blue],
            tap_state: ManaTapState::FromTap,
        }],
    );
    assert_eq!(
        blue(&runner, P0),
        2,
        "P0 taps its own Island: base {{U}} + additional {{U}} = 2 blue"
    );
    assert_eq!(blue(&runner, P1), 0, "P1 gets nothing when P0 taps");

    // (2) P1 (a DIFFERENT player, not the caster) taps THEIR Island. The bonus
    // must go to P1's pool — proving the recipient is TriggeringPlayer, not the
    // caster P0.
    let _ = runner
        .state_mut()
        .add_mana_to_pool(P1, ManaUnit::new(ManaType::Blue, isl1, false, vec![]));
    check_delayed_triggers(
        runner.state_mut(),
        &[GameEvent::TappedForMana {
            player_id: PlayerId(1),
            source_id: isl1,
            produced: vec![ManaType::Blue],
            tap_state: ManaTapState::FromTap,
        }],
    );
    assert_eq!(
        blue(&runner, P1),
        2,
        "P1 taps their Island: bonus routes to P1 (TriggeringPlayer), not caster P0"
    );
    assert_eq!(
        blue(&runner, P0),
        2,
        "P0's pool is unchanged when P1 taps — bonus is NOT the caster's"
    );

    // (3) End-of-turn cleanup purges the multi-fire delayed trigger (CR 603.7b).
    let mut cleanup_events = Vec::new();
    execute_cleanup(runner.state_mut(), &mut cleanup_events);
    assert_eq!(
        runner.state().delayed_triggers.len(),
        0,
        "the until-end-of-turn delayed trigger must be purged at cleanup"
    );

    // A subsequent Island tap yields NO bonus (the WheneverEvent is gone).
    for p in [P0, P1] {
        runner
            .state_mut()
            .players
            .iter_mut()
            .find(|ps| ps.id == p)
            .unwrap()
            .mana_pool
            .clear();
    }
    let _ = runner
        .state_mut()
        .add_mana_to_pool(P0, ManaUnit::new(ManaType::Blue, isl0, false, vec![]));
    check_delayed_triggers(
        runner.state_mut(),
        &[GameEvent::TappedForMana {
            player_id: PlayerId(0),
            source_id: isl0,
            produced: vec![ManaType::Blue],
            tap_state: ManaTapState::FromTap,
        }],
    );
    assert_eq!(
        blue(&runner, P0),
        1,
        "after cleanup: only the base {{U}}, no bonus — the delayed trigger expired"
    );
}

/// Runtime half of the issue #8428 fix: the corrected AST must actually put the
/// TARGET OPPONENT's cards in front of the controller. Drives Brain Maggot's
/// verbatim Oracle text through the real cast pipeline (CR 601.2 cast → ETB
/// trigger per CR 603.2 → CR 603.3d target choice → CR 701.20a reveal) and
/// asserts on the hand the engine offers for the choose clause.
///
/// A parse-only assertion cannot see this: the reveal resolver reads its player
/// from `ability.targets` first, and a `TriggeringPlayer` effect target builds
/// NO player slot at all, so the wrong-hand behavior only becomes visible once
/// the trigger reaches the stack.
#[test]
fn brain_maggot_reveals_the_target_opponents_hand_at_runtime() {
    use engine::types::phase::Phase;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![
            engine::types::mana::ManaUnit::new(
                engine::types::mana::ManaType::Black,
                engine::types::identifiers::ObjectId(98_420),
                false,
                Vec::new(),
            ),
            engine::types::mana::ManaUnit::new(
                engine::types::mana::ManaType::Black,
                engine::types::identifiers::ObjectId(98_421),
                false,
                Vec::new(),
            ),
        ],
    );

    let maggot = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Brain Maggot",
            1,
            1,
            "When this creature enters, target opponent reveals their hand and you choose a nonland card from it. Exile that card until this creature leaves the battlefield.",
        )
        .id();

    // Distinct hands so the revealed set identifies its owner unambiguously.
    let mine = scenario.add_card_to_hand(P0, "Duress");
    let theirs_a = scenario.add_card_to_hand(P1, "Llanowar Elves");
    let theirs_b = scenario.add_card_to_hand(P1, "Giant Growth");

    let mut runner = scenario.build();
    let outcome = runner.cast(maggot).target_player(P1).resolve();

    let WaitingFor::RevealChoice { player, cards, .. } = outcome.final_waiting_for() else {
        panic!(
            "expected the reveal's choose prompt, got {:?}",
            outcome.final_waiting_for()
        );
    };
    assert_eq!(
        *player, P0,
        "CR 109.5: \"you choose\" is the ability's controller, not the revealing player"
    );

    let revealed: std::collections::HashSet<_> = cards.iter().copied().collect();
    assert!(
        revealed.contains(&theirs_a) && revealed.contains(&theirs_b),
        "the TARGET OPPONENT's hand must be revealed, got {revealed:?}"
    );
    assert!(
        !revealed.contains(&mine),
        "the controller's own hand must NOT be revealed (issue #8428), got {revealed:?}"
    );
}

/// Gilded Drake's trigger body as printed (the text after "When this creature enters, ").
const GILDED_DRAKE_BODY: &str = "exchange control of this creature and up to one target creature \
    an opponent controls. If you don't or can't make an exchange, sacrifice this creature. This \
    ability still resolves if its target becomes illegal.";

/// Every node reachable through `sub_ability` / `else_ability`, root first.
fn chain_nodes(root: &AbilityDefinition) -> Vec<&AbilityDefinition> {
    let mut nodes = vec![root];
    let mut index = 0;
    while let Some(node) = nodes.get(index).copied() {
        nodes.extend(node.sub_ability.as_deref());
        nodes.extend(node.else_ability.as_deref());
        index += 1;
    }
    nodes
}

fn has_unbound_subject_node(root: &AbilityDefinition) -> bool {
    chain_nodes(root).iter().any(|node| {
        matches!(&*node.effect, Effect::Unimplemented { name, .. } if name == "unbound_subject")
    })
}

/// PU3 (CR 101.1 + CR 608.2b): Gilded Drake's verbatim text parses fully. The
/// trailing override sentence becomes the execute root's disposition instead of
/// a strict-failure node; the exchange and its rider are unchanged.
///
/// Fails on revert: the sentence stays an `unbound_subject` node and the root
/// keeps the default disposition.
#[test]
fn gilded_drake_trailing_still_resolves_sentence_stamps_the_execute_root() {
    let execute = drake_shaped_trigger_execute(GILDED_DRAKE_TEXT);

    let opponent_creature =
        TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::Opponent));
    assert_eq!(
        *execute.effect,
        Effect::ExchangeControl {
            target_a: TargetFilter::SelfRef,
            target_b: opponent_creature,
        },
        "REACH GUARD: the execute is the paired exchange"
    );
    assert_eq!(
        execute.multi_target,
        Some(MultiTargetSpec::up_to(QuantityExpr::Fixed { value: 1 })),
        "phase 1's up-to-one slot is preserved"
    );
    assert!(
        !has_unimplemented(&execute),
        "no strict-failure node remains anywhere in the chain: {execute:?}"
    );
    assert!(ability_definition_supported(&execute));
    assert_eq!(
        execute.illegal_targets_disposition,
        IllegalTargetsDisposition::StillResolves
    );

    let rider = execute
        .sub_ability
        .as_deref()
        .expect("the sacrifice rider follows the exchange");
    assert!(
        matches!(
            &*rider.effect,
            Effect::Sacrifice {
                target: TargetFilter::SelfRef,
                ..
            }
        ),
        "expected the rider to sacrifice the Drake, got {:?}",
        rider.effect
    );
    assert_eq!(
        rider.condition,
        Some(AbilityCondition::Not {
            condition: Box::new(AbilityCondition::EffectOutcome {
                signal: EffectOutcomeSignal::OptionalEffectPerformed,
            }),
        }),
        "CR 608.2c: \"If you don't or can't make an exchange\" gates the rider"
    );
    assert_eq!(
        rider.illegal_targets_disposition,
        IllegalTargetsDisposition::DoesNotResolve,
        "the disposition is stamped on the root only"
    );
    assert!(rider.sub_ability.is_none(), "nothing follows the rider");
}

/// PU5 (CR 101.1 + CR 608.2b): the override is detached only when it is the
/// last sentence of the body and an effect precedes it. Any other placement
/// returns the text unchanged with the default, so the sentence stays a strict
/// failure. Paired positives: the verbatim-body leg below and PU3.
#[test]
fn still_resolves_sentence_is_detached_only_as_the_last_sentence() {
    let (kept, disposition) = extract_illegal_targets_disposition(GILDED_DRAKE_BODY);
    assert_eq!(disposition, IllegalTargetsDisposition::StillResolves);
    assert_eq!(
        kept,
        "exchange control of this creature and up to one target creature an opponent controls. \
         If you don't or can't make an exchange, sacrifice this creature.",
        "the previous sentence keeps its period"
    );

    for unchanged in [
        // Not the last sentence.
        "exchange control of this creature and up to one target creature an opponent controls. \
         This ability still resolves if its target becomes illegal. Draw a card.",
        // Inside a longer sentence, not a sentence of its own.
        "exchange control of this creature and up to one target creature an opponent controls. \
         If you don't or can't make an exchange, sacrifice this creature; this ability still \
         resolves if its target becomes illegal.",
        // The whole body: no effect for the override to govern.
        "This ability still resolves if its target becomes illegal.",
    ] {
        let (kept, disposition) = extract_illegal_targets_disposition(unchanged);
        assert_eq!(kept, unchanged);
        assert_eq!(disposition, IllegalTargetsDisposition::DoesNotResolve);
    }

    // Full pipeline: the sentence placed before the rider is not detached.
    let not_trailing = drake_shaped_trigger_execute(
        "Flying\nWhen this creature enters, exchange control of this creature and up to one \
         target creature an opponent controls. This ability still resolves if its target becomes \
         illegal. If you don't or can't make an exchange, sacrifice this creature.",
    );
    assert!(
        matches!(&*not_trailing.effect, Effect::ExchangeControl { .. }),
        "REACH GUARD: the body parsed past the exchange, got {:?}",
        not_trailing.effect
    );
    assert!(has_unbound_subject_node(&not_trailing));
    assert!(chain_nodes(&not_trailing)
        .iter()
        .all(|node| node.illegal_targets_disposition == IllegalTargetsDisposition::DoesNotResolve));

    // Full pipeline: a body that is only the sentence stays a strict failure.
    let only_override = drake_shaped_trigger_execute(
        "When this creature enters, this ability still resolves if its target becomes illegal.",
    );
    assert!(has_unbound_subject_node(&only_override));
    assert!(!ability_definition_supported(&only_override));
    assert_eq!(
        only_override.illegal_targets_disposition,
        IllegalTargetsDisposition::DoesNotResolve
    );
}

/// PU6 (CR 603.12 + CR 608.2b): a shape fixture, not a printed card. When the
/// targeted exchange sits on a reflexive "When you do" node, that node is a
/// separate triggered ability with its own targets, so a stamp on the chain's
/// root could not govern "its target". The override fails closed: the sentence
/// stays a strict failure and no node carries the override.
///
/// Fails on revert of the fail-close: the root is stamped and the card reports
/// supported while the reflexive ability would still fizzle. Paired positive: PU3.
#[test]
fn still_resolves_sentence_fails_closed_beside_a_reflexive_when_you_do_node() {
    let execute = drake_shaped_trigger_execute(
        "When this creature enters, you may pay {1}. When you do, exchange control of this \
         creature and up to one target creature an opponent controls. This ability still \
         resolves if its target becomes illegal.",
    );
    let nodes = chain_nodes(&execute);
    assert!(
        nodes.iter().any(|node| node
            .condition
            .as_ref()
            .is_some_and(AbilityCondition::has_when_you_do_marker)),
        "REACH GUARD: the chain carries the reflexive marker the fail-close reads: {execute:?}"
    );
    assert!(nodes
        .iter()
        .all(|node| node.illegal_targets_disposition == IllegalTargetsDisposition::DoesNotResolve));
    assert!(has_unimplemented(&execute));
    assert!(!ability_definition_supported(&execute));
}
