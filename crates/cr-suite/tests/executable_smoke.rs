//! Integration tests for authored executable scenarios (in-memory).

use cr_suite::runner::run_scenario;
use cr_suite::schema::{
    AssertionSpec, CreatureSpec, LightningBoltSpec, PlayerSetup, ScenarioFile, ScenarioStatus,
    ScenarioStep, SetupSpec,
};

use cr_suite::assert::{
    assert_attacker_declared, assert_in_command_zone, assert_library_count, assert_player_poison,
    assert_priority_player, stack_is_empty, HandleMap,
};
use engine::game::scenario::GameScenario;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

fn base_two_player(p0_life: i32, p1_life: i32) -> SetupSpec {
    SetupSpec {
        phase: "PreCombatMain".into(),
        players: vec![
            PlayerSetup {
                id: 0,
                life: p0_life,
                hand: vec![],
                library_top: vec![],
            },
            PlayerSetup {
                id: 1,
                life: p1_life,
                hand: vec![],
                library_top: vec![],
            },
        ],
        creatures: vec![],
        lightning_bolts: vec![],
        seed: Some(42),
    }
}

fn executable(
    rule: &str,
    section: u32,
    text: &str,
    setup: SetupSpec,
    steps: Vec<ScenarioStep>,
    assertions: Vec<AssertionSpec>,
) -> ScenarioFile {
    ScenarioFile {
        rule: rule.into(),
        section,
        title: format!("CR {rule}"),
        status: ScenarioStatus::Executable,
        text: text.into(),
        notes: String::new(),
        setup: Some(setup),
        steps,
        assertions,
        meta: Default::default(),
    }
}

#[test]
fn cr_704_5a_bolt_to_zero_life_loses() {
    let mut setup = base_two_player(20, 3);
    setup.lightning_bolts.push(LightningBoltSpec {
        id: "bolt".into(),
        player: 0,
    });
    let scenario = executable(
        "704.5a",
        704,
        "A player with 0 or less life loses the game.",
        setup,
        vec![
            ScenarioStep::CastLightningBolt {
                spell: "bolt".into(),
                target_player: Some(1),
                target_creature: None,
            },
            ScenarioStep::ResolveTop,
        ],
        vec![
            AssertionSpec::PlayerLife { player: 1, life: 0 },
            AssertionSpec::GameOver { winner: Some(0) },
        ],
    );
    run_scenario(&scenario).expect("704.5a should pass via DealDamage");
}

#[test]
fn cr_704_5f_zero_toughness_dies() {
    let mut setup = base_two_player(20, 20);
    setup.creatures.push(CreatureSpec {
        id: "zero_t".into(),
        player: 0,
        name: "Zero Toughness".into(),
        power: 1,
        toughness: 0,
        keywords: vec![],
        summoning_sickness: false,
    });
    let scenario = executable(
        "704.5f",
        704,
        "Creature with toughness 0 or less is put into its owner's graveyard.",
        setup,
        vec![ScenarioStep::CheckSbas],
        vec![AssertionSpec::CreatureInGraveyard {
            creature: "zero_t".into(),
        }],
    );
    run_scenario(&scenario).expect("704.5f should pass");
}

#[test]
fn cr_704_5g_bolt_lethal_destroys() {
    let mut setup = base_two_player(20, 20);
    setup.creatures.push(CreatureSpec {
        id: "bear".into(),
        player: 1,
        name: "Bear".into(),
        power: 2,
        toughness: 2,
        keywords: vec![],
        summoning_sickness: false,
    });
    setup.lightning_bolts.push(LightningBoltSpec {
        id: "bolt".into(),
        player: 0,
    });
    let scenario = executable(
        "704.5g",
        704,
        "Creature with lethal damage is destroyed.",
        setup,
        vec![
            ScenarioStep::CastLightningBolt {
                spell: "bolt".into(),
                target_player: None,
                target_creature: Some("bear".into()),
            },
            ScenarioStep::ResolveTop,
        ],
        vec![AssertionSpec::CreatureInGraveyard {
            creature: "bear".into(),
        }],
    );
    run_scenario(&scenario).expect("704.5g should pass via DealDamage");
}

#[test]
fn cr_104_1_bolt_ends_game() {
    let mut setup = base_two_player(20, 3);
    setup.lightning_bolts.push(LightningBoltSpec {
        id: "bolt".into(),
        player: 0,
    });
    let scenario = executable(
        "104.1",
        104,
        "A game ends immediately when a player wins.",
        setup,
        vec![
            ScenarioStep::CastLightningBolt {
                spell: "bolt".into(),
                target_player: Some(1),
                target_creature: None,
            },
            ScenarioStep::ResolveTop,
        ],
        vec![AssertionSpec::GameOver { winner: Some(0) }],
    );
    run_scenario(&scenario).expect("104.1 should pass via DealDamage");
}

// --- Assertion-helper discriminating tests (CR 117 / 401 / 122 / 408 / 508) ---
//
// These cover assertion kinds not reached by the executable on-disk fixtures
// (which are deferred pending runner steps). Each pairs a positive case with a
// negative case so the comparison logic fails if reverted.

fn build_runner_at(phase: Phase) -> engine::game::scenario::GameRunner {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(phase);
    scenario.with_life(PlayerId(0), 20);
    scenario.with_life(PlayerId(1), 20);
    scenario.build()
}

#[test]
fn priority_player_matches_active_player_and_rejects_other() {
    let runner = build_runner_at(Phase::PreCombatMain);
    // at_phase sets priority_player = active_player (CR 117.3a start-of-phase).
    let holder = runner.state().priority_player;
    let other = PlayerId(if holder.0 == 0 { 1 } else { 0 });
    assert!(assert_priority_player(&runner, holder).is_ok());
    assert!(
        assert_priority_player(&runner, other).is_err(),
        "must reject the wrong priority player"
    );
}

#[test]
fn library_count_reads_exact_size_and_rejects_off_by_one() {
    let runner = build_runner_at(Phase::PreCombatMain);
    let actual = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == PlayerId(0))
        .map(|p| p.library.len())
        .unwrap();
    assert!(assert_library_count(&runner, PlayerId(0), actual).is_ok());
    assert!(
        assert_library_count(&runner, PlayerId(0), actual + 1).is_err(),
        "must reject an off-by-one library count"
    );
}

#[test]
fn player_poison_reads_counter_field() {
    let mut runner = build_runner_at(Phase::PreCombatMain);
    // Direct field write in a unit test (not a scenario step) to exercise the
    // read path; the runner has no production poison source yet.
    if let Some(p) = runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == PlayerId(1))
    {
        p.poison_counters = 3;
    }
    assert!(assert_player_poison(&runner, PlayerId(1), 3).is_ok());
    assert!(
        assert_player_poison(&runner, PlayerId(1), 4).is_err(),
        "must reject the wrong poison count"
    );
    assert!(
        assert_player_poison(&runner, PlayerId(0), 3).is_err(),
        "player 0 has no poison"
    );
}

#[test]
fn stack_is_empty_on_fresh_state() {
    let runner = build_runner_at(Phase::PreCombatMain);
    assert!(
        stack_is_empty(&runner).is_ok(),
        "fresh state has empty stack"
    );
}

#[test]
fn command_zone_rejects_a_battlefield_creature() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let id = scenario.add_creature(PlayerId(0), "Bear", 2, 2).id();
    let runner = scenario.build();
    let mut handles = HandleMap::new();
    handles.insert("bear".to_string(), id);
    // A battlefield creature is NOT in the command zone (reach-guard: the handle
    // resolves to a real object, so this is not a vacuous "unknown handle" pass).
    assert!(
        assert_in_command_zone(&runner, &handles, "bear").is_err(),
        "battlefield creature must not report as in the command zone"
    );
    assert!(
        assert_in_command_zone(&runner, &handles, "missing").is_err(),
        "unknown handle must error"
    );
}

#[test]
fn attacker_declared_errs_without_combat() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let id = scenario.add_creature(PlayerId(0), "Bear", 2, 2).id();
    let runner = scenario.build();
    let mut handles = HandleMap::new();
    handles.insert("bear".to_string(), id);
    // No combat in progress → the assertion must fail (it must not silently pass
    // when CombatState is None). Handle resolves to a real object (reach-guard).
    let err = assert_attacker_declared(&runner, &handles, "bear")
        .expect_err("must fail when no combat is in progress");
    assert!(err.detail.contains("no combat"), "detail: {}", err.detail);
}
