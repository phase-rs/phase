//! Regression for issue #2357: Norin the Wary must exile itself when any creature
//! attacks, not only when a player casts a spell.
//!
//! https://github.com/phase-rs/phase/issues/2357

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zone_pipeline::{move_object_for_test, ZoneMoveRequest};
use engine::parser::oracle::parse_oracle_text;
use engine::types::actions::GameAction;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const NORIN_ORACLE: &str = "When a player casts a spell or a creature attacks, exile Norin. \
Return it to the battlefield under its owner's control at the beginning of the next end step.";
const DIVINATION: &str = "Draw two cards.";

/// Answer the one prompt in front of the game the way a player with no
/// further choices would: no attackers, no blockers, pass priority.
fn step(runner: &mut GameRunner) {
    let action = match runner.state().waiting_for.clone() {
        WaitingFor::OrderTriggers { .. } => {
            engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            return;
        }
        WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
            attacks: vec![],
            bands: vec![],
        },
        WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
            assignments: vec![],
        },
        WaitingFor::Priority { .. } => GameAction::PassPriority,
        other => panic!("unexpected prompt {other:?}"),
    };
    runner.act(action).expect("the no-choice answer is legal");
}

fn pass_until(runner: &mut GameRunner, done: impl Fn(&GameRunner) -> bool) {
    for _ in 0..600 {
        if done(runner) {
            return;
        }
        step(runner);
    }
    panic!(
        "never reached the awaited state; turn {} phase {:?} waiting {:?}",
        runner.state().turn_number,
        runner.state().phase,
        runner.state().waiting_for
    );
}

fn drain_stack(runner: &mut GameRunner) {
    pass_until(runner, |r| {
        r.state().stack.is_empty()
            && !matches!(r.state().waiting_for, WaitingFor::OrderTriggers { .. })
    });
}

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

/// Move `id` to its owner's graveyard through the zone pipeline, so
/// replacement effects apply.
fn put_into_graveyard(runner: &mut GameRunner, id: ObjectId) {
    let mut events = Vec::new();
    move_object_for_test(
        runner.state_mut(),
        ZoneMoveRequest::effect(id, Zone::Graveyard, id),
        &mut events,
    );
    assert_eq!(zone(runner, id), Zone::Graveyard);
}

/// Drive to the next end step, where the delayed return must be the one
/// trigger on the stack, and resolve it.
fn resolve_the_end_step_return(runner: &mut GameRunner) {
    assert_eq!(runner.state().delayed_triggers.len(), 1);
    pass_until(runner, |r| r.state().phase == Phase::End);
    let stack: Vec<_> = runner.state().stack.iter().collect();
    assert!(
        matches!(
            stack.as_slice(),
            [entry] if matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
        ),
        "the delayed return is on the stack at the end step: {stack:?}"
    );
    // CR 603.7b: a delayed triggered ability triggers only once.
    assert!(runner.state().delayed_triggers.is_empty());
    drain_stack(runner);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// P0 controls Norin and casts Divination for free; returns the runner after
/// the spell-cast trigger exiled Norin and the spell resolved.
fn norin_exiled_by_a_spell_cast() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest"; 10]);
    scenario.with_library_top(P1, &["Forest"; 10]);
    let norin = scenario
        .add_creature(P0, "Norin the Wary", 2, 1)
        .as_legendary()
        .with_subtypes(vec!["Human", "Warrior"])
        .from_oracle_text(NORIN_ORACLE)
        .id();
    let divination = scenario
        .add_spell_to_hand_from_oracle(P0, "Divination", false, DIVINATION)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();
    runner.cast(divination).resolve();
    drain_stack(&mut runner);
    assert_eq!(zone(&runner, norin), Zone::Exile);
    assert_eq!(runner.state().delayed_triggers.len(), 1);
    (runner, norin, divination)
}

/// CR 608.2c + CR 603.7e: "exile Norin. Return it …" returns Norin, the
/// object the previous instruction named, not the cast spell the trigger
/// condition introduced (CR 109.2b).
#[test]
fn norin_returns_at_next_end_step_after_spell_cast() {
    let (mut runner, norin, divination) = norin_exiled_by_a_spell_cast();

    resolve_the_end_step_return(&mut runner);
    assert_eq!(zone(&runner, norin), Zone::Battlefield);
    assert_eq!(runner.state().objects[&norin].controller, P0);
    // The cast spell is the other candidate referent; the return leaves it alone.
    assert_eq!(zone(&runner, divination), Zone::Graveyard);
}

/// CR 603.7c + CR 400.7: a Norin that left exile before the end step is a new
/// object the delayed return does not find.
#[test]
fn norin_moved_out_of_exile_is_not_returned() {
    let (mut runner, norin, _) = norin_exiled_by_a_spell_cast();
    put_into_graveyard(&mut runner, norin);

    resolve_the_end_step_return(&mut runner);
    assert_eq!(zone(&runner, norin), Zone::Graveyard);
}

#[test]
fn norin_parses_spell_cast_and_creature_attacks_triggers() {
    let parsed = parse_oracle_text(
        NORIN_ORACLE,
        "Norin the Wary",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Warrior".to_string()],
    );
    assert_eq!(
        parsed.triggers.len(),
        2,
        "cross-subject 'or a creature attacks' must split into two triggers"
    );
    assert_eq!(parsed.triggers[0].mode, TriggerMode::SpellCast);
    assert_eq!(parsed.triggers[1].mode, TriggerMode::Attacks);
}

#[test]
fn norin_exiles_on_creature_attack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest"; 10]);
    scenario.with_library_top(P1, &["Forest"; 10]);

    let norin = scenario
        .add_creature_from_oracle(P0, "Norin the Wary", 2, 1, NORIN_ORACLE)
        .id();
    let attacker = scenario.add_creature(P1, "Hostile Bear", 2, 2).id();

    let mut runner = scenario.build();

    // Play out P0's turn without attacking, up to P1's declare-attackers step.
    pass_until(&mut runner, |r| {
        r.state().active_player == P1
            && matches!(r.state().waiting_for, WaitingFor::DeclareAttackers { .. })
    });
    // Reach guard: no trigger has fired yet, so Norin is still on the battlefield.
    assert_eq!(zone(&runner, norin), Zone::Battlefield);
    assert!(runner.state().delayed_triggers.is_empty());

    // An opponent's creature attacks: "a creature attacks" is not limited to
    // Norin's controller's creatures.
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P0))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");
    drain_stack(&mut runner);

    assert_eq!(
        zone(&runner, norin),
        Zone::Exile,
        "Norin must exile itself when a creature attacks (issue #2357)"
    );
    // The attack trigger resolved during P1's turn, before its end step.
    assert_eq!(runner.state().active_player, P1);
    assert_ne!(runner.state().phase, Phase::End);

    // CR 603.7e: the attack half's delayed return brings Norin back too.
    resolve_the_end_step_return(&mut runner);
    assert_eq!(zone(&runner, norin), Zone::Battlefield);
    assert_eq!(runner.state().objects[&norin].controller, P0);
}
