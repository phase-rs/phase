use crate::support::shared_card_db;
use engine::ai_support::legal_actions_full;
use engine::game::combat::AttackTarget;
use engine::game::engine::EngineError;
use engine::game::keywords::object_has_effective_keyword_kind;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityTag, ChoiceType, Comparator, CountScope, CyclingHistoryWindow, Effect, QuantityExpr,
    QuantityRef, ResolvedAbility, TargetFilter, TargetRef, TriggerCondition, TriggerDefinition,
};
use engine::types::actions::GameAction;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::{
    CastPaymentMode, CyclingOccurrence, CyclingRecord, GameState, ManaChoice, ManaChoicePrompt,
    NamedChoiceSourceBinding, PayCostKind, StackEntryKind, WaitingFor, ZoneDeparture,
};
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
use engine::types::keywords::{Keyword, KeywordKind};
use engine::types::mana::{ManaColor, ManaType};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::resolved_commands::{
    ResolvedLedgerEdit, ResolvedRulesCommand, ResolvedRulesJournal,
    ResolvedStackEntryFinalizeCommand,
};
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

fn main_phase() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

fn printed(scenario: &mut GameScenario, player: PlayerId, name: &str, zone: Zone) -> ObjectId {
    scenario.add_real_card(
        player,
        name,
        zone,
        shared_card_db().expect("printed fixture"),
    )
}

fn street_wraith(scenario: &mut GameScenario) -> ObjectId {
    printed(scenario, P0, "Street Wraith", Zone::Hand)
}

fn activation(
    source_id: ObjectId,
    ability_index: usize,
    payment_mode: CastPaymentMode,
) -> GameAction {
    GameAction::ActivateAbility {
        source_id,
        ability_index,
        payment_mode,
    }
}

fn offered(runner: &GameRunner, source: ObjectId) -> Vec<usize> {
    legal_actions_full(runner.state())
        .0
        .iter()
        .filter_map(|action| match action {
            GameAction::ActivateAbility {
                source_id,
                ability_index,
                ..
            } if *source_id == source => Some(*ability_index),
            _ => None,
        })
        .collect()
}

fn act(runner: &mut GameRunner, action: GameAction) -> Vec<GameEvent> {
    runner.act(action).expect("public action succeeds").events
}

fn tap_land(runner: &mut GameRunner, land: ObjectId) -> Vec<GameEvent> {
    let (_, _, grouped) = legal_actions_full(runner.state());
    let action = grouped[&land]
        .iter()
        .find(|action| matches!(action, GameAction::TapLandForMana { .. }))
        .expect("land's mana ability is offered")
        .clone();
    act(runner, action)
}

fn priority(runner: &GameRunner) {
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ),
        "{:?}",
        runner.state().waiting_for
    );
}

fn passes(runner: &mut GameRunner) -> Vec<GameEvent> {
    priority(runner);
    let mut events = act(runner, GameAction::PassPriority);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ));
    events.extend(act(runner, GameAction::PassPriority));
    events
}

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn cycle_events(events: &[GameEvent]) -> Vec<(PlayerId, ObjectId)> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::Cycled {
                player_id,
                object_id,
            } => Some((*player_id, *object_id)),
            _ => None,
        })
        .collect()
}

fn discard_subject(event: &GameEvent) -> Option<(PlayerId, ObjectId)> {
    match event {
        GameEvent::Discarded {
            player_id,
            object_id,
            ..
        } => Some((*player_id, *object_id)),
        _ => None,
    }
}

fn keyword_events(events: &[GameEvent]) -> Vec<(AbilityTag, PlayerId, ObjectId, bool)> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::KeywordAbilityActivated {
                ability_tag,
                player_id,
                source_id,
                is_mana_ability,
            } => Some((*ability_tag, *player_id, *source_id, *is_mana_ability)),
            _ => None,
        })
        .collect()
}

fn journaled_cycle(runner: &GameRunner, source: ObjectId) {
    let records = &runner.state().abilities_activated_this_turn_by_player[&P0];
    let records: Vec<_> = records
        .iter()
        .filter(|record| record.source == source)
        .collect();
    assert_eq!(records.len(), 1);
    let record = records[0];
    assert_eq!(
        (record.activator, record.source_zone, record.ability_tag),
        (P0, Zone::Hand, Some(AbilityTag::Cycling))
    );
}

fn assert_cycle(events: &[GameEvent], source: ObjectId) {
    assert_eq!(
        events
            .iter()
            .filter_map(discard_subject)
            .collect::<Vec<_>>(),
        [(P0, source)]
    );
    assert_eq!(cycle_events(events), [(P0, source)]);
    assert!(keyword_events(events)
        .iter()
        .all(|event| event.0 != AbilityTag::Cycling));
}

fn activation_entry(runner: &GameRunner, source: ObjectId) -> ObjectId {
    let entry = runner.state().stack.front().expect("placed activation");
    assert_eq!(entry.controller, P0);
    assert!(
        matches!(entry.kind, StackEntryKind::ActivatedAbility { source_id, .. } if source_id == source)
    );
    entry.id
}

// CR 702.29a + CR 602.2b: Life and discard are paid before the cycling draw.
// CR 119.4: A player with one life cannot pay two life.
#[test]
fn v1_direct_ordinary_cycle() {
    let mut scenario = main_phase();
    let first = street_wraith(&mut scenario);
    let second = street_wraith(&mut scenario);
    printed(&mut scenario, P1, "Island", Zone::Library);
    let second_draw = scenario.add_card_to_library_top(P0, "Mountain");
    let first_draw = scenario.add_card_to_library_top(P0, "Island");
    let mut runner = scenario.build();
    let first_entry = cycle_wraith(&mut runner, first);
    let first_turn = runner.state().turn_number;
    let first_record = cycle_record("Street Wraith", first_turn);
    assert_placement(runner.state(), first_entry, &first_record, 0);
    assert_history(runner.state(), std::slice::from_ref(&first_record));
    resolve_cycle_draw(&mut runner, first_draw);
    assert_history(runner.state(), std::slice::from_ref(&first_record));
    assert_eq!(cycle_command_counts(runner.state()), (1, 1));

    advance_turn(&mut runner);
    if matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ) {
        act(&mut runner, GameAction::PassPriority);
    }
    let second_turn = runner.state().turn_number;
    assert!(second_turn > first_turn);
    priority(&runner);
    let second_entry = cycle_wraith(&mut runner, second);
    assert_ne!(first_entry, second_entry);
    let second_record = cycle_record("Street Wraith", second_turn);
    assert_placement(runner.state(), second_entry, &second_record, 1);
    let expected = [first_record, second_record];
    assert_history(runner.state(), &expected);
    resolve_cycle_draw(&mut runner, second_draw);
    assert_history(runner.state(), &expected);

    let mut scenario = main_phase();
    scenario.with_life(P0, 1);
    let card = street_wraith(&mut scenario);
    let mut runner = scenario.build();
    let index = wraith_index(&runner, card);
    let result = runner.act(activation(card, index, CastPaymentMode::Auto));
    assert!(matches!(
        result,
        Err(EngineError::ActionNotAllowed(reason)) if reason == "Cannot pay activation cost"
    ));
    priority(&runner);
    assert!(runner.state().stack.is_empty());
    assert!(runner
        .state()
        .abilities_activated_this_turn_by_player
        .is_empty());
    assert_eq!(zone(&runner, card), Zone::Hand);
    assert_eq!(runner.state().players[0].life, 1);
    assert_history(runner.state(), &[]);
    assert_eq!(cycle_command_counts(runner.state()), (0, 0));
}

// CR 702.29e + CR 702.29f: The hand grant pays for a search and counts as cycling.
// CR 603.3d governs target selection for Snare Tactician's trigger. CR 603.3 and
// CR 405.2 put it above the pending slivercycling ability.
// CR 701.23b: The search can fail to find even with a Sliver in the library.
// CancelCast abandons the pending activation before payment. CR 602.2b and
// CR 601.2h-i govern completion, and CR 702.29f ties the cycle to discarding the
// card to pay the typecycling cost.
#[test]
fn v2_granted_slivercycling_after_discard() {
    let mut control = main_phase();
    let card = printed(&mut control, P0, "Sidewinder Sliver", Zone::Hand);
    for _ in 0..3 {
        control.add_basic_land(P0, ManaColor::Green);
    }
    assert!(offered(&control.build(), card).is_empty());

    for (second_grant, cancel, find) in [
        (false, false, true),
        (true, false, true),
        (false, true, false),
    ] {
        let mut scenario = main_phase();
        let card = printed(&mut scenario, P0, "Sidewinder Sliver", Zone::Hand);
        let snare = printed(&mut scenario, P0, "Snare Tactician", Zone::Battlefield);
        let homing = printed(&mut scenario, P1, "Homing Sliver", Zone::Battlefield);
        let elves = printed(&mut scenario, P1, "Llanowar Elves", Zone::Battlefield);
        if second_grant {
            printed(&mut scenario, P0, "Homing Sliver", Zone::Battlefield);
        }
        let sliver = printed(&mut scenario, P0, "Homing Sliver", Zone::Library);
        let non_sliver = scenario.add_card_to_library_top(P0, "Island");
        let lands =
            std::array::from_fn::<_, 3, _>(|_| scenario.add_basic_land(P0, ManaColor::Green));
        let mut runner = scenario.build();
        let indices = offered(&runner, card);
        assert!(!indices.is_empty(), "Homing grants a cycling offer");
        assert!(object_has_effective_keyword_kind(
            runner.state(),
            card,
            KeywordKind::Typecycling
        ));
        let index = indices[0];
        assert!(index >= runner.state().objects[&card].abilities.len());
        assert!(!runner.state().objects[&homing].tapped && !runner.state().objects[&elves].tapped);
        let mut events = Vec::new();
        for attempt in 0..(1 + usize::from(cancel)) {
            events.extend(act(
                &mut runner,
                activation(card, index, CastPaymentMode::Manual),
            ));
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::ManaPayment { player: P0, .. }
            ));
            let pending = runner
                .state()
                .waiting_for
                .pending_cast_ref()
                .or(runner.state().pending_cast.as_deref())
                .expect("pending activation");
            let record = pending
                .ability
                .activation_record
                .as_deref()
                .expect("bound activation record");
            assert_eq!(
                (record.activator, record.source, record.ability_tag),
                (P0, card, Some(AbilityTag::Cycling))
            );
            assert_eq!(zone(&runner, card), Zone::Hand);
            assert!(cycle_events(&events).is_empty());
            assert_eq!(runner.state().players[0].mana_pool.total(), 0);
            assert!(lands
                .iter()
                .all(|land| !runner.state().objects[land].tapped));
            if cancel && attempt == 0 {
                events.extend(act(&mut runner, GameAction::CancelCast));
                priority(&runner);
                assert!(runner.state().stack.is_empty());
                assert!(runner
                    .state()
                    .abilities_activated_this_turn_by_player
                    .is_empty());
                assert!(cycle_events(&events).is_empty());
                assert!(events.iter().all(|event| discard_subject(event).is_none()));
                assert_eq!(zone(&runner, card), Zone::Hand);
                assert_history(runner.state(), &[]);
                assert_eq!(cycle_command_counts(runner.state()), (0, 0));
            }
        }
        for land in lands {
            events.extend(tap_land(&mut runner, land));
        }
        events.extend(act(&mut runner, GameAction::PassPriority));
        assert!(lands.iter().all(|land| runner.state().objects[land].tapped));
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        assert_eq!(zone(&runner, card), Zone::Graveyard);
        let search = activation_entry(&runner, card);
        let expected = [cycle_record(
            "Sidewinder Sliver",
            runner.state().turn_number,
        )];
        let (_, finalize_position, finalize) =
            assert_placement(runner.state(), search, &expected[0], 0);
        assert_history(runner.state(), &expected);
        let reader = ResolvedAbility::new(Effect::NoOp, vec![], card, P0);
        assert_cycle_windows(runner.state(), &reader, CountScope::Controller, [1, 1]);
        assert_eq!(cycle_command_counts(runner.state()), (1, 1));
        if second_grant {
            assert_granted_replay_contract(runner.state(), finalize);
        }
        assert!(!object_has_effective_keyword_kind(
            runner.state(),
            card,
            KeywordKind::Typecycling
        ));
        journaled_cycle(&runner, card);
        assert_cycle(&events, card);

        let WaitingFor::TriggerTargetSelection {
            player,
            target_slots,
            ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "expected Tactician target choice, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(*player, P0);
        let cycled = GameEvent::Cycled {
            player_id: P0,
            object_id: card,
        };
        let trigger_position = runner
            .state()
            .resolved_rules_journal
            .entries()
            .iter()
            .position(|row| {
                matches!(
                    &row.command,
                    Some(ResolvedRulesCommand::StackPush(command))
                        if command.entry.source_id == snare
                            && matches!(
                                &command.entry.kind,
                                StackEntryKind::TriggeredAbility {
                                    trigger_event: Some(event),
                                    ..
                                } if event == &cycled
                            )
                )
            })
            .expect("Snare Tactician's collected trigger for this cycle was pushed");
        assert!(finalize_position < trigger_position);
        assert_eq!(target_slots.len(), 1);
        let targets = &target_slots[0].legal_targets;
        assert_eq!(targets.len(), 2);
        assert!(
            targets.contains(&TargetRef::Object(homing))
                && targets.contains(&TargetRef::Object(elves))
        );
        events.extend(act(
            &mut runner,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(elves)),
            },
        ));
        priority(&runner);
        assert_eq!(runner.state().stack.len(), 2);
        assert!(!offered(&runner, card).contains(&index));
        let trigger = runner.state().stack.back().unwrap();
        assert_eq!(trigger.source_id, snare);
        assert!(matches!(
            trigger.kind,
            StackEntryKind::TriggeredAbility { .. }
        ));
        assert_eq!(
            trigger.ability().unwrap().targets,
            [TargetRef::Object(elves)]
        );
        events.extend(passes(&mut runner));
        priority(&runner);
        assert!(runner.state().objects[&elves].tapped);
        assert!(!runner.state().objects[&homing].tapped);
        assert_eq!(runner.state().stack.len(), 1);
        assert_eq!(runner.state().stack.back().unwrap().id, search);
        events.extend(passes(&mut runner));
        let WaitingFor::SearchChoice {
            player,
            cards,
            count,
            reveal,
            allows_partial_find,
            ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "expected Sliver search choice, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(
            (*player, *count, *reveal, *allows_partial_find),
            (P0, 1, true, true)
        );
        assert_eq!(cards.as_slice(), &[sliver]);
        events.extend(act(
            &mut runner,
            GameAction::SelectCards {
                cards: if find { vec![sliver] } else { vec![] },
            },
        ));
        priority(&runner);
        assert!(runner.state().stack.is_empty());
        assert_eq!(
            zone(&runner, sliver),
            if find { Zone::Hand } else { Zone::Library }
        );
        assert_eq!(zone(&runner, non_sliver), Zone::Library);
        assert_cycle(&events, card);
        assert_history(runner.state(), &expected);
    }
}

fn mana(color: ManaType, amount: usize) -> Vec<engine::types::mana::ManaUnit> {
    (0..amount)
        .map(|_| engine::types::mana::ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect()
}

fn haven_payment(runner: &mut GameRunner, haven: ObjectId, accept: bool) {
    let WaitingFor::OptionalEffectChoice {
        player, source_id, ..
    } = runner.state().waiting_for
    else {
        panic!(
            "expected Drake Haven payment, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!((player, source_id), (P0, haven));
    let before = runner.state().players[0].mana_pool.total();
    assert!(before >= 1, "the payment must be available");
    let events = act(runner, GameAction::DecideOptionalEffect { accept });
    assert!(cycle_events(&events).is_empty());
    priority(runner);
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        before - usize::from(accept)
    );
}

fn assert_drakes(runner: &GameRunner, count: usize) {
    let drakes: Vec<_> = runner
        .state()
        .objects
        .values()
        .filter(|object| {
            object.zone == Zone::Battlefield && object.is_token && object.name == "Drake"
        })
        .collect();
    assert_eq!(drakes.len(), count);
    for drake in drakes {
        assert_eq!(drake.controller, P0);
        assert_eq!((drake.power, drake.toughness), (Some(2), Some(2)));
        assert_eq!(drake.effective_colors(), [ManaColor::Blue]);
        assert!(drake.keywords.contains(&Keyword::Flying));
    }
}

// CR 702.29d: A cycle produces one compound trigger, whose payment is optional.
#[test]
fn v3_drake_haven_cycle_payment_and_decline() {
    for accept in [true, false] {
        let mut scenario = main_phase();
        let haven = printed(&mut scenario, P0, "Drake Haven", Zone::Battlefield);
        let card = street_wraith(&mut scenario);
        let draw = scenario.add_card_to_library_top(P0, "Island");
        scenario.with_mana_pool(P0, mana(ManaType::Blue, 1));
        let mut runner = scenario.build();
        let index = offered(&runner, card)[0];
        let events = act(&mut runner, activation(card, index, CastPaymentMode::Auto));
        priority(&runner);
        let cycling = activation_entry(&runner, card);
        assert_eq!(runner.state().stack.len(), 2);
        let trigger = runner.state().stack.back().unwrap();
        assert_eq!(trigger.source_id, haven);
        assert!(matches!(
            trigger.kind,
            StackEntryKind::TriggeredAbility { .. }
        ));
        assert_cycle(&events, card);
        passes(&mut runner);
        haven_payment(&mut runner, haven, accept);
        assert_drakes(&runner, usize::from(accept));
        assert_eq!(runner.state().stack.len(), 1);
        assert_eq!(runner.state().stack.back().unwrap().id, cycling);
        assert_eq!(zone(&runner, draw), Zone::Library);
        passes(&mut runner);
        priority(&runner);
        assert!(runner.state().stack.is_empty());
        assert_eq!(zone(&runner, draw), Zone::Hand);
    }
}

// CR 603.2c + CR 603.3b: Two separate discards give two independently ordered triggers.
#[test]
fn v3_drake_haven_looting_discard_order_and_payment() {
    let mut scenario = main_phase();
    let haven = printed(&mut scenario, P0, "Drake Haven", Zone::Battlefield);
    let looting = printed(&mut scenario, P0, "Faithless Looting", Zone::Hand);
    let cycler = street_wraith(&mut scenario);
    let other = printed(&mut scenario, P0, "Llanowar Elves", Zone::Hand);
    let first_draw = scenario.add_card_to_library_top(P0, "Island");
    let second_draw = scenario.add_card_to_library_top(P0, "Mountain");
    let mut pool = mana(ManaType::Red, 1);
    pool.extend(mana(ManaType::Blue, 2));
    scenario.with_mana_pool(P0, pool);
    let mut runner = scenario.build();
    runner.cast(looting).commit();
    priority(&runner);
    assert_eq!(runner.state().stack.back().unwrap().source_id, looting);
    let mut events = passes(&mut runner);
    assert_eq!(zone(&runner, first_draw), Zone::Hand);
    assert_eq!(zone(&runner, second_draw), Zone::Hand);
    let WaitingFor::DiscardChoice {
        player,
        source_id,
        count,
        cards,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("expected Looting discard prompt");
    };
    assert_eq!((*player, *source_id, *count), (P0, looting, 2));
    assert_eq!(cards.len(), 4);
    assert!([cycler, other, first_draw, second_draw]
        .iter()
        .all(|card| cards.contains(card)));
    events.extend(act(
        &mut runner,
        GameAction::SelectCards {
            cards: vec![cycler, other],
        },
    ));
    assert_eq!(
        events
            .iter()
            .filter_map(discard_subject)
            .collect::<Vec<_>>(),
        [(P0, cycler), (P0, other)]
    );
    assert!(cycle_events(&events).is_empty());
    let WaitingFor::OrderTriggers { player, triggers } = &runner.state().waiting_for else {
        panic!(
            "expected two Haven triggers to order, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(triggers.len(), 2);
    assert!(triggers.iter().all(|trigger| trigger.source_id == haven));
    let pending = runner
        .state()
        .pending_trigger_order
        .as_ref()
        .expect("ordering context");
    let group = pending
        .groups
        .iter()
        .find(|group| group.controller == P0)
        .unwrap();
    let subjects: Vec<_> = group
        .triggers
        .iter()
        .map(|ctx| {
            ctx.pending
                .trigger_event
                .as_ref()
                .and_then(discard_subject)
                .expect("discard provenance")
        })
        .collect();
    assert!(subjects.contains(&(P0, cycler)) && subjects.contains(&(P0, other)));
    assert_eq!(subjects.len(), 2);
    act(&mut runner, GameAction::OrderTriggers { order: vec![1, 0] });
    priority(&runner);
    assert_eq!(runner.state().stack.len(), 2);
    let entries: Vec<_> = runner
        .state()
        .stack
        .iter()
        .zip(subjects.iter().rev())
        .map(|(entry, subject)| {
            let StackEntryKind::TriggeredAbility {
                source_id,
                trigger_event,
                ..
            } = &entry.kind
            else {
                panic!("expected Haven stack entry");
            };
            assert_eq!((*source_id, entry.controller), (haven, P0));
            assert_eq!(
                trigger_event.as_ref().and_then(discard_subject),
                Some(*subject)
            );
            entry.id
        })
        .collect();
    assert_ne!(entries[0], entries[1]);
    passes(&mut runner);
    haven_payment(&mut runner, haven, true);
    assert_drakes(&runner, 1);
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(runner.state().stack.back().unwrap().id, entries[0]);
    passes(&mut runner);
    haven_payment(&mut runner, haven, false);
    assert_drakes(&runner, 1);
    assert!(runner.state().stack.is_empty());
}

// CR 702.142a + CR 702.142b: Declare the attack before activating Boast.
#[test]
fn v4_stack_side_boast() {
    let mut scenario = main_phase();
    let usher = printed(&mut scenario, P0, "Usher of the Fallen", Zone::Battlefield);
    let plains = scenario.add_basic_land(P0, ManaColor::White);
    let other_land = scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();
    assert!(
        offered(&runner, usher).is_empty(),
        "funded but has not attacked"
    );
    runner.advance_to_phase(Phase::BeginCombat);
    runner
        .declare_attackers(&[(usher, AttackTarget::Player(P1))])
        .expect("declare Usher");
    priority(&runner);
    assert!(runner.state().objects[&usher].tapped);
    let indices = offered(&runner, usher);
    assert_eq!(indices.len(), 1);
    tap_land(&mut runner, plains);
    tap_land(&mut runner, other_land);
    let events = act(
        &mut runner,
        activation(usher, indices[0], CastPaymentMode::Auto),
    );
    priority(&runner);
    activation_entry(&runner, usher);
    assert_eq!(
        keyword_events(&events),
        [(AbilityTag::Boast, P0, usher, false)]
    );
    let tokens = |runner: &GameRunner| {
        runner
            .state()
            .objects
            .values()
            .filter(|object| {
                object.zone == Zone::Battlefield
                    && object.is_token
                    && object.controller == P0
                    && object.name == "Human Warrior"
                    && object.power == Some(1)
                    && object.toughness == Some(1)
                    && object.effective_colors() == [ManaColor::White]
                    && object
                        .card_types
                        .subtypes
                        .iter()
                        .any(|kind| kind == "Human")
                    && object
                        .card_types
                        .subtypes
                        .iter()
                        .any(|kind| kind == "Warrior")
            })
            .count()
    };
    assert_eq!(tokens(&runner), 0);
    let events = passes(&mut runner);
    priority(&runner);
    assert!(runner.state().stack.is_empty());
    assert!(keyword_events(&events).is_empty());
    assert_eq!(tokens(&runner), 1);
}

// CR 702.177a + CR 605.1a + CR 605.3b: Exhaust's mana mode completes off the
// stack after the color choice. Its draw mode still places a stack entry.
#[test]
fn v5_exhaust_mana_flag_and_untagged_mana() {
    let mut scenario = main_phase();
    let loot = printed(&mut scenario, P0, "Loot, the Pathfinder", Zone::Battlefield);
    scenario.with_mana_pool(P0, mana(ManaType::Green, 1));
    let mut runner = scenario.build();
    let events = act(&mut runner, activation(loot, 0, CastPaymentMode::Auto));
    let WaitingFor::ChooseManaColor {
        player,
        choice: ManaChoicePrompt::SingleColor { options },
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected Loot's color choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(options.len(), 5);
    assert!(options.contains(&ManaType::Blue));
    assert!(keyword_events(&events).is_empty());
    assert!(runner.state().stack.is_empty());
    let events = act(
        &mut runner,
        GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Blue),
            count: 1,
        },
    );
    priority(&runner);
    assert_eq!(
        keyword_events(&events),
        [(AbilityTag::Exhaust, P0, loot, true)]
    );
    assert!(runner.state().objects[&loot].tapped);
    assert_eq!(runner.state().players[0].mana_pool.total(), 3);
    assert_eq!(
        runner.state().players[0]
            .mana_pool
            .count_color(ManaType::Blue),
        3
    );
    assert!(runner.state().stack.is_empty());

    let mut scenario = main_phase();
    let elves = printed(&mut scenario, P0, "Llanowar Elves", Zone::Battlefield);
    let mut runner = scenario.build();
    let events = act(&mut runner, activation(elves, 0, CastPaymentMode::Auto));
    priority(&runner);
    assert!(runner.state().objects[&elves].tapped);
    assert_eq!(
        runner.state().players[0]
            .mana_pool
            .count_color(ManaType::Green),
        1
    );
    assert!(runner.state().stack.is_empty());
    assert!(keyword_events(&events).is_empty());

    let mut scenario = main_phase();
    let loot = printed(&mut scenario, P0, "Loot, the Pathfinder", Zone::Battlefield);
    let drawn =
        ["Island", "Mountain", "Forest"].map(|name| scenario.add_card_to_library_top(P0, name));
    scenario.with_mana_pool(P0, mana(ManaType::Blue, 1));
    let mut runner = scenario.build();
    let events = act(&mut runner, activation(loot, 1, CastPaymentMode::Auto));
    priority(&runner);
    activation_entry(&runner, loot);
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(
        keyword_events(&events),
        [(AbilityTag::Exhaust, P0, loot, false)]
    );
    assert!(drawn.iter().all(|id| zone(&runner, *id) == Zone::Library));
    let events = passes(&mut runner);
    priority(&runner);
    assert!(runner.state().stack.is_empty());
    assert!(keyword_events(&events).is_empty());
    assert!(drawn.iter().all(|id| zone(&runner, *id) == Zone::Hand));
}

#[test]
fn printed_yidaro_and_phoenix_cycling_conditions() {
    let parse = |text, name, keywords: &[&str], subtypes: &[&str]| {
        parse_oracle_text(
            text,
            name,
            &keywords
                .iter()
                .map(|value| (*value).to_owned())
                .collect::<Vec<_>>(),
            &["Creature".to_owned()],
            &subtypes
                .iter()
                .map(|value| (*value).to_owned())
                .collect::<Vec<_>>(),
        )
    };
    let yidaro = parse(
        "Trample, haste\nCycling {1}{R}\nWhen you cycle this card, shuffle it into your library from your graveyard. If you've cycled a card named Yidaro, Wandering Monster four or more times this game, put it onto the battlefield from your graveyard instead. (Do this before you draw.)",
        "Yidaro, Wandering Monster", &["Cycling", "Haste", "Trample"], &["Dinosaur", "Turtle"],
    );
    assert!(yidaro
        .extracted_keywords
        .iter()
        .any(|keyword| matches!(keyword, Keyword::Cycling(_))));
    let cycle = yidaro
        .triggers
        .iter()
        .find(|trigger| {
            trigger.mode == TriggerMode::Cycled && trigger.valid_card == Some(TargetFilter::SelfRef)
        })
        .expect("Yidaro self-cycle trigger");
    assert_yidaro_branch(cycle);

    let phoenix = parse(
        "Flying\nWhen this creature enters, you may return target instant or sorcery card with a cycling ability from your graveyard to your hand.\nAt the beginning of each end step, if you cycled two or more cards this turn, return this card from your graveyard to your hand.",
        "Spellpyre Phoenix", &["Flying"], &["Phoenix"],
    );
    let end = phoenix
        .triggers
        .iter()
        .find(|trigger| trigger.mode == TriggerMode::Phase && trigger.phase == Some(Phase::End))
        .expect("Phoenix end-step trigger");
    assert_phoenix_definition(end);
}

fn cycle_record(name: &str, turn: u32) -> CyclingRecord {
    CyclingRecord {
        name: name.to_owned(),
        turn,
    }
}

fn assert_history(state: &GameState, expected: &[CyclingRecord]) {
    let actual: Vec<_> = state
        .cycles_completed_this_game_by_player
        .get(&P0)
        .into_iter()
        .flatten()
        .cloned()
        .collect();
    assert_eq!(actual, expected);
}

fn wraith_index(runner: &GameRunner, card: ObjectId) -> usize {
    runner.state().objects[&card]
        .abilities
        .iter()
        .position(|def| def.ability_tag == Some(AbilityTag::Cycling))
        .expect("printed cycling")
}

fn cycle_wraith(runner: &mut GameRunner, card: ObjectId) -> ObjectId {
    let life = runner.state().players[0].life;
    let index = wraith_index(runner, card);
    let events = act(runner, activation(card, index, CastPaymentMode::Auto));
    assert_eq!(runner.state().players[0].life, life - 2);
    assert_eq!(zone(runner, card), Zone::Graveyard);
    journaled_cycle(runner, card);
    assert_cycle(&events, card);
    activation_entry(runner, card)
}

fn resolve_cycle_draw(runner: &mut GameRunner, card: ObjectId) {
    assert_eq!(zone(runner, card), Zone::Library);
    let size = runner.state().players[0].hand.len();
    let events = passes(runner);
    assert!(cycle_events(&events).is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert_eq!(zone(runner, card), Zone::Hand);
    assert_eq!(runner.state().players[0].hand.len(), size + 1);
}

fn next_turn_action(state: &GameState) -> (PlayerId, GameAction) {
    match state.waiting_for {
        WaitingFor::Priority { player } => (player, GameAction::PassPriority),
        WaitingFor::DeclareAttackers { player, .. } => (
            player,
            GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
        ),
        WaitingFor::DeclareBlockers { player, .. } => (
            player,
            GameAction::DeclareBlockers {
                assignments: vec![],
            },
        ),
        ref other => panic!("unexpected turn transition prompt {other:?}"),
    }
}

fn advance_turn(runner: &mut GameRunner) {
    let turn = runner.state().turn_number;
    for _ in 0..48 {
        if runner.state().turn_number > turn {
            return;
        }
        let (_, action) = next_turn_action(runner.state());
        act(runner, action);
    }
    panic!("expected the next turn");
}

fn completed_cycle(command: &ResolvedRulesCommand) -> Option<&ResolvedLedgerEdit> {
    match command {
        ResolvedRulesCommand::LedgerEdit(command)
            if matches!(command.edit, ResolvedLedgerEdit::CompletedCycle { .. }) =>
        {
            Some(&command.edit)
        }
        _ => None,
    }
}

fn cycling_finalize(command: &ResolvedRulesCommand) -> Option<&ResolvedStackEntryFinalizeCommand> {
    match command {
        ResolvedRulesCommand::StackEntryFinalize(command)
            if command.resulting_cycling_occurrence.is_some() =>
        {
            Some(command)
        }
        _ => None,
    }
}

fn cycle_command_counts(state: &GameState) -> (usize, usize) {
    let commands = || {
        state
            .resolved_rules_journal
            .entries()
            .iter()
            .filter_map(|e| e.command.as_ref())
    };
    (
        commands().filter_map(completed_cycle).count(),
        commands().filter_map(cycling_finalize).count(),
    )
}

fn assert_placement<'a>(
    state: &'a GameState,
    entry: ObjectId,
    record: &CyclingRecord,
    index: usize,
) -> (usize, usize, &'a ResolvedStackEntryFinalizeCommand) {
    let journal = state.resolved_rules_journal.entries();
    let mut edits = journal.iter().enumerate().filter_map(|(position, row)| {
        let edit = completed_cycle(row.command.as_ref()?)?;
        matches!(edit, ResolvedLedgerEdit::CompletedCycle { stack_entry, .. } if *stack_entry == entry)
            .then_some((position, edit))
    });
    let (edit_position, edit) = edits.next().expect("completed cycle edit for this entry");
    assert!(edits.next().is_none());
    assert_eq!(
        edit,
        &ResolvedLedgerEdit::CompletedCycle {
            player: P0,
            record: record.clone(),
            expected_history_len: index,
            stack_entry: entry,
        },
    );
    let mut finalizers = journal.iter().enumerate().filter_map(|(position, row)| {
        let finalize = cycling_finalize(row.command.as_ref()?)?;
        (finalize.object == entry).then_some((position, finalize))
    });
    let (finalize_position, finalize) =
        finalizers.next().expect("cycling finalizer for this entry");
    assert!(finalizers.next().is_none());
    assert!(edit_position < finalize_position);
    let push = journal.iter().position(|row| matches!(
        &row.command, Some(ResolvedRulesCommand::StackPush(command)) if command.entry.id == entry
    )).expect("the exact entry was pushed");
    assert!(push < edit_position);
    let occurrence = CyclingOccurrence {
        activator: P0,
        journal_index: index,
    };
    assert_eq!(finalize.resulting_cycling_occurrence, Some(occurrence));
    let live = &state.stack[finalize.entry_position];
    assert_eq!(live.id, entry);
    assert_eq!(&live.kind, finalize.resulting_kind.as_ref());
    assert_eq!(live.ability().unwrap().cycling_occurrence, Some(occurrence));
    (edit_position, finalize_position, finalize)
}

#[test]
fn phase2_v3_cycle_survives_resolution_turn_cleanup_and_departure() {
    let mut scenario = main_phase();
    let card = street_wraith(&mut scenario);
    let draw = scenario.add_card_to_library_top(P0, "Island");
    printed(&mut scenario, P1, "Island", Zone::Library);
    let mut runner = scenario.build();
    let turn = runner.state().turn_number;
    let expected = [cycle_record("Street Wraith", turn)];
    cycle_wraith(&mut runner, card);
    assert_history(runner.state(), &expected);
    resolve_cycle_draw(&mut runner, draw);
    assert_history(runner.state(), &expected);
    advance_turn(&mut runner);
    assert!(runner.state().turn_number > turn);
    assert!(runner
        .state()
        .abilities_activated_this_turn_by_player
        .is_empty());
    assert_history(runner.state(), &expected);
    act(&mut runner, GameAction::Concede { player_id: P0 });
    assert!(runner.state().players[0].is_eliminated);
    assert_history(runner.state(), &expected);
}

// CR 701.6a: Countering the exact cycling entry prevents its draw.
#[test]
fn phase2_v3_stifle_counters_cycling_without_erasing_history() {
    let mut scenario = main_phase();
    let card = street_wraith(&mut scenario);
    let stifle = printed(&mut scenario, P0, "Stifle", Zone::Hand);
    scenario.with_mana_pool(P0, mana(ManaType::Blue, 1));
    let draw = scenario.add_card_to_library_top(P0, "Island");
    let mut runner = scenario.build();
    let cycling = cycle_wraith(&mut runner, card);
    let expected = [cycle_record("Street Wraith", runner.state().turn_number)];
    assert_history(runner.state(), &expected);
    let cast = runner.cast(stifle).target_object(cycling).commit();
    let stack = &cast.state().stack;
    assert_eq!(stack.len(), 2);
    assert_eq!(stack[0].id, cycling);
    assert_eq!(stack[1].source_id, stifle);
    assert_eq!(
        stack[1].ability().unwrap().targets,
        [TargetRef::Object(cycling)]
    );
    cast.resolve();
    priority(&runner);
    assert!(runner.state().stack.is_empty());
    assert_eq!(zone(&runner, stifle), Zone::Graveyard);
    assert_eq!(zone(&runner, draw), Zone::Library);
    assert_eq!(runner.state().players[0].life, 18);
    assert_history(runner.state(), &expected);
}

// CR 701.9a and CR 702.29c distinguish Looting's discard from the cycling cost.
#[test]
fn phase2_v4_looting_retains_the_prior_cycle_only() {
    let mut scenario = main_phase();
    let prior = street_wraith(&mut scenario);
    let discard = street_wraith(&mut scenario);
    let other = printed(&mut scenario, P0, "Sidewinder Sliver", Zone::Hand);
    let looting = printed(&mut scenario, P0, "Faithless Looting", Zone::Hand);
    scenario.with_mana_pool(P0, mana(ManaType::Red, 1));
    let draws = ["Island", "Mountain"].map(|name| scenario.add_card_to_library_top(P0, name));
    let cycle_draw = scenario.add_card_to_library_top(P0, "Forest");
    let mut runner = scenario.build();
    cycle_wraith(&mut runner, prior);
    resolve_cycle_draw(&mut runner, cycle_draw);
    let expected = [cycle_record("Street Wraith", runner.state().turn_number)];
    assert_history(runner.state(), &expected);
    assert_eq!(cycle_command_counts(runner.state()), (1, 1));
    runner.cast(looting).commit();
    let mut events = passes(&mut runner);
    assert!(draws.iter().all(|id| zone(&runner, *id) == Zone::Hand));
    let WaitingFor::DiscardChoice {
        player,
        source_id,
        count,
        cards,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("expected Looting to ask for two cards")
    };
    assert_eq!((*player, *source_id, *count), (P0, looting, 2));
    assert!(cards.contains(&discard) && cards.contains(&other));
    events.extend(act(
        &mut runner,
        GameAction::SelectCards {
            cards: vec![discard, other],
        },
    ));
    assert_eq!(
        events
            .iter()
            .filter_map(discard_subject)
            .collect::<Vec<_>>(),
        [(P0, discard), (P0, other)]
    );
    assert!(cycle_events(&events).is_empty());
    assert_eq!(zone(&runner, discard), Zone::Graveyard);
    assert_eq!(zone(&runner, other), Zone::Graveyard);
    assert_eq!(zone(&runner, looting), Zone::Graveyard);
    priority(&runner);
    assert!(runner.state().stack.is_empty());
    assert_eq!(cycle_command_counts(runner.state()), (1, 1));
    assert_history(runner.state(), &expected);
}

#[test]
fn phase2_v6_action_replay_seeks_across_cycle_placements() {
    use engine::database::{CardDatabase, ComboTable};
    use engine::game::deck_loading::{DeckList, PlayerDeckList};
    use engine::game::engine::apply;
    use engine::game::replay::{reconstruct_initial_state, ReplayPlayer};
    use engine::types::actions::MulliganChoice;
    use engine::types::format::FormatConfig;
    use engine::types::replay::{ReplayHeader, ReplayLog};
    use std::sync::Arc;

    let db =
        Arc::new(CardDatabase::from_integration_fixture_cached().expect("shared printed fixture"));
    // A seven-card opening hand from this nine-card deck always holds two Wraiths.
    let mut main_deck = vec!["Street Wraith".to_owned(); 4];
    main_deck.extend(vec!["Island".to_owned(); 5]);
    let header = ReplayHeader {
        format_config: FormatConfig::freeform(),
        match_config: Default::default(),
        player_count: 2,
        first_player: Some(0),
        seed: 257,
        deck_data: Some(DeckList {
            player: PlayerDeckList {
                main_deck,
                ..Default::default()
            },
            opponent: PlayerDeckList {
                main_deck: vec!["Island".to_owned(); 10],
                ..Default::default()
            },
            ..Default::default()
        }),
    };
    let combos = ComboTable::default();
    let mut live = reconstruct_initial_state(&header, Some(&db), &combos).unwrap();
    let mut log = ReplayLog::new(header);
    let submit = |live: &mut GameState, log: &mut ReplayLog, actor, action: GameAction| {
        let result = apply(live, actor, action.clone()).expect("legal recorded action");
        assert_eq!(
            result.disposition,
            engine::types::game_state::ActionDisposition::Applied
        );
        log.push_action(actor, action);
        result.events
    };
    for actor in [P0, P1] {
        assert!(
            matches!(&live.waiting_for, WaitingFor::MulliganDecision { pending, .. }
            if pending.iter().any(|entry| entry.player == actor))
        );
        submit(
            &mut live,
            &mut log,
            actor,
            GameAction::MulliganDecision {
                choice: MulliganChoice::Keep,
            },
        );
    }
    assert!(matches!(
        live.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    let cards: Vec<_> = live.players[0]
        .hand
        .iter()
        .copied()
        .filter(|id| live.objects[id].name == "Street Wraith")
        .take(2)
        .collect();
    assert_eq!(cards.len(), 2);
    let before = log.actions.len() as u32;
    let first_turn = live.turn_number;
    let mut placements = Vec::new();
    let mut expected = Vec::new();
    for (index, card) in cards.into_iter().enumerate() {
        let ability_index = live.objects[&card]
            .abilities
            .iter()
            .position(|ability| ability.ability_tag == Some(AbilityTag::Cycling))
            .unwrap();
        let events = submit(
            &mut live,
            &mut log,
            P0,
            activation(card, ability_index, CastPaymentMode::Auto),
        );
        assert_cycle(&events, card);
        let entry = live.stack.back().expect("cycling reached the stack").id;
        expected.push(cycle_record("Street Wraith", live.turn_number));
        assert_history(&live, &expected);
        assert_placement(&live, entry, &expected[index], index);
        assert_eq!(live.objects[&card].zone, Zone::Graveyard);
        assert_eq!(live.players[0].life, 18 - 2 * index as i32);
        placements.push((log.actions.len() as u32, entry, card, live.players[0].life));
        if index == 0 {
            let draw = live.players[0].library[0];
            for actor in [P0, P1] {
                submit(&mut live, &mut log, actor, GameAction::PassPriority);
            }
            assert!(live.stack.is_empty());
            assert_eq!(live.objects[&draw].zone, Zone::Hand);
            for _ in 0..48 {
                if live.turn_number > first_turn
                    && matches!(live.waiting_for, WaitingFor::Priority { player: P0 })
                {
                    break;
                }
                let (actor, action) = next_turn_action(&live);
                submit(&mut live, &mut log, actor, action);
            }
            assert!(live.turn_number > first_turn);
            assert!(matches!(
                live.waiting_for,
                WaitingFor::Priority { player: P0 }
            ));
        }
    }
    assert_ne!(placements[0].1, placements[1].1);
    let log = serde_json::from_slice(&serde_json::to_vec(&log).unwrap()).unwrap();
    let mut replay = ReplayPlayer::load(log, Some(&db), &combos).unwrap();
    for _ in 0..2 {
        assert_history(replay.seek(before).unwrap(), &[]);
        for (index, &(action_count, entry, card, life)) in placements.iter().enumerate() {
            let state = replay.seek(action_count).unwrap();
            assert_history(state, &expected[..=index]);
            assert_placement(state, entry, &expected[index], index);
            assert_eq!(state.players[0].life, life);
            assert_eq!(state.objects[&card].zone, Zone::Graveyard);
        }
    }
}

fn apply_cycle_journal_command(state: &mut GameState, command: &ResolvedRulesCommand) {
    use engine::game::{ledger, stack, triggers, zones};
    match command {
        ResolvedRulesCommand::PlayerEdit(command) => {
            state.apply_resolved_player_edit(command).unwrap();
        }
        ResolvedRulesCommand::ZoneChange(command) => {
            zones::apply_resolved_zone_change(state, command).unwrap();
        }
        ResolvedRulesCommand::LedgerEdit(command) => {
            ledger::apply_resolved_ledger_edit(state, command).unwrap();
        }
        ResolvedRulesCommand::Information(command) => {
            state.apply_resolved_information(command).unwrap();
        }
        ResolvedRulesCommand::TriggerCollection(command) => {
            triggers::apply_resolved_trigger_collection(state, command).unwrap();
        }
        ResolvedRulesCommand::StackPush(command) => {
            stack::apply_resolved_stack_push(state, command).unwrap();
        }
        ResolvedRulesCommand::StackRemoval(command) => {
            stack::apply_resolved_stack_removal(state, command).unwrap();
        }
        ResolvedRulesCommand::StackEntryFinalize(command) => {
            stack::apply_resolved_stack_entry_finalize(state, command).unwrap();
        }
        ResolvedRulesCommand::FrameTransition(command) => {
            state.apply_resolved_frame_transition(command).unwrap();
        }
        other => panic!("unexpected command in Street Wraith journal: {other:?}"),
    }
}

fn activated_graph(kind: &mut StackEntryKind) -> &mut ResolvedAbility {
    let StackEntryKind::ActivatedAbility { ability, .. } = kind else {
        panic!("expected an activated graph");
    };
    ability
}

fn assert_no_cycle_replay_mutation(before: &GameState, after: &GameState) {
    assert_eq!(
        after.cycles_completed_this_game_by_player,
        before.cycles_completed_this_game_by_player
    );
    assert_eq!(after.stack, before.stack);
    assert_eq!(after.stack_paid_facts, before.stack_paid_facts);
}

#[test]
fn phase2_v7_v8_resolved_journal_replays_both_cycles_and_rejects_corruption() {
    let mut scenario = main_phase();
    let sources = [street_wraith(&mut scenario), street_wraith(&mut scenario)];
    let draw = scenario.add_card_to_library_top(P0, "Island");
    let mut runner = scenario.build();
    let start = runner.state().clone();
    assert_history(&start, &[]);
    let from = start.resolved_rules_journal.entries().len();
    let turn = start.turn_number;
    let record = cycle_record("Street Wraith", turn);
    let first = cycle_wraith(&mut runner, sources[0]);
    let (first_edit, first_finalize, first_command) =
        assert_placement(runner.state(), first, &record, 0);
    let first_kind = first_command.resulting_kind.clone();
    resolve_cycle_draw(&mut runner, draw);
    let second = cycle_wraith(&mut runner, sources[1]);
    assert_ne!(first, second);
    assert_eq!(runner.state().turn_number, turn);
    let (second_edit, second_finalize, second_command) =
        assert_placement(runner.state(), second, &record, 1);
    assert!(first_edit < first_finalize && first_finalize < second_edit);
    let expected = [record.clone(), record];
    assert_history(runner.state(), &expected);
    assert_eq!(cycle_command_counts(runner.state()), (2, 2));
    let wire = serde_json::to_value(&runner.state().resolved_rules_journal).unwrap();
    let journal: ResolvedRulesJournal = serde_json::from_value(wire.clone()).unwrap();
    let mut replay = start.clone();
    for (position, row) in journal.entries().iter().enumerate().skip(from) {
        let Some(command) = &row.command else {
            continue;
        };
        apply_cycle_journal_command(&mut replay, command);
        match position {
            p if p == first_finalize => {
                assert_history(&replay, &expected[..1]);
                assert_eq!(replay.stack[0].id, first);
                assert_eq!(&replay.stack[0].kind, first_kind.as_ref());
            }
            p if p == second_finalize => {
                assert_history(&replay, &expected);
                assert_eq!(replay.stack[0].id, second);
                assert_eq!(
                    &replay.stack[0].kind,
                    second_command.resulting_kind.as_ref()
                );
            }
            _ => {}
        }
    }
    assert_eq!(replay.players[0].life, 16);
    assert_eq!(replay.objects[&draw].zone, Zone::Hand);
    assert_history(&replay, &expected);

    assert_cycle_apply_rejections(&start, &journal, from, second_edit, second_finalize);
    assert_cycle_decode_rejections(&wire, &journal, first_edit, second_edit, second_finalize);

    let mut snapshot = serde_json::to_value(runner.state()).unwrap();
    let current: GameState = serde_json::from_value(snapshot.clone()).unwrap();
    assert_history(&current, &expected);
    assert_eq!(current.stack, runner.state().stack);
    snapshot
        .as_object_mut()
        .unwrap()
        .remove("cycles_completed_this_game_by_player")
        .expect("the current snapshot contains the completed history");
    let legacy: GameState = serde_json::from_value(snapshot).unwrap();
    assert_history(&legacy, &[]);
}

fn assert_cycle_apply_rejections(
    start: &GameState,
    journal: &ResolvedRulesJournal,
    from: usize,
    second_edit: usize,
    second_finalize: usize,
) {
    use engine::game::{
        ledger::apply_resolved_ledger_edit, stack::apply_resolved_stack_entry_finalize,
    };
    use engine::types::resolved_commands::{
        ResolvedLedgerEditReplayInvariantError as LedgerError,
        ResolvedStackEntryFinalizeReplayInvariantError as FinalizeError,
    };

    let entries = journal.entries();
    let Some(ResolvedRulesCommand::LedgerEdit(edit)) = &entries[second_edit].command else {
        panic!("second cycle ledger command");
    };
    let Some(ResolvedRulesCommand::StackEntryFinalize(finalize)) =
        &entries[second_finalize].command
    else {
        panic!("second cycle finalizer");
    };
    let mut prefix = start.clone();
    for row in entries.iter().skip(from).take(second_edit - from) {
        if let Some(command) = &row.command {
            apply_cycle_journal_command(&mut prefix, command);
        }
    }
    assert_eq!(prefix.stack[finalize.entry_position].id, finalize.object);
    assert_eq!(
        &prefix.stack[finalize.entry_position].kind,
        finalize.expected_old_kind.as_ref()
    );
    assert_eq!(prefix.cycles_completed_this_game_by_player[&P0].len(), 1);

    let mut paired = serde_json::to_value(journal).unwrap();
    let cycle =
        &mut paired["entries"][second_edit]["command"]["LedgerEdit"]["edit"]["CompletedCycle"];
    cycle["player"] = serde_json::to_value(P1).unwrap();
    cycle["expected_history_len"] = 0.into();

    let occurrence = Some(CyclingOccurrence {
        activator: P1,
        journal_index: 0,
    });
    let mut paired_finalize = finalize.as_ref().clone();
    paired_finalize.resulting_cycling_occurrence = occurrence;
    let root = activated_graph(&mut paired_finalize.resulting_kind);
    assert!(root.sub_ability.is_none());
    root.cycling_occurrence = occurrence;
    paired["entries"][second_finalize]["command"]["StackEntryFinalize"] =
        serde_json::to_value(paired_finalize).unwrap();

    let paired: ResolvedRulesJournal =
        serde_json::from_value(paired).expect("paired edit and finalizer pass decode validation");
    let Some(ResolvedRulesCommand::LedgerEdit(paired_edit)) =
        &paired.entries()[second_edit].command
    else {
        panic!("second cycle ledger command");
    };
    assert_eq!(prefix.stack[finalize.entry_position].controller, P0);
    let mut state = prefix.clone();
    assert_eq!(
        apply_resolved_ledger_edit(&mut state, paired_edit),
        Err(LedgerError::CompletedCyclePreconditionMismatch),
        "paired corruption credits a player who did not activate the ability"
    );
    assert_no_cycle_replay_mutation(&prefix, &state);

    let mut non_cycling = prefix.clone();
    let entry = activated_graph(&mut non_cycling.stack[finalize.entry_position].kind);
    assert_eq!(entry.context.ability_tag, Some(AbilityTag::Cycling));
    entry.context.ability_tag = None;
    let before = non_cycling.clone();
    assert_eq!(
        apply_resolved_ledger_edit(&mut non_cycling, edit),
        Err(LedgerError::CompletedCyclePreconditionMismatch),
        "a completed cycle names an activation that is not cycling"
    );
    assert_no_cycle_replay_mutation(&before, &non_cycling);

    for (label, expected) in [
        (
            "wrong prefix",
            LedgerError::CompletedCyclePreconditionMismatch,
        ),
        ("unknown seat", LedgerError::UnknownPlayer(PlayerId(99))),
        (
            "empty name",
            LedgerError::CompletedCyclePreconditionMismatch,
        ),
        ("duplicate", LedgerError::CompletedCyclePreconditionMismatch),
    ] {
        let mut state = prefix.clone();
        let mut forged = edit.clone();
        let ResolvedLedgerEdit::CompletedCycle {
            player,
            expected_history_len,
            record,
            ..
        } = &mut forged.edit
        else {
            unreachable!()
        };
        match label {
            "wrong prefix" => *expected_history_len += 1,
            "unknown seat" => *player = PlayerId(99),
            "empty name" => record.name.clear(),
            "duplicate" => apply_resolved_ledger_edit(&mut state, edit).unwrap(),
            _ => unreachable!(),
        }
        let before = state.clone();
        assert_eq!(
            apply_resolved_ledger_edit(&mut state, &forged),
            Err(expected),
            "{label}"
        );
        assert_no_cycle_replay_mutation(&before, &state);
    }

    for label in [
        "divergent predecessor",
        "graph mismatch",
        "missing coordinate",
        "legacy coordinate with a stamped graph",
    ] {
        let mut state = prefix.clone();
        for (position, row) in entries
            .iter()
            .enumerate()
            .skip(second_edit)
            .take(second_finalize - second_edit)
        {
            if position == second_edit
                && matches!(
                    label,
                    "missing coordinate" | "legacy coordinate with a stamped graph"
                )
            {
                continue;
            }
            if let Some(command) = &row.command {
                apply_cycle_journal_command(&mut state, command);
            }
        }
        let mut forged = finalize.as_ref().clone();
        let expected = match label {
            "divergent predecessor" => {
                let kind = &mut state.stack.get_mut(finalize.entry_position).unwrap().kind;
                *kind = finalize.resulting_kind.as_ref().clone();
                FinalizeError::EntryKindMismatch(finalize.entry_position)
            }
            "graph mismatch" => {
                activated_graph(&mut forged.resulting_kind).cycling_occurrence = None;
                FinalizeError::CyclingOccurrenceMismatch(finalize.object)
            }
            "missing coordinate" => FinalizeError::CyclingOccurrenceMismatch(finalize.object),
            "legacy coordinate with a stamped graph" => {
                forged.resulting_cycling_occurrence = None;
                FinalizeError::CyclingOccurrenceMismatch(finalize.object)
            }
            _ => unreachable!(),
        };
        let before = state.clone();
        assert_eq!(
            apply_resolved_stack_entry_finalize(&mut state, &forged),
            Err(expected),
            "{label}"
        );
        assert_no_cycle_replay_mutation(&before, &state);
        if label == "legacy coordinate with a stamped graph" {
            let root = activated_graph(&mut forged.resulting_kind);
            assert!(root.sub_ability.is_none());
            assert!(root.cycling_occurrence.take().is_some());
            apply_resolved_stack_entry_finalize(&mut state, &forged).unwrap();
            assert_eq!(
                state.stack[forged.entry_position].kind,
                *forged.resulting_kind
            );
            assert_eq!(
                state.cycles_completed_this_game_by_player,
                before.cycles_completed_this_game_by_player
            );
        }
    }
}

fn assert_journal_decode_error(wire: serde_json::Value, expected: &str, specimen: &str) {
    let error = serde_json::from_value::<ResolvedRulesJournal>(wire)
        .expect_err(specimen)
        .to_string();
    assert!(error.contains(expected), "{specimen}: {error}");
}

fn assert_cycle_decode_rejections(
    wire: &serde_json::Value,
    journal: &ResolvedRulesJournal,
    first_edit: usize,
    second_edit: usize,
    second_finalize: usize,
) {
    const PRIOR_EDIT: &str =
        "cycling provenance requires exactly one matching prior completed-cycle ledger edit";
    let finalizer =
        cycling_finalize(journal.entries()[second_finalize].command.as_ref().unwrap()).unwrap();
    let cycle_edit = |wire: &serde_json::Value, i: usize| {
        wire["entries"][i]["command"]["LedgerEdit"]["edit"]["CompletedCycle"].clone()
    };
    let first = cycle_edit(wire, first_edit);
    let second = cycle_edit(wire, second_edit);
    assert_ne!(first["stack_entry"], second["stack_entry"]);
    for (field, value) in [
        ("player", serde_json::to_value(P1).unwrap()),
        (
            "expected_history_len",
            first["expected_history_len"].clone(),
        ),
        ("stack_entry", first["stack_entry"].clone()),
    ] {
        let mut forged = wire.clone();
        forged["entries"][second_edit]["command"]["LedgerEdit"]["edit"]["CompletedCycle"][field] =
            value;
        assert_journal_decode_error(forged, PRIOR_EDIT, field);
    }

    let mut missing = wire.clone();
    missing["entries"][second_edit]
        .as_object_mut()
        .unwrap()
        .remove("command");
    assert_journal_decode_error(missing.clone(), PRIOR_EDIT, "missing edit");

    let mut stamped_legacy = missing.clone();
    stamped_legacy["entries"][second_finalize]["command"]["StackEntryFinalize"]
        ["resulting_cycling_occurrence"] = serde_json::Value::Null;
    assert_journal_decode_error(
        stamped_legacy.clone(),
        "cycling provenance disagrees",
        "legacy coordinate with a stamped graph",
    );
    let mut legacy = finalizer.clone();
    legacy.resulting_cycling_occurrence = None;
    let root = activated_graph(&mut legacy.resulting_kind);
    assert!(root.sub_ability.is_none());
    assert!(root.cycling_occurrence.take().is_some());
    stamped_legacy["entries"][second_finalize]["command"]["StackEntryFinalize"] =
        serde_json::to_value(legacy).unwrap();
    serde_json::from_value::<ResolvedRulesJournal>(stamped_legacy)
        .expect("an unstamped legacy finalizer decodes without a completed-cycle edit");

    let mut late = missing;
    let ordinal = late["next_command_ordinal"].as_u64().unwrap();
    let mut entry = wire["entries"][second_edit].clone();
    entry["ordinal"] = ordinal.into();
    let node = &entry["node"];
    let owner = late["nodes"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|candidate| candidate["identity"] == *node)
        .expect("the copied edit's cause owns the new ordinal");
    owner["journal_ordinals"]
        .as_array_mut()
        .unwrap()
        .push(ordinal.into());
    late["entries"].as_array_mut().unwrap().push(entry);
    late["next_command_ordinal"] = (ordinal + 1).into();
    assert_journal_decode_error(late, PRIOR_EDIT, "later edit");

    let mut crossed = wire.clone();
    crossed["entries"][second_edit]["command"]["LedgerEdit"]["edit"]["CompletedCycle"]["player"] =
        serde_json::to_value(P1).unwrap();
    // Keep both pushes and the first pair intact. Reuse the second activation's
    // usage-ledger slot, preserving its ordinal and causal node.
    let usage_edit = journal
        .entries()
        .iter()
        .enumerate()
        .take(second_edit)
        .rev()
        .find_map(|(position, row)| match &row.command {
            Some(ResolvedRulesCommand::LedgerEdit(command))
                if matches!(command.edit, ResolvedLedgerEdit::AbilityActivated { .. }) =>
            {
                Some(position)
            }
            _ => None,
        })
        .expect("the second activation recorded its usage before completing");
    assert!(first_edit < usage_edit && usage_edit < second_finalize);
    let mut candidate = wire["entries"][first_edit]["command"].clone();
    candidate["LedgerEdit"]["edit"]["CompletedCycle"]["expected_history_len"] =
        second["expected_history_len"].clone();
    candidate["LedgerEdit"]["cause"] = wire["entries"][usage_edit]["node"].clone();
    crossed["entries"][usage_edit]["command"] = candidate;
    assert_journal_decode_error(crossed, PRIOR_EDIT, "crossed candidates");

    let mut forged = finalizer.clone();
    let reused = CyclingOccurrence {
        activator: P0,
        journal_index: 0,
    };
    forged.resulting_cycling_occurrence = Some(reused);
    let root = activated_graph(&mut forged.resulting_kind);
    assert!(
        root.sub_ability.is_none(),
        "Street Wraith has a single draw node"
    );
    root.cycling_occurrence = Some(reused);
    let mut reused_first = wire.clone();
    reused_first["entries"][second_finalize]["command"]["StackEntryFinalize"] =
        serde_json::to_value(forged).unwrap();
    assert_journal_decode_error(
        reused_first,
        PRIOR_EDIT,
        "first occurrence on the second entry",
    );

    let mut duplicate = wire.clone();
    let mut copy = wire["entries"][second_edit]["command"].clone();
    copy["LedgerEdit"]["cause"] = wire["entries"][usage_edit]["node"].clone();
    duplicate["entries"][usage_edit]["command"] = copy;
    assert_journal_decode_error(duplicate, PRIOR_EDIT, "two matching earlier edits");
}

fn assert_granted_replay_contract(state: &GameState, finalize: &ResolvedStackEntryFinalizeCommand) {
    use engine::game::stack::apply_resolved_stack_entry_finalize;
    use engine::types::game_state::StackPaidSnapshot;
    use engine::types::resolved_commands::ResolvedStackEntryFinalizeReplayInvariantError as Error;

    let wire = serde_json::to_value(&state.resolved_rules_journal).unwrap();
    let decoded: ResolvedRulesJournal = serde_json::from_value(wire.clone()).unwrap();
    let position = decoded
        .entries()
        .iter()
        .position(|row| row.command.as_ref().and_then(cycling_finalize) == Some(finalize))
        .expect("granted graph survives journal decode");
    let StackEntryKind::ActivatedAbility { ability: root, .. } = finalize.resulting_kind.as_ref()
    else {
        panic!("granted cycling finalizes an activated ability");
    };
    let movement = root.sub_ability.as_deref().expect("slivercycling move");
    let shuffle = movement
        .sub_ability
        .as_deref()
        .expect("slivercycling shuffle");
    assert!(matches!(root.effect, Effect::SearchLibrary { .. }));
    assert!(matches!(movement.effect, Effect::ChangeZone { .. }));
    assert!(matches!(shuffle.effect, Effect::Shuffle { .. }));
    assert!(shuffle.sub_ability.is_none());
    let occurrence = CyclingOccurrence {
        activator: P0,
        journal_index: 0,
    };
    for node in [root.as_ref(), movement, shuffle] {
        assert_eq!(node.cycling_occurrence, Some(occurrence));
    }

    for (label, stamp) in [
        ("missing root", None),
        (
            "wrong root",
            Some(CyclingOccurrence {
                journal_index: 1,
                ..occurrence
            }),
        ),
        ("missing descendant", None),
        (
            "wrong descendant",
            Some(CyclingOccurrence {
                journal_index: 1,
                ..occurrence
            }),
        ),
    ] {
        let mut bad = finalize.clone();
        let graph = activated_graph(&mut bad.resulting_kind);
        let node = if label.ends_with("root") {
            graph
        } else {
            graph.sub_ability.as_deref_mut().unwrap()
        };
        node.cycling_occurrence = stamp;
        let mut forged = wire.clone();
        forged["entries"][position]["command"]["StackEntryFinalize"] =
            serde_json::to_value(bad).unwrap();
        assert_journal_decode_error(forged, "cycling provenance disagrees", label);
    }

    let mut bad_kind = wire.clone();
    bad_kind["entries"][position]["command"]["StackEntryFinalize"]["resulting_kind"] = serde_json::json!({"type": "Spell", "data": {"card_id": state.objects[&state.stack[0].source_id].card_id}});
    assert_journal_decode_error(
        bad_kind,
        "different pre-finalize entry",
        "wrong cycling result kind",
    );
    let mut missing_kind = wire.clone();
    missing_kind["entries"][position]["command"]["StackEntryFinalize"]
        .as_object_mut()
        .unwrap()
        .remove("resulting_kind");
    assert_journal_decode_error(
        missing_kind,
        "missing field `resulting_kind`",
        "missing cycling result kind",
    );

    let mut predecessor = state.clone();
    predecessor
        .stack
        .get_mut(finalize.entry_position)
        .unwrap()
        .kind = finalize.expected_old_kind.as_ref().clone();
    assert_eq!(finalize.expected_old_paid_facts, None);
    assert!(!predecessor.stack_paid_facts.contains_key(&finalize.object));
    let mut positive = predecessor.clone();
    apply_resolved_stack_entry_finalize(&mut positive, finalize).unwrap();
    assert_eq!(positive.stack, state.stack);

    let mut conflicting = predecessor.clone();
    conflicting
        .stack_paid_facts
        .insert(finalize.object, StackPaidSnapshot::default());
    let before = conflicting.clone();
    assert_eq!(
        apply_resolved_stack_entry_finalize(&mut conflicting, finalize),
        Err(Error::PaidFactsMismatch(finalize.object))
    );
    assert_no_cycle_replay_mutation(&before, &conflicting);
    assert_eq!(
        serde_json::to_value(&conflicting.objects).unwrap(),
        serde_json::to_value(&before.objects).unwrap()
    );

    let mut legacy = finalize.clone();
    legacy.resulting_cycling_occurrence = None;
    legacy.resulting_kind = legacy.expected_old_kind.clone();
    let edit_position = decoded
        .entries()
        .iter()
        .position(|row| row.command.as_ref().and_then(completed_cycle).is_some())
        .unwrap();
    let mut without_edit = wire;
    without_edit["entries"][edit_position]
        .as_object_mut()
        .unwrap()
        .remove("command");
    let mut descendant_stamped = finalize.clone();
    descendant_stamped.resulting_cycling_occurrence = None;
    activated_graph(&mut descendant_stamped.resulting_kind).cycling_occurrence = None;
    let mut forged = without_edit.clone();
    forged["entries"][position]["command"]["StackEntryFinalize"] =
        serde_json::to_value(&descendant_stamped).unwrap();
    assert_journal_decode_error(
        forged,
        "cycling provenance disagrees",
        "legacy descendant stamp",
    );
    let mut restored = predecessor.clone();
    restored.cycles_completed_this_game_by_player.clear();
    let before = restored.clone();
    assert_eq!(
        apply_resolved_stack_entry_finalize(&mut restored, &descendant_stamped),
        Err(Error::CyclingOccurrenceMismatch(finalize.object))
    );
    assert_no_cycle_replay_mutation(&before, &restored);

    for omit_occurrence in [false, true] {
        let mut old = without_edit.clone();
        let mut result = serde_json::to_value(&legacy).unwrap();
        if omit_occurrence {
            result
                .as_object_mut()
                .unwrap()
                .remove("resulting_cycling_occurrence");
        } else {
            result["resulting_cycling_occurrence"] = serde_json::Value::Null;
        }
        old["entries"][position]["command"]["StackEntryFinalize"] = result;
        let old: ResolvedRulesJournal = serde_json::from_value(old).unwrap();
        assert!(old.entries().iter().all(|row| row
            .command
            .as_ref()
            .and_then(completed_cycle)
            .is_none()));
        let Some(ResolvedRulesCommand::StackEntryFinalize(decoded)) =
            &old.entries()[position].command
        else {
            panic!("legacy command is still a finalizer");
        };
        let mut restored = predecessor.clone();
        restored.cycles_completed_this_game_by_player.clear();
        apply_resolved_stack_entry_finalize(&mut restored, decoded).unwrap();
        assert_eq!(
            restored.stack[decoded.entry_position].kind,
            *legacy.resulting_kind
        );
        assert!(restored.stack_paid_facts.is_empty());
        let root = restored.stack[decoded.entry_position].ability().unwrap();
        let movement = root.sub_ability.as_deref().unwrap();
        let shuffle = movement.sub_ability.as_deref().unwrap();
        assert!([root, movement, shuffle]
            .iter()
            .all(|node| node.cycling_occurrence.is_none()));
        assert_history(&restored, &[]);
    }
}

fn read_cycles(
    state: &GameState,
    ability: &ResolvedAbility,
    scope: CountScope,
    window: CyclingHistoryWindow,
    name: Option<&str>,
) -> i32 {
    engine::game::quantity::resolve_quantity_with_targets(
        state,
        &QuantityExpr::Ref {
            qty: QuantityRef::CardsCycled {
                scope,
                window,
                name: name.map(str::to_owned),
            },
        },
        ability,
    )
}

fn assert_cycle_windows(
    state: &GameState,
    ability: &ResolvedAbility,
    scope: CountScope,
    expected: [i32; 2],
) {
    assert_eq!(
        [
            CyclingHistoryWindow::ThisTurn,
            CyclingHistoryWindow::ThisGame
        ]
        .map(|window| read_cycles(state, ability, scope.clone(), window, None)),
        expected,
        "{scope:?}",
    );
}

fn return_with_raise_dead(runner: &mut GameRunner, spell: ObjectId, target: ObjectId) {
    let card_id = runner.state().objects[&spell].card_id;
    act(
        runner,
        GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        },
    );
    let WaitingFor::TargetSelection {
        player: P0,
        selection,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("expected Raise Dead's graveyard target");
    };
    assert!(selection
        .current_legal_targets
        .contains(&TargetRef::Object(target)));
    act(
        runner,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target)),
        },
    );
    passes(runner);
    priority(runner);
    assert!(runner.state().stack.is_empty());
    assert_eq!(zone(runner, target), Zone::Hand);
}

#[test]
fn phase3_v1_counts_repeated_cycles_across_turn_windows() {
    let mut scenario = main_phase();
    let wraith = street_wraith(&mut scenario);
    let later = street_wraith(&mut scenario);
    let raise_dead = printed(&mut scenario, P0, "Raise Dead", Zone::Hand);
    printed(&mut scenario, P0, "Sidewinder Sliver", Zone::Graveyard);
    let draws =
        ["Forest", "Mountain", "Island"].map(|name| scenario.add_card_to_library_top(P0, name));
    scenario.add_card_to_library_top(P1, "Island");
    scenario.with_mana_pool(P0, mana(ManaType::Black, 1));
    let mut runner = scenario.build();
    let ability = ResolvedAbility::new(Effect::NoOp, vec![], wraith, P0);
    let turn = runner.state().turn_number;
    assert_cycle_windows(runner.state(), &ability, CountScope::Controller, [0, 0]);
    let first = cycle_wraith(&mut runner, wraith);
    let record = cycle_record("Street Wraith", turn);
    assert_placement(runner.state(), first, &record, 0);
    assert_cycle_windows(runner.state(), &ability, CountScope::Controller, [1, 1]);
    resolve_cycle_draw(&mut runner, draws[2]);
    let discarded = runner.state().objects[&wraith].snapshot_for_zone_change(
        wraith,
        Some(Zone::Graveyard),
        Zone::Hand,
    );
    return_with_raise_dead(&mut runner, raise_dead, wraith);
    let returned = runner.state().objects[&wraith].snapshot_for_zone_change(
        wraith,
        Some(Zone::Hand),
        Zone::Graveyard,
    );
    assert_ne!(
        discarded
            .trigger_source_context()
            .unwrap()
            .identity
            .reference,
        returned
            .trigger_source_context()
            .unwrap()
            .identity
            .reference
    );

    // The same physical card has a new incarnation. Check the new activation
    // directly because the per-turn activation journal also holds its prior cycle.
    let index = wraith_index(&runner, wraith);
    let life = runner.life(P0);
    let events = act(
        &mut runner,
        activation(wraith, index, CastPaymentMode::Auto),
    );
    assert_eq!(runner.life(P0), life - 2);
    assert_cycle(&events, wraith);
    let second = activation_entry(&runner, wraith);
    assert_placement(runner.state(), second, &record, 1);
    assert_cycle_windows(runner.state(), &ability, CountScope::Controller, [2, 2]);
    resolve_cycle_draw(&mut runner, draws[1]);

    advance_turn(&mut runner);
    assert_eq!(runner.state().turn_number, turn + 1);
    assert_eq!(runner.state().active_player, P1);
    if matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ) {
        act(&mut runner, GameAction::PassPriority);
    }
    priority(&runner);
    assert!(runner.state().stack.is_empty());
    assert_cycle_windows(runner.state(), &ability, CountScope::Controller, [0, 2]);
    let third = cycle_wraith(&mut runner, later);
    assert_placement(
        runner.state(),
        third,
        &cycle_record("Street Wraith", turn + 1),
        2,
    );
    assert_cycle_windows(runner.state(), &ability, CountScope::Controller, [1, 3]);
    resolve_cycle_draw(&mut runner, draws[0]);
}

fn priority_for(runner: &mut GameRunner, seat: PlayerId, player_count: usize) {
    for _ in 0..player_count {
        let WaitingFor::Priority { player } = runner.state().waiting_for else {
            panic!("expected priority while rotating to {seat:?}");
        };
        if player == seat {
            return;
        }
        act(runner, GameAction::PassPriority);
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == seat)
    );
}

fn cycle_population(
    mut scenario: GameScenario,
    player_count: usize,
    cycle_counts: [usize; 3],
) -> (GameRunner, [ObjectId; 3], GameEvent) {
    scenario.at_phase(Phase::PreCombatMain);
    let seats = [P0, P1, PlayerId(2)];
    let sources =
        seats.map(|seat| printed(&mut scenario, seat, "Sidewinder Sliver", Zone::Battlefield));
    let cards = seats.map(|seat| {
        (0..cycle_counts[usize::from(seat.0)])
            .map(|_| printed(&mut scenario, seat, "Street Wraith", Zone::Hand))
            .collect::<Vec<_>>()
    });
    for n in 0..player_count {
        scenario.with_library_top(PlayerId(n as u8), &["Island"; 5]);
    }
    let mut runner = scenario.build();
    let mut last_cycle = None;
    for (seat, cards) in seats.into_iter().zip(cards) {
        for (journal_index, card) in cards.into_iter().enumerate() {
            priority_for(&mut runner, seat, player_count);
            let paid_life =
                |runner: &GameRunner| engine::game::players::team_life_total(runner.state(), seat);
            let life_before = paid_life(&runner);
            let index = wraith_index(&runner, card);
            let events = act(&mut runner, activation(card, index, CastPaymentMode::Auto));
            assert_eq!(paid_life(&runner), life_before - 2);
            assert_eq!(cycle_events(&events), [(seat, card)]);
            last_cycle = events
                .into_iter()
                .find(|event| matches!(event, GameEvent::Cycled { .. }));
            assert_eq!(zone(&runner, card), Zone::Graveyard);
            let entry = runner
                .state()
                .stack
                .front()
                .expect("paid cycling activation");
            assert_eq!(entry.controller, seat);
            assert!(
                matches!(entry.kind, StackEntryKind::ActivatedAbility { source_id, .. } if source_id == card)
            );
            assert_eq!(
                entry.ability().unwrap().cycling_occurrence,
                Some(CyclingOccurrence {
                    activator: seat,
                    journal_index
                }),
            );
            assert_eq!(
                runner.state().cycles_completed_this_game_by_player[&seat][journal_index],
                cycle_record("Street Wraith", runner.state().turn_number),
            );
            for _ in 0..player_count {
                if runner.state().stack.is_empty() {
                    break;
                }
                assert!(matches!(
                    runner.state().waiting_for,
                    WaitingFor::Priority { .. }
                ));
                act(&mut runner, GameAction::PassPriority);
            }
            assert!(runner.state().stack.is_empty());
            let WaitingFor::Priority { player } = runner.state().waiting_for else {
                panic!("cycling draw must return to priority");
            };
            assert_eq!(player, P0);
        }
    }
    (
        runner,
        sources,
        last_cycle.expect("last player's completed cycle"),
    )
}

fn cast_black_vise(runner: &mut GameRunner, vise: ObjectId, chosen: PlayerId) {
    priority(runner);
    let card_id = runner.state().objects[&vise].card_id;
    act(
        runner,
        GameAction::CastSpell {
            object_id: vise,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        },
    );
    assert_eq!(zone(runner, vise), Zone::Stack);
    for _ in 0..3 {
        act(runner, GameAction::PassPriority);
    }
    // CR 614.12a: the choice is made before the artifact enters.
    let WaitingFor::NamedChoice {
        player: P0,
        choice_type: ChoiceType::Opponent { .. },
        source: Some(source),
        options,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("expected Black Vise's as-enters opponent choice");
    };
    assert_eq!(source.binding, NamedChoiceSourceBinding::PendingEntry);
    assert_eq!(source.prompt.identity.reference.object_id, vise);
    assert!(!runner.state().battlefield.contains(&vise));
    let choice = chosen.0.to_string();
    assert!(options.contains(&choice));
    act(runner, GameAction::ChooseOption { choice });
    priority(runner);
    assert!(runner.state().stack.is_empty());
    assert_eq!(zone(runner, vise), Zone::Battlefield);
    assert_eq!(runner.state().objects[&vise].chosen_player(), Some(chosen));
}

#[test]
fn phase3_v2_counts_teammates_departed_seats_and_bound_players() {
    use engine::types::format::FormatConfig;
    let other = PlayerId(2);
    let (team, sources, _) = cycle_population(
        GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 42),
        4,
        [2, 0, 3],
    );
    let observer = |source| ResolvedAbility::new(Effect::NoOp, vec![], source, P0);
    // P0 cycles as the team's priority representative and P1 reads the history.
    let ability = ResolvedAbility::new(Effect::NoOp, vec![], sources[1], P1);
    for (scope, n) in [
        (CountScope::Controller, 0),
        (CountScope::All, 5),
        (CountScope::Opponents, 3),
    ] {
        assert_cycle_windows(team.state(), &ability, scope, [n, n]);
    }

    let mut scenario = GameScenario::new_with_format(FormatConfig::free_for_all(), 3, 42);
    let opponents = [P1, other];
    let vises = opponents.map(|_| printed(&mut scenario, P0, "Black Vise", Zone::Hand));
    scenario.with_mana_pool(P0, mana(ManaType::Colorless, vises.len()));
    let (mut runner, sources, event) = cycle_population(scenario, 3, [1, 2, 3]);
    let mut ability = observer(sources[2]);
    assert_eq!(runner.state().objects[&ability.source_id].owner, other);
    ability.original_controller = Some(P1);
    for scope in [
        CountScope::Controller,
        CountScope::Owner,
        CountScope::ScopedPlayer,
    ] {
        assert_cycle_windows(runner.state(), &ability, scope, [2, 2]);
    }
    runner.state_mut().current_trigger_event = Some(event);
    assert_cycle_windows(runner.state(), &ability, CountScope::ScopedPlayer, [3, 3]);
    ability.scoped_player = Some(P0);
    assert_cycle_windows(runner.state(), &ability, CountScope::ScopedPlayer, [1, 1]);
    runner.state_mut().current_trigger_event = None;

    for (vise, chosen) in vises.into_iter().zip(opponents) {
        cast_black_vise(&mut runner, vise, chosen);
    }
    let capture = |state: &GameState, source| {
        state.objects[&source]
            .snapshot_for_zone_change(source, Some(Zone::Battlefield), Zone::Battlefield)
            .trigger_source_context()
            .unwrap()
            .clone()
    };
    let mut ability = observer(vises[0]);
    for (bound, n) in [(Some(vises[1]), 3), (Some(vises[0]), 2), (None, 2)] {
        ability.trigger_source = bound.map(|source| capture(runner.state(), source));
        assert_cycle_windows(
            runner.state(),
            &ability,
            CountScope::SourceChosenPlayer,
            [n, n],
        );
    }
    ability.source_id = sources[2];
    assert_cycle_windows(
        runner.state(),
        &ability,
        CountScope::SourceChosenPlayer,
        [0, 0],
    );

    let ability = observer(sources[0]);
    priority_for(&mut runner, P1, 3);
    let cycled_cards = runner.state().players[1].graveyard.clone();
    assert_eq!(cycled_cards.len(), 2);
    assert!(cycled_cards
        .iter()
        .all(|id| runner.state().objects[id].name == "Street Wraith"));
    let departure_start = runner.state().zone_changes_this_turn.len();
    act(&mut runner, GameAction::Concede { player_id: P1 });
    assert!(runner.state().players[1].is_eliminated);
    // CR 800.4a: the departed player's owned objects leave the game.
    let owned_objects = std::iter::once((sources[1], Zone::Battlefield))
        .chain(cycled_cards.into_iter().map(|id| (id, Zone::Graveyard)));
    for (object_id, origin) in owned_objects {
        let record = runner
            .state()
            .zone_changes_this_turn
            .iter()
            .skip(departure_start)
            .find(|record| record.object_id == object_id)
            .expect("concession records each owned object's departure");
        assert_eq!(record.departure, ZoneDeparture::LeftGame);
        assert_eq!(record.from_zone, Some(origin));
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    for (scope, n) in [
        (CountScope::Controller, 1),
        (CountScope::All, 6),
        (CountScope::Opponents, 5),
    ] {
        assert_cycle_windows(runner.state(), &ability, scope, [n, n]);
    }
    let turn = runner.state().turn_number;
    advance_turn(&mut runner);
    assert_eq!(runner.state().turn_number, turn + 1);
    assert_eq!(runner.state().active_player, other);
    for (scope, n) in [
        (CountScope::Controller, 1),
        (CountScope::All, 6),
        (CountScope::Opponents, 5),
    ] {
        assert_cycle_windows(runner.state(), &ability, scope, [0, n]);
    }
}

#[test]
fn phase3_v3_counts_recorded_names_and_excludes_ordinary_discard() {
    const YIDARO: &str = "Yidaro, Wandering Monster";
    let mut scenario = main_phase();
    let first = printed(&mut scenario, P0, YIDARO, Zone::Hand);
    let second = printed(&mut scenario, P0, YIDARO, Zone::Hand);
    let discard = printed(&mut scenario, P0, YIDARO, Zone::Hand);
    let wraith = street_wraith(&mut scenario);
    let raise_dead = printed(&mut scenario, P0, "Raise Dead", Zone::Hand);
    printed(&mut scenario, P0, "Sidewinder Sliver", Zone::Graveyard);
    let looting = printed(&mut scenario, P0, "Faithless Looting", Zone::Hand);
    scenario.with_library_top(
        P0,
        &["Island", "Mountain", "Forest", "Plains", "Swamp", "Island"],
    );
    let swamp = scenario.add_basic_land(P0, ManaColor::Black);
    scenario.with_mana_pool(P0, mana(ManaType::Red, 5));
    let mut runner = scenario.build();
    let ability = ResolvedAbility::new(Effect::NoOp, vec![], first, P0);
    let count = |runner: &GameRunner, name| {
        read_cycles(
            runner.state(),
            &ability,
            CountScope::Controller,
            CyclingHistoryWindow::ThisGame,
            name,
        )
    };
    let turn = runner.state().turn_number;
    for (index, card) in [first, second].into_iter().enumerate() {
        let index_on_card = wraith_index(&runner, card);
        let events = act(
            &mut runner,
            activation(card, index_on_card, CastPaymentMode::Auto),
        );
        assert_cycle(&events, card);
        assert_eq!(zone(&runner, card), Zone::Graveyard);
        let entry = runner.state().stack.iter().find(|entry| {
            matches!(entry.kind, StackEntryKind::ActivatedAbility { source_id, .. } if source_id == card)
        }).expect("Yidaro's paid cycle under its trigger");
        assert_eq!(entry.controller, P0);
        assert_eq!(
            entry.ability().unwrap().cycling_occurrence,
            Some(CyclingOccurrence {
                activator: P0,
                journal_index: index
            }),
        );
        assert_eq!(
            runner.state().cycles_completed_this_game_by_player[&P0][index],
            cycle_record(YIDARO, turn),
        );
        assert_eq!(count(&runner, Some(YIDARO)), index as i32 + 1);
        for _ in 0..runner.state().stack.len() {
            passes(&mut runner);
        }
        priority(&runner);
        assert!(runner.state().stack.is_empty());
        if index == 0 {
            if zone(&runner, card) == Zone::Graveyard {
                tap_land(&mut runner, swamp);
                return_with_raise_dead(&mut runner, raise_dead, card);
            }
            assert_ne!(zone(&runner, card), Zone::Graveyard);
            assert_eq!(count(&runner, Some(YIDARO)), 1);
        }
    }
    let entry = cycle_wraith(&mut runner, wraith);
    assert_placement(
        runner.state(),
        entry,
        &cycle_record("Street Wraith", turn),
        2,
    );
    passes(&mut runner);
    priority(&runner);
    assert!(runner.state().stack.is_empty());
    runner.cast(looting).commit();
    let mut events = passes(&mut runner);
    let WaitingFor::DiscardChoice {
        player: P0,
        source_id,
        count: 2,
        cards,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("expected Faithless Looting to ask for two cards");
    };
    assert_eq!(*source_id, looting);
    assert!(cards.contains(&discard));
    let other = *cards
        .iter()
        .find(|&&card| card != discard)
        .expect("second discard");
    events.extend(act(
        &mut runner,
        GameAction::SelectCards {
            cards: vec![discard, other],
        },
    ));
    assert!(events
        .iter()
        .any(|event| discard_subject(event) == Some((P0, discard))));
    assert!(cycle_events(&events).is_empty());
    assert_eq!(zone(&runner, discard), Zone::Graveyard);
    priority(&runner);
    assert!(runner.state().stack.is_empty());
    assert_history(
        runner.state(),
        &[
            cycle_record(YIDARO, turn),
            cycle_record(YIDARO, turn),
            cycle_record("Street Wraith", turn),
        ],
    );
    assert_eq!(count(&runner, None), 3);
    for (name, expected) in [
        (YIDARO, 2),
        ("yidaro, wandering monster", 2),
        ("Homing Sliver", 0),
    ] {
        assert_eq!(count(&runner, Some(name)), expected);
    }
}

const YIDARO: &str = "Yidaro, Wandering Monster";

fn assert_yidaro_branch(trigger: &engine::types::ability::TriggerDefinition) {
    use engine::types::ability::{AbilityCondition, Comparator, LibraryShuffleMode};

    assert_eq!(trigger.mode, TriggerMode::Cycled);
    assert_eq!(trigger.valid_card, Some(TargetFilter::SelfRef));
    let movement = trigger.execute.as_deref().expect("Yidaro's movement");
    assert!(
        matches!(
            movement.effect.as_ref(),
            Effect::ChangeZone {
                target: TargetFilter::SelfRef,
                origin: None,
                destination: Zone::Library,
                library_shuffle: LibraryShuffleMode::TerminalShuffle,
                ..
            }
        ),
        "regenerate Yidaro's card export and integration fixture: {movement:#?}"
    );
    let branch = movement
        .sub_ability
        .as_deref()
        .expect("Yidaro's instead branch");
    assert!(
        matches!(&branch.condition, Some(AbilityCondition::ConditionInstead { inner })
        if matches!(inner.as_ref(), AbilityCondition::QuantityCheck {
            lhs: QuantityExpr::Ref { qty: QuantityRef::CardsCycled {
                scope: CountScope::Controller, window: CyclingHistoryWindow::ThisGame, ..
            }},
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 4 },
        }))
    );
    assert!(matches!(
        branch.effect.as_ref(),
        Effect::ChangeZone {
            target: TargetFilter::SelfRef,
            origin: Some(Zone::Graveyard),
            destination: Zone::Battlefield,
            ..
        }
    ));
    assert!(matches!(
        branch
            .else_ability
            .as_deref()
            .map(|def| def.effect.as_ref()),
        Some(Effect::Shuffle {
            target: TargetFilter::Controller
        })
    ));
}

const YIDARO_SCENARIO_LIBRARY: &[&str] = &[
    "Island", "Mountain", "Forest", "Plains", "Swamp", "Island", "Forest",
];

fn yidaro_scenario(p0_library: &[&str]) -> GameScenario {
    let mut scenario = main_phase();
    for (seat, library) in [(P0, p0_library), (P1, YIDARO_SCENARIO_LIBRARY)] {
        scenario.with_library_top(seat, library);
        for _ in 0..8 {
            scenario.add_basic_land(seat, ManaColor::Red);
        }
    }
    scenario
}

#[derive(Clone, Copy)]
struct PendingYidaroCycle {
    source: ObjectId,
    controller: PlayerId,
    draw: ObjectId,
    trigger: ObjectId,
}

fn cycle_yidaro(runner: &mut GameRunner, seat: PlayerId, source: ObjectId) -> PendingYidaroCycle {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == seat)
    );
    let [definition] = runner.state().objects[&source]
        .base_trigger_definitions
        .as_slice()
    else {
        panic!("expected one printed Yidaro trigger");
    };
    assert_yidaro_branch(definition);
    let lands: Vec<_> = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            let object = &runner.state().objects[id];
            object.controller == seat && object.name == "Mountain" && !object.tapped
        })
        .take(2)
        .collect();
    assert_eq!(lands.len(), 2);
    for land in lands {
        tap_land(runner, land);
    }

    let index = wraith_index(runner, source);
    assert!(offered(runner, source).contains(&index));
    let turn = runner.state().turn_number;
    let journal_index = runner
        .state()
        .cycles_completed_this_game_by_player
        .get(&seat)
        .map_or(0, |rows| rows.len());
    let events = act(runner, activation(source, index, CastPaymentMode::Auto));
    assert_eq!(cycle_events(&events), [(seat, source)]);
    assert_cycle_draw_count(&events, seat, 0);
    assert_eq!(zone(runner, source), Zone::Graveyard);
    assert_eq!(
        runner.state().cycles_completed_this_game_by_player[&seat][journal_index],
        cycle_record(YIDARO, turn)
    );

    let mut stack = runner.state().stack.iter().rev();
    let trigger = stack.next().expect("Yidaro trigger above draw");
    let draw = stack.next().expect("Yidaro cycling draw");
    assert_eq!(trigger.controller, seat);
    let ability = trigger.ability().expect("stacked self-cycle trigger");
    assert!(ability.targets.is_empty() && ability.chosen_target_slots.slots().is_empty());
    assert!(
        engine::game::ability_utils::build_target_slots(runner.state(), ability)
            .expect("self-trigger target slots")
            .is_empty()
    );
    let discarded = runner.state().objects[&source].snapshot_for_zone_change(
        source,
        Some(Zone::Graveyard),
        Zone::Library,
    );
    assert_eq!(
        ability
            .trigger_source
            .as_ref()
            .expect("bound discard")
            .identity,
        discarded.trigger_source_context().unwrap().identity,
    );
    assert_eq!(draw.controller, seat);
    let StackEntryKind::TriggeredAbility { source_id, .. } = trigger.kind else {
        panic!("expected self-cycle trigger on top");
    };
    assert_eq!(source_id, source);
    let StackEntryKind::ActivatedAbility { source_id, .. } = draw.kind else {
        panic!("expected paid cycling beneath the trigger");
    };
    assert_eq!(source_id, source);
    assert_eq!(
        draw.ability().unwrap().cycling_occurrence,
        Some(CyclingOccurrence {
            activator: seat,
            journal_index,
        })
    );
    PendingYidaroCycle {
        source,
        controller: seat,
        draw: draw.id,
        trigger: trigger.id,
    }
}

fn assert_cycle_draw_count(events: &[GameEvent], seat: PlayerId, count: usize) {
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event,
        GameEvent::CardDrawn { player_id, .. } if *player_id == seat))
            .count(),
        count
    );
}

// CR 117.3b + CR 117.3c + CR 117.4: begin with the seat holding
// priority and resolve only the current top entry.
fn pass_top_entry(runner: &mut GameRunner, entry: ObjectId) -> Vec<GameEvent> {
    assert_eq!(
        runner.state().stack.back().map(|entry| entry.id),
        Some(entry)
    );
    let WaitingFor::Priority { player } = runner.state().waiting_for else {
        panic!("expected priority before first pass");
    };
    assert!([P0, P1].contains(&player));
    let mut events = act(runner, GameAction::PassPriority);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player: next } if next != player)
    );
    assert_eq!(
        runner.state().stack.back().map(|entry| entry.id),
        Some(entry)
    );
    events.extend(act(runner, GameAction::PassPriority));
    assert!(runner
        .state()
        .stack
        .iter()
        .all(|pending| pending.id != entry));
    events
}

fn resolve_yidaro_trigger(
    runner: &mut GameRunner,
    cycle: PendingYidaroCycle,
    destination: Option<Zone>,
    shuffles: usize,
) -> Vec<GameEvent> {
    let start = runner.state().resolved_rules_journal.entries().len();
    let events = pass_top_entry(runner, cycle.trigger);
    assert_eq!(
        runner.state().stack.back().map(|entry| entry.id),
        Some(cycle.draw)
    );
    assert_cycle_draw_count(&events, cycle.controller, 0);
    let moves = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Graveyard),
                to,
                ..
            } if *object_id == cycle.source => Some(*to),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(moves, destination.into_iter().collect::<Vec<_>>());
    let journal_players: Vec<_> = runner
        .state()
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(start)
        .filter_map(|entry| match &entry.command {
            Some(ResolvedRulesCommand::LibraryShuffle(command)) => Some(command.player),
            _ => None,
        })
        .collect();
    let event_players: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::ShuffledLibrary,
                ..
            } => Some(*player_id),
            _ => None,
        })
        .collect();
    assert_eq!(journal_players, vec![cycle.controller; shuffles]);
    assert_eq!(event_players, journal_players);
    events
}

fn finish_yidaro_draw(runner: &mut GameRunner, cycle: PendingYidaroCycle) {
    let events = pass_top_entry(runner, cycle.draw);
    assert_cycle_draw_count(&events, cycle.controller, 1);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player }
        if player == runner.state().active_player)
    );
}

fn cycle_yidaro_into_library(runner: &mut GameRunner, seat: PlayerId, source: ObjectId) {
    let cycle = cycle_yidaro(runner, seat, source);
    resolve_yidaro_trigger(runner, cycle, Some(Zone::Library), 1);
    assert_eq!(zone(runner, source), Zone::Library);
    finish_yidaro_draw(runner, cycle);
    assert!(runner.state().stack.is_empty());
}

#[test]
fn phase5_v2_three_cycles_shuffle_four_enter() {
    // CR 100.2a: Three cycling copies plus the graveyard bystander fit the four-copy limit.
    // Two filler cards guarantee that the first three cycles and the Wraith draw
    // return at least one of those copies to hand, with a card left to draw on cycle four.
    let mut scenario = yidaro_scenario(&["Island", "Mountain"]);
    let cards: [_; 3] = std::array::from_fn(|_| printed(&mut scenario, P0, YIDARO, Zone::Hand));
    let bystander = printed(&mut scenario, P0, YIDARO, Zone::Graveyard);
    let opponent = printed(&mut scenario, P1, YIDARO, Zone::Hand);
    let wraith = street_wraith(&mut scenario);
    let mut runner = scenario.build();

    for card in &cards[..2] {
        cycle_yidaro_into_library(&mut runner, P0, *card);
        assert_eq!(zone(&runner, bystander), Zone::Graveyard);
    }
    let first_turn = runner.state().turn_number;
    advance_turn(&mut runner);
    assert_eq!(runner.state().turn_number, first_turn + 1);
    priority_for(&mut runner, P1, 2);
    cycle_yidaro_into_library(&mut runner, P1, opponent);
    assert_eq!(
        runner.state().cycles_completed_this_game_by_player[&P1].len(),
        1
    );
    priority_for(&mut runner, P0, 2);
    let wraith_entry = cycle_wraith_for_phase5(&mut runner, wraith);
    let events = pass_top_entry(&mut runner, wraith_entry);
    assert_cycle_draw_count(&events, P0, 1);

    priority_for(&mut runner, P0, 2);
    cycle_yidaro_into_library(&mut runner, P0, cards[2]);
    assert_eq!(zone(&runner, bystander), Zone::Graveyard);

    priority_for(&mut runner, P0, 2);
    let recycled = cards
        .into_iter()
        .find(|&card| zone(&runner, card) == Zone::Hand)
        .expect("a previously cycled Yidaro was drawn back into hand");
    let fourth = cycle_yidaro(&mut runner, P0, recycled);
    let records = &runner.state().cycles_completed_this_game_by_player[&P0];
    assert_eq!(records.len(), 5);
    assert_eq!(records[0], cycle_record(YIDARO, first_turn));
    assert_eq!(records[1], cycle_record(YIDARO, first_turn));
    resolve_yidaro_trigger(&mut runner, fourth, Some(Zone::Battlefield), 0);
    assert_eq!(zone(&runner, fourth.source), Zone::Battlefield);
    assert_eq!(zone(&runner, bystander), Zone::Graveyard);
    finish_yidaro_draw(&mut runner, fourth);
    assert!(runner.state().stack.is_empty());
}

fn cycle_wraith_for_phase5(runner: &mut GameRunner, source: ObjectId) -> ObjectId {
    let index = wraith_index(runner, source);
    assert!(offered(runner, source).contains(&index));
    let entry = cycle_wraith(runner, source);
    assert_eq!(
        runner.state().cycles_completed_this_game_by_player[&P0][2],
        cycle_record("Street Wraith", runner.state().turn_number)
    );
    entry
}

#[test]
fn phase5_v2a_pending_third_trigger_reads_fourth_cycle() {
    let mut scenario = yidaro_scenario(YIDARO_SCENARIO_LIBRARY);
    let [first, second, a, b] =
        std::array::from_fn(|_| printed(&mut scenario, P0, YIDARO, Zone::Hand));
    let mut runner = scenario.build();
    for card in [first, second] {
        cycle_yidaro_into_library(&mut runner, P0, card);
    }
    let a = cycle_yidaro(&mut runner, P0, a);
    assert_eq!(
        runner.state().cycles_completed_this_game_by_player[&P0].len(),
        3
    );
    priority_for(&mut runner, P0, 2);
    let b = cycle_yidaro(&mut runner, P0, b);
    assert_eq!(
        runner.state().cycles_completed_this_game_by_player[&P0].len(),
        4
    );
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        [a.draw, a.trigger, b.draw, b.trigger]
    );
    resolve_yidaro_trigger(&mut runner, b, Some(Zone::Battlefield), 0);
    finish_yidaro_draw(&mut runner, b);
    assert_eq!(zone(&runner, b.source), Zone::Battlefield);

    // CR 704.5j: Both successful entries reach the legend choice.
    resolve_yidaro_trigger(&mut runner, a, Some(Zone::Battlefield), 0);
    let WaitingFor::ChooseLegend {
        player: P0,
        candidates,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("expected the controller's legend choice");
    };
    assert_eq!(candidates.len(), 2);
    assert!(candidates.contains(&a.source) && candidates.contains(&b.source));
    act(&mut runner, GameAction::ChooseLegend { keep: a.source });
    assert_eq!(zone(&runner, a.source), Zone::Battlefield);
    assert_eq!(zone(&runner, b.source), Zone::Graveyard);
    finish_yidaro_draw(&mut runner, a);
    assert!(runner.state().stack.is_empty());
}

#[test]
fn phase5_v3_departed_source_keeps_only_the_selected_branch() {
    for prior_cycles in [2, 3] {
        let mut scenario = yidaro_scenario(YIDARO_SCENARIO_LIBRARY);
        let earlier: Vec<_> = (0..prior_cycles)
            .map(|_| printed(&mut scenario, P0, YIDARO, Zone::Hand))
            .collect();
        let source = printed(&mut scenario, P0, YIDARO, Zone::Hand);
        let crypt = printed(&mut scenario, P1, "Tormod's Crypt", Zone::Battlefield);
        let mut runner = scenario.build();
        for card in earlier {
            cycle_yidaro_into_library(&mut runner, P0, card);
        }
        let cycle = cycle_yidaro(&mut runner, P0, source);
        let discarded_incarnation = runner.state().objects[&source].incarnation;
        assert_eq!(
            runner.state().cycles_completed_this_game_by_player[&P0].len(),
            prior_cycles + 1
        );
        priority_for(&mut runner, P1, 2);
        let offered_crypt = offered(&runner, crypt);
        let &[index] = offered_crypt.as_slice() else {
            panic!("expected one Crypt activation, got {offered_crypt:?}");
        };
        act(&mut runner, activation(crypt, index, CastPaymentMode::Auto));
        let WaitingFor::TargetSelection {
            player: P1,
            selection,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("expected Crypt's player target");
        };
        let target = TargetRef::Player(P0);
        assert!(selection.current_legal_targets.contains(&target));
        act(
            &mut runner,
            GameAction::ChooseTarget {
                target: Some(target),
            },
        );
        let entry = runner.state().stack.back().expect("Crypt ability");
        assert!(
            matches!(entry.kind, StackEntryKind::ActivatedAbility { source_id, .. } if source_id == crypt)
        );
        let entry_id = entry.id;
        let events = pass_top_entry(&mut runner, entry_id);
        assert!(events
            .iter()
            .any(|event| matches!(event, GameEvent::ZoneChanged {
            object_id, from: Some(Zone::Graveyard), to: Zone::Exile, ..
        } if *object_id == source)));
        assert_eq!(zone(&runner, source), Zone::Exile);
        assert_ne!(
            runner.state().objects[&source].incarnation,
            discarded_incarnation
        );

        // CR 400.7 + CR 701.24c + CR 614.6: the exact discarded incarnation
        // departed, so only the default instruction can still shuffle.
        let events =
            resolve_yidaro_trigger(&mut runner, cycle, None, usize::from(prior_cycles == 2));
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::ZoneChanged {
            object_id, ..
        } if *object_id == source)));
        assert_eq!(zone(&runner, source), Zone::Exile);
        finish_yidaro_draw(&mut runner, cycle);
        assert!(runner.state().stack.is_empty());
    }
}

const PHOENIX: &str = "Spellpyre Phoenix";

fn phoenix_guard() -> TriggerCondition {
    TriggerCondition::QuantityComparison {
        lhs: QuantityExpr::Ref {
            qty: QuantityRef::CardsCycled {
                scope: CountScope::Controller,
                window: CyclingHistoryWindow::ThisTurn,
                name: None,
            },
        },
        comparator: Comparator::GE,
        rhs: QuantityExpr::Fixed { value: 2 },
    }
}

fn assert_phoenix_definition(trigger: &TriggerDefinition) {
    assert_eq!(trigger.trigger_zones, [Zone::Graveyard]);
    assert_eq!(trigger.condition, Some(phoenix_guard()));
    assert!(trigger.valid_target.is_none() && !trigger.optional);
    let body = trigger.execute.as_deref().expect("printed Phoenix return");
    assert!(matches!(
        body.effect.as_ref(),
        Effect::ChangeZone {
            origin: Some(Zone::Graveyard),
            destination: Zone::Hand,
            target: TargetFilter::SelfRef,
            ..
        }
    ));
    assert!(body.target_prompt.is_none());
    assert!(body.sub_ability.is_none() && body.else_ability.is_none());
}

fn phoenix_scenario() -> GameScenario {
    let mut scenario = main_phase();
    for player in [P0, P1] {
        scenario.with_library_top(player, &["Island"; 8]);
    }
    scenario
}

fn finish_phoenix_wraith_cycle(runner: &mut GameRunner, player: PlayerId, card: ObjectId) {
    priority_for(runner, player, 2);
    let index = wraith_index(runner, card);
    assert!(offered(runner, card).contains(&index));
    let life = runner.life(player);
    let events = act(runner, activation(card, index, CastPaymentMode::Auto));
    assert_eq!(cycle_events(&events), [(player, card)]);
    assert_cycle_draw_count(&events, player, 0);
    assert_eq!(runner.life(player), life - 2);
    assert_eq!(zone(runner, card), Zone::Graveyard);
    assert_eq!(runner.state().stack.len(), 1);
    let entry = runner
        .state()
        .stack
        .back()
        .expect("placed cycling activation");
    assert_eq!(entry.controller, player);
    assert!(
        matches!(entry.kind, StackEntryKind::ActivatedAbility { source_id, .. } if source_id == card)
    );
    let entry_id = entry.id;
    let events = pass_top_entry(runner, entry_id);
    assert_cycle_draw_count(&events, player, 1);
    assert!(runner.state().stack.is_empty());
}

fn assert_phoenix_history(runner: &GameRunner, player: PlayerId, turns: &[u32]) {
    let rows = runner
        .state()
        .cycles_completed_this_game_by_player
        .get(&player)
        .into_iter()
        .flatten();
    let expected: Vec<_> = turns
        .iter()
        .map(|&turn| cycle_record("Street Wraith", turn))
        .collect();
    assert_eq!(
        rows.collect::<Vec<_>>(),
        expected.iter().collect::<Vec<_>>()
    );
}

fn enter_phoenix_end_step(runner: &mut GameRunner, active: PlayerId, sources: &[ObjectId]) {
    for source in sources {
        let definition = runner.state().objects[source]
            .base_trigger_definitions
            .iter()
            .find(|trigger| trigger.mode == TriggerMode::Phase && trigger.phase == Some(Phase::End))
            .expect("exported Phoenix end-step ability");
        assert_phoenix_definition(definition);
    }
    assert_ne!(runner.state().phase, Phase::End);
    let turn = runner.state().turn_number;
    for _ in 0..48 {
        let (_, action) = next_turn_action(runner.state());
        let events = act(runner, action);
        assert_eq!(runner.state().turn_number, turn);
        if events
            .iter()
            .any(|event| matches!(event, GameEvent::PhaseChanged { phase: Phase::End }))
        {
            assert_eq!(runner.state().phase, Phase::End);
            assert_eq!(runner.state().active_player, active);
            assert!(
                matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == active)
            );
            return;
        }
    }
    panic!("expected the public end-step event");
}

fn assert_phoenix_entry(runner: &GameRunner, source: ObjectId, player: PlayerId) -> ObjectId {
    let entry = runner
        .state()
        .stack
        .back()
        .expect("Phoenix trigger must stack");
    assert_eq!(entry.controller, player);
    let StackEntryKind::TriggeredAbility {
        source_id,
        condition,
        trigger_event,
        ability,
        ..
    } = &entry.kind
    else {
        panic!("expected a triggered ability")
    };
    assert_eq!(*source_id, source);
    assert_eq!(*condition, Some(phoenix_guard()));
    assert!(matches!(
        trigger_event,
        Some(GameEvent::PhaseChanged { phase: Phase::End })
    ));
    let bound = &ability
        .trigger_source
        .as_ref()
        .expect("bound Phoenix source")
        .identity;
    assert_eq!(bound.expected_zone, Zone::Graveyard);
    assert_eq!(
        bound.reference,
        ObjectIncarnationRef::of(source, runner.state().objects[&source].incarnation)
    );
    assert!(ability.targets.is_empty() && ability.chosen_target_slots.slots().is_empty());
    entry.id
}

fn assert_phoenix_move(events: &[GameEvent], source: ObjectId, origin: Zone, destination: Zone) {
    assert!(events
        .iter()
        .any(|event| matches!(event, GameEvent::ZoneChanged {
        object_id, from: Some(from), to, ..
    } if *object_id == source && *from == origin && *to == destination)));
}

fn resolve_phoenix_return(
    runner: &mut GameRunner,
    source: ObjectId,
    owner: PlayerId,
    entry: ObjectId,
) {
    let events = pass_top_entry(runner, entry);
    assert_phoenix_move(&events, source, Zone::Graveyard, Zone::Hand);
    assert_eq!(zone(runner, source), Zone::Hand);
    assert!(runner.state().players[usize::from(owner.0)]
        .hand
        .contains(&source));
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

fn phoenix_with_two_cycles(
    mut scenario: GameScenario,
    source: ObjectId,
    other_sources: &[ObjectId],
) -> (GameRunner, ObjectId) {
    let wraiths = [street_wraith(&mut scenario), street_wraith(&mut scenario)];
    let mut runner = scenario.build();
    let turn = runner.state().turn_number;
    for wraith in wraiths {
        finish_phoenix_wraith_cycle(&mut runner, P0, wraith);
    }
    assert_phoenix_history(&runner, P0, &[turn, turn]);
    let sources: Vec<_> = std::iter::once(source)
        .chain(other_sources.iter().copied())
        .collect();
    enter_phoenix_end_step(&mut runner, P0, &sources);
    assert_eq!(runner.state().stack.len(), 1);
    let entry = assert_phoenix_entry(&runner, source, P0);
    (runner, entry)
}

#[test]
fn phase6_v2_two_cycles_return_phoenix() {
    let mut scenario = phoenix_scenario();
    let source = printed(&mut scenario, P0, PHOENIX, Zone::Graveyard);
    let (mut runner, entry) = phoenix_with_two_cycles(scenario, source, &[]);
    resolve_phoenix_return(&mut runner, source, P0, entry);
    assert!(runner.state().stack.is_empty());
}

#[test]
fn phase6_v3_zero_cycles_or_one_in_each_turn_do_not_trigger() {
    for across_turns in [false, true] {
        let mut scenario = phoenix_scenario();
        let source = printed(&mut scenario, P0, PHOENIX, Zone::Graveyard);
        let [first, second] = [street_wraith(&mut scenario), street_wraith(&mut scenario)];
        let mut runner = scenario.build();
        let first_turn = runner.state().turn_number;
        let active = if across_turns {
            finish_phoenix_wraith_cycle(&mut runner, P0, first);
            advance_turn(&mut runner);
            finish_phoenix_wraith_cycle(&mut runner, P0, second);
            let current_turn = runner.state().turn_number;
            assert_ne!(first_turn, current_turn);
            assert_phoenix_history(&runner, P0, &[first_turn, current_turn]);
            P1
        } else {
            assert_phoenix_history(&runner, P0, &[]);
            P0
        };
        enter_phoenix_end_step(&mut runner, active, &[source]);
        assert!(runner.state().stack.is_empty());
        assert_eq!(zone(&runner, source), Zone::Graveyard);
    }
}

fn announce_phoenix_test_spell(runner: &mut GameRunner, spell: ObjectId) -> Vec<GameEvent> {
    let card_id = runner.state().objects[&spell].card_id;
    act(
        runner,
        GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        },
    )
}

fn resolve_phoenix_test_spell(runner: &mut GameRunner, spell: ObjectId) -> Vec<GameEvent> {
    let stack = runner.state().stack.back().expect("cast spell on top");
    assert_eq!(stack.source_id, spell);
    assert!(matches!(stack.kind, StackEntryKind::Spell { .. }));
    let entry = stack.id;
    pass_top_entry(runner, entry)
}

fn cast_phoenix_test_targeted_spell(
    runner: &mut GameRunner,
    spell: ObjectId,
    target: ObjectId,
    player: PlayerId,
) -> Vec<GameEvent> {
    announce_phoenix_test_spell(runner, spell);
    let WaitingFor::TargetSelection {
        player: choosing,
        selection,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("expected the printed spell's target selection")
    };
    assert_eq!(*choosing, player);
    let target = TargetRef::Object(target);
    assert!(selection.current_legal_targets.contains(&target));
    act(
        runner,
        GameAction::ChooseTarget {
            target: Some(target),
        },
    );
    resolve_phoenix_test_spell(runner, spell)
}

#[test]
fn phase6_v4_cycles_predate_graveyard_entry_on_opponents_turn() {
    let mut scenario = phoenix_scenario();
    let source = printed(&mut scenario, P0, PHOENIX, Zone::Battlefield);
    // Give Murder a second legal target so casting opens the target prompt.
    let decoy = scenario.add_vanilla(P1, 1, 1);
    let wraiths = [street_wraith(&mut scenario), street_wraith(&mut scenario)];
    let murder = printed(&mut scenario, P1, "Murder", Zone::Hand);
    let swamps = [(); 3].map(|()| scenario.add_basic_land(P1, ManaColor::Black));
    let mut runner = scenario.build();
    advance_turn(&mut runner);
    assert_eq!(runner.state().active_player, P1);
    let turn = runner.state().turn_number;
    for wraith in wraiths {
        finish_phoenix_wraith_cycle(&mut runner, P0, wraith);
        assert_eq!(zone(&runner, source), Zone::Battlefield);
    }
    assert_phoenix_history(&runner, P0, &[turn, turn]);
    assert_phoenix_history(&runner, P1, &[]);
    priority_for(&mut runner, P1, 2);
    for swamp in swamps {
        tap_land(&mut runner, swamp);
    }
    let events = cast_phoenix_test_targeted_spell(&mut runner, murder, source, P1);
    assert_phoenix_move(&events, source, Zone::Battlefield, Zone::Graveyard);
    assert_eq!(zone(&runner, decoy), Zone::Battlefield);
    enter_phoenix_end_step(&mut runner, P1, &[source]);
    assert_eq!(runner.state().stack.len(), 1);
    let entry = assert_phoenix_entry(&runner, source, P0);
    resolve_phoenix_return(&mut runner, source, P0, entry);
    assert!(runner.state().stack.is_empty());
}

#[test]
fn phase6_v5_same_physical_card_counts_twice() {
    let mut scenario = phoenix_scenario();
    let source = printed(&mut scenario, P0, PHOENIX, Zone::Graveyard);
    let wraith = street_wraith(&mut scenario);
    let raise_dead = printed(&mut scenario, P0, "Raise Dead", Zone::Hand);
    let swamp = scenario.add_basic_land(P0, ManaColor::Black);
    let mut runner = scenario.build();
    let turn = runner.state().turn_number;
    finish_phoenix_wraith_cycle(&mut runner, P0, wraith);
    assert_phoenix_history(&runner, P0, &[turn]);
    let discarded = runner.state().objects[&wraith].incarnation;
    tap_land(&mut runner, swamp);
    return_with_raise_dead(&mut runner, raise_dead, wraith);
    assert_ne!(runner.state().objects[&wraith].incarnation, discarded);
    finish_phoenix_wraith_cycle(&mut runner, P0, wraith);
    assert_phoenix_history(&runner, P0, &[turn, turn]);
    enter_phoenix_end_step(&mut runner, P0, &[source]);
    assert_eq!(runner.state().stack.len(), 1);
    let entry = assert_phoenix_entry(&runner, source, P0);
    resolve_phoenix_return(&mut runner, source, P0, entry);
    assert!(runner.state().stack.is_empty());
}

#[test]
fn phase6_v6_other_players_cycles_and_battlefield_copies_are_excluded() {
    let mut scenario = phoenix_scenario();
    let source = printed(&mut scenario, P0, PHOENIX, Zone::Graveyard);
    let wraiths = [(); 2].map(|()| printed(&mut scenario, P1, "Street Wraith", Zone::Hand));
    let mut runner = scenario.build();
    advance_turn(&mut runner);
    for wraith in wraiths {
        finish_phoenix_wraith_cycle(&mut runner, P1, wraith);
    }
    let turn = runner.state().turn_number;
    assert_phoenix_history(&runner, P1, &[turn, turn]);
    assert_phoenix_history(&runner, P0, &[]);
    enter_phoenix_end_step(&mut runner, P1, &[source]);
    assert!(runner.state().stack.is_empty());
    assert_eq!(zone(&runner, source), Zone::Graveyard);

    let mut scenario = phoenix_scenario();
    let source = printed(&mut scenario, P0, PHOENIX, Zone::Graveyard);
    let battlefield = printed(&mut scenario, P0, PHOENIX, Zone::Battlefield);
    let (mut runner, entry) = phoenix_with_two_cycles(scenario, source, &[battlefield]);
    resolve_phoenix_return(&mut runner, source, P0, entry);
    assert!(runner.state().stack.is_empty());
    assert_eq!(zone(&runner, battlefield), Zone::Battlefield);
}

fn resolve_phoenix_without_return(
    runner: &mut GameRunner,
    entry: ObjectId,
    source: ObjectId,
    expected_zone: Zone,
) {
    assert_eq!(runner.state().stack.len(), 1);
    let events = pass_top_entry(runner, entry);
    assert!(!events
        .iter()
        .any(|event| matches!(event, GameEvent::ZoneChanged {
        object_id, ..
    } if *object_id == source)));
    assert_eq!(zone(runner, source), expected_zone);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn phase6_v7_exiled_source_leaves_trigger_pending() {
    let mut scenario = phoenix_scenario();
    let source = printed(&mut scenario, P0, PHOENIX, Zone::Graveyard);
    let decoy = printed(&mut scenario, P0, PHOENIX, Zone::Battlefield);
    let cremate = printed(&mut scenario, P0, "Cremate", Zone::Hand);
    let swamp = scenario.add_basic_land(P0, ManaColor::Black);
    let (mut runner, entry) = phoenix_with_two_cycles(scenario, source, &[decoy]);
    tap_land(&mut runner, swamp);
    let events = cast_phoenix_test_targeted_spell(&mut runner, cremate, source, P0);
    assert_phoenix_move(&events, source, Zone::Graveyard, Zone::Exile);
    let turn = runner.state().turn_number;
    assert_phoenix_history(&runner, P0, &[turn, turn]);
    resolve_phoenix_without_return(&mut runner, entry, source, Zone::Exile);
    assert_eq!(zone(&runner, decoy), Zone::Battlefield);
}

#[test]
fn phase6_v7_same_card_back_in_graveyard_has_a_new_incarnation() {
    let mut scenario = phoenix_scenario();
    let source = printed(&mut scenario, P0, PHOENIX, Zone::Graveyard);
    let mannequin = printed(&mut scenario, P0, "Makeshift Mannequin", Zone::Hand);
    let rites = printed(&mut scenario, P0, "Village Rites", Zone::Hand);
    let other_creature = scenario.add_vanilla(P0, 1, 1);
    let swamps = [(); 5].map(|()| scenario.add_basic_land(P0, ManaColor::Black));
    let (mut runner, entry) = phoenix_with_two_cycles(scenario, source, &[]);
    let card_id = runner.state().objects[&source].card_id;
    let incarnation = runner.state().objects[&source].incarnation;
    for swamp in swamps {
        tap_land(&mut runner, swamp);
    }
    let events = cast_phoenix_test_targeted_spell(&mut runner, mannequin, source, P0);
    assert_phoenix_move(&events, source, Zone::Graveyard, Zone::Battlefield);
    assert_eq!(runner.state().stack.front().unwrap().id, entry);
    announce_phoenix_test_spell(&mut runner, rites);
    let WaitingFor::PayCost {
        player: P0,
        kind: PayCostKind::Sacrifice,
        choices,
        count: 1,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("expected Village Rites' sacrifice cost selection")
    };
    assert!(choices.contains(&source) && choices.contains(&other_creature));
    let events = act(
        &mut runner,
        GameAction::SelectCards {
            cards: vec![source],
        },
    );
    assert_phoenix_move(&events, source, Zone::Battlefield, Zone::Graveyard);
    resolve_phoenix_test_spell(&mut runner, rites);

    let top = runner
        .state()
        .stack
        .back()
        .expect("original Phoenix trigger");
    if top.id != entry {
        assert!(matches!(&top.kind, StackEntryKind::TriggeredAbility {
            source_id,
            trigger_event: Some(GameEvent::ZoneChanged { to: Zone::Battlefield, .. }),
            ..
        } if *source_id == source));
        let enters_entry = top.id;
        pass_top_entry(&mut runner, enters_entry);
        if matches!(runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player: P0, source_id, .. } if source_id == source)
        {
            act(
                &mut runner,
                GameAction::DecideOptionalEffect { accept: false },
            );
        }
    }

    let current = &runner.state().objects[&source];
    assert_eq!(current.card_id, card_id);
    assert_eq!(current.zone, Zone::Graveyard);
    assert_ne!(current.incarnation, incarnation);
    let pending = runner
        .state()
        .stack
        .back()
        .expect("original Phoenix trigger");
    assert_eq!(pending.id, entry);
    assert_eq!(pending.controller, P0);
    assert_eq!(
        pending
            .ability()
            .unwrap()
            .trigger_source
            .as_ref()
            .unwrap()
            .identity
            .reference,
        ObjectIncarnationRef::of(source, incarnation)
    );
    let turn = runner.state().turn_number;
    assert_phoenix_history(&runner, P0, &[turn, turn]);
    resolve_phoenix_without_return(&mut runner, entry, source, Zone::Graveyard);
}

#[test]
fn phase6_v8_resolution_rechecks_the_live_ledger() {
    let mut scenario = phoenix_scenario();
    let source = printed(&mut scenario, P0, PHOENIX, Zone::Graveyard);
    let (mut control, entry) = phoenix_with_two_cycles(scenario, source, &[]);
    let mut changed = GameRunner::from_state(control.state().clone());
    // CR 603.4 + CR 608.2a: isolate the resolution recheck with a test-only
    // ledger edit. The same graveyard incarnation and original entry still exist.
    let turn = changed.state().turn_number;
    assert_eq!(
        changed
            .state_mut()
            .cycles_completed_this_game_by_player
            .get_mut(&P0)
            .unwrap()
            .pop_back(),
        Some(cycle_record("Street Wraith", turn))
    );
    assert_phoenix_history(&changed, P0, &[turn]);
    assert_eq!(assert_phoenix_entry(&changed, source, P0), entry);
    resolve_phoenix_return(&mut control, source, P0, entry);
    assert!(control.state().stack.is_empty());
    resolve_phoenix_without_return(&mut changed, entry, source, Zone::Graveyard);
}

#[test]
fn phase6_v9_multiple_qualifying_players_stack_in_apnap_order() {
    let mut scenario = phoenix_scenario();
    let players = [P0, P1];
    let sources = players.map(|player| printed(&mut scenario, player, PHOENIX, Zone::Graveyard));
    let wraiths = players.map(|player| {
        [(); 2].map(|()| printed(&mut scenario, player, "Street Wraith", Zone::Hand))
    });
    let mut runner = scenario.build();
    let turn = runner.state().turn_number;
    for (player, cards) in players.into_iter().zip(wraiths) {
        for card in cards {
            finish_phoenix_wraith_cycle(&mut runner, player, card);
        }
        assert_phoenix_history(&runner, player, &[turn, turn]);
    }
    enter_phoenix_end_step(&mut runner, P0, &sources);
    assert_eq!(runner.state().stack.len(), 2);
    // CR 603.3b: The nonactive player's trigger resolves first.
    let nonactive = assert_phoenix_entry(&runner, sources[1], P1);
    resolve_phoenix_return(&mut runner, sources[1], P1, nonactive);
    assert_eq!(zone(&runner, sources[0]), Zone::Graveyard);
    let active = assert_phoenix_entry(&runner, sources[0], P0);
    resolve_phoenix_return(&mut runner, sources[0], P0, active);
    assert!(runner.state().stack.is_empty());
}
