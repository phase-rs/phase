#![allow(unused_imports)]
use super::*;

/// CR 702.2c + CR 702.19b: Deathtouch + trample assigns lethal (1) to each blocker, tramples rest
#[test]
fn deathtouch_trample_assigns_one_to_blocker_tramples_rest() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker_id = {
        let mut b = scenario.add_creature(P0, "Nightfall Predator", 5, 5);
        b.deathtouch().trample();
        b.id()
    };
    let blocker_id = scenario.add_creature(P1, "Bear", 2, 2).id();
    let mut runner = scenario.build();

    run_combat(
        &mut runner,
        vec![attacker_id],
        vec![(blocker_id, attacker_id)],
    );

    let state = runner.state();
    // With deathtouch, only 1 damage is needed for lethal. 5 - 1 = 4 tramples to player.
    assert!(
        !state.battlefield.contains(&blocker_id),
        "Blocker should die to 1 deathtouch damage"
    );
    assert_eq!(
        runner.life(P1),
        16,
        "4 trample damage should go to defending player (5 power - 1 lethal)"
    );

    // Snapshot for regression anchoring
    insta::assert_json_snapshot!(
        "keywords_deathtouch_trample_damage_assignment",
        runner.snapshot()
    );
}

/// CR 702.15b: Lifelink -- controller gains life equal to damage dealt
#[test]
fn lifelink_gains_life_on_combat_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker_id = {
        let mut b = scenario.add_creature(P0, "Angel", 3, 3);
        b.lifelink();
        b.id()
    };
    let mut runner = scenario.build();

    run_combat(&mut runner, vec![attacker_id], vec![]);

    assert_eq!(
        runner.life(P0),
        23,
        "Lifelink attacker's controller should gain 3 life"
    );
    assert_eq!(runner.life(P1), 17, "Defending player should take 3 damage");
}

/// CR 702.15b + CR 510.1b: Lifelink + first strike -- life gained in first strike step
#[test]
fn lifelink_first_strike_gains_life_in_first_step() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker_id = {
        let mut b = scenario.add_creature(P0, "Paladin", 2, 2);
        b.lifelink().first_strike();
        b.id()
    };
    let blocker_id = scenario.add_creature(P1, "Beast", 3, 3).id();
    let mut runner = scenario.build();

    run_combat(
        &mut runner,
        vec![attacker_id],
        vec![(blocker_id, attacker_id)],
    );

    // First strike step: 2/2 lifelink+first_strike deals 2 to 3/3 blocker. P0 gains 2 life.
    // 2 damage < 3 toughness, blocker survives first strike step.
    // Regular step: 3/3 blocker deals 3 to 2/2 attacker (lethal). Attacker dies.
    // The 2/2 first_strike already dealt damage so does NOT deal again in regular step.
    assert_eq!(
        runner.life(P0),
        22,
        "P0 should gain 2 life from first strike lifelink damage"
    );

    // Attacker (2/2) took 3 damage from blocker in regular step -- should die
    assert!(
        !runner.state().battlefield.contains(&attacker_id),
        "2/2 attacker should die to 3/3 blocker's regular damage"
    );
}

/// CR 702.9a + CR 702.3a: Flying blocked only by flying or reach
#[test]
fn flying_cannot_be_blocked_by_ground_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker_id = {
        let mut b = scenario.add_creature(P0, "Bird", 2, 2);
        b.flying();
        b.id()
    };
    let ground_blocker = scenario.add_creature(P1, "Bear", 2, 2).id();
    let mut runner = scenario.build();

    // Advance to DeclareAttackers
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker_id, AttackTarget::Player(P1))],
        })
        .expect("DeclareAttackers should succeed");

    // Ground creature cannot block flying creature
    let result = runner.act(GameAction::DeclareBlockers {
        assignments: vec![(ground_blocker, attacker_id)],
    });
    assert!(
        result.is_err(),
        "Ground creature should not be able to block flying attacker"
    );
}

/// CR 702.9a + CR 702.17a: Flying creature blocked by reach creature
#[test]
fn flying_blocked_by_reach_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker_id = {
        let mut b = scenario.add_creature(P0, "Bird", 2, 2);
        b.flying();
        b.id()
    };
    let reach_blocker = {
        let mut b = scenario.add_creature(P1, "Spider", 1, 3);
        b.reach();
        b.id()
    };
    let mut runner = scenario.build();

    run_combat(
        &mut runner,
        vec![attacker_id],
        vec![(reach_blocker, attacker_id)],
    );

    let state = runner.state();
    // Reach creature can block flying -- combat damage exchanged normally
    // 2/2 flying vs 1/3 reach: blocker takes 2 damage (survives), attacker takes 1 damage (survives)
    assert!(
        state.battlefield.contains(&reach_blocker),
        "1/3 reach blocker should survive 2 damage"
    );
    assert_eq!(
        state.objects[&reach_blocker].damage_marked, 2,
        "Reach blocker should have 2 damage from flying attacker"
    );
    assert_eq!(
        state.objects[&attacker_id].damage_marked, 1,
        "Flying attacker should have 1 damage from reach blocker"
    );
}

/// CR 702.15b + CR 702.19a: Trample + lifelink -- excess damage tramples and heals
#[test]
fn trample_lifelink_excess_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker_id = {
        let mut b = scenario.add_creature(P0, "Wurm", 5, 5);
        b.trample().lifelink();
        b.id()
    };
    let blocker_id = scenario.add_creature(P1, "Bear", 2, 2).id();
    let mut runner = scenario.build();

    run_combat(
        &mut runner,
        vec![attacker_id],
        vec![(blocker_id, attacker_id)],
    );

    // 5/5 trample+lifelink vs 2/2 blocker:
    // 2 damage to blocker (lethal), 3 tramples to player
    // Lifelink: controller gains 5 life (total damage dealt = 2 + 3)
    assert!(
        !runner.state().battlefield.contains(&blocker_id),
        "2/2 blocker should die"
    );
    assert_eq!(
        runner.life(P1),
        17,
        "3 trample damage should go to defending player"
    );
    assert_eq!(
        runner.life(P0),
        25,
        "P0 should gain 5 life from lifelink (2 to blocker + 3 to player)"
    );

    // Snapshot for regression anchoring
    insta::assert_json_snapshot!("keywords_trample_lifelink_excess", runner.snapshot());
}

/// CR 702.12a: Vigilance -- attacker doesn't tap
#[test]
fn vigilance_attacker_does_not_tap() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker_id = {
        let mut b = scenario.add_creature(P0, "Knight", 2, 2);
        b.vigilance();
        b.id()
    };
    let mut runner = scenario.build();

    // Advance to DeclareAttackers
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker_id, AttackTarget::Player(P1))],
        })
        .expect("Vigilance creature should be able to attack");

    assert!(
        !runner.state().objects[&attacker_id].tapped,
        "Vigilance attacker should NOT be tapped after declaring attack"
    );
}
