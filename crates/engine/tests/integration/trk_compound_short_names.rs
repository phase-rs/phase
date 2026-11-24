//! TRK compound printed short names (CR 201.5c).
//!
//! Captain James T. Kirk and Captain Kathryn Janeway use first-and-last-word
//! self-references in their printed Oracle text. These tests use the verbatim
//! Oracle through `parse_oracle_text` and the live scenario pipeline so reverting
//! the shared normalizer makes both parser shape and gameplay fail.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, ControllerRef, Effect, EffectKind, FilterProp, ModalSelectionConstraint,
    TargetFilter, TypeFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const KIRK_ORACLE: &str = "Whenever Captain Kirk enters or attacks, choose one. If you have no cards in hand, choose one or more instead.\n• Discard a card, then draw a card.\n• Create a 1/1 red Officer creature token.\n• Creatures you control get +1/+0 until end of turn.";

const JANEWAY_ORACLE: &str = "You may play an additional land on each of your turns.\nWhenever Captain Janeway or another creature you control enters, that creature explores. (Reveal the top card of your library. Put that card into your hand if it's a land. Otherwise, put a +1/+1 counter on the creature, then put the card back or put it into your graveyard.)";

/// Unsummon {U} Instant — verbatim Oracle text.
const UNSUMMON: &str = "Return target creature to its owner's hand.";

fn ability_contains_explore(ability: &AbilityDefinition) -> bool {
    matches!(*ability.effect, Effect::Explore)
        || ability
            .sub_ability
            .as_deref()
            .is_some_and(ability_contains_explore)
        || ability
            .else_ability
            .as_deref()
            .is_some_and(ability_contains_explore)
        || ability.mode_abilities.iter().any(ability_contains_explore)
}

#[test]
fn kirk_full_oracle_parses_modal_enters_or_attacks_trigger() {
    let parsed = parse_oracle_text(
        KIRK_ORACLE,
        "Captain James T. Kirk",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Soldier".to_string()],
    );
    let trigger = parsed
        .triggers
        .iter()
        .find(|trigger| trigger.mode == TriggerMode::EntersOrAttacks)
        .expect("Captain Kirk must parse as one EntersOrAttacks trigger");
    assert_eq!(trigger.valid_card, Some(TargetFilter::SelfRef));
    let execute = trigger.execute.as_ref().expect("Kirk trigger execute");
    let modal = execute.modal.as_ref().expect("Kirk trigger modal");
    assert_eq!((modal.min_choices, modal.max_choices), (1, 1));
    assert_eq!(modal.mode_count, 3);
    assert_eq!(execute.mode_abilities.len(), 3);
    assert!(modal.constraints.iter().any(|constraint| matches!(
        constraint,
        ModalSelectionConstraint::ConditionalMaxChoices {
            max_choices: 3,
            otherwise_max_choices: 1,
            ..
        }
    )));
    assert!(matches!(
        *execute.mode_abilities[0].effect,
        Effect::Discard { .. }
    ));
    assert!(matches!(
        *execute.mode_abilities[1].effect,
        Effect::Token { .. }
    ));
    assert!(matches!(
        *execute.mode_abilities[2].effect,
        Effect::PumpAll { .. }
    ));
    assert!(
        !format!("{parsed:?}").contains("Unimplemented"),
        "verbatim Kirk Oracle must contain no partial-support placeholder"
    );
}

#[test]
fn janeway_full_oracle_parses_compound_subject_explore() {
    let parsed = parse_oracle_text(
        JANEWAY_ORACLE,
        "Captain Kathryn Janeway",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Scientist".to_string()],
    );
    let trigger = parsed
        .triggers
        .iter()
        .find(|trigger| trigger.mode == TriggerMode::ChangesZone)
        .expect("Captain Janeway must parse as an enters trigger");
    let TargetFilter::Or { filters } = trigger
        .valid_card
        .as_ref()
        .expect("Janeway trigger subject")
    else {
        panic!("Janeway trigger must retain its compound subject");
    };
    assert!(filters.contains(&TargetFilter::SelfRef));
    assert!(filters.iter().any(|filter| matches!(
        filter,
        TargetFilter::Typed(typed)
            if typed.type_filters.contains(&TypeFilter::Creature)
                && typed.controller == Some(ControllerRef::You)
                && typed.properties.contains(&FilterProp::Another)
    )));
    assert!(
        trigger
            .execute
            .as_deref()
            .is_some_and(ability_contains_explore),
        "Janeway's compound-subject trigger must execute Explore"
    );
    assert!(
        !format!("{parsed:?}").contains("Unimplemented"),
        "verbatim Janeway Oracle must contain no partial-support placeholder"
    );
}

fn drive_to_kirk_modal(runner: &mut GameRunner) -> (usize, usize, usize) {
    for _ in 0..128 {
        match runner.state().waiting_for.clone() {
            WaitingFor::AbilityModeChoice { modal, .. } => {
                return (modal.min_choices, modal.max_choices, modal.mode_count);
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority while driving Kirk trigger");
            }
            other => panic!("unexpected prompt while driving Kirk trigger: {other:?}"),
        }
    }
    panic!("Kirk trigger did not reach AbilityModeChoice");
}

fn add_kirk_to_hand(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_hand_from_oracle(P0, "Captain James T. Kirk", 3, 3, KIRK_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id()
}

#[test]
fn kirk_enters_branch_offers_the_live_modal_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_to_hand(P0, "Spare Card", 1, 1);
    let kirk = add_kirk_to_hand(&mut scenario);
    let mut runner = scenario.build();

    let card_id = runner.state().objects[&kirk].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: kirk,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Kirk");

    assert_eq!(
        drive_to_kirk_modal(&mut runner),
        (1, 1, 3),
        "Kirk entering with a card in hand must offer exactly one of three modes"
    );
}

#[test]
fn kirk_attacks_branch_offers_the_same_live_modal_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_to_hand(P0, "Spare Card", 1, 1);
    let kirk = scenario
        .add_creature_from_oracle(P0, "Captain James T. Kirk", 3, 3, KIRK_ORACLE)
        .id();
    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(kirk, AttackTarget::Player(P1))])
        .expect("declare Kirk as an attacker");

    assert_eq!(
        drive_to_kirk_modal(&mut runner),
        (1, 1, 3),
        "Kirk attacking must route through the same EntersOrAttacks modal"
    );
}

#[test]
fn janeway_other_creature_entry_explores_that_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let nonland = scenario
        .add_spell_to_library_top(P0, "Explore Probe", true)
        .id();
    let janeway = scenario
        .add_creature_from_oracle(P0, "Captain Kathryn Janeway", 2, 3, JANEWAY_ORACLE)
        .id();
    let explorer = scenario.add_creature_to_hand(P0, "Away Team", 2, 2).id();
    let mut runner = scenario.build();

    runner.cast(explorer).resolve();

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::DigChoice { ref cards, .. } if cards == &vec![nonland]
    ));
    runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("put the explored nonland into the graveyard");

    assert_eq!(runner.state().objects[&nonland].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&explorer]
            .counters
            .values()
            .copied()
            .sum::<u32>(),
        1,
        "the entering Away Team must receive the explore counter"
    );
    assert_eq!(
        runner.state().objects[&janeway]
            .counters
            .values()
            .copied()
            .sum::<u32>(),
        0,
        "Janeway must not receive Away Team's explore counter"
    );
}

// ---------------------------------------------------------------------------
// Departed-entrant row (issue #10021, impl-review F1): the "another creature"
// entrant leaves in response to Janeway's trigger and still explores through
// LKI (CR 701.44c) instead of Janeway exploring herself.
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

/// CR 701.44a + CR 701.44c (nonland branch): P0 controls Captain Kathryn
/// Janeway and casts Grizzly Bears; with Janeway's trigger on the stack, P1
/// casts Unsummon on the Bears; Lightning Bolt is on top of P0's library.
/// The departed Bears still explore through LKI — P0 reveals — no +1/+1
/// counter lands on Janeway or anywhere else, and P0 may put the Bolt into
/// the graveyard.
///
/// GUARD (issue #10021, impl-review F1): Janeway parses as a
/// `TargetOnly { ParentTarget }` head with a bare `Explore` child. The
/// departed bear's identity reaches the explore through the push-time
/// parent-target seeding (targets plus the arrival pin, both surviving to
/// resolution), so the completion names the departed bear — never Janeway —
/// and no counter lands anywhere. Green at base; it pins the correct
/// behavior while the stolen-subject controller question (the surviving
/// targets shadow the LKI pin, so a thief's library is never read) stays
/// open as a separate defect.
#[test]
fn janeway_bounced_other_creature_still_explores_without_counter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let janeway = scenario
        .add_creature_from_oracle(P0, "Captain Kathryn Janeway", 2, 3, JANEWAY_ORACLE)
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
    pass_until_trigger_on_stack(&mut runner, &mut events, janeway);
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
    // (not Janeway) and its controller revealed.
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
    // ... and no counter landed anywhere — in particular Janeway did not
    // explore herself.
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, GameEvent::CounterAdded { .. })),
        "no counter may be placed for a departed explorer, events: {events:?}"
    );
    assert_all_plus1_zero(&runner, "a departed explorer puts no counter on anything");
    assert_eq!(
        plus1(&runner, janeway),
        0,
        "Captain Kathryn Janeway must not explore herself"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty(),
        "resolution must settle after the disposition, got {:?}",
        runner.state().waiting_for
    );
}
