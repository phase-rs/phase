//! Regression for GitHub issue #1360 — Power Fist.
//!
//! Oracle: Equipped creature has trample and "Whenever this creature deals
//! combat damage to a player, put that many +1/+1 counters on it."
//!
//! The Discord report: the trigger does not fire when the equipped creature is
//! blocked but still deals combat damage to a player (trample excess).

use engine::game::effects::attach::attach_to;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle_static::parse_static_line_multi;
use engine::types::ability::{ContinuousModification, FilterProp, TypedFilter};
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;

use engine::game::combat::AttackTarget;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;

use super::rules::run_combat;

const POWER_FIST_ORACLE: &str = "Equipped creature has trample and \"Whenever this creature deals combat damage to a player, put that many +1/+1 counters on it.\"\nEquip {2}";

fn p1p1_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .expect("object on battlefield")
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn setup() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let attacker = scenario.add_creature(P0, "Equipped Bruiser", 5, 5).id();
    let blocker = scenario.add_creature(P1, "Blocker", 2, 2).id();
    // Artifact — Equipment (not a creature): a 0/0 creature would die to SBAs
    // after priority passes and strip the granted trample/trigger before combat.
    let equipment = scenario
        .add_creature(P0, "Power Fist", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text(POWER_FIST_ORACLE)
        .id();

    let mut runner = scenario.build();
    attach_to(runner.state_mut(), equipment, attacker);
    evaluate_layers(runner.state_mut());

    (runner, attacker, blocker)
}

#[test]
fn equipment_granted_trample_survives_repeated_layer_evaluation() {
    let (mut runner, attacker, _) = setup();
    assert!(
        runner
            .state()
            .objects
            .get(&attacker)
            .unwrap()
            .has_keyword(&Keyword::Trample),
        "first layer pass should grant trample from Power Fist"
    );
    evaluate_layers(runner.state_mut());
    assert!(
        runner
            .state()
            .objects
            .get(&attacker)
            .unwrap()
            .has_keyword(&Keyword::Trample),
        "second layer pass must re-apply equipment-granted trample (CR 613.7)"
    );
}

#[test]
fn power_fist_static_parses_trample_and_granted_trigger() {
    let line = "Equipped creature has trample and \"Whenever this creature deals combat damage to a player, put that many +1/+1 counters on it.\"";
    let defs = parse_static_line_multi(line);
    assert_eq!(
        defs.len(),
        1,
        "expected one continuous static, got {defs:?}"
    );
    let def = &defs[0];
    assert_eq!(
        def.affected,
        Some(engine::types::ability::TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::EquippedBy]),
        ))
    );
    assert!(
        def.modifications.iter().any(|m| {
            matches!(m, ContinuousModification::AddKeyword { keyword } if keyword == &Keyword::Trample)
        }),
        "expected AddKeyword(Trample), got {:?}",
        def.modifications
    );
    assert!(
        def.modifications
            .iter()
            .any(|m| { matches!(m, ContinuousModification::GrantTrigger { .. }) }),
        "expected GrantTrigger for combat-damage counters, got {:?}",
        def.modifications
    );
}

/// CR 702.19b: Direct keyword grant control — trample on the attacker must
/// split damage when blocked by a 2/2.
#[test]
fn trample_splits_damage_when_blocked_single_blocker() {
    let (mut runner, attacker, blocker) = setup();
    {
        let obj = runner.state_mut().objects.get_mut(&attacker).unwrap();
        obj.base_keywords.push(Keyword::Trample);
        obj.keywords.push(Keyword::Trample);
    }
    let life_before = runner.life(P1);
    run_combat(&mut runner, vec![attacker], vec![(blocker, attacker)]);
    assert_eq!(runner.life(P1), life_before - 3);
}

fn assert_has_trample(runner: &GameRunner, attacker: ObjectId, step: &str) {
    assert!(
        runner
            .state()
            .objects
            .get(&attacker)
            .unwrap()
            .has_keyword(&Keyword::Trample),
        "{step}: equipped creature must have trample from Power Fist"
    );
}

/// Step through combat and verify trample is present when damage is assigned.
#[test]
fn equipment_trample_present_through_combat_until_damage() {
    let (mut runner, attacker, blocker) = setup();
    assert_has_trample(&runner, attacker, "after setup");

    runner.pass_both_players();
    assert_has_trample(&runner, attacker, "after pre-combat priority");

    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("declare attackers");
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner.pass_both_players();
    }
    assert_has_trample(&runner, attacker, "after declare attackers");

    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) {
        runner
            .act(GameAction::DeclareBlockers {
                assignments: vec![(blocker, attacker)],
            })
            .expect("declare blockers");
    }
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner.pass_both_players();
    }
    assert_has_trample(&runner, attacker, "after declare blockers");

    // Auto-advance should reach combat damage; if still at priority, pass through.
    for _ in 0..6 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            runner.pass_both_players();
        } else {
            break;
        }
    }
    assert_has_trample(&runner, attacker, "at combat damage step");
}

/// CR 702.19b + CR 603.2: 5/5 with trample blocked by 2/2 assigns 2 to the
/// blocker and 3 to the defending player; the granted trigger must fire for 3.
#[test]
fn power_fist_trigger_fires_on_trample_damage_to_player() {
    let (mut runner, attacker, blocker) = setup();
    assert!(
        runner
            .state()
            .objects
            .get(&attacker)
            .unwrap()
            .has_keyword(&Keyword::Trample),
        "layers must grant trample to the equipped creature before combat damage"
    );
    let life_before = runner.life(P1);

    run_combat(&mut runner, vec![attacker], vec![(blocker, attacker)]);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P1),
        life_before - 3,
        "CR 510.1c: 3 trample damage should reach the defending player"
    );
    assert_eq!(
        p1p1_counters(&runner, attacker),
        3,
        "CR 603.2: Power Fist trigger should put counters equal to combat damage dealt to a player"
    );
}
