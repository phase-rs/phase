//! Regression for issue #2871: Currency Converter's {T} ability must not create
//! a token when no card is exiled with it.
//!
//! https://github.com/phase-rs/phase/issues/2871

use engine::game::scenario::{GameScenario, P0};
use engine::game::zones::create_object;
use engine::types::card_type::CoreType;
use engine::types::game_state::{ExileLink, ExileLinkKind};
use engine::types::identifiers::CardId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const CURRENCY_CONVERTER_ABILITY: &str = "{T}: Put a card exiled with this artifact into its owner's graveyard. If it's a land card, create a Treasure token. If it's a nonland card, create a 2/2 black Rogue creature token.";
const INVERSE_CARD_TYPE_RIDER_ABILITY: &str = "{T}: Put a card exiled with this artifact into its owner's graveyard. If it's a nonland card, create a 2/2 black Rogue creature token. If it's a land card, create a Treasure token.";

#[test]
fn issue_2871_currency_converter_tap_creates_no_token_without_exiled_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let converter = scenario
        .add_creature(P0, "Currency Converter", 0, 0)
        .as_artifact()
        .from_oracle_text(
            "{T}: Put a card exiled with this artifact into its owner's graveyard. \
             If it's a land card, create a Treasure token. \
             If it's a nonland card, create a 2/2 black Rogue creature token.",
        )
        .id();

    let mut runner = scenario.build();

    let treasure_before = count_battlefield_tokens(runner.state(), "Treasure");
    let rogue_before = count_battlefield_tokens(runner.state(), "Rogue");

    runner.activate(converter, 0).resolve();

    let treasure_after = count_battlefield_tokens(runner.state(), "Treasure");
    let rogue_after = count_battlefield_tokens(runner.state(), "Rogue");

    assert_eq!(
        treasure_after, treasure_before,
        "must not create Treasure with no exiled card"
    );
    assert_eq!(
        rogue_after, rogue_before,
        "must not create Rogue with no exiled card"
    );
}

#[test]
fn issue_2871_currency_converter_land_creates_treasure_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let converter = scenario
        .add_creature(P0, "Currency Converter", 0, 0)
        .as_artifact()
        .from_oracle_text(CURRENCY_CONVERTER_ABILITY)
        .id();

    let mut runner = scenario.build();
    let state = runner.state_mut();
    let land = create_object(state, CardId(100), P0, "Mountain".to_string(), Zone::Exile);
    {
        let object = state.objects.get_mut(&land).expect("land exists");
        object.card_types.core_types.push(CoreType::Land);
        object.base_card_types = object.card_types.clone();
    }
    state.exile_links.push(ExileLink {
        source_id: converter,
        exiled_id: land,
        kind: ExileLinkKind::TrackedBySource,
    });

    let treasure_before = count_battlefield_tokens(runner.state(), "Treasure");
    let rogue_before = count_battlefield_tokens(runner.state(), "Rogue");

    runner.activate(converter, 0).resolve();

    assert_eq!(runner.state().objects[&land].zone, Zone::Graveyard);
    assert_eq!(
        count_battlefield_tokens(runner.state(), "Treasure"),
        treasure_before + 1,
        "a land exiled with Currency Converter creates one Treasure token"
    );
    assert_eq!(
        count_battlefield_tokens(runner.state(), "Rogue"),
        rogue_before,
        "a land exiled with Currency Converter does not create a Rogue token"
    );
}

#[test]
fn issue_2871_currency_converter_nonland_creates_rogue_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let converter = scenario
        .add_creature(P0, "Currency Converter", 0, 0)
        .as_artifact()
        .from_oracle_text(CURRENCY_CONVERTER_ABILITY)
        .id();

    let mut runner = scenario.build();
    let state = runner.state_mut();
    let nonland = create_object(
        state,
        CardId(101),
        P0,
        "Grizzly Bears".to_string(),
        Zone::Exile,
    );
    {
        let object = state.objects.get_mut(&nonland).expect("nonland exists");
        object.card_types.core_types.push(CoreType::Creature);
        object.base_card_types = object.card_types.clone();
    }
    state.exile_links.push(ExileLink {
        source_id: converter,
        exiled_id: nonland,
        kind: ExileLinkKind::TrackedBySource,
    });

    let treasure_before = count_battlefield_tokens(runner.state(), "Treasure");
    let rogue_before = count_battlefield_tokens(runner.state(), "Rogue");

    runner.activate(converter, 0).resolve();

    assert_eq!(runner.state().objects[&nonland].zone, Zone::Graveyard);
    assert_eq!(
        count_battlefield_tokens(runner.state(), "Treasure"),
        treasure_before,
        "a nonland exiled with Currency Converter does not create a Treasure token"
    );
    assert_eq!(
        count_battlefield_tokens(runner.state(), "Rogue"),
        rogue_before + 1,
        "a nonland exiled with Currency Converter creates one Rogue token"
    );
}

#[test]
fn issue_2871_inverse_card_type_riders_create_treasure_for_land() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let source = scenario
        .add_creature(P0, "Inverse Card Type Riders", 0, 0)
        .as_artifact()
        .from_oracle_text(INVERSE_CARD_TYPE_RIDER_ABILITY)
        .id();

    let mut runner = scenario.build();
    let state = runner.state_mut();
    let land = create_object(state, CardId(102), P0, "Forest".to_string(), Zone::Exile);
    {
        let object = state.objects.get_mut(&land).expect("land exists");
        object.card_types.core_types.push(CoreType::Land);
        object.base_card_types = object.card_types.clone();
    }
    state.exile_links.push(ExileLink {
        source_id: source,
        exiled_id: land,
        kind: ExileLinkKind::TrackedBySource,
    });

    let treasure_before = count_battlefield_tokens(runner.state(), "Treasure");
    let rogue_before = count_battlefield_tokens(runner.state(), "Rogue");

    runner.activate(source, 0).resolve();

    assert_eq!(runner.state().objects[&land].zone, Zone::Graveyard);
    assert_eq!(
        count_battlefield_tokens(runner.state(), "Treasure"),
        treasure_before + 1,
        "the positive second rider runs when the first negated rider is false"
    );
    assert_eq!(
        count_battlefield_tokens(runner.state(), "Rogue"),
        rogue_before,
        "a land does not create the first nonland rider's Rogue token"
    );
}

#[test]
fn issue_2871_inverse_card_type_riders_create_rogue_for_nonland() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let source = scenario
        .add_creature(P0, "Inverse Card Type Riders", 0, 0)
        .as_artifact()
        .from_oracle_text(INVERSE_CARD_TYPE_RIDER_ABILITY)
        .id();

    let mut runner = scenario.build();
    let state = runner.state_mut();
    let nonland = create_object(
        state,
        CardId(103),
        P0,
        "Grizzly Bears".to_string(),
        Zone::Exile,
    );
    {
        let object = state.objects.get_mut(&nonland).expect("nonland exists");
        object.card_types.core_types.push(CoreType::Creature);
        object.base_card_types = object.card_types.clone();
    }
    state.exile_links.push(ExileLink {
        source_id: source,
        exiled_id: nonland,
        kind: ExileLinkKind::TrackedBySource,
    });

    let treasure_before = count_battlefield_tokens(runner.state(), "Treasure");
    let rogue_before = count_battlefield_tokens(runner.state(), "Rogue");

    runner.activate(source, 0).resolve();

    assert_eq!(runner.state().objects[&nonland].zone, Zone::Graveyard);
    assert_eq!(
        count_battlefield_tokens(runner.state(), "Treasure"),
        treasure_before,
        "a nonland does not create the second land rider's Treasure token"
    );
    assert_eq!(
        count_battlefield_tokens(runner.state(), "Rogue"),
        rogue_before + 1,
        "the first negated rider creates one Rogue token for a nonland"
    );
}

fn count_battlefield_tokens(state: &engine::types::game_state::GameState, subtype: &str) -> usize {
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|obj| {
            obj.card_types
                .subtypes
                .iter()
                .any(|s| s.eq_ignore_ascii_case(subtype))
        })
        .count()
}
