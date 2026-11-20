//! Regression for issue #10315: a player with protection from everything (The
//! One Ring's cast trigger) must remain attackable. CR 702.16j lists what
//! protection from everything stops — targeting, Auras, and damage — and
//! attacks are not on that list; The One Ring's ruling says creatures can
//! still attack that player, but the combat damage is prevented.
//!
//! Before the fix, `get_valid_attack_targets` dropped the protected player, so
//! in a two-player game the opponent had no legal attack target at all.
//!
//! https://github.com/phase-rs/phase/issues/10315

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::static_abilities::player_has_protection_from_everything;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

const STARTING_LIFE: i32 = 20;
const ATTACKER_POWER: i32 = 3;

fn add_colorless(runner: &mut GameRunner, player: PlayerId, amount: usize) {
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .unwrap()
        .mana_pool;
    for _ in 0..amount {
        pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
}

/// Hand the turn to P1 (P0's protection lasts "until your next turn", so it
/// covers all of P1's turn) and advance to P1's declare-attackers step.
fn advance_to_p1_declare_attackers(runner: &mut GameRunner) {
    let state = runner.state_mut();
    state.turn_number += 1;
    state.phase = Phase::PreCombatMain;
    state.active_player = P1;
    state.priority_player = P1;
    state.waiting_for = WaitingFor::Priority { player: P1 };
    runner.advance_to_combat();
}

/// Drive P1's attack on P0 through combat damage, asserting the declaration is
/// offered and accepted.
fn attack_p0(runner: &mut GameRunner, attacker: ObjectId) {
    let WaitingFor::DeclareAttackers {
        valid_attack_targets,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "P1 must reach the declare-attackers prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(
        valid_attack_targets.contains(&AttackTarget::Player(P0)),
        "P0 must be offered as an attack target, got {valid_attack_targets:?}"
    );

    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P0))])
        .expect("CR 702.16j: attacking a player with protection from everything is legal");

    for _ in 0..16 {
        if matches!(
            runner.state().phase,
            Phase::EndCombat | Phase::PostCombatMain
        ) {
            break;
        }
        runner.pass_both_players();
    }
    assert!(
        matches!(
            runner.state().phase,
            Phase::EndCombat | Phase::PostCombatMain
        ),
        "combat must finish, stuck at {:?} / {:?}",
        runner.state().phase,
        runner.state().waiting_for
    );
}

#[test]
fn one_ring_protected_player_can_be_attacked_and_damage_is_prevented() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, STARTING_LIFE);
    let ring = scenario.add_real_card(P0, "The One Ring", Zone::Hand, db);
    let attacker = scenario
        .add_creature(P1, "Attacker", ATTACKER_POWER, ATTACKER_POWER)
        .id();
    let mut runner = scenario.build();

    // Cast it so the "if you cast it" trigger grants protection (CR 702.16j).
    add_colorless(&mut runner, P0, 4);
    runner.cast(ring).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&ring].zone, Zone::Battlefield);
    assert!(
        player_has_protection_from_everything(runner.state(), P0),
        "casting The One Ring must give P0 protection from everything"
    );

    advance_to_p1_declare_attackers(&mut runner);
    assert!(
        player_has_protection_from_everything(runner.state(), P0),
        "protection must last through P1's turn (until P0's next turn)"
    );
    attack_p0(&mut runner, attacker);

    assert_eq!(
        runner.life(P0),
        STARTING_LIFE,
        "CR 702.16j: combat damage to a player with protection from everything is prevented"
    );
}

/// Control: the same attack with no protection deals its damage, so the
/// prevention above comes from protection rather than from the attack fizzling.
#[test]
fn unprotected_player_takes_the_same_attack_damage() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, STARTING_LIFE);
    // Put directly onto the battlefield: not cast, so its trigger grants nothing.
    scenario.add_real_card(P0, "The One Ring", Zone::Battlefield, db);
    let attacker = scenario
        .add_creature(P1, "Attacker", ATTACKER_POWER, ATTACKER_POWER)
        .id();
    let mut runner = scenario.build();
    assert!(!player_has_protection_from_everything(runner.state(), P0));

    advance_to_p1_declare_attackers(&mut runner);
    attack_p0(&mut runner, attacker);

    assert_eq!(runner.life(P0), STARTING_LIFE - ATTACKER_POWER);
}
