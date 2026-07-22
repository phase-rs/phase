//! Integration tests for authored executable scenarios (in-memory).

use cr_suite::runner::run_scenario;
use cr_suite::schema::{
    AssertionSpec, CreatureSpec, LightningBoltSpec, PlayerSetup, ScenarioFile, ScenarioStatus,
    ScenarioStep, SetupSpec,
};

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
