//! Path of Discovery {3}{G} Enchantment — "Whenever a creature you control
//! enters, it explores."
//!
//! CR 603.6a: each time an event puts one or more permanents onto the
//! battlefield, every permanent on the battlefield is checked for matching
//! enters-the-battlefield triggers. CR 701.44a: to explore, the exploring
//! permanent's controller reveals the top card of their library; a revealed
//! land goes to hand, otherwise a +1/+1 counter goes on the exploring
//! permanent and the player may put the revealed card into their graveyard.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{EffectKind, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Path of Discovery {3}{G} Enchantment — verbatim Oracle text.
const PATH_OF_DISCOVERY: &str = "Whenever a creature you control enters, it explores.";

/// Unsummon {U} Instant — verbatim Oracle text.
const UNSUMMON: &str = "Return target creature to its owner's hand.";

/// Rubinia Soulsinger {2}{U}{U} Legendary Creature — Human Wizard 2/3 —
/// verbatim Oracle text.
const RUBINIA: &str = "You may choose not to untap Rubinia Soulsinger during your untap step.\n{T}: Gain control of target creature for as long as you control Rubinia Soulsinger and Rubinia Soulsinger remains tapped.";

/// Reality Ripple {U} Instant — verbatim Oracle text.
const REALITY_RIPPLE: &str = "Target artifact, creature, or land phases out. (While it's phased \
out, it's treated as though it doesn't exist. It phases in before its controller untaps during \
their next untap step.)";

/// Merfolk Branchwalker {1}{G} Creature — Merfolk Scout 2/1 — verbatim Oracle
/// text.
const MERFOLK_BRANCHWALKER: &str = "When this creature enters, it explores. (Reveal the top card of your library. Put that card into your hand if it's a land. Otherwise, put a +1/+1 counter on this creature, then put the card back or put it into your graveyard.)";

fn board() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Path of Discovery", PATH_OF_DISCOVERY);
    scenario
}

/// CR 701.44a (land branch): the entering creature explores, the revealed
/// land is put into its controller's hand, and no counter is placed.
#[test]
fn path_of_discovery_explore_puts_revealed_land_into_hand() {
    let mut scenario = board();
    let land = scenario.add_land_to_library_top(P0, "Forest").id();
    let bear = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(bear).resolve();

    assert_eq!(
        outcome.zone_of(bear),
        Zone::Battlefield,
        "reach: the cast creature entered the battlefield"
    );
    assert_eq!(
        outcome.zone_of(land),
        Zone::Hand,
        "explore must put the revealed land into its controller's hand"
    );
    assert_eq!(
        outcome.counters(bear, CounterType::Plus1Plus1),
        0,
        "a land reveal puts no counter on the exploring creature"
    );
    // DISCRIMINATING: the explore completion names the entering creature as
    // the explorer. Pre-fix the chain fell back to the trigger source, so
    // this named Path of Discovery instead of the bear.
    assert!(
        outcome.events().iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Explore,
                source_id,
                ..
            } if *source_id == bear
        )),
        "the explore completion must name the entering creature, events: {:?}",
        outcome.events()
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the trigger must resolve fully, got {:?}",
        outcome.final_waiting_for()
    );
    assert!(
        outcome.state().stack.is_empty(),
        "the stack must be empty after the trigger resolves"
    );
}

/// CR 701.44a (nonland branch): the entering creature explores, gets a
/// +1/+1 counter, and its controller chooses the revealed card's disposition.
#[test]
fn path_of_discovery_explore_counters_on_nonland_then_graveyard_choice() {
    let mut scenario = board();
    let bolt = scenario
        .add_spell_to_library_top(P0, "Lightning Bolt", true)
        .id();
    let bear = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(bear).resolve();

    assert_eq!(
        outcome.zone_of(bear),
        Zone::Battlefield,
        "reach: the cast creature entered the battlefield"
    );
    assert_eq!(
        outcome.counters(bear, CounterType::Plus1Plus1),
        1,
        "a nonland reveal must put a +1/+1 counter on the exploring creature"
    );
    match outcome.final_waiting_for() {
        WaitingFor::DigChoice { cards, player, .. } => {
            assert_eq!(*player, P0);
            assert_eq!(
                cards.as_slice(),
                &[bolt],
                "explore must offer the revealed card's disposition"
            );
        }
        other => panic!("expected the explore DigChoice, got {other:?}"),
    }

    // CR 701.44a: the player may put the revealed card into their graveyard.
    runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("declining the put-back is legal");
    let state = runner.state();
    assert_eq!(state.objects[&bolt].zone, Zone::Graveyard);
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }) && state.stack.is_empty(),
        "resolution must settle after the disposition, got {:?}",
        state.waiting_for
    );
}

// ---------------------------------------------------------------------------
// Departed-entrant rows (issue #10021, finding F1): the entrant leaves in
// response to Path's trigger and still explores through LKI (CR 701.44c).
// ---------------------------------------------------------------------------

/// Commit a cast (answering its cast-time prompts from `targets`) and collect
/// the commit events, leaving the spell on the stack for manual driving.
fn commit_cast(
    runner: &mut GameRunner,
    spell: ObjectId,
    targets: &[ObjectId],
    events: &mut Vec<GameEvent>,
) {
    let committed = runner.cast(spell).target_objects(targets).commit();
    events.extend(committed.events().iter().cloned());
}

/// Answer one ordering prompt with the identity order, collecting events.
fn answer_order_identity(runner: &mut GameRunner, events: &mut Vec<GameEvent>, count: usize) {
    let result = runner
        .act(GameAction::OrderTriggers {
            order: (0..count).collect(),
        })
        .expect("trigger ordering is accepted");
    events.extend(result.events);
}

/// Pass priority once as the current holder, collecting events.
fn pass_once(runner: &mut GameRunner, events: &mut Vec<GameEvent>) {
    let result = runner
        .act(GameAction::PassPriority)
        .expect("priority pass is accepted");
    events.extend(result.events);
}

/// Pass until `source`'s triggered ability is on the stack under a Priority
/// window (the responder has not acted yet).
fn pass_until_trigger_on_stack(
    runner: &mut GameRunner,
    events: &mut Vec<GameEvent>,
    source: ObjectId,
) {
    for _ in 0..16 {
        let trigger_up = runner.state().stack.iter().any(|entry| {
            entry.source_id == source
                && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
        });
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if trigger_up => return,
            WaitingFor::Priority { .. } => pass_once(runner, events),
            WaitingFor::OrderTriggers { triggers, .. } => {
                let count = triggers.len();
                answer_order_identity(runner, events, count);
            }
            other => panic!("unexpected prompt while casting: {other:?}"),
        }
    }
    panic!("the trigger never reached the stack");
}

/// Pass until `who` holds priority (the stack is untouched).
fn pass_until_player_priority(runner: &mut GameRunner, events: &mut Vec<GameEvent>, who: PlayerId) {
    for _ in 0..4 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { player } if *player == who => return,
            WaitingFor::Priority { .. } => pass_once(runner, events),
            other => panic!("lost the priority window, got {other:?}"),
        }
    }
    panic!("priority never reached {who:?}");
}

/// Pass until the stack holds exactly `len` entries under a Priority window.
fn pass_until_stack_len(runner: &mut GameRunner, events: &mut Vec<GameEvent>, len: usize) {
    for _ in 0..16 {
        if runner.state().stack.len() == len
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            return;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => pass_once(runner, events),
            WaitingFor::OrderTriggers { triggers, .. } => {
                let count = triggers.len();
                answer_order_identity(runner, events, count);
            }
            other => panic!("unexpected prompt while settling the stack: {other:?}"),
        }
    }
    panic!("the stack never reached {len} entries");
}

/// Pass until the explore's DigChoice parks (answering ordering on the way).
fn pass_until_digchoice(runner: &mut GameRunner, events: &mut Vec<GameEvent>) {
    for _ in 0..16 {
        match &runner.state().waiting_for {
            WaitingFor::DigChoice { .. } => return,
            WaitingFor::Priority { .. } => pass_once(runner, events),
            WaitingFor::OrderTriggers { triggers, .. } => {
                let count = triggers.len();
                answer_order_identity(runner, events, count);
            }
            other => panic!("unexpected prompt while resolving the explore: {other:?}"),
        }
    }
    panic!("the explore DigChoice never arrived");
}

/// Pass until Priority with an empty stack (no prompt may park).
fn pass_until_settled(runner: &mut GameRunner, events: &mut Vec<GameEvent>) {
    for _ in 0..24 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            return;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => pass_once(runner, events),
            WaitingFor::OrderTriggers { triggers, .. } => {
                let count = triggers.len();
                answer_order_identity(runner, events, count);
            }
            other => panic!("unexpected prompt while settling: {other:?}"),
        }
    }
    panic!("resolution never settled");
}

/// Put `source`'s ability `index` on the stack targeting `target` and stop
/// there, so the test can interleave further responses before it resolves.
fn activate_onto_stack(
    runner: &mut GameRunner,
    events: &mut Vec<GameEvent>,
    source: ObjectId,
    index: usize,
    target: ObjectId,
) {
    let result = runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activation is accepted");
    events.extend(result.events);
    for _ in 0..8 {
        match &runner.state().waiting_for {
            WaitingFor::TargetSelection { .. } => {
                let result = runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("the target is accepted");
                events.extend(result.events);
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected prompt while activating: {other:?}"),
        }
    }
    assert!(
        !runner.state().stack.is_empty(),
        "the ability is on the stack, not resolved"
    );
}

/// Assert the explore DigChoice unconditionally: actor plus full options.
fn assert_digchoice(runner: &GameRunner, player: PlayerId, options: &[ObjectId], why: &str) {
    match &runner.state().waiting_for {
        WaitingFor::DigChoice {
            player: actual,
            cards,
            selectable_cards,
            ..
        } => {
            assert_eq!(*actual, player, "{why} (actor)");
            assert_eq!(cards.as_slice(), options, "{why} (options)");
            assert_eq!(selectable_cards.as_slice(), options, "{why} (selectable)");
        }
        other => panic!("{why}: expected the explore DigChoice, got {other:?}"),
    }
}

fn plus1(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// Every object on every zone row must carry zero +1/+1 counters.
fn assert_all_plus1_zero(runner: &GameRunner, why: &str) {
    for (id, object) in runner.state().objects.iter() {
        let count = object
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0);
        assert_eq!(
            count, 0,
            "{why}: {} ({id:?}) carries {count} +1/+1 counters",
            object.name,
        );
    }
}

/// CR 701.44a + CR 701.44c (nonland branch): P0 controls Path of Discovery
/// and casts Grizzly Bears; with Path's trigger on the stack, P1 casts
/// Unsummon on the Bears; Lightning Bolt is on top of P0's library. The
/// departed Bears still explore through LKI — P0 reveals — no +1/+1 counter
/// lands on Path or anywhere else, and P0 may put the Bolt into the graveyard.
#[test]
fn path_bounced_in_response_still_explores_without_counter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let path = scenario
        .add_enchantment_from_oracle(P0, "Path of Discovery", PATH_OF_DISCOVERY)
        .id();
    let bolt = scenario
        .add_spell_to_library_top(P0, "Lightning Bolt", true)
        .id();
    let bear = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .with_mana_cost(ManaCost::zero())
        .id();
    let unsummon = scenario
        .add_spell_to_hand_from_oracle(P1, "Unsummon", true, UNSUMMON)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let mut events = Vec::new();

    commit_cast(&mut runner, bear, &[], &mut events);
    pass_until_trigger_on_stack(&mut runner, &mut events, path);
    pass_until_player_priority(&mut runner, &mut events, P1);
    commit_cast(&mut runner, unsummon, &[bear], &mut events);
    pass_until_stack_len(&mut runner, &mut events, 1);
    // REACH: the bounce resolved while the trigger waits — the bear is back in
    // P0's hand, so the negatives below cannot pass for want of a departure.
    assert_eq!(
        runner.state().objects[&bear].zone,
        Zone::Hand,
        "reach: Unsummon returned the bear to hand"
    );
    assert!(
        runner.state().players[P0.0 as usize].hand.contains(&bear),
        "reach: the bear is in its owner's hand"
    );

    pass_until_digchoice(&mut runner, &mut events);
    assert_digchoice(
        &runner,
        P0,
        &[bolt],
        "the departed bear's controller must get the explore choice",
    );
    // CR 701.44a: decline the put-back; the Bolt goes to the graveyard.
    let result = runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("declining the put-back is legal");
    events.extend(result.events);
    assert_eq!(
        runner.state().objects[&bolt].zone,
        Zone::Graveyard,
        "the declined card must go to the graveyard"
    );

    // CR 701.44c: the departed bear explored — the completion names the bear
    // (not Path) and its controller revealed.
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Explore,
                source_id,
                ..
            } if *source_id == bear
        )),
        "the explore completion must name the departed bear, events: {events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::CardsRevealed { player, card_ids, .. }
                if *player == P0 && card_ids.as_slice() == [bolt]
        )),
        "P0 must reveal the Bolt, events: {events:?}"
    );
    // ... and no counter landed anywhere — in particular Path did not explore
    // itself.
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, GameEvent::CounterAdded { .. })),
        "no counter may be placed for a departed explorer, events: {events:?}"
    );
    assert_all_plus1_zero(&runner, "a departed explorer puts no counter on anything");
    assert_eq!(
        plus1(&runner, path),
        0,
        "Path of Discovery must not explore itself"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty(),
        "resolution must settle after the disposition, got {:?}",
        runner.state().waiting_for
    );
}

/// CR 701.44c (stolen hostile): as above, but P1 first steals the bear with
/// Rubinia Soulsinger, then bounces it — both in response to Path's trigger.
/// The explore uses the thief's LKI: P1 (not the owner P0) reveals from P1's
/// library, and still no counter lands anywhere.
#[test]
fn path_stolen_then_bounced_explores_through_thief_lki_without_counter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let path = scenario
        .add_enchantment_from_oracle(P0, "Path of Discovery", PATH_OF_DISCOVERY)
        .id();
    // P1's top is the Bolt; P0's top is a decoy, so reading the wrong library
    // reveals the wrong card.
    let bolt = scenario
        .add_spell_to_library_top(P1, "Lightning Bolt", true)
        .id();
    let _decoy = scenario
        .add_spell_to_library_top(P0, "Giant Growth", true)
        .id();
    let bear = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .with_mana_cost(ManaCost::zero())
        .id();
    let unsummon = scenario
        .add_spell_to_hand_from_oracle(P1, "Unsummon", true, UNSUMMON)
        .with_mana_cost(ManaCost::zero())
        .id();
    let rubinia = scenario
        .add_creature_from_oracle(P1, "Rubinia Soulsinger", 2, 3, RUBINIA)
        .id();
    let mut runner = scenario.build();
    let mut events = Vec::new();

    commit_cast(&mut runner, bear, &[], &mut events);
    pass_until_trigger_on_stack(&mut runner, &mut events, path);
    pass_until_player_priority(&mut runner, &mut events, P1);
    activate_onto_stack(&mut runner, &mut events, rubinia, 0, bear);
    pass_until_stack_len(&mut runner, &mut events, 1);
    // REACH: the theft resolved while the trigger waits.
    assert_eq!(
        runner.state().objects[&bear].controller,
        P1,
        "reach: Rubinia stole the bear"
    );
    pass_until_player_priority(&mut runner, &mut events, P1);
    commit_cast(&mut runner, unsummon, &[bear], &mut events);
    pass_until_stack_len(&mut runner, &mut events, 1);
    // REACH: the bounce resolved — the bear is back in its owner's hand.
    assert_eq!(
        runner.state().objects[&bear].zone,
        Zone::Hand,
        "reach: Unsummon returned the bear to hand"
    );
    assert!(
        runner.state().players[P0.0 as usize].hand.contains(&bear),
        "reach: the bear is in its owner's (P0's) hand"
    );

    pass_until_digchoice(&mut runner, &mut events);
    assert_digchoice(
        &runner,
        P1,
        &[bolt],
        "the thief (LKI controller) must get the explore choice",
    );
    let result = runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("declining the put-back is legal");
    events.extend(result.events);
    assert_eq!(
        runner.state().objects[&bolt].zone,
        Zone::Graveyard,
        "the declined card must go to the graveyard"
    );

    // CR 701.44c: LKI names the thief — P1 reveals P1's Bolt, and the
    // completion still names the bear.
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::CardsRevealed { player, card_ids, .. }
                if *player == P1 && card_ids.as_slice() == [bolt]
        )),
        "P1 must reveal P1's Bolt, events: {events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Explore,
                source_id,
                ..
            } if *source_id == bear
        )),
        "the explore completion must name the departed bear, events: {events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::ControllerChanged {
                object_id,
                old_controller,
                new_controller,
                ..
            } if *object_id == bear && *old_controller == P0 && *new_controller == P1
        )),
        "reach: the theft must be published, events: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, GameEvent::CounterAdded { .. })),
        "no counter may be placed for a departed explorer, events: {events:?}"
    );
    assert_all_plus1_zero(&runner, "a departed explorer puts no counter on anything");
    assert_eq!(
        plus1(&runner, path),
        0,
        "Path of Discovery must not explore itself"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty(),
        "resolution must settle after the disposition, got {:?}",
        runner.state().waiting_for
    );
}

/// CR 701.44a (land branch) + CR 701.44c: the bounced bear explores, the
/// revealed land is put into its controller's hand, and no counter is placed.
#[test]
fn path_bounced_in_response_puts_revealed_land_into_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let path = scenario
        .add_enchantment_from_oracle(P0, "Path of Discovery", PATH_OF_DISCOVERY)
        .id();
    let land = scenario.add_land_to_library_top(P0, "Forest").id();
    let bear = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .with_mana_cost(ManaCost::zero())
        .id();
    let unsummon = scenario
        .add_spell_to_hand_from_oracle(P1, "Unsummon", true, UNSUMMON)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let mut events = Vec::new();

    commit_cast(&mut runner, bear, &[], &mut events);
    pass_until_trigger_on_stack(&mut runner, &mut events, path);
    pass_until_player_priority(&mut runner, &mut events, P1);
    commit_cast(&mut runner, unsummon, &[bear], &mut events);
    pass_until_stack_len(&mut runner, &mut events, 1);
    // REACH: the bounce resolved while the trigger waits.
    assert_eq!(
        runner.state().objects[&bear].zone,
        Zone::Hand,
        "reach: Unsummon returned the bear to hand"
    );
    pass_until_settled(&mut runner, &mut events);

    // CR 701.44a: the land reveal goes to hand with no counter; CR 701.44c:
    // the completion names the departed bear.
    assert!(
        runner.state().players[P0.0 as usize].hand.contains(&land),
        "explore must put the revealed land into its controller's hand"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Explore,
                source_id,
                ..
            } if *source_id == bear
        )),
        "the explore completion must name the departed bear, events: {events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::CardsRevealed { player, card_ids, .. }
                if *player == P0 && card_ids.as_slice() == [land]
        )),
        "P0 must reveal the land, events: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, GameEvent::CounterAdded { .. })),
        "a land reveal puts no counter anywhere, events: {events:?}"
    );
    assert_all_plus1_zero(&runner, "a land reveal puts no counter on anything");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty(),
        "resolution must settle, got {:?}",
        runner.state().waiting_for
    );
}

/// CR 701.44a (empty library) + CR 701.44c: the bounced bear explores with its
/// controller's library empty — nothing is revealed, no DigChoice parks, no
/// counter lands, and the completion still names the bear.
#[test]
fn path_bounced_in_response_with_empty_library_explores_without_counter_or_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let path = scenario
        .add_enchantment_from_oracle(P0, "Path of Discovery", PATH_OF_DISCOVERY)
        .id();
    let bear = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .with_mana_cost(ManaCost::zero())
        .id();
    let unsummon = scenario
        .add_spell_to_hand_from_oracle(P1, "Unsummon", true, UNSUMMON)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    runner.state_mut().players[P0.0 as usize].library.clear();
    assert!(
        runner.state().players[P0.0 as usize].library.is_empty(),
        "reach: P0's library is empty"
    );
    let mut events = Vec::new();

    commit_cast(&mut runner, bear, &[], &mut events);
    pass_until_trigger_on_stack(&mut runner, &mut events, path);
    pass_until_player_priority(&mut runner, &mut events, P1);
    commit_cast(&mut runner, unsummon, &[bear], &mut events);
    pass_until_stack_len(&mut runner, &mut events, 1);
    // REACH: the bounce resolved while the trigger waits.
    assert_eq!(
        runner.state().objects[&bear].zone,
        Zone::Hand,
        "reach: Unsummon returned the bear to hand"
    );
    pass_until_settled(&mut runner, &mut events);

    // The explore ran (completion names the bear) with nothing to reveal.
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Explore,
                source_id,
                ..
            } if *source_id == bear
        )),
        "the explore completion must name the departed bear, events: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, GameEvent::CardsRevealed { .. })),
        "an empty library reveals nothing, events: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, GameEvent::CounterAdded { .. })),
        "no counter may be placed for a departed explorer, events: {events:?}"
    );
    assert_all_plus1_zero(&runner, "a departed explorer puts no counter on anything");
    assert_eq!(
        plus1(&runner, path),
        0,
        "Path of Discovery must not explore itself"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty(),
        "resolution must settle with no parked choice, got {:?}",
        runner.state().waiting_for
    );
}

// ---------------------------------------------------------------------------
// Phased-out explorer rows (issue #10021, finding F1): the entrant phases out
// in response to Path's trigger (or its own) and still explores (CR 701.44a +
// CR 701.44b), but CR 702.26b bars the +1/+1 counter.
// ---------------------------------------------------------------------------

/// CR 702.26b + CR 701.44a + CR 701.44c (nonland branch, pinned arm): P0
/// controls Path of Discovery and casts Grizzly Bears; with Path's trigger on
/// the stack, P1 casts Reality Ripple on the Bears; Lightning Bolt is on top
/// of P0's library. The phased-out Bears still explore — P0 reveals — but no
/// +1/+1 counter lands on the Bears, on Path, or anywhere else, and P0 may
/// put the Bolt into the graveyard.
#[test]
fn path_phased_out_in_response_explores_without_counter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let path = scenario
        .add_enchantment_from_oracle(P0, "Path of Discovery", PATH_OF_DISCOVERY)
        .id();
    let bolt = scenario
        .add_spell_to_library_top(P0, "Lightning Bolt", true)
        .id();
    let bear = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .with_mana_cost(ManaCost::zero())
        .id();
    let ripple = scenario
        .add_spell_to_hand_from_oracle(P1, "Reality Ripple", true, REALITY_RIPPLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let mut events = Vec::new();

    commit_cast(&mut runner, bear, &[], &mut events);
    pass_until_trigger_on_stack(&mut runner, &mut events, path);
    pass_until_player_priority(&mut runner, &mut events, P1);
    commit_cast(&mut runner, ripple, &[bear], &mut events);
    pass_until_stack_len(&mut runner, &mut events, 1);
    // REACH: the Ripple resolved while the trigger waits — the bear is phased
    // out yet still on the battlefield (CR 702.26d), so the negatives below
    // cannot pass for want of a phase-out. The carried pin stays current for
    // the same reason, which is exactly why the phased-out bear reaches the
    // pinned arm of the counter guard.
    assert_eq!(
        runner.state().objects[&bear].zone,
        Zone::Battlefield,
        "reach: the phased-out bear stays on the battlefield"
    );
    assert!(
        runner.state().objects[&bear].is_phased_out(),
        "reach: Reality Ripple phased the bear out"
    );

    pass_until_digchoice(&mut runner, &mut events);
    assert_digchoice(
        &runner,
        P0,
        &[bolt],
        "the phased-out bear's controller must get the explore choice",
    );
    // CR 701.44a: decline the put-back; the Bolt goes to the graveyard.
    let result = runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("declining the put-back is legal");
    events.extend(result.events);
    assert_eq!(
        runner.state().objects[&bolt].zone,
        Zone::Graveyard,
        "the declined card must go to the graveyard"
    );

    // CR 701.44b + CR 701.44c: the phased-out bear explored — the completion
    // names the bear (not Path) and its controller revealed.
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Explore,
                source_id,
                ..
            } if *source_id == bear
        )),
        "the explore completion must name the phased-out bear, events: {events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::CardsRevealed { player, card_ids, .. }
                if *player == P0 && card_ids.as_slice() == [bolt]
        )),
        "P0 must reveal the Bolt, events: {events:?}"
    );
    // CR 702.26b: ... and no counter landed anywhere — in particular neither
    // the bear nor Path received one.
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, GameEvent::CounterAdded { .. })),
        "no counter may be placed for a phased-out explorer, events: {events:?}"
    );
    assert_eq!(
        plus1(&runner, bear),
        0,
        "the phased-out bear must receive no counter"
    );
    assert_eq!(
        plus1(&runner, path),
        0,
        "Path of Discovery must not explore itself"
    );
    assert_all_plus1_zero(&runner, "a phased-out explorer puts no counter on anything");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty(),
        "resolution must settle after the disposition, got {:?}",
        runner.state().waiting_for
    );
}

/// CR 702.26b + CR 701.44a (nonland branch, unpinned arm): P0 casts Merfolk
/// Branchwalker; with its own enter-trigger on the stack, P1 casts Reality
/// Ripple on the Walker; Lightning Bolt is on top of P0's library. The bare
/// self-explore head carries no pin, so this routes through the unpinned arm
/// of the counter guard: the phased-out Walker still explores — P0 reveals —
/// but no +1/+1 counter lands anywhere.
#[test]
fn merfolk_branchwalker_phased_out_in_response_explores_without_counter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bolt = scenario
        .add_spell_to_library_top(P0, "Lightning Bolt", true)
        .id();
    let walker = scenario
        .add_creature_to_hand_from_oracle(P0, "Merfolk Branchwalker", 2, 1, MERFOLK_BRANCHWALKER)
        .with_mana_cost(ManaCost::zero())
        .id();
    let ripple = scenario
        .add_spell_to_hand_from_oracle(P1, "Reality Ripple", true, REALITY_RIPPLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let mut events = Vec::new();

    commit_cast(&mut runner, walker, &[], &mut events);
    pass_until_trigger_on_stack(&mut runner, &mut events, walker);
    pass_until_player_priority(&mut runner, &mut events, P1);
    commit_cast(&mut runner, ripple, &[walker], &mut events);
    pass_until_stack_len(&mut runner, &mut events, 1);
    // REACH: the Ripple resolved while the trigger waits — the walker is
    // phased out yet still on the battlefield (CR 702.26d).
    assert_eq!(
        runner.state().objects[&walker].zone,
        Zone::Battlefield,
        "reach: the phased-out walker stays on the battlefield"
    );
    assert!(
        runner.state().objects[&walker].is_phased_out(),
        "reach: Reality Ripple phased the walker out"
    );

    pass_until_digchoice(&mut runner, &mut events);
    assert_digchoice(
        &runner,
        P0,
        &[bolt],
        "the phased-out walker's controller must get the explore choice",
    );
    // CR 701.44a: decline the put-back; the Bolt goes to the graveyard.
    let result = runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("declining the put-back is legal");
    events.extend(result.events);
    assert_eq!(
        runner.state().objects[&bolt].zone,
        Zone::Graveyard,
        "the declined card must go to the graveyard"
    );

    // CR 701.44b: the phased-out walker explored — the completion names the
    // walker and its controller revealed.
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Explore,
                source_id,
                ..
            } if *source_id == walker
        )),
        "the explore completion must name the phased-out walker, events: {events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::CardsRevealed { player, card_ids, .. }
                if *player == P0 && card_ids.as_slice() == [bolt]
        )),
        "P0 must reveal the Bolt, events: {events:?}"
    );
    // CR 702.26b: ... and no counter landed anywhere.
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, GameEvent::CounterAdded { .. })),
        "no counter may be placed for a phased-out explorer, events: {events:?}"
    );
    assert_eq!(
        plus1(&runner, walker),
        0,
        "the phased-out walker must receive no counter"
    );
    assert_all_plus1_zero(&runner, "a phased-out explorer puts no counter on anything");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty(),
        "resolution must settle after the disposition, got {:?}",
        runner.state().waiting_for
    );
}
