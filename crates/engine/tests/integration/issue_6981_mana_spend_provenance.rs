//! Regression for issue #6981: mana's conditional spell effects must remain on
//! the individual mana unit through payment and permanent entry.

use engine::game::commander::record_commander_cast;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, ManaChoice, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaSpellGrant, ManaType, ManaUnit};
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
        .find(|ability| {
            matches!(
                &*ability.effect,
                Effect::Mana { grants, .. } if !grants.is_empty()
            )
        })
        .expect("card must parse a mana ability with a conditional spell grant");
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

// Issue #8076: "If this/that mana is spent to cast [a creature spell], that
// creature enters with an additional +1/+1 counter on it" — verbatim Oracle text.
const BIOPHAGUS_ORACLE: &str = "Genomic Enhancement — {T}: Add one mana of any color. If this mana is spent to cast a creature spell, that creature enters with an additional +1/+1 counter on it.";
const ANIMAL_ATTENDANT_ORACLE: &str = "{T}: Add one mana of any color. If that mana is spent to cast a non-Human creature spell, that creature enters with an additional +1/+1 counter on it.";
const GUILDMAGES_FORUM_ORACLE: &str = "{T}: Add {C}.\n{1}, {T}: Add one mana of any color. If that mana is spent on a multicolored creature spell, that creature enters with an additional +1/+1 counter on it.";

fn green_pip() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Green],
        generic: 0,
    }
}

/// CR 605.3b: Activate `source`'s mana ability through `apply` and answer its
/// any-color prompt with green, leaving the produced unit in the pool.
fn tap_for_green(runner: &mut GameRunner, source: ObjectId, ability_index: usize) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activating the mana ability must succeed");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseManaColor { .. }
        ),
        "any-color mana ability must prompt for a color, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Green),
            count: 1,
        })
        .expect("choosing green must succeed");
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Green),
        1,
        "the mana ability must add exactly one green mana"
    );
}

/// Cast `creature` (cost {G}) from the pool, resolve it, and return its
/// +1/+1 counter count on the battlefield. Asserts the pool paid the cost.
fn cast_and_count_counters(runner: &mut GameRunner, creature: ObjectId) -> u32 {
    runner
        .cast(creature)
        .resolve()
        .assert_zone(&[creature], Zone::Battlefield);
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        0,
        "the {{G}} cost must be paid from the pooled mana"
    );
    runner.state().objects[&creature]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or_default()
}

/// CR 106.6a + CR 614.1c: Biophagus's mana spent on a creature spell makes
/// that creature enter with one additional +1/+1 counter; green mana from
/// another source does not.
#[test]
fn biophagus_mana_spent_on_creature_spell_adds_counter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let biophagus = scenario
        .add_creature_from_oracle(P0, "Biophagus", 1, 3, BIOPHAGUS_ORACLE)
        .id();
    let creature = scenario
        .add_creature_to_hand(P0, "Provenance Bear", 2, 2)
        .with_mana_cost(green_pip())
        .id();
    let mut runner = scenario.build();

    tap_for_green(&mut runner, biophagus, 0);
    assert!(
        runner.state().objects[&biophagus].tapped,
        "the {{T}} cost must tap Biophagus"
    );
    assert_eq!(
        cast_and_count_counters(&mut runner, creature),
        1,
        "Biophagus mana must add one +1/+1 counter to the creature it cast"
    );

    // Control: the same creature paid with green mana from another source.
    let mut control = GameScenario::new();
    control.at_phase(Phase::PreCombatMain);
    control.add_creature_from_oracle(P0, "Biophagus", 1, 3, BIOPHAGUS_ORACLE);
    let creature = control
        .add_creature_to_hand(P0, "Provenance Bear", 2, 2)
        .with_mana_cost(green_pip())
        .id();
    control.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![])],
    );
    let mut control_runner = control.build();
    assert_eq!(
        cast_and_count_counters(&mut control_runner, creature),
        0,
        "mana not produced by Biophagus must not add a counter"
    );
}

/// CR 106.6 + CR 205.3m: Animal Attendant's rider applies only to non-Human
/// creature spells.
#[test]
fn animal_attendant_mana_adds_counter_only_to_non_human_creatures() {
    for (subtype, expected) in [("Human", 0), ("Bear", 1)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let attendant = scenario
            .add_creature_from_oracle(P0, "Animal Attendant", 2, 2, ANIMAL_ATTENDANT_ORACLE)
            .id();
        let creature = scenario
            .add_creature_to_hand(P0, "Provenance Creature", 2, 2)
            .with_mana_cost(green_pip())
            .with_subtypes(vec![subtype])
            .id();
        let mut runner = scenario.build();

        tap_for_green(&mut runner, attendant, 0);
        assert_eq!(
            cast_and_count_counters(&mut runner, creature),
            expected,
            "Animal Attendant mana on a {subtype} creature spell"
        );
    }
}

/// CR 106.6 + CR 105.2: Guildmages' Forum's rider applies only to
/// multicolored creature spells.
#[test]
fn guildmages_forum_mana_adds_counter_only_to_multicolored_creatures() {
    for (colors, expected) in [
        (vec![ManaColor::Green], 0),
        (vec![ManaColor::Green, ManaColor::White], 1),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let forum = scenario
            .add_land_from_oracle(P0, "Guildmages' Forum", GUILDMAGES_FORUM_ORACLE)
            .id();
        let creature = scenario
            .add_creature_to_hand(P0, "Provenance Creature", 2, 2)
            .with_mana_cost(green_pip())
            .with_color(colors.clone())
            .id();
        // Pays the activation's {1}.
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(
                ManaType::Colorless,
                ObjectId(0),
                false,
                vec![],
            )],
        );
        let mut runner = scenario.build();

        tap_for_green(&mut runner, forum, 1);
        assert_eq!(
            runner.state().players[P0.0 as usize].mana_pool.total(),
            1,
            "the activation's {{1}} must be paid, leaving only the Forum's mana"
        );
        assert_eq!(
            cast_and_count_counters(&mut runner, creature),
            expected,
            "Guildmages' Forum mana on a creature spell colored {colors:?}"
        );
    }
}
