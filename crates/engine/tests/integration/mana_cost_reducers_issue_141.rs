//! Regression for issue #141: Delighted Halfling restricted mana + Goblin
//! Anarchomancer cost reduction.
//!
//! https://github.com/phase-rs/phase/issues/141

use engine::game::scenario::{GameScenario, P0};
use engine::game::zones::create_object;
use engine::parser::oracle_static::parse_static_line;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{
    ManaColor, ManaCost, ManaCostShard, ManaRestriction, ManaType, ManaUnit,
};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Doors of Durin is {3}{R}{G}; Goblin Anarchomancer should reduce generic by 1.
#[test]
fn goblin_anarchomancer_reduces_only_red_or_green_spell_costs() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut game = scenario.build();

    let state = game.state_mut();
    let reducer = create_object(
        state,
        CardId(9000),
        P0,
        "Goblin Anarchomancer".to_string(),
        Zone::Battlefield,
    );
    let anarchomancer_static =
        parse_static_line("Each spell you cast that's red or green costs {1} less to cast.")
            .expect("Goblin Anarchomancer static should parse");
    state
        .objects
        .get_mut(&reducer)
        .unwrap()
        .static_definitions
        .push(anarchomancer_static);

    let doors = create_object(
        state,
        CardId(9001),
        P0,
        "Doors of Durin".to_string(),
        Zone::Hand,
    );
    {
        let obj = state.objects.get_mut(&doors).unwrap();
        obj.card_types.supertypes.push(Supertype::Legendary);
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Red, ManaCostShard::Green],
            generic: 3,
        };
        obj.color = vec![ManaColor::Red, ManaColor::Green];
        obj.base_color = obj.color.clone();
    }

    let colorless_artifact = create_object(
        state,
        CardId(9002),
        P0,
        "Colorless Artifact".to_string(),
        Zone::Hand,
    );
    {
        let obj = state.objects.get_mut(&colorless_artifact).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.mana_cost = ManaCost::generic(3);
    }

    let cost =
        engine::game::casting::display_spell_cost(game.state(), P0, doors).expect("display cost");
    let ManaCost::Cost { generic, .. } = cost else {
        panic!("expected ManaCost::Cost, got {cost:?}");
    };
    assert_eq!(
        generic, 2,
        "Doors of Durin should display {{2}}{{R}}{{G}} with Anarchomancer on board (issue #141)"
    );

    let cost = engine::game::casting::display_spell_cost(game.state(), P0, colorless_artifact)
        .expect("display cost");
    let ManaCost::Cost { generic, .. } = cost else {
        panic!("expected ManaCost::Cost, got {cost:?}");
    };
    assert_eq!(
        generic, 3,
        "Goblin Anarchomancer must not reduce non-red/non-green spells"
    );

    {
        let player = game
            .state_mut()
            .players
            .iter_mut()
            .find(|player| player.id == P0)
            .unwrap();
        for mana_type in [
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Red,
            ManaType::Green,
        ] {
            player
                .mana_pool
                .add(ManaUnit::new(mana_type, ObjectId(0), false, vec![]));
        }
    }

    let outcome = game.cast(doors).resolve();
    outcome.assert_zone(&[doors], Zone::Battlefield);
    assert_eq!(
        outcome.mana_pool_total(P0),
        0,
        "the reduced {{2}}{{R}}{{G}} cost should consume the exact four-mana pool"
    );
}

/// Delighted Halfling-style restricted mana must pay for a commander.
#[test]
fn restricted_mana_pays_legendary_commander_through_cast_pipeline() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mut game = scenario.build();
    let state = game.state_mut();
    state.format_config.command_zone = true;
    let commander_id = create_object(
        state,
        CardId(9001),
        P0,
        "Test Commander".to_string(),
        Zone::Command,
    );
    {
        let obj = state.objects.get_mut(&commander_id).unwrap();
        obj.card_types.supertypes.push(Supertype::Legendary);
        obj.card_types.core_types.push(CoreType::Creature);
        obj.is_commander = true;
        obj.mana_cost = ManaCost::generic(1);
    }

    let restriction = ManaRestriction::OnlyForSpellType("Legendary".to_string());
    state
        .players
        .iter_mut()
        .find(|player| player.id == P0)
        .unwrap()
        .mana_pool
        .add(ManaUnit::new(
            ManaType::Green,
            ObjectId(1),
            false,
            vec![restriction],
        ));

    let outcome = game.cast(commander_id).resolve();
    outcome.assert_zone(&[commander_id], Zone::Battlefield);
    assert_eq!(
        outcome.mana_pool_total(P0),
        0,
        "legendary-restricted mana must be eligible for commander casts (issue #141)"
    );
}
