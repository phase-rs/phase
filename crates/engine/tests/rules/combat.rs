#![allow(unused_imports)]
use super::*;

/// CR 510.1: Unblocked attacker deals combat damage to defending player
#[test]
fn unblocked_attacker_deals_damage_to_player() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker_id = scenario.add_creature(P0, "Bear", 2, 2).id();
    let mut runner = scenario.build();

    run_combat(&mut runner, vec![attacker_id], vec![]);

    let state = runner.state();
    let p1_life = state.players.iter().find(|p| p.id == P1).unwrap().life;
    assert_eq!(
        p1_life, 18,
        "Defending player should take 2 damage from unblocked 2/2"
    );
}

/// CR 510.1c: Blocked creature and blocker exchange damage
#[test]
fn blocked_creature_and_blocker_exchange_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker_id = scenario.add_creature(P0, "Centaur", 3, 3).id();
    let blocker_id = scenario.add_creature(P1, "Bear", 2, 2).id();
    let mut runner = scenario.build();

    run_combat(
        &mut runner,
        vec![attacker_id],
        vec![(blocker_id, attacker_id)],
    );

    let state = runner.state();
    // Blocker (2/2) took 3 damage (lethal) -- should be in graveyard after SBAs
    assert!(
        !state.battlefield.contains(&blocker_id),
        "2/2 blocker should die to 3 damage"
    );
    // Attacker (3/3) took 2 damage -- survives
    let attacker = &state.objects[&attacker_id];
    assert_eq!(
        attacker.damage_marked, 2,
        "3/3 attacker should have 2 damage marked"
    );
    assert!(
        state.battlefield.contains(&attacker_id),
        "3/3 attacker should survive with 2 damage"
    );
}

/// CR 510.1b: First strike damage resolves before regular damage
#[test]
fn first_strike_kills_before_regular_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker_id = {
        let mut b = scenario.add_creature(P0, "Knight", 2, 2);
        b.first_strike();
        b.id()
    };
    let blocker_id = scenario.add_creature(P1, "Bear", 3, 2).id();
    let mut runner = scenario.build();

    run_combat(
        &mut runner,
        vec![attacker_id],
        vec![(blocker_id, attacker_id)],
    );

    let state = runner.state();
    // First strike 2/2 deals 2 to blocker with toughness 2 = lethal.
    // Blocker dies before dealing regular damage.
    assert!(
        !state.battlefield.contains(&blocker_id),
        "Blocker should die to first strike damage before dealing regular damage"
    );
    assert_eq!(
        state.objects[&attacker_id].damage_marked, 0,
        "First strike attacker should take 0 damage (blocker died before regular step)"
    );

    // Snapshot for regression anchoring
    insta::assert_json_snapshot!(
        "combat_first_strike_kills_before_regular",
        runner.snapshot()
    );
}

/// CR 510.1c: Double strike deals damage in both steps
#[test]
fn double_strike_deals_damage_in_both_steps() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker_id = {
        let mut b = scenario.add_creature(P0, "Champion", 3, 3);
        b.double_strike();
        b.id()
    };
    let blocker_id = scenario.add_creature(P1, "Rhino", 5, 5).id();
    let mut runner = scenario.build();

    run_combat(
        &mut runner,
        vec![attacker_id],
        vec![(blocker_id, attacker_id)],
    );

    let state = runner.state();
    // Double strike 3/3 deals 3 in first strike step + 3 in regular step = 6 total
    // 6 >= 5 toughness = lethal, blocker should die
    assert!(
        !state.battlefield.contains(&blocker_id),
        "5/5 blocker should die to 6 total damage from double strike 3/3"
    );
}

/// CR 702.2b: Defender can't attack
#[test]
fn defender_cannot_attack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wall_id = {
        let mut b = scenario.add_creature(P0, "Wall", 0, 4);
        b.defender();
        b.id()
    };
    let mut runner = scenario.build();

    // Pass priority to get to DeclareAttackers
    runner.pass_both_players();

    // Trying to declare a defender as attacker should fail
    let result = runner.act(GameAction::DeclareAttackers {
        attacks: vec![(wall_id, AttackTarget::Player(P1))],
    });
    assert!(
        result.is_err(),
        "Creature with Defender should not be able to attack"
    );
}

/// CR 510.1: Multiple attackers and blockers resolve correctly
#[test]
fn multiple_attackers_mixed_blocking() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker1 = scenario.add_creature(P0, "Centaur", 3, 3).id();
    let attacker2 = scenario.add_creature(P0, "Bear", 2, 2).id();
    let blocker = scenario.add_creature(P1, "Guard", 2, 2).id();
    let mut runner = scenario.build();

    // One blocker blocks attacker1, attacker2 is unblocked
    run_combat(
        &mut runner,
        vec![attacker1, attacker2],
        vec![(blocker, attacker1)],
    );

    // Unblocked attacker2 (2/2) deals 2 damage to P1
    assert_eq!(
        runner.life(P1),
        18,
        "Unblocked 2/2 should deal 2 damage to defending player"
    );

    // Blocked exchange: 3/3 vs 2/2 -- blocker dies, attacker takes 2 damage
    let state = runner.state();
    assert!(
        !state.battlefield.contains(&blocker),
        "2/2 blocker should die to 3/3 attacker"
    );
    assert_eq!(
        state.objects[&attacker1].damage_marked, 2,
        "3/3 attacker should have 2 damage from blocker"
    );

    // Snapshot for regression anchoring
    insta::assert_json_snapshot!(
        "combat_multiple_attackers_mixed_blocking",
        runner.snapshot()
    );
}

/// CR 510.1: Attacker taps when attacking (no vigilance)
#[test]
fn attacker_taps_when_attacking() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker_id = scenario.add_creature(P0, "Bear", 2, 2).id();
    let mut runner = scenario.build();

    // Pass priority to get to DeclareAttackers
    runner.pass_both_players();

    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker_id, AttackTarget::Player(P1))],
        })
        .expect("DeclareAttackers should succeed");

    assert!(
        runner.state().objects[&attacker_id].tapped,
        "Attacker without vigilance should be tapped after declaring attack"
    );
}
