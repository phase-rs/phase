//! Regression: "<verb> twice instead" cast-from-graveyard count replacement
//! (Secrets of the Key class). Swallowed-clause plan unit 7c.
//!
//! Secrets of the Key reads: "Investigate. If this spell was cast from a
//! graveyard, investigate twice instead." The parser captures this as a
//! sub-ability `Investigate` carrying
//! `condition: ConditionInstead { inner: CastFromZone { Graveyard } }` and
//! `repeat_for: Fixed(2)`. There is nothing structurally swallowed — the
//! `DynamicQty` swallow warning on "twice" is a detector false positive.
//!
//! These tests PROVE the runtime is already correct (CR 608.2c — conditional
//! "instead" swap during resolution): casting from a graveyard must produce
//! exactly two Clue tokens; casting from hand must produce exactly one. They
//! are the mandatory gate for the `swallow_check.rs` detector suppression in
//! plan unit 7c — if they fail, the warning hides a real bug and suppression
//! is unjustified.

use std::path::Path;
use std::sync::OnceLock;

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn load_db() -> Option<&'static CardDatabase> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../client/public/card-data.json");
    if !path.exists() {
        return None;
    }
    static DB: OnceLock<CardDatabase> = OnceLock::new();
    Some(DB.get_or_init(|| CardDatabase::from_export(&path).expect("export should load")))
}

fn add_mana_to(
    runner: &mut engine::game::scenario::GameRunner,
    player: engine::types::player::PlayerId,
    mana: &[ManaType],
) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .unwrap()
        .mana_pool;
    for m in mana {
        pool.add(ManaUnit::new(*m, dummy, false, vec![]));
    }
}

/// Count battlefield Clue tokens controlled by `player`. Clue tokens carry the
/// "Clue" subtype (CR 111.10f).
fn count_clues(
    state: &engine::types::game_state::GameState,
    player: engine::types::player::PlayerId,
) -> usize {
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|obj| obj.controller == player)
        .filter(|obj| {
            obj.card_types
                .subtypes
                .iter()
                .any(|s| s.eq_ignore_ascii_case("Clue"))
        })
        .count()
}

/// CR 608.2c: Casting Secrets of the Key from the GRAVEYARD (via Flashback
/// {3}{U}) satisfies the `ConditionInstead { CastFromZone { Graveyard } }`
/// gate, swaps to the sub-ability with `repeat_for: Fixed(2)`, and resolves
/// `Investigate` twice — exactly 2 Clue tokens.
#[test]
fn secrets_of_the_key_from_graveyard_makes_two_clues() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let secrets = scenario.add_real_card(P0, "Secrets of the Key", Zone::Graveyard, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let clues_before = count_clues(runner.state(), P0);

    // Pay the Flashback cost {3}{U} from the pool (auto-paid by the driver) and
    // resolve through the canonical pipeline.
    add_mana_to(
        &mut runner,
        P0,
        &[
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Blue,
        ],
    );
    let outcome = runner.cast(secrets).resolve();

    let clues_after = count_clues(outcome.state(), P0);
    assert_eq!(
        clues_after - clues_before,
        2,
        "Secrets of the Key cast from graveyard must investigate twice — 2 Clue tokens"
    );
}

/// CR 608.2c: Casting Secrets of the Key from HAND leaves the
/// `ConditionInstead` gate false, so the base `Investigate` resolves once —
/// exactly 1 Clue token.
#[test]
fn secrets_of_the_key_from_hand_makes_one_clue() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let secrets = scenario.add_real_card(P0, "Secrets of the Key", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let clues_before = count_clues(runner.state(), P0);

    // Pay the printed mana cost {U} from the pool (auto-paid by the driver).
    add_mana_to(&mut runner, P0, &[ManaType::Blue]);
    let outcome = runner.cast(secrets).resolve();

    let clues_after = count_clues(outcome.state(), P0);
    assert_eq!(
        clues_after - clues_before,
        1,
        "Secrets of the Key cast from hand must investigate once — 1 Clue token"
    );
}
