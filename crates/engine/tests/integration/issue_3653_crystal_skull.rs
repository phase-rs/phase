//! Regression for GitHub issue #3653 — Crystal Skull, Isu Spyglass must parse
//! and enforce its historic top-of-library play/cast permission.
//!
//! https://github.com/phase-rs/phase/issues/3653

use engine::game::casting::spell_objects_available_to_cast;
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

const CRYSTAL_SKULL_ORACLE: &str = "You may look at the top card of your library any time.\n\
You may play historic lands and cast historic spells from the top of your library.\n\
{T}: Add {U}.";

fn move_to_top_of_library(
    state: &mut engine::types::game_state::GameState,
    obj_id: ObjectId,
    owner: PlayerId,
) {
    let player = state.players.iter_mut().find(|p| p.id == owner).unwrap();
    player.library.retain(|id| *id != obj_id);
    player.library.push_front(obj_id);
    let obj = state.objects.get_mut(&obj_id).unwrap();
    obj.zone = Zone::Library;
}

fn add_mana(runner: &mut engine::game::scenario::GameRunner, mana: &[ManaType]) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool;
    for m in mana {
        pool.add(ManaUnit::new(*m, dummy, false, vec![]));
    }
}

#[test]
fn crystal_skull_carries_historic_top_of_library_permission() {
    let mut scenario = GameScenario::new();
    let skull_id = scenario
        .add_creature(P0, "Crystal Skull, Isu Spyglass", 0, 0)
        .as_artifact()
        .from_oracle_text(CRYSTAL_SKULL_ORACLE)
        .id();
    let runner = scenario.build();
    let obj = runner.state().objects.get(&skull_id).unwrap();
    assert!(
        obj.static_definitions
            .iter_unchecked()
            .any(|d| matches!(d.mode, StaticMode::MayLookAtTopOfLibrary)),
        "Crystal Skull must grant MayLookAtTopOfLibrary"
    );
    let top_perm = obj
        .static_definitions
        .iter_unchecked()
        .find(|d| matches!(d.mode, StaticMode::TopOfLibraryCastPermission { .. }))
        .expect("TopOfLibraryCastPermission static");
    assert!(
        top_perm.affected.is_some(),
        "historic top-of-library permission must carry an affected filter"
    );
}

/// CR 700.6 + CR 401.5: historic artifacts on top of library must surface as
/// castable while Crystal Skull is in play.
#[test]
fn crystal_skull_surfaces_historic_artifact_on_library_top() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let _skull = scenario
        .add_creature(P0, "Crystal Skull, Isu Spyglass", 0, 0)
        .as_artifact()
        .from_oracle_text(CRYSTAL_SKULL_ORACLE)
        .id();
    let top_id = scenario.add_real_card(P0, "Sol Ring", Zone::Library, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    move_to_top_of_library(runner.state_mut(), top_id, P0);
    add_mana(&mut runner, &[ManaType::Colorless]);

    let available = spell_objects_available_to_cast(runner.state(), P0);
    assert!(
        available.contains(&top_id),
        "Crystal Skull must surface historic artifacts on the library top; available={available:?}"
    );

    let legal = engine::ai_support::legal_actions(runner.state());
    assert!(
        legal
            .iter()
            .any(|a| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == top_id)),
        "legal_actions must expose CastSpell for a historic library top"
    );
}

/// CR 305.1 + CR 700.6: historic lands on top of library must surface as
/// playable while Crystal Skull is in play.
#[test]
fn crystal_skull_surfaces_historic_land_on_library_top() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let _skull = scenario
        .add_creature(P0, "Crystal Skull, Isu Spyglass", 0, 0)
        .as_artifact()
        .from_oracle_text(CRYSTAL_SKULL_ORACLE)
        .id();
    let top_id = scenario.add_card_to_library_top(P0, "Legendary Test Land");
    let mut runner = scenario.build();
    {
        let obj = runner.state_mut().objects.get_mut(&top_id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.supertypes.push(Supertype::Legendary);
        obj.base_card_types = obj.card_types.clone();
    }

    let legal = engine::ai_support::legal_actions(runner.state());
    assert!(
        legal
            .iter()
            .any(|a| matches!(a, GameAction::PlayLand { object_id, .. } if *object_id == top_id)),
        "legal_actions must expose PlayLand for a historic library top"
    );

    let card_id = runner.state().objects.get(&top_id).unwrap().card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: top_id,
            card_id,
        })
        .expect("Crystal Skull should allow playing the historic land");
    assert_eq!(
        runner.state().objects.get(&top_id).unwrap().zone,
        Zone::Battlefield
    );
}

/// CR 700.6: non-historic spells on top of library must stay uncastable.
#[test]
fn crystal_skull_blocks_non_historic_spell_on_library_top() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let _skull = scenario
        .add_creature(P0, "Crystal Skull, Isu Spyglass", 0, 0)
        .as_artifact()
        .from_oracle_text(CRYSTAL_SKULL_ORACLE)
        .id();
    let top_id = scenario.add_real_card(P0, "Lightning Bolt", Zone::Library, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    move_to_top_of_library(runner.state_mut(), top_id, P0);
    add_mana(&mut runner, &[ManaType::Red]);

    let available = spell_objects_available_to_cast(runner.state(), P0);
    assert!(
        !available.contains(&top_id),
        "non-historic library top must not be castable through Crystal Skull"
    );
}
