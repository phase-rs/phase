use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::counter::CounterType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SUNDERING_ARCHAIC_ORACLE: &str = "Converge — When this creature enters, exile target nonland permanent an opponent controls with mana value less than or equal to the number of colors of mana spent to cast this creature.\n{2}: Put target card from a graveyard on the bottom of its owner's library.";

const RANCOROUS_ARCHAIC_ORACLE: &str = "Trample, reach\nConverge — This creature enters with a +1/+1 counter on it for each color of mana spent to cast it.";

fn mana_pool(types: &[ManaType]) -> Vec<ManaUnit> {
    types
        .iter()
        .copied()
        .map(|mana_type| ManaUnit::new(mana_type, ObjectId(0), false, vec![]))
        .collect()
}

fn add_mana_value_creature(scenario: &mut GameScenario, name: &str, mana_value: u32) -> ObjectId {
    let mut creature = scenario.add_creature(P1, name, 1, 1);
    creature.with_mana_cost(ManaCost::generic(mana_value));
    creature.id()
}

fn plus_one_counters(state: &GameState, object: ObjectId) -> u32 {
    state.objects[&object]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn add_sundering_archaic(scenario: &mut GameScenario) -> ObjectId {
    let mut archaic = scenario.add_creature_to_hand_from_oracle(
        P0,
        "Sundering Archaic",
        3,
        3,
        SUNDERING_ARCHAIC_ORACLE,
    );
    archaic.with_mana_cost(ManaCost::generic(6));
    archaic.id()
}

fn add_rancorous_archaic(scenario: &mut GameScenario) -> ObjectId {
    let mut archaic = scenario.add_creature_to_hand_from_oracle(
        P0,
        "Rancorous Archaic",
        2,
        2,
        RANCOROUS_ARCHAIC_ORACLE,
    );
    archaic.with_mana_cost(ManaCost::generic(5));
    archaic.id()
}

fn add_spell_copier(scenario: &mut GameScenario) -> ObjectId {
    let mut copier =
        scenario.add_spell_to_hand_from_oracle(P0, "Spell Copier", true, "Copy target spell.");
    copier.with_mana_cost(ManaCost::generic(0));
    copier.id()
}

#[test]
fn sundering_archaic_one_color_survives_trigger_collection() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let archaic = add_sundering_archaic(&mut scenario);
    let mana_value_one = add_mana_value_creature(&mut scenario, "One-Drop", 1);
    let mana_value_two = add_mana_value_creature(&mut scenario, "Two-Drop", 2);
    scenario.with_mana_pool(
        P0,
        mana_pool(&[
            ManaType::White,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ]),
    );
    let mut runner = scenario.build();

    // CR 601.2h + CR 400.7d + CR 603.6a: the colored mana paid for the
    // creature spell remains available to the permanent's ETB ability.
    let outcome = runner
        .cast(archaic)
        .target_objects(&[mana_value_two, mana_value_one])
        .resolve();

    outcome.assert_zone(&[mana_value_one], Zone::Exile);
    outcome.assert_zone(&[mana_value_two], Zone::Battlefield);
}

#[test]
fn sundering_archaic_two_colors_survive_trigger_collection() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let archaic = add_sundering_archaic(&mut scenario);
    let mana_value_two = add_mana_value_creature(&mut scenario, "Two-Drop", 2);
    let mana_value_three = add_mana_value_creature(&mut scenario, "Three-Drop", 3);
    scenario.with_mana_pool(
        P0,
        mana_pool(&[
            ManaType::White,
            ManaType::Blue,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ]),
    );
    let mut runner = scenario.build();

    let outcome = runner
        .cast(archaic)
        .target_objects(&[mana_value_three, mana_value_two])
        .resolve();

    outcome.assert_zone(&[mana_value_two], Zone::Exile);
    outcome.assert_zone(&[mana_value_three], Zone::Battlefield);
}

#[test]
fn sundering_archaic_spell_copy_has_zero_colors() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let archaic = add_sundering_archaic(&mut scenario);
    let copier = add_spell_copier(&mut scenario);
    let victim_one = add_mana_value_creature(&mut scenario, "First One-Drop", 1);
    let victim_two = add_mana_value_creature(&mut scenario, "Second One-Drop", 1);
    scenario.with_mana_pool(
        P0,
        mana_pool(&[
            ManaType::White,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ]),
    );
    let mut runner = scenario.build();

    runner.cast(archaic).commit();
    runner
        .cast(copier)
        .target_objects(&[archaic, victim_one, victim_two])
        .resolve();

    // CR 707.10 + CR 707.10f: a spell copy was not cast and becomes a token
    // permanent, so it has zero colors spent while the original retains one.
    let archaics_on_battlefield = runner
        .state()
        .objects
        .values()
        .filter(|object| object.name == "Sundering Archaic" && object.zone == Zone::Battlefield)
        .count();
    let copied_archaic = runner
        .state()
        .objects
        .values()
        .find(|object| {
            object.name == "Sundering Archaic"
                && object.zone == Zone::Battlefield
                && object.is_token
        })
        .map(|object| object.id)
        .expect("the copied permanent spell must become a token permanent");
    let exiled_victims = [victim_one, victim_two]
        .iter()
        .filter(|victim| runner.state().objects[victim].zone == Zone::Exile)
        .count();
    let remaining_victims = [victim_one, victim_two]
        .iter()
        .filter(|victim| runner.state().objects[victim].zone == Zone::Battlefield)
        .count();

    assert_eq!(archaics_on_battlefield, 2);
    assert_ne!(archaic, copied_archaic);
    assert_eq!(
        runner.state().objects[&archaic]
            .colors_spent_to_cast
            .distinct_colors(),
        1
    );
    assert_eq!(
        runner.state().objects[&copied_archaic]
            .colors_spent_to_cast
            .distinct_colors(),
        0
    );
    assert_eq!(exiled_victims, 1);
    assert_eq!(remaining_victims, 1);
}

#[test]
fn rancorous_archaic_replacement_path_keeps_original_colors_and_copy_zero() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let archaic = add_rancorous_archaic(&mut scenario);
    let copier = add_spell_copier(&mut scenario);
    scenario.with_mana_pool(
        P0,
        mana_pool(&[
            ManaType::White,
            ManaType::Blue,
            ManaType::Black,
            ManaType::Red,
            ManaType::Green,
        ]),
    );
    let mut runner = scenario.build();

    runner.cast(archaic).commit();
    runner.cast(copier).target_object(archaic).resolve();

    // CR 614.1c + CR 707.10 + CR 707.10f: the original's five-color payment
    // determines its enters-with replacement, while the uncast copy becomes a
    // token permanent with no cast-payment record.
    let copied_archaic = runner
        .state()
        .objects
        .values()
        .find(|object| {
            object.name == "Rancorous Archaic"
                && object.zone == Zone::Battlefield
                && object.is_token
        })
        .map(|object| object.id)
        .expect("the copied permanent spell must become a token permanent");

    assert_eq!(runner.state().objects[&archaic].zone, Zone::Battlefield);
    assert!(!runner.state().objects[&archaic].is_token);
    assert_eq!(plus_one_counters(runner.state(), archaic), 5);
    assert_eq!(plus_one_counters(runner.state(), copied_archaic), 0);
}
