//! CR 702.29c: “When you cycle this card” means “When you discard this card to pay an
//! activation cost of a cycling ability.” These abilities trigger from whatever zone the card
//! winds up in after it’s cycled.
//!
//! Titanoth Rex:
//! Trample
//! Cycling {1}{G} ({1}{G}, Discard this card: Draw a card.)
//! When you cycle this card, put a trample counter on target creature you control.
//!
//! Leyline of the Void:
//! If this card is in your opening hand, you may begin the game with it on the battlefield.
//! If a card would be put into an opponent's graveyard from anywhere, exile it instead.
//!
//! Rest in Peace:
//! When this enchantment enters, exile all graveyards.
//! If a card or token would be put into a graveyard from anywhere, exile it instead.
//!
//! Fractured Sanity:
//! Each opponent mills fourteen cards.
//! Cycling {1}{U} ({1}{U}, Discard this card: Draw a card.)
//! When you cycle this card, each opponent mills four cards.
//!
//! Shark Typhoon:
//! Whenever you cast a noncreature spell, create an X/X blue Shark creature token with flying, where X is that spell's mana value.
//! Cycling {X}{1}{U} ({X}{1}{U}, Discard this card: Draw a card.)
//! When you cycle this card, create an X/X blue Shark creature token with flying.
//!
//! Yidaro, Wandering Monster:
//! Trample, haste
//! Cycling {1}{R}
//! When you cycle this card, shuffle it into your library from your graveyard. If you've cycled a card named Yidaro, Wandering Monster four or more times this game, put it onto the battlefield from your graveyard instead. (Do this before you draw.)
//!
//! Drake Haven:
//! Whenever you cycle or discard a card, you may pay {1}. If you do, create a 2/2 blue Drake creature token with flying.

use engine::ai_support::legal_actions_full;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{AbilityTag, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::KeywordKind;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const TITANOTH_REX: &str = "Trample\nCycling {1}{G} ({1}{G}, Discard this card: Draw a card.)\nWhen you cycle this card, put a trample counter on target creature you control.";
const LEYLINE_OF_THE_VOID: &str = "If this card is in your opening hand, you may begin the game with it on the battlefield.\nIf a card would be put into an opponent's graveyard from anywhere, exile it instead.";
const REST_IN_PEACE: &str = "When this enchantment enters, exile all graveyards.\nIf a card or token would be put into a graveyard from anywhere, exile it instead.";
const FRACTURED_SANITY: &str = "Each opponent mills fourteen cards.\nCycling {1}{U} ({1}{U}, Discard this card: Draw a card.)\nWhen you cycle this card, each opponent mills four cards.";
const SHARK_TYPHOON: &str = "Whenever you cast a noncreature spell, create an X/X blue Shark creature token with flying, where X is that spell's mana value.\nCycling {X}{1}{U} ({X}{1}{U}, Discard this card: Draw a card.)\nWhen you cycle this card, create an X/X blue Shark creature token with flying.";
const YIDARO: &str = "Trample, haste\nCycling {1}{R}\nWhen you cycle this card, shuffle it into your library from your graveyard. If you've cycled a card named Yidaro, Wandering Monster four or more times this game, put it onto the battlefield from your graveyard instead. (Do this before you draw.)";
const DRAKE_HAVEN: &str = "Whenever you cycle or discard a card, you may pay {1}. If you do, create a 2/2 blue Drake creature token with flying.";

fn scenario(mana: &[ManaType]) -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Cycling draw", "Second cycling draw"]);
    scenario.with_mana_pool(
        P0,
        mana.iter()
            .enumerate()
            .map(|(index, &color)| {
                ManaUnit::new(color, ObjectId(9000 + index as u64), false, vec![])
            })
            .collect(),
    );
    scenario
}

fn rex(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_hand_from_oracle(P0, "Titanoth Rex", 11, 11, TITANOTH_REX)
        .id()
}

fn leyline(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_enchantment_from_oracle(P1, "Leyline of the Void", LEYLINE_OF_THE_VOID)
        .id()
}

fn rest_in_peace(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_enchantment_from_oracle(P0, "Rest in Peace", REST_IN_PEACE)
        .id()
}

fn friendly_creatures(scenario: &mut GameScenario) -> [ObjectId; 2] {
    [
        scenario.add_creature(P0, "First creature", 2, 2).id(),
        scenario.add_creature(P0, "Second creature", 2, 2).id(),
    ]
}

fn opposing_creature(scenario: &mut GameScenario) -> ObjectId {
    scenario.add_creature(P1, "Opposing creature", 2, 2).id()
}

fn act(runner: &mut GameRunner, action: GameAction) -> Vec<GameEvent> {
    runner.act(action).expect("public action succeeds").events
}

fn activate_cycle(runner: &mut GameRunner, source: ObjectId) -> Vec<GameEvent> {
    let (actions, _, _) = legal_actions_full(runner.state());
    let offered: Vec<_> = actions
        .into_iter()
        .filter(|action| matches!(action, GameAction::ActivateAbility { source_id, ability_index, .. }
            if *source_id == source
                && runner.state().objects[&source].abilities[*ability_index].ability_tag == Some(AbilityTag::Cycling)))
        .collect();
    let [GameAction::ActivateAbility { ability_index, .. }] = offered.as_slice() else {
        panic!("expected exactly one cycling activation for {source:?}: {offered:?}");
    };
    act(
        runner,
        GameAction::ActivateAbility {
            source_id: source,
            ability_index: *ability_index,
            payment_mode: CastPaymentMode::Auto,
        },
    )
}

fn priority(runner: &GameRunner) {
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ),
        "expected P0 priority, got {:?}",
        runner.state().waiting_for
    );
}

fn passes(runner: &mut GameRunner) {
    priority(runner);
    act(runner, GameAction::PassPriority);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ));
    act(runner, GameAction::PassPriority);
}

fn cycle_events(events: &[GameEvent]) -> Vec<(engine::types::player::PlayerId, ObjectId)> {
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

fn discard_count(events: &[GameEvent], card: ObjectId) -> usize {
    events
        .iter()
        .filter(
            |event| matches!(event, GameEvent::Discarded { object_id, .. } if *object_id == card),
        )
        .count()
}

fn zone(runner: &GameRunner, card: ObjectId) -> Zone {
    runner.state().objects[&card].zone
}

fn assert_undrawn(runner: &GameRunner, draw: ObjectId) {
    assert_eq!(runner.state().players[0].library[0], draw);
    assert!(!runner.state().players[0].hand.contains(&draw));
}

fn assert_draw_completed(runner: &GameRunner, draw: ObjectId, hand_size: usize) {
    priority(runner);
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().players[0].hand.len(), hand_size);
    assert!(runner.state().players[0].hand.contains(&draw));
}

fn choose_rex_target(
    runner: &mut GameRunner,
    source: ObjectId,
    creatures: [ObjectId; 2],
) -> ObjectId {
    // CR 702.29c: The triggered ability is delivered from the cycled card's zone.
    let WaitingFor::TriggerTargetSelection {
        player: P0,
        target_slots,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected Rex target prompt, got {:?}; stack: {:?}",
            runner.state().waiting_for,
            runner.state().stack
        );
    };
    let [slot] = target_slots.as_slice() else {
        panic!("expected one target slot: {target_slots:?}");
    };
    assert_eq!(slot.legal_targets.len(), 2);
    for creature in creatures {
        assert!(slot.legal_targets.contains(&TargetRef::Object(creature)));
    }
    let top = runner
        .state()
        .stack
        .last()
        .expect("targeted trigger on stack");
    assert!(matches!(top.kind, StackEntryKind::TriggeredAbility { .. }));
    assert_eq!(top.source_id, source);
    assert_eq!(top.controller, P0);
    let TargetRef::Object(chosen) = slot.legal_targets[1] else {
        unreachable!("both targets were checked to be creatures");
    };
    act(
        runner,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(chosen)),
        },
    );
    chosen
}

fn trample_counters(runner: &GameRunner, creature: ObjectId) -> u32 {
    runner.state().objects[&creature]
        .counters
        .get(&CounterType::Keyword(KeywordKind::Trample))
        .copied()
        .unwrap_or_default()
}

fn assert_rex_counters(
    runner: &GameRunner,
    creatures: [ObjectId; 2],
    opposing: ObjectId,
    chosen: ObjectId,
) {
    // CR 122.1b: The chosen creature receives the trample counter.
    for creature in creatures.into_iter().chain([opposing]) {
        assert_eq!(
            trample_counters(runner, creature),
            u32::from(creature == chosen)
        );
    }
}

fn assert_cycling_bottom(runner: &GameRunner, source: ObjectId) {
    let entry = runner
        .state()
        .stack
        .front()
        .expect("cycling ability on stack");
    assert!(matches!(
        entry.kind,
        StackEntryKind::ActivatedAbility { .. }
    ));
    assert_eq!(entry.source_id, source);
}

fn resolve_rex_and_draw(
    runner: &mut GameRunner,
    source: ObjectId,
    creatures: [ObjectId; 2],
    opposing: ObjectId,
    draw: ObjectId,
    hand_size: usize,
) {
    let chosen = choose_rex_target(runner, source, creatures);
    assert_eq!(runner.state().stack.len(), 2);
    assert_cycling_bottom(runner, source);
    assert_undrawn(runner, draw);
    passes(runner);
    assert_rex_counters(runner, creatures, opposing, chosen);
    assert_eq!(runner.state().stack.len(), 1);
    assert_undrawn(runner, draw);
    passes(runner);
    assert_draw_completed(runner, draw, hand_size);
}

fn cycle_rex_to(destination: Zone) {
    let mut scenario = scenario(&[ManaType::Green, ManaType::Colorless]);
    if destination == Zone::Exile {
        leyline(&mut scenario);
    }
    let source = rex(&mut scenario);
    let creatures = friendly_creatures(&mut scenario);
    let opposing = opposing_creature(&mut scenario);
    let mut runner = scenario.build();
    let draw = runner.state().players[0].library[0];
    let events = activate_cycle(&mut runner, source);
    // CR 701.9a + CR 614.6: The discard settles in the graveyard or the replacement destination.
    assert_eq!(zone(&runner, source), destination);
    assert_eq!(
        runner.state().players[0].graveyard.contains(&source),
        destination == Zone::Graveyard
    );
    assert_eq!(discard_count(&events, source), 1);
    assert_eq!(cycle_events(&events), [(P0, source)]);
    resolve_rex_and_draw(&mut runner, source, creatures, opposing, draw, 1);
}

#[test]
fn titanoth_rex_cycled_under_leyline_of_the_void_triggers_from_exile() {
    cycle_rex_to(Zone::Exile);
}

#[test]
fn titanoth_rex_cycled_without_replacement_triggers_from_graveyard() {
    cycle_rex_to(Zone::Graveyard);
}

#[test]
fn fractured_sanity_cycled_under_leyline_of_the_void_mills_four() {
    let mut scenario = scenario(&[ManaType::Blue, ManaType::Colorless]);
    leyline(&mut scenario);
    scenario.with_library_top(P1, &["One", "Two", "Three", "Four", "Five"]);
    let card = scenario
        .add_spell_to_hand_from_oracle(P0, "Fractured Sanity", false, FRACTURED_SANITY)
        .id();
    let mut runner = scenario.build();
    let draw = runner.state().players[0].library[0];
    let library = runner.state().players[1].library.clone();
    activate_cycle(&mut runner, card);
    assert_eq!(zone(&runner, card), Zone::Exile);
    while !runner.state().stack.is_empty() {
        passes(&mut runner);
    }
    assert_draw_completed(&runner, draw, 1);
    // CR 701.17a: Mill moves the top four cards of the opponent's library to their graveyard.
    assert!(runner.state().players[1]
        .library
        .iter()
        .eq(library.iter().skip(4)));
    let graveyard = &runner.state().players[1].graveyard;
    assert_eq!(graveyard.len(), 4);
    assert!(library.iter().take(4).all(|id| graveyard.contains(id)));
}

#[test]
fn shark_typhoon_cycled_under_rest_in_peace_reads_x() {
    let mut scenario = scenario(&[ManaType::Blue; 4]);
    rest_in_peace(&mut scenario);
    let card = scenario
        .add_enchantment_to_hand_from_oracle(P0, "Shark Typhoon", SHARK_TYPHOON)
        .id();
    let mut runner = scenario.build();
    let draw = runner.state().players[0].library[0];
    activate_cycle(&mut runner, card);
    // CR 107.3a: The activating player announces X.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ChooseXValue { player: P0, min, max, .. }
        if min <= 2 && 2 <= max),
        "expected X prompt with 2 in range: {:?}",
        runner.state().waiting_for
    );
    act(&mut runner, GameAction::ChooseX { value: 2 });
    while matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
        act(&mut runner, GameAction::PassPriority);
    }
    priority(&runner);
    assert_eq!(zone(&runner, card), Zone::Exile);
    while !runner.state().stack.is_empty() {
        passes(&mut runner);
    }
    assert_draw_completed(&runner, draw, 1);
    let sharks: Vec<_> = runner
        .state()
        .objects
        .values()
        .filter(|object| {
            object.zone == Zone::Battlefield && object.is_token && object.name == "Shark"
        })
        .collect();
    // CR 107.3i: The self-cycle trigger uses the announced X.
    let [shark] = sharks.as_slice() else {
        panic!("expected one Shark, got {}", sharks.len())
    };
    assert_eq!(
        (shark.controller, shark.power, shark.toughness),
        (P0, Some(2), Some(2))
    );
}

fn replacement_choice(index: usize) {
    let mut scenario = scenario(&[ManaType::Green, ManaType::Colorless]);
    let leyline = leyline(&mut scenario);
    let rest = rest_in_peace(&mut scenario);
    let source = rex(&mut scenario);
    let creatures = friendly_creatures(&mut scenario);
    let opposing = opposing_creature(&mut scenario);
    let mut runner = scenario.build();
    let draw = runner.state().players[0].library[0];
    let events = activate_cycle(&mut runner, source);
    // CR 616.1: The owner chooses between the applicable replacements.
    let WaitingFor::ReplacementChoice {
        player: P0,
        candidate_count: 2,
        candidates,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected replacement choice: {:?}",
            runner.state().waiting_for
        );
    };
    let offered: Vec<_> = candidates
        .iter()
        .map(|candidate| candidate.source_id)
        .collect();
    assert_eq!(offered.len(), 2);
    assert!(
        offered.contains(&leyline) && offered.contains(&rest),
        "{offered:?}"
    );
    assert_eq!(zone(&runner, source), Zone::Hand);
    assert!(cycle_events(&events).is_empty());
    assert!(runner.state().stack.is_empty());

    let chosen = offered[index];
    let events = act(&mut runner, GameAction::ChooseReplacement { index });
    let applied: Vec<_> = events
        .iter()
        .enumerate()
        .filter_map(|(position, event)| match event {
            GameEvent::ReplacementApplied { source_id, .. } if offered.contains(source_id) => {
                Some((position, *source_id))
            }
            _ => None,
        })
        .collect();
    // CR 616.1e + CR 616.1f: Only the chosen replacement applies to this move.
    let [(applied_at, applied_source)] = applied.as_slice() else {
        panic!("expected one replacement after choice: {events:?}");
    };
    assert_eq!(*applied_source, chosen);
    assert_eq!(zone(&runner, source), Zone::Exile);
    assert_eq!(cycle_events(&events), [(P0, source)]);
    let moved_at = events.iter().position(|event| matches!(event, GameEvent::ZoneChanged { object_id, to: Zone::Exile, .. } if *object_id == source)).expect("Rex moved to exile");
    let cycled_at = events
        .iter()
        .position(|event| matches!(event, GameEvent::Cycled { .. }))
        .expect("Rex cycled");
    assert!(*applied_at < moved_at && moved_at < cycled_at, "{events:?}");
    resolve_rex_and_draw(&mut runner, source, creatures, opposing, draw, 1);
}

#[test]
fn titanoth_rex_under_rest_in_peace_and_leyline_orders_the_replacements_then_triggers_index_0() {
    replacement_choice(0);
}

#[test]
fn titanoth_rex_under_rest_in_peace_and_leyline_orders_the_replacements_then_triggers_index_1() {
    replacement_choice(1);
}

#[test]
fn exiled_titanoth_rex_does_not_trigger_when_another_copy_is_cycled() {
    let mut scenario = scenario(&[
        ManaType::Green,
        ManaType::Colorless,
        ManaType::Green,
        ManaType::Colorless,
    ]);
    leyline(&mut scenario);
    let first = rex(&mut scenario);
    let second = rex(&mut scenario);
    let creatures = friendly_creatures(&mut scenario);
    let opposing = opposing_creature(&mut scenario);
    let mut runner = scenario.build();
    let draw = runner.state().players[0].library[0];
    let events = activate_cycle(&mut runner, first);
    assert_eq!(zone(&runner, first), Zone::Exile);
    assert_eq!(cycle_events(&events), [(P0, first)]);
    resolve_rex_and_draw(&mut runner, first, creatures, opposing, draw, 2);

    let events = activate_cycle(&mut runner, second);
    assert_eq!(zone(&runner, first), Zone::Exile);
    assert_eq!(zone(&runner, second), Zone::Exile);
    assert_eq!(cycle_events(&events), [(P0, second)]);
    // CR 702.29c: “this card” binds to the second cycled object, despite the shared name.
    let sources: Vec<_> = runner
        .state()
        .stack
        .iter()
        .filter(|entry| matches!(entry.kind, StackEntryKind::TriggeredAbility { .. }))
        .map(|entry| entry.source_id)
        .collect();
    assert_eq!(sources, [second]);
    assert_eq!(runner.state().stack.len(), 2);
    assert_cycling_bottom(&runner, second);
    choose_rex_target(&mut runner, second, creatures);
    passes(&mut runner);
    passes(&mut runner);
    priority(&runner);
    assert!(runner.state().stack.is_empty());
}

fn decline_drake_haven(runner: &mut GameRunner, haven: ObjectId) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::OptionalEffectChoice { player: P0, source_id, .. }
        if source_id == haven),
        "{:?}",
        runner.state().waiting_for
    );
    act(runner, GameAction::DecideOptionalEffect { accept: false });
    assert!(!runner
        .state()
        .objects
        .values()
        .any(|object| object.is_token && object.name == "Drake"));
}

fn order_self_cycle_and_drake_haven(with_targets: bool, order: [usize; 2]) {
    let mut scenario = scenario(&[ManaType::Green, ManaType::Colorless, ManaType::Colorless]);
    leyline(&mut scenario);
    let source = rex(&mut scenario);
    let haven = scenario
        .add_enchantment_from_oracle(P0, "Drake Haven", DRAKE_HAVEN)
        .id();
    let opposing = opposing_creature(&mut scenario);
    let creatures = with_targets.then(|| friendly_creatures(&mut scenario));
    let mut runner = scenario.build();
    let draw = runner.state().players[0].library[0];
    let events = activate_cycle(&mut runner, source);
    assert_eq!(zone(&runner, source), Zone::Exile);
    assert_eq!(cycle_events(&events), [(P0, source)]);

    // CR 603.3b + CR 702.29d: P0 orders Rex's trigger and one Drake Haven trigger.
    let WaitingFor::OrderTriggers {
        player: P0,
        triggers,
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected trigger ordering for {source:?} and {haven:?}, got {:?}; stack: {:?}",
            runner.state().waiting_for,
            runner.state().stack
        );
    };
    let prompt_sources: Vec<_> = triggers.iter().map(|trigger| trigger.source_id).collect();
    assert_eq!(prompt_sources.len(), 2, "{prompt_sources:?}");
    assert!(
        prompt_sources.contains(&source) && prompt_sources.contains(&haven),
        "{prompt_sources:?}"
    );
    act(
        &mut runner,
        GameAction::OrderTriggers {
            order: order.to_vec(),
        },
    );

    if let Some(creatures) = creatures {
        let chosen = choose_rex_target(&mut runner, source, creatures);
        priority(&runner);
        assert_cycling_bottom(&runner, source);
        let stack = &runner.state().stack;
        assert_eq!(stack.len(), 3);
        assert!(stack
            .iter()
            .skip(1)
            .all(|entry| matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })));
        let sources: Vec<_> = stack.iter().skip(1).map(|entry| entry.source_id).collect();
        assert_eq!(
            sources,
            order.map(|index| prompt_sources[index]),
            "prompt {prompt_sources:?}, submitted {order:?}"
        );
        for top in sources.into_iter().rev() {
            passes(&mut runner);
            if top == haven {
                decline_drake_haven(&mut runner, haven);
            } else {
                assert_rex_counters(&runner, creatures, opposing, chosen);
            }
        }
    } else {
        // CR 603.3d: The observed Rex trigger is removed when no legal target can be chosen.
        priority(&runner);
        assert_eq!(runner.state().stack.len(), 2);
        assert_cycling_bottom(&runner, source);
        let top = runner.state().stack.back().unwrap();
        assert!(matches!(top.kind, StackEntryKind::TriggeredAbility { .. }));
        assert_eq!(top.source_id, haven);
        let state = runner.state();
        assert!(!state
            .deferred_triggers
            .iter()
            .chain(
                state
                    .pending_trigger_order
                    .iter()
                    .flat_map(|order| &order.groups)
                    .flat_map(|group| &group.triggers)
            )
            .any(|context| context.pending.source_id == source));
        assert!(!state
            .pending_trigger
            .as_ref()
            .is_some_and(|pending| pending.source_id == source));
        passes(&mut runner);
        decline_drake_haven(&mut runner, haven);
        assert_eq!(trample_counters(&runner, opposing), 0);
    }
    assert_undrawn(&runner, draw);
    passes(&mut runner);
    assert_draw_completed(&runner, draw, 1);
    assert_eq!(zone(&runner, source), Zone::Exile);
}

#[test]
fn titanoth_rex_trigger_with_no_legal_target_is_removed_at_placement() {
    for order in [[0, 1], [1, 0]] {
        order_self_cycle_and_drake_haven(false, order);
    }
}

#[test]
fn titanoth_rex_trigger_ordered_beside_drake_haven_is_placed_when_a_target_exists() {
    for order in [[0, 1], [1, 0]] {
        order_self_cycle_and_drake_haven(true, order);
    }
}

#[test]
fn yidaro_cycled_under_leyline_of_the_void_stays_in_exile() {
    let mut scenario = scenario(&[ManaType::Red, ManaType::Colorless]);
    leyline(&mut scenario);
    let source = scenario
        .add_creature_to_hand_from_oracle(P0, "Yidaro, Wandering Monster", 8, 8, YIDARO)
        .id();
    let mut runner = scenario.build();
    let draw = runner.state().players[0].library[0];
    let events = activate_cycle(&mut runner, source);
    assert_eq!(cycle_events(&events), [(P0, source)]);
    assert_eq!(zone(&runner, source), Zone::Exile);
    assert_eq!(
        runner.state().cycles_completed_this_game_by_player[&P0].len(),
        1
    );
    while !runner.state().stack.is_empty() {
        passes(&mut runner);
    }
    assert_eq!(zone(&runner, source), Zone::Exile);
    assert_draw_completed(&runner, draw, 1);
}
