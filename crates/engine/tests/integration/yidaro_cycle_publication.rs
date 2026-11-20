use crate::support::shared_card_db;
use engine::ai_support::legal_actions_full;
use engine::game::combat::AttackTarget;
use engine::game::engine::EngineError;
use engine::game::keywords::object_has_effective_keyword_kind;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityTag, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastPaymentMode, ManaChoice, ManaChoicePrompt, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, KeywordKind};
use engine::types::mana::{ManaColor, ManaType};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const STREET_WRAITH: &str = "Swampwalk (This creature can't be blocked as long as defending player controls a Swamp.)\nCycling—Pay 2 life. (Pay 2 life, Discard this card: Draw a card.)";

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
    scenario
        .add_creature_to_hand(P0, "Street Wraith", 3, 4)
        .with_subtypes(vec!["Wraith"])
        .with_color(vec![ManaColor::Black])
        .with_mana_cost(
            engine::parser::oracle_util::parse_mana_symbols("{3}{B}{B}")
                .unwrap()
                .0,
        )
        .from_oracle_text_with_keywords(&["Cycling", "Landwalk", "Swampwalk"], STREET_WRAITH)
        .id()
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
    for life in [20, 1] {
        let mut scenario = main_phase();
        scenario.with_life(P0, life);
        let card = street_wraith(&mut scenario);
        let draw = scenario.add_card_to_library_top(P0, "Island");
        let mut runner = scenario.build();
        let index = runner.state().objects[&card]
            .abilities
            .iter()
            .position(|def| def.ability_tag == Some(AbilityTag::Cycling))
            .expect("printed cycling");
        let result = runner.act(activation(card, index, CastPaymentMode::Auto));
        priority(&runner);
        if life == 1 {
            assert!(matches!(
                result,
                Err(EngineError::ActionNotAllowed(reason)) if reason == "Cannot pay activation cost"
            ));
            assert!(runner.state().stack.is_empty());
            assert!(runner
                .state()
                .abilities_activated_this_turn_by_player
                .is_empty());
            assert_eq!(zone(&runner, card), Zone::Hand);
            assert_eq!(runner.state().players[0].life, 1);
        } else {
            let events = result.expect("funded life payment").events;
            assert_eq!(runner.state().players[0].life, 18);
            assert_eq!(zone(&runner, card), Zone::Graveyard);
            activation_entry(&runner, card);
            assert_eq!(runner.state().stack.len(), 1);
            journaled_cycle(&runner, card);
            assert_cycle(&events, card);
            assert_eq!(zone(&runner, draw), Zone::Library);
            assert!(cycle_events(&passes(&mut runner)).is_empty());
            priority(&runner);
            assert!(runner.state().stack.is_empty());
            assert_eq!(zone(&runner, draw), Zone::Hand);
        }
    }
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

fn contains_strict_failure(value: &serde_json::Value, name: &str) -> bool {
    value["type"] == "Unimplemented" && value["name"] == name
        || match value {
            serde_json::Value::Object(fields) => fields
                .values()
                .any(|value| contains_strict_failure(value, name)),
            serde_json::Value::Array(items) => items
                .iter()
                .any(|value| contains_strict_failure(value, name)),
            _ => false,
        }
}

#[test]
fn v6_history_clauses_remain_strict_failures() {
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
    assert!(contains_strict_failure(
        &serde_json::to_value(cycle).unwrap(),
        "instead_condition"
    ));

    let phoenix = parse(
        "Flying\nWhen this creature enters, you may return target instant or sorcery card with a cycling ability from your graveyard to your hand.\nAt the beginning of each end step, if you cycled two or more cards this turn, return this card from your graveyard to your hand.",
        "Spellpyre Phoenix", &["Flying"], &["Phoenix"],
    );
    let end = phoenix
        .triggers
        .iter()
        .find(|trigger| trigger.mode == TriggerMode::Phase && trigger.phase == Some(Phase::End))
        .expect("Phoenix end-step trigger");
    assert!(contains_strict_failure(
        &serde_json::to_value(end).unwrap(),
        "unparsed_condition"
    ));
}
