//! Repro for the reported double-fire: casting a search tutor that puts a land
//! onto the battlefield (Prishe's Wanderings / Nature's Lore pattern) fires each
//! landfall observer trigger twice for the single land entry.
//!
//! A landfall permanent ("Whenever a land enters the battlefield under your
//! control, <effect>") must produce EXACTLY ONE trigger per land entry.

use engine::game::scenario::{GameScenario, P0};
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const LANDFALL_ORACLE: &str =
    "Whenever a land enters the battlefield under your control, draw a card.";

const NATURES_LORE_ORACLE: &str =
    "Search your library for a Forest card, put that card onto the battlefield, then shuffle.";

fn seed_forest_on_library_top(
    runner: &mut engine::game::scenario::GameRunner,
) -> engine::types::identifiers::ObjectId {
    use engine::types::card_type::CoreType;
    let card_id = engine::types::identifiers::CardId(runner.state().next_object_id);
    let id = engine::game::zones::create_object(
        runner.state_mut(),
        card_id,
        P0,
        "Forest".to_string(),
        Zone::Library,
    );
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Land);
    obj.base_card_types = obj.card_types.clone();
    obj.card_types.subtypes.push("Forest".to_string());
    runner.state_mut().players[P0.0 as usize]
        .library
        .insert(0, id);
    id
}

#[test]
fn landfall_fires_once_per_land_etb_from_search_tutor() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::Green);

    // A landfall permanent on the battlefield.
    scenario
        .add_creature_from_oracle(P0, "Landfall Scout", 1, 1, LANDFALL_ORACLE)
        .id();

    let natures_lore = scenario
        .add_spell_to_hand_from_oracle(P0, "Nature's Lore", false, NATURES_LORE_ORACLE)
        .id();

    let mut runner = scenario.build();
    let forest = seed_forest_on_library_top(&mut runner);
    runner.cast(natures_lore).search_first_legal().resolve();

    // The land entered the battlefield exactly once.
    assert_eq!(
        runner.state().objects[&forest].zone,
        Zone::Battlefield,
        "Nature's Lore must put the searched Forest onto the battlefield"
    );

    // Drain the search tutor to a priority window so deferred triggers settle.
    runner.advance_until_stack_empty();

    // Exactly ONE landfall trigger must have fired for the single land entry.
    let landfall_count = runner
        .state()
        .deferred_triggers
        .iter()
        .filter(|ctx| {
            ctx.pending
                .description
                .as_deref()
                .unwrap_or("")
                .contains("Whenever a land enters")
        })
        .count();
    assert_eq!(
        landfall_count,
        1,
        "landfall must fire exactly once per land ETB; deferred=[{:?}]",
        runner
            .state()
            .deferred_triggers
            .iter()
            .map(|c| c.pending.description.clone().unwrap_or_default())
            .collect::<Vec<_>>()
    );
}

/// The reported shape: TWO separate landfall permanents on the battlefield and
/// one land entering via a search tutor. Each landfall permanent must fire
/// exactly ONE trigger for the single land — two total, never four (the
/// double-fire reported for Sazh's Chocobo + Bird token).
#[test]
fn two_landfall_sources_fire_once_each_for_single_search_land() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::Green);

    scenario
        .add_creature_from_oracle(P0, "Landfall Scout", 1, 1, LANDFALL_ORACLE)
        .id();
    scenario
        .add_creature_from_oracle(P0, "Landfall Warden", 2, 2, LANDFALL_ORACLE)
        .id();

    let natures_lore = scenario
        .add_spell_to_hand_from_oracle(P0, "Nature's Lore", false, NATURES_LORE_ORACLE)
        .id();

    let mut runner = scenario.build();
    let forest = seed_forest_on_library_top(&mut runner);
    runner.cast(natures_lore).search_first_legal().resolve();
    runner.advance_until_stack_empty();

    // The land entered the battlefield exactly once.
    assert_eq!(
        runner.state().objects[&forest].zone,
        Zone::Battlefield,
        "Nature's Lore must put the searched Forest onto the battlefield"
    );

    let landfall_count = runner
        .state()
        .deferred_triggers
        .iter()
        .filter(|ctx| {
            ctx.pending
                .description
                .as_deref()
                .unwrap_or("")
                .contains("Whenever a land enters")
        })
        .count();
    assert_eq!(
        landfall_count,
        2,
        "two landfall sources must fire exactly once each for the single land ETB; deferred=[{:?}]",
        runner
            .state()
            .deferred_triggers
            .iter()
            .map(|c| (
                c.pending.source_id,
                c.pending.description.clone().unwrap_or_default()
            ))
            .collect::<Vec<_>>()
    );
}
