//! Regression: GitHub issue #1521 — Obliterate must destroy every artifact,
//! creature, and land, not equalize to one permanent per player.

use std::path::Path;
use std::sync::OnceLock;

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::zones::create_object;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

fn load_db() -> Option<&'static CardDatabase> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../client/public/card-data.json");
    if !path.exists() {
        return None;
    }
    static DB: OnceLock<CardDatabase> = OnceLock::new();
    Some(DB.get_or_init(|| CardDatabase::from_export(&path).expect("export should load")))
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

fn add_permanent(
    state: &mut engine::types::game_state::GameState,
    owner: PlayerId,
    card_id: u64,
    name: &str,
    core_type: CoreType,
) -> ObjectId {
    let id = create_object(
        state,
        CardId(card_id),
        owner,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(core_type);
    obj.base_card_types = obj.card_types.clone();
    id
}

/// Loads Obliterate from the generated card-data export and resolves it through
/// the same cast/stack path gameplay uses. The board is intentionally
/// asymmetric: if this regresses to Balance-style equalization, several matching
/// permanents will survive.
#[test]
fn obliterate_destroys_all_artifacts_creatures_and_lands_from_card_data() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let obliterate = scenario.add_real_card(P0, "Obliterate", Zone::Hand, db);
    let mut runner = scenario.build();
    let p0_artifact = add_permanent(
        runner.state_mut(),
        P0,
        101,
        "Player Artifact",
        CoreType::Artifact,
    );
    let p0_creature = add_permanent(
        runner.state_mut(),
        P0,
        102,
        "Player Creature",
        CoreType::Creature,
    );
    let p0_land = add_permanent(runner.state_mut(), P0, 103, "Player Land", CoreType::Land);
    let p1_artifact = add_permanent(
        runner.state_mut(),
        P1,
        201,
        "Opponent Artifact",
        CoreType::Artifact,
    );
    let p1_creature = add_permanent(
        runner.state_mut(),
        P1,
        202,
        "Opponent Creature",
        CoreType::Creature,
    );
    let p1_land = add_permanent(runner.state_mut(), P1, 203, "Opponent Land", CoreType::Land);
    let p1_enchantment = add_permanent(
        runner.state_mut(),
        P1,
        204,
        "Opponent Enchantment",
        CoreType::Enchantment,
    );

    add_mana(
        &mut runner,
        &[
            ManaType::Red,
            ManaType::Red,
            ManaType::Red,
            ManaType::Red,
            ManaType::Red,
            ManaType::Red,
            ManaType::Red,
            ManaType::Red,
        ],
    );

    // Non-targeted, pool-funded mass destruction: the driver auto-pays from the
    // pool and resolves to a clean Priority window (it never surfaces a target
    // prompt — Obliterate has no targets).
    let outcome = runner.cast(obliterate).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "Obliterate must resolve to a clean priority window, got {:?}",
        outcome.final_waiting_for()
    );

    outcome.assert_zone(
        &[
            p0_artifact,
            p0_creature,
            p0_land,
            p1_artifact,
            p1_creature,
            p1_land,
        ],
        Zone::Graveyard,
    );
    outcome.assert_zone(&[p1_enchantment], Zone::Battlefield);
}
