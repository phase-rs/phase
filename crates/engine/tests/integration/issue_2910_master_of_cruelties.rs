//! Regressions for Master of Cruelties — issues #2910 and #8082.
//!
//! https://github.com/phase-rs/phase/issues/2910
//! https://github.com/phase-rs/phase/issues/8082
//!
//! #2910: the "can only attack alone" line must parse as a static CombatAlone
//! restriction, not fall through to Unimplemented with empty static_abilities.
//!
//! #8082: "that player's life total becomes 1" must set the DEFENDING player's
//! life total. The possessive subject bound the bare `ParentTarget` anaphor, which
//! resolved to the trigger event's attacker — the ATTACKING player — so an
//! unblocked Master of Cruelties set its own controller's life total to 1.
//! CR 508.5: when an ability of an attacking creature refers to a defending
//! player, that player is the player the creature is attacking. CR 509.3g: an
//! "attacks and isn't blocked" ability triggers even when the creature was never
//! declared as an attacker (for example, put onto the battlefield attacking by
//! Kaalia of the Vast).

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{Effect, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::statics::{CombatAloneAction, CombatAloneRequirement, StaticMode};

const MASTER_OF_CRUELTIES_ORACLE: &str = "\
First strike, deathtouch\n\
This creature can only attack alone.\n\
Whenever this creature attacks a player and isn't blocked, that player's life total becomes 1. This creature assigns no combat damage this combat.";

/// Verbatim Oracle text (Scryfall). Needed by the #8082 test that reproduces the
/// reported path: Kaalia puts Master of Cruelties onto the battlefield attacking.
const KAALIA_OF_THE_VAST_ORACLE: &str = "Flying\n\
Whenever Kaalia attacks an opponent, you may put an Angel, Demon, or Dragon creature card from your hand onto the battlefield tapped and attacking that opponent.";

#[test]
fn master_of_cruelties_parses_attack_alone_static() {
    let parsed = parse_oracle_text(
        MASTER_OF_CRUELTIES_ORACLE,
        "Master of Cruelties",
        &["First strike".to_string(), "Deathtouch".to_string()],
        &["Creature".to_string()],
        &["Demon".to_string()],
    );
    assert!(
        parsed.statics.iter().any(|sd| {
            matches!(
                sd.mode,
                StaticMode::CombatAlone {
                    action: CombatAloneAction::Attack,
                    requirement: CombatAloneRequirement::MustBeSole,
                }
            )
        }),
        "expected CombatAlone(Attack, MustBeSole) static, got {:?}",
        parsed.statics
    );
}

#[test]
fn master_of_cruelties_has_no_unimplemented_attack_alone_leak() {
    let parsed = parse_oracle_text(
        "This creature can only attack alone.",
        "Master of Cruelties",
        &[],
        &["Creature".to_string()],
        &["Demon".to_string()],
    );
    assert!(
        !parsed.statics.is_empty(),
        "attack-alone line must produce static abilities"
    );
    assert!(
        parsed
            .statics
            .iter()
            .all(|sd| { !matches!(&sd.mode, StaticMode::Other(name) if name == "Unimplemented") }),
        "static line must not leak Unimplemented: {:?}",
        parsed.statics
    );
}

/// SHAPE: CR 508.5 + CR 119.5 — the unblocked-attack trigger's life-total
/// instruction names the DEFENDING player. Before #8082 it was the bare
/// `ParentTarget` anaphor, which at resolution read the trigger event's attacker
/// (the attacking player).
#[test]
fn master_of_cruelties_unblocked_trigger_names_the_defending_player() {
    let parsed = parse_oracle_text(
        MASTER_OF_CRUELTIES_ORACLE,
        "Master of Cruelties",
        &["First strike".to_string(), "Deathtouch".to_string()],
        &["Creature".to_string()],
        &["Demon".to_string()],
    );
    let trigger = parsed
        .triggers
        .iter()
        .find(|trigger| {
            trigger
                .description
                .as_deref()
                .is_some_and(|description| description.contains("life total becomes 1"))
        })
        .expect("the 'attacks a player and isn't blocked' trigger must parse");
    let execute = trigger
        .execute
        .as_ref()
        .expect("the trigger must carry its effect");
    let Effect::SetLifeTotal { target, amount } = &*execute.effect else {
        panic!("expected SetLifeTotal, got {:?}", execute.effect);
    };
    assert_eq!(
        *target,
        TargetFilter::DefendingPlayer,
        "CR 508.5: \"that player\" is the player this creature is attacking"
    );
    assert!(
        matches!(
            amount,
            engine::types::ability::QuantityExpr::Fixed { value: 1 }
        ),
        "Master of Cruelties sets the life total to 1, got {amount:?}"
    );
}

/// Drive the REAL combat pipeline to the point where `attacker` attacked
/// unblocked, then resolve the resulting trigger.
fn declare_unblocked_attack(runner: &mut GameRunner, attacker: ObjectId) {
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner.pass_both_players();
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { .. }
        ),
        "expected DeclareBlockers, got {:?}",
        runner.state().waiting_for
    );
    // CR 509.1: the defending player declares no blockers — the attacker is
    // unblocked, which emits `BlockersDeclared` and fires CR 509.3g.
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![],
        })
        .expect("empty DeclareBlockers should succeed");
    runner.advance_until_stack_empty();
}

/// CR 508.5 + CR 119.5 + CR 509.3g: a declared, unblocked Master of Cruelties
/// attack sets the defending player's life total to 1 and leaves the attacking
/// player's life total alone. Reverting the parser fix flips both assertions.
#[test]
fn master_of_cruelties_unblocked_attack_sets_the_defending_players_life_to_one() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let master = scenario
        .add_creature_from_oracle(P0, "Master of Cruelties", 1, 4, MASTER_OF_CRUELTIES_ORACLE)
        .id();
    // Idle defender so the DeclareBlockers prompt appears (it declares no blocks).
    scenario.add_creature(P1, "Idle Bystander", 1, 1);

    let mut runner = scenario.build();
    declare_unblocked_attack(&mut runner, master);

    let state = runner.state();
    assert_eq!(
        state.players[P1.0 as usize].life, 1,
        "CR 508.5: the defending player's life total becomes 1"
    );
    assert_eq!(
        state.players[P0.0 as usize].life, 20,
        "the attacking player's life total must not be set instead"
    );
    assert!(
        state.stack.is_empty(),
        "the trigger must have finished resolving"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "expected Priority after the trigger resolves, got {:?}",
        state.waiting_for
    );
}

/// Resolve Kaalia's attack trigger: accept the "you may put …" clause (the
/// lone Angel, Demon, or Dragon in hand is selected automatically) and drain the
/// trigger off the stack.
fn resolve_kaalia_attack_trigger(runner: &mut GameRunner) {
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("ordering the attack trigger should succeed");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accepting Kaalia's may-clause should succeed");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass should resolve the attack trigger");
            }
            other => panic!("unexpected waiting_for resolving Kaalia's trigger: {other:?}"),
        }
    }
    panic!("Kaalia's attack trigger did not resolve");
}

/// CR 509.3g + CR 508.5: the reported path. Kaalia of the Vast puts Master of
/// Cruelties onto the battlefield tapped and attacking — the creature was never
/// declared as an attacker, but its unblocked-attack trigger still fires, and
/// "that player" is the opponent it is attacking (Kaalia's attack trigger put it
/// into combat attacking that opponent), not its own controller.
#[test]
fn kaalia_cheated_master_of_cruelties_sets_the_defending_players_life_to_one() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let kaalia = scenario
        .add_creature_from_oracle(P0, "Kaalia of the Vast", 2, 2, KAALIA_OF_THE_VAST_ORACLE)
        .id();
    let master_card = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Master of Cruelties",
            1,
            4,
            MASTER_OF_CRUELTIES_ORACLE,
        )
        .with_subtypes(vec!["Demon"])
        .id();
    // Idle defender so the DeclareBlockers prompt appears (it declares no blocks).
    scenario.add_creature(P1, "Idle Bystander", 1, 1);

    let mut runner = scenario.build();
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(kaalia, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("Kaalia can attack the only opponent");
    resolve_kaalia_attack_trigger(&mut runner);

    // Reach guards: the trigger really did put Master of Cruelties onto the
    // battlefield, as an attacking creature against P1.
    let state = runner.state();
    assert_eq!(
        state.objects[&master_card].zone,
        engine::types::zones::Zone::Battlefield,
        "Kaalia's trigger must put Master of Cruelties onto the battlefield"
    );
    assert!(
        state
            .combat
            .as_ref()
            .is_some_and(|combat| combat.attackers.iter().any(|attacker| {
                attacker.object_id == master_card && attacker.defending_player == P1
            })),
        "CR 508.4: Master of Cruelties must be attacking the opponent Kaalia attacked"
    );

    // CR 509.1: no blockers — the attacking Demon is unblocked.
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner.pass_both_players();
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { .. }
        ),
        "expected DeclareBlockers, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![],
        })
        .expect("empty DeclareBlockers should succeed");
    runner.advance_until_stack_empty();

    let state = runner.state();
    assert_eq!(
        state.players[P1.0 as usize].life, 1,
        "CR 508.5: the attacked player's life total becomes 1"
    );
    assert_eq!(
        state.players[P0.0 as usize].life, 20,
        "the attacking player's life total must not be set instead"
    );
    assert!(
        state.stack.is_empty(),
        "the trigger must have finished resolving"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "expected Priority after the trigger resolves, got {:?}",
        state.waiting_for
    );
}
