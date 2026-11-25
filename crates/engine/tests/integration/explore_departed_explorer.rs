//! Departed explorers (issue #10021, finding F1): a permanent that is no longer
//! on the battlefield as an effect instructs it to explore still explores —
//! its last known information determines which object explored and who
//! controlled it (CR 701.44c), no +1/+1 counter lands on anything, and
//! "whenever a creature explores" abilities trigger as appropriate.
//!
//! T3/T3b cover self-explore (Jadelight Ranger, bounced in response, friendly
//! and stolen); T4 guards that a damage-triggered self-explore (Francisco,
//! Fowl Marauder) explores itself rather than the damaging Pirate; T5 covers
//! the observer clause via Wildgrowth Walker.

use engine::game::combat::AttackTarget;
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

/// Jadelight Ranger {1}{G}{G} Creature — Merfolk Scout Ranger 2/1 — verbatim
/// Oracle text.
const JADELIGHT: &str = "When this creature enters, it explores, then it explores again. (Reveal the top card of your library. Put that card into your hand if it's a land. Otherwise, put a +1/+1 counter on this creature, then put the card back or put it into your graveyard. Then repeat this process.)";

/// Francisco, Fowl Marauder {1}{B} Legendary Creature — Bird Pirate 0/1 —
/// verbatim Oracle text.
const FRANCISCO: &str = "Flying\nFrancisco can't block.\nWhenever one or more Pirates you control deal damage to a player, Francisco explores.\nPartner (You can have two commanders if both have partner.)";

/// Wildgrowth Walker {1}{G} Creature — Elemental 1/3 — verbatim Oracle text.
const WILDGROWTH_WALKER: &str =
    "Whenever a creature you control explores, put a +1/+1 counter on this creature and you gain 3 life.";

/// Unsummon {U} Instant — verbatim Oracle text.
const UNSUMMON: &str = "Return target creature to its owner's hand.";

/// Path of Discovery {3}{G} Enchantment — verbatim Oracle text.
const PATH_OF_DISCOVERY: &str = "Whenever a creature you control enters, it explores.";

/// Rubinia Soulsinger {2}{U}{U} Legendary Creature — Human Wizard 2/3 —
/// verbatim Oracle text.
const RUBINIA: &str = "You may choose not to untap Rubinia Soulsinger during your untap step.\n{T}: Gain control of target creature for as long as you control Rubinia Soulsinger and Rubinia Soulsinger remains tapped.";

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

/// Total +1/+1 counters across every zone row.
fn total_plus1(runner: &GameRunner) -> u32 {
    runner
        .state()
        .objects
        .iter()
        .map(|(_, object)| {
            object
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0)
        })
        .sum()
}

/// Decline the parked DigChoice put-back, collecting events.
fn decline_digchoice(runner: &mut GameRunner, events: &mut Vec<GameEvent>) {
    let result = runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("declining the put-back is legal");
    events.extend(result.events);
}

/// CR 701.44a + CR 701.44c: P0 casts Jadelight Ranger ("it explores, then it
/// explores again"); with its trigger on the stack, P1 casts Unsummon on it.
/// Both explores still happen through LKI — P0 reveals twice — and no +1/+1
/// counter lands anywhere.
#[test]
fn jadelight_bounced_in_response_explores_twice_without_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Bolt on top, Shock beneath: the two explores reveal distinct cards.
    let shock = scenario.add_spell_to_library_top(P0, "Shock", true).id();
    let bolt = scenario
        .add_spell_to_library_top(P0, "Lightning Bolt", true)
        .id();
    let jadelight = scenario
        .add_creature_to_hand_from_oracle(P0, "Jadelight Ranger", 2, 1, JADELIGHT)
        .with_mana_cost(ManaCost::zero())
        .id();
    let unsummon = scenario
        .add_spell_to_hand_from_oracle(P1, "Unsummon", true, UNSUMMON)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let mut events = Vec::new();

    commit_cast(&mut runner, jadelight, &[], &mut events);
    pass_until_trigger_on_stack(&mut runner, &mut events, jadelight);
    pass_until_player_priority(&mut runner, &mut events, P1);
    commit_cast(&mut runner, unsummon, &[jadelight], &mut events);
    pass_until_stack_len(&mut runner, &mut events, 1);
    // REACH: the bounce resolved while the trigger waits.
    assert_eq!(
        runner.state().objects[&jadelight].zone,
        Zone::Hand,
        "reach: Unsummon returned Jadelight to hand"
    );
    assert!(
        runner.state().players[P0.0 as usize]
            .hand
            .contains(&jadelight),
        "reach: Jadelight is in its owner's hand"
    );

    // First explore: P0 reveals the Bolt and declines the put-back.
    pass_until_digchoice(&mut runner, &mut events);
    assert_digchoice(
        &runner,
        P0,
        &[bolt],
        "the first explore must offer the Bolt to P0",
    );
    decline_digchoice(&mut runner, &mut events);
    assert_eq!(
        runner.state().objects[&bolt].zone,
        Zone::Graveyard,
        "the first declined card must go to the graveyard"
    );
    // Second explore: P0 reveals the Shock and declines the put-back.
    pass_until_digchoice(&mut runner, &mut events);
    assert_digchoice(
        &runner,
        P0,
        &[shock],
        "the second explore must offer the Shock to P0",
    );
    decline_digchoice(&mut runner, &mut events);
    assert_eq!(
        runner.state().objects[&shock].zone,
        Zone::Graveyard,
        "the second declined card must go to the graveyard"
    );
    pass_until_settled(&mut runner, &mut events);

    // CR 701.44c: both explores happened through LKI — two completions naming
    // Jadelight, two P0 reveals — with no counter on anything.
    let completions = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::Explore,
                    source_id,
                    ..
                } if *source_id == jadelight
            )
        })
        .count();
    assert_eq!(
        completions, 2,
        "both explores must complete naming Jadelight, events: {events:?}"
    );
    for card in [bolt, shock] {
        assert!(
            events.iter().any(|event| matches!(
                event,
                GameEvent::CardsRevealed { player, card_ids, .. }
                    if *player == P0 && card_ids.as_slice() == [card]
            )),
            "P0 must reveal {card:?}, events: {events:?}"
        );
    }
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, GameEvent::CounterAdded { .. })),
        "no counter may be placed for a departed explorer, events: {events:?}"
    );
    assert_all_plus1_zero(&runner, "a departed explorer puts no counter on anything");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty(),
        "resolution must settle, got {:?}",
        runner.state().waiting_for
    );
}

/// CR 701.44c (stolen hostile self): as above, but P1 first steals Jadelight
/// with Rubinia Soulsinger, then bounces it — both in response to its own
/// trigger. The explores use the thief's LKI: P1 reveals twice from P1's
/// library, and still no counter lands anywhere.
#[test]
fn jadelight_stolen_then_bounced_explores_twice_through_thief_lki() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // P1's top two are Bolt over Shock; P0's top is a decoy, so reading the
    // wrong library reveals the wrong card.
    let shock = scenario.add_spell_to_library_top(P1, "Shock", true).id();
    let bolt = scenario
        .add_spell_to_library_top(P1, "Lightning Bolt", true)
        .id();
    let _decoy = scenario
        .add_spell_to_library_top(P0, "Giant Growth", true)
        .id();
    let jadelight = scenario
        .add_creature_to_hand_from_oracle(P0, "Jadelight Ranger", 2, 1, JADELIGHT)
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

    commit_cast(&mut runner, jadelight, &[], &mut events);
    pass_until_trigger_on_stack(&mut runner, &mut events, jadelight);
    pass_until_player_priority(&mut runner, &mut events, P1);
    activate_onto_stack(&mut runner, &mut events, rubinia, 0, jadelight);
    pass_until_stack_len(&mut runner, &mut events, 1);
    // REACH: the theft resolved while the trigger waits.
    assert_eq!(
        runner.state().objects[&jadelight].controller,
        P1,
        "reach: Rubinia stole Jadelight"
    );
    pass_until_player_priority(&mut runner, &mut events, P1);
    commit_cast(&mut runner, unsummon, &[jadelight], &mut events);
    pass_until_stack_len(&mut runner, &mut events, 1);
    // REACH: the bounce resolved — Jadelight is back in its owner's hand.
    assert_eq!(
        runner.state().objects[&jadelight].zone,
        Zone::Hand,
        "reach: Unsummon returned Jadelight to hand"
    );
    assert!(
        runner.state().players[P0.0 as usize]
            .hand
            .contains(&jadelight),
        "reach: Jadelight is in its owner's (P0's) hand"
    );

    pass_until_digchoice(&mut runner, &mut events);
    assert_digchoice(
        &runner,
        P1,
        &[bolt],
        "the thief (LKI controller) must get the first explore choice",
    );
    decline_digchoice(&mut runner, &mut events);
    assert_eq!(
        runner.state().objects[&bolt].zone,
        Zone::Graveyard,
        "the first declined card must go to the graveyard"
    );
    pass_until_digchoice(&mut runner, &mut events);
    assert_digchoice(
        &runner,
        P1,
        &[shock],
        "the thief (LKI controller) must get the second explore choice",
    );
    decline_digchoice(&mut runner, &mut events);
    assert_eq!(
        runner.state().objects[&shock].zone,
        Zone::Graveyard,
        "the second declined card must go to the graveyard"
    );
    pass_until_settled(&mut runner, &mut events);

    // CR 701.44c: LKI names the thief — P1 reveals P1's Bolt then Shock, and
    // both completions still name Jadelight.
    for card in [bolt, shock] {
        assert!(
            events.iter().any(|event| matches!(
                event,
                GameEvent::CardsRevealed { player, card_ids, .. }
                    if *player == P1 && card_ids.as_slice() == [card]
            )),
            "P1 must reveal {card:?}, events: {events:?}"
        );
    }
    let completions = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::Explore,
                    source_id,
                    ..
                } if *source_id == jadelight
            )
        })
        .count();
    assert_eq!(
        completions, 2,
        "both explores must complete naming Jadelight, events: {events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::ControllerChanged {
                object_id,
                old_controller,
                new_controller,
                ..
            } if *object_id == jadelight && *old_controller == P0 && *new_controller == P1
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
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty(),
        "resolution must settle, got {:?}",
        runner.state().waiting_for
    );
}

/// CR 701.44a (self-explore guard): Francisco, Fowl Marauder parses as a
/// `DamageDoneOnceByController` trigger executing a bare `Explore` — no
/// seeded pins, no event-filter parent — so explore selection always falls
/// through to the source. When another Pirate deals damage, Francisco (live on
/// the battlefield) explores: the counter lands on Francisco, never the
/// Pirate, and the completion names Francisco.
#[test]
fn francisco_explores_himself_not_the_pirate() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let francisco = scenario
        .add_creature_from_oracle(P0, "Francisco, Fowl Marauder", 0, 1, FRANCISCO)
        .id();
    // Daring Buccaneer {1}{R} 2/2 Pirate, staged as a vanilla attacking body:
    // only its Pirate subtype (the trigger's damage source) matters here.
    let mut pirate_builder = scenario.add_creature(P0, "Daring Buccaneer", 2, 2);
    pirate_builder.with_subtypes(vec!["Human", "Pirate"]);
    let pirate = pirate_builder.id();
    let bolt = scenario
        .add_spell_to_library_top(P0, "Lightning Bolt", true)
        .id();
    let mut runner = scenario.build();
    let mut events = Vec::new();
    let p1_life_before = runner.state().players[P1.0 as usize].life;

    runner.advance_to_combat();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareAttackers { .. }
        ),
        "expected the declare-attackers prompt, got {:?}",
        runner.state().waiting_for
    );
    let result = runner
        .declare_attackers(&[(pirate, AttackTarget::Player(P1))])
        .expect("attack declaration is accepted");
    events.extend(result.events);

    // Drive combat damage plus the damage trigger until the explore parks.
    for _ in 0..40 {
        match &runner.state().waiting_for {
            WaitingFor::DigChoice { .. } => break,
            WaitingFor::Priority { .. } => pass_once(&mut runner, &mut events),
            WaitingFor::DeclareBlockers { .. } => {
                let result = runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("declining to block is legal");
                events.extend(result.events);
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let count = triggers.len();
                answer_order_identity(&mut runner, &mut events, count);
            }
            other => panic!("unexpected prompt during combat: {other:?}"),
        }
    }
    // REACH: the Pirate dealt combat damage, so the damage trigger fired.
    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        p1_life_before - 2,
        "reach: the unblocked Pirate dealt 2 combat damage"
    );
    assert_digchoice(
        &runner,
        P0,
        &[bolt],
        "Francisco's explore must offer the Bolt to P0",
    );
    decline_digchoice(&mut runner, &mut events);
    assert_eq!(
        runner.state().objects[&bolt].zone,
        Zone::Graveyard,
        "the declined card must go to the graveyard"
    );
    pass_until_settled(&mut runner, &mut events);

    // The counter lands on Francisco (live), never on the damaging Pirate.
    assert_eq!(
        plus1(&runner, francisco),
        1,
        "a nonland reveal must put a +1/+1 counter on Francisco"
    );
    assert_eq!(
        plus1(&runner, pirate),
        0,
        "the damaging Pirate must not receive Francisco's explore counter"
    );
    assert_eq!(
        total_plus1(&runner),
        1,
        "exactly one +1/+1 counter may exist, events: {events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Explore,
                source_id,
                ..
            } if *source_id == francisco
        )),
        "the explore completion must name Francisco, events: {events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::CardsRevealed { player, card_ids, .. }
                if *player == P0 && card_ids.as_slice() == [bolt]
        )),
        "P0 must reveal the Bolt, events: {events:?}"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty(),
        "resolution must settle, got {:?}",
        runner.state().waiting_for
    );
}

/// CR 701.44b + the explore ruling ("Effects that trigger 'whenever a
/// creature you control explores' trigger if appropriate"): the bounced bear's
/// explore still fires Wildgrowth Walker's trigger — Walker gets a +1/+1
/// counter and P0 gains 3 life — while the departed bear itself gets nothing.
#[test]
fn wildgrowth_walker_fires_on_departed_explorer() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let path = scenario
        .add_enchantment_from_oracle(P0, "Path of Discovery", PATH_OF_DISCOVERY)
        .id();
    let walker = scenario
        .add_creature_from_oracle(P0, "Wildgrowth Walker", 1, 3, WILDGROWTH_WALKER)
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
    let p0_life_before = runner.state().players[P0.0 as usize].life;

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

    // The land reveal still went to hand and the completion names the bear.
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
    // The observer clause: Walker's trigger fired on the departed explore.
    assert_eq!(
        plus1(&runner, walker),
        1,
        "Walker's trigger must put a +1/+1 counter on Walker"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        p0_life_before + 3,
        "Walker's trigger must gain P0 3 life"
    );
    assert_eq!(
        total_plus1(&runner),
        1,
        "Walker's is the only +1/+1 counter, events: {events:?}"
    );
    assert_eq!(plus1(&runner, path), 0, "Path must not explore itself");
    assert_eq!(plus1(&runner, bear), 0, "the departed bear gets no counter");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty(),
        "resolution must settle, got {:?}",
        runner.state().waiting_for
    );
}
