//! Regression for issue #6981: mana's conditional spell effects must remain on
//! the individual mana unit through payment and permanent entry.

use engine::game::commander::record_commander_cast;
use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::Effect;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaSpellGrant, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

const BOSEIJU_ORACLE: &str = "Boseiju enters tapped.\n{T}, Pay 2 life: Add {C}. If that mana is spent on an instant or sorcery spell, that spell can't be countered.";
const OPAL_PALACE_ORACLE: &str = "{T}: Add {C}.\n{1}, {T}: Add one mana of any color in your commander's color identity. If you spend this mana to cast your commander, it enters with a number of additional +1/+1 counters on it equal to the number of times it's been cast from the command zone this game.";

fn mana_grants(oracle: &str, name: &str) -> Vec<ManaSpellGrant> {
    let parsed = parse_oracle_text(oracle, name, &[], &["Land".to_string()], &[]);
    let ability = parsed
        .abilities
        .iter()
        .find(|ability| matches!(*ability.effect, Effect::Mana { .. }))
        .expect("card must parse an activated mana ability");
    let Effect::Mana { grants, .. } = &*ability.effect else {
        unreachable!("mana ability search must produce a Mana effect");
    };
    grants.clone()
}

fn mana_with_grants(color: ManaType, source: ObjectId, grants: Vec<ManaSpellGrant>) -> ManaUnit {
    let mut mana = ManaUnit::new(color, source, false, vec![]);
    mana.grants = grants;
    mana
}

fn spell_cant_be_countered(state: &engine::types::game_state::GameState, spell: ObjectId) -> bool {
    state.objects[&spell]
        .static_definitions
        .iter_unchecked()
        .any(|definition| definition.mode == StaticMode::CantBeCountered)
}

#[test]
fn boseiju_grant_matches_instants_and_not_creature_spells() {
    let grants = mana_grants(BOSEIJU_ORACLE, "Boseiju, Who Shelters All");
    assert!(matches!(
        grants.as_slice(),
        [ManaSpellGrant::CantBeCountered { .. }]
    ));

    let mut instant_scenario = GameScenario::new();
    instant_scenario.at_phase(Phase::PreCombatMain);
    let boseiju = instant_scenario
        .add_land_from_oracle(P0, "Boseiju, Who Shelters All", BOSEIJU_ORACLE)
        .id();
    let instant = instant_scenario
        .add_spell_to_hand(P0, "Provenance Instant", true)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    instant_scenario.with_mana_pool(
        P0,
        vec![mana_with_grants(
            ManaType::Colorless,
            boseiju,
            grants.clone(),
        )],
    );
    let mut instant_runner = instant_scenario.build();
    let instant_commit = instant_runner.cast(instant).commit();
    assert!(
        spell_cant_be_countered(instant_commit.state(), instant),
        "Boseiju mana must make an instant spell uncounterable"
    );

    let mut creature_scenario = GameScenario::new();
    creature_scenario.at_phase(Phase::PreCombatMain);
    let boseiju = creature_scenario
        .add_land_from_oracle(P0, "Boseiju, Who Shelters All", BOSEIJU_ORACLE)
        .id();
    let creature = creature_scenario
        .add_creature_to_hand(P0, "Provenance Creature", 1, 1)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    creature_scenario.with_mana_pool(
        P0,
        vec![mana_with_grants(ManaType::Colorless, boseiju, grants)],
    );
    let mut creature_runner = creature_scenario.build();
    let creature_commit = creature_runner.cast(creature).commit();
    assert!(
        !spell_cant_be_countered(creature_commit.state(), creature),
        "Boseiju mana must not make a creature spell uncounterable"
    );
}

#[test]
fn opal_palace_grant_counts_the_current_command_zone_cast_at_entry() {
    let grants = mana_grants(OPAL_PALACE_ORACLE, "Opal Palace");
    assert!(matches!(
        grants.as_slice(),
        [ManaSpellGrant::EntersWithCounters {
            counter_type: CounterType::Plus1Plus1,
            ..
        }]
    ));

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let opal = scenario
        .add_land_from_oracle(P0, "Opal Palace", OPAL_PALACE_ORACLE)
        .id();
    let commander = scenario
        .add_creature(P0, "Provenance Commander", 2, 2)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    scenario.with_commander(commander);
    scenario.with_mana_pool(
        P0,
        vec![
            mana_with_grants(ManaType::Colorless, opal, grants),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    let mut runner = scenario.build();
    runner.state_mut().format_config.command_zone = true;
    record_commander_cast(runner.state_mut(), commander);
    record_commander_cast(runner.state_mut(), commander);

    runner
        .cast(commander)
        .resolve()
        .assert_zone(&[commander], Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&commander]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or_default(),
        3,
        "Opal Palace includes the commander spell currently being cast"
    );
}
