//! CR 122.1 + CR 603.1 + CR 608.2k: "draw a card for each kind of counter on
//! it" in a trigger whose condition names an object other than the trigger's
//! own ("Whenever a creature you control deals combat damage to a player",
//! "Whenever another creature you control dies") names that other object — the
//! event's creature. The census of that object is not read, so the clause is an
//! explicit `Effect::Unimplemented` gap reported UNSUPPORTED through the public
//! coverage authority (`card_face_gaps`). It is never rebound to a census of the
//! listener's own counters. The runtime tests pin that: the listener and the
//! event's creature carry different numbers of counter kinds, and the listener's
//! number is never drawn. Synthetic class cards (no printed card yet); the self
//! form ("Whenever this creature …") is the control that reads its own census.

use engine::game::combat::AttackTarget;
use engine::game::coverage::card_face_gaps;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, Effect, QuantityExpr, QuantityRef, TargetFilter, TriggerDefinition,
};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::counter::parse_counter_type;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const DAMAGE_LISTENER: &str = "Whenever a creature you control deals combat damage to a player, \
                               draw a card for each kind of counter on it.";
const DIES_LISTENER: &str =
    "Whenever another creature you control dies, draw a card for each kind of counter on it.";
const SELF_DAMAGE_CONTROL: &str = "Whenever this creature deals combat damage to a player, draw \
                                   a card for each kind of counter on it.";
const DESTROY_TARGET_CREATURE: &str = "Destroy target creature.";

fn face(name: &str, oracle: &str) -> CardFace {
    let parsed = parse_oracle_text(oracle, name, &[], &["Creature".to_string()], &[]);
    CardFace {
        name: name.to_string(),
        oracle_text: Some(oracle.to_string()),
        abilities: parsed.abilities,
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        replacements: parsed.replacements,
        keywords: parsed.extracted_keywords,
        ..Default::default()
    }
}

/// The draw count of each trigger's effect on the face.
fn trigger_draw_counts(face: &CardFace) -> Vec<QuantityExpr> {
    face.triggers
        .iter()
        .filter_map(|trigger: &TriggerDefinition| trigger.execute.as_deref())
        .filter_map(|execute: &AbilityDefinition| match &*execute.effect {
            Effect::Draw { count, .. } => Some(count.clone()),
            _ => None,
        })
        .collect()
}

/// Whether any `DistinctCounterKindsAmong` census appears anywhere on the face.
fn has_kinds_census(face: &CardFace) -> bool {
    serde_json::to_string(face)
        .expect("a parsed face serializes")
        .contains("DistinctCounterKindsAmong")
}

/// CR 603.1 + CR 608.2k: a census of the trigger event's creature is an
/// unsupported gap with no census read — while the self form, the reach guard,
/// parses to a census of the trigger's own object with no gap.
#[test]
fn counter_kind_census_of_a_trigger_events_other_creature_is_unsupported() {
    let control = face("Stadium Courier", SELF_DAMAGE_CONTROL);
    assert_eq!(
        trigger_draw_counts(&control),
        vec![QuantityExpr::Ref {
            qty: QuantityRef::DistinctCounterKindsAmong {
                filter: TargetFilter::SelfRef,
            },
        }],
        "reach guard: the self form reads its own census"
    );
    assert_eq!(card_face_gaps(&control), Vec::<String>::new());

    for (name, text) in [
        ("Tallymark Warden", DAMAGE_LISTENER),
        ("Mourning Tallyman", DIES_LISTENER),
    ] {
        let face = face(name, text);
        assert!(
            !has_kinds_census(&face),
            "{text:?}: no counter-kind census may be read, got {:?}",
            face.triggers
        );
        assert!(
            !card_face_gaps(&face).is_empty(),
            "{text:?}: the card must be unsupported"
        );
    }
}

fn library_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].library.len()
}

fn seed_library(scenario: &mut GameScenario, player: PlayerId) {
    for name in ["Library A", "Library B", "Library C", "Library D"] {
        scenario.add_card_to_library_top(player, name);
    }
}

/// Pass priority until the stack is empty. Panics on any prompt it is not
/// taught to answer.
fn drive_to_empty_stack(runner: &mut GameRunner) {
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    panic!("the stack never emptied");
}

/// How many stack objects resolved among `events` (CR 608.2).
fn stack_resolutions(events: &[GameEvent]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, GameEvent::StackResolved { .. }))
        .count()
}

/// Puts slime, time, and lore counters on `listener` — three kinds, a number
/// no other object on the board carries.
fn give_three_kinds(scenario: &mut GameScenario, listener: ObjectId) {
    for kind in ["slime", "time", "lore"] {
        scenario.with_counter(listener, parse_counter_type(kind), 1);
    }
}

/// CR 510.2 + CR 603.1 + CR 608.2k: the listener (three counter kinds) stays
/// home while another creature it watches (charge and oil — two kinds, three
/// counters) deals combat damage. The clause is an explicit unimplemented gap,
/// so no census-based draw happens — in particular the listener's own three
/// kinds are never drawn.
#[test]
fn combat_damage_listener_never_draws_its_own_counter_kinds() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let listener = scenario
        .add_creature_from_oracle(P0, "Tallymark Warden", 1, 1, DAMAGE_LISTENER)
        .id();
    give_three_kinds(&mut scenario, listener);
    let attacker = scenario.add_creature(P0, "Runner", 2, 2).id();
    scenario.with_counter(attacker, parse_counter_type("charge"), 1);
    scenario.with_counter(attacker, parse_counter_type("oil"), 2);
    seed_library(&mut scenario, P0);
    let mut runner = scenario.build();

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("only the watched creature attacks");
    let outcome = runner.combat_damage();
    assert_eq!(
        outcome.life_delta(P1),
        -2,
        "reach guard: the watched creature dealt combat damage to a player"
    );
    // The listener's trigger is the only object that can use the stack here.
    assert_eq!(
        stack_resolutions(outcome.events()),
        1,
        "reach guard: the listener's combat-damage trigger fired and resolved"
    );
    assert_ne!(
        outcome.hand_drawn(P0),
        3,
        "the listener's own three counter kinds must never be drawn"
    );
    // The clause is an explicit unimplemented gap: nothing is drawn at all.
    outcome.assert_hand_drawn(P0, 0);
}

/// CR 700.4 + CR 603.10a + CR 608.2k: the listener (three counter kinds)
/// watches another creature (charge and oil — two kinds) die. The clause is an
/// explicit unimplemented gap, so no census-based draw happens — in particular
/// the listener's own three kinds are never drawn.
#[test]
fn dies_listener_never_draws_its_own_counter_kinds() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let listener = scenario
        .add_creature_from_oracle(P0, "Mourning Tallyman", 1, 1, DIES_LISTENER)
        .id();
    give_three_kinds(&mut scenario, listener);
    let dier = scenario.add_creature(P0, "Fallen Runner", 2, 2).id();
    scenario.with_counter(dier, parse_counter_type("charge"), 1);
    scenario.with_counter(dier, parse_counter_type("oil"), 2);
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Execution", true, DESTROY_TARGET_CREATURE)
        .with_mana_cost(ManaCost::zero())
        .id();
    seed_library(&mut scenario, P0);
    let mut runner = scenario.build();
    let library_before = library_len(&runner, P0);

    let outcome = runner.cast(destroy).target_object(dier).resolve();
    // The spell and the listener's dies trigger are the only stack objects.
    assert_eq!(
        stack_resolutions(outcome.events()),
        2,
        "reach guard: the listener's dies trigger fired and resolved"
    );
    drive_to_empty_stack(&mut runner);

    assert_eq!(
        runner.state().objects[&dier].zone,
        Zone::Graveyard,
        "reach guard: the watched creature died"
    );
    assert_eq!(
        runner.state().objects[&listener].zone,
        Zone::Battlefield,
        "reach guard: the listener survived to watch it"
    );
    let drawn = library_before - library_len(&runner, P0);
    assert_ne!(
        drawn, 3,
        "the listener's own three counter kinds must never be drawn"
    );
    // The clause is an explicit unimplemented gap: nothing is drawn at all.
    assert_eq!(drawn, 0);
}
