//! BUG A repro: the Momir Basic random-token pool must NOT include transform
//! (TDFC) back faces.
//!
//! `rehydrate_card_db_metadata` (crates/engine/src/game/printed_cards.rs:1234)
//! builds the pool by iterating `db.face_index.values()` — which contains BOTH
//! faces of every multi-face card (oracle_loader.rs:65-67) — and keys each by
//! `face.mana_cost.mana_value()`. A transform DFC's BACK face has no castable
//! mana cost, so MTGJSON sends it with no `manaCost`; synthesis maps that to
//! `ManaCost::NoCost` (synthesis.rs:9739-9743), whose `mana_value()` is 0
//! (mana.rs:894-896). The back face is therefore added to the pool keyed at
//! MV 0 — a bogus Momir pick that is not a real "creature card".
//!
//! CR 202.3b: a nonmodal double-faced card's back face is treated as having
//! the mana cost of its FRONT face, and a copy of the back face has mana value
//! 0. CR 712.8a: outside the battlefield/stack a DFC has only its front face's
//! characteristics — the back face is not an independent card object there.
//! CR 202.1b: a face with no mana symbols has no mana cost (`NoCost`). The back
//! face is the same physical card, not a separately castable creature card, and
//! must not appear in the pool at MV 0 (or at all).
//!
//! The shared MTGJSON test fixture contains exactly such a card:
//! `Delver of Secrets // Insectile Aberration`. The back face
//! "Insectile Aberration" is a creature with no mana cost.

use std::path::Path;
use std::sync::OnceLock;

use engine::database::card_db::CardDatabase;
use engine::game::printed_cards::rehydrate_game_from_card_db;
use engine::types::format::FormatConfig;
use engine::types::game_state::GameState;

fn fixture_db() -> &'static CardDatabase {
    static DB: OnceLock<CardDatabase> = OnceLock::new();
    DB.get_or_init(|| {
        let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
        CardDatabase::from_mtgjson(&data.join("mtgjson/test_fixture.json"))
            .expect("CardDatabase::from_mtgjson should succeed")
    })
}

/// Build the Momir pool through the REAL builder and return the (state, db).
fn build_momir_pool() -> GameState {
    let db = fixture_db();
    // Sanity: the fixture really does carry the transform DFC we rely on.
    assert!(
        db.get_face_by_name("Insectile Aberration").is_some(),
        "test fixture must contain the transform back face 'Insectile Aberration'"
    );
    let mut state = GameState::new(FormatConfig::momir(), 2, 42);
    // Drives `rehydrate_card_db_metadata`, which builds `momir_pool` /
    // `momir_pool_faces` exactly as the live engine does on game start.
    rehydrate_game_from_card_db(&mut state, db);
    assert!(
        !state.momir_pool.is_empty(),
        "the Momir pool must be populated by the real builder"
    );
    state
}

/// BUG A DISCRIMINATING TEST.
///
/// Asserts the built pool does NOT contain the transform back face
/// "Insectile Aberration". BEFORE the fix this FAILS: the back face is present
/// at MV 0. AFTER the fix (excluding `NoCost` creature faces) it passes.
#[test]
fn momir_pool_excludes_transform_back_face() {
    let state = build_momir_pool();

    let all_pool_names: Vec<&String> = state.momir_pool.values().flatten().collect();

    assert!(
        !all_pool_names
            .iter()
            .any(|n| n.as_str() == "Insectile Aberration"),
        "CR 202.3b: the transform back face 'Insectile Aberration' must NOT be in \
         the Momir pool — it is not a separately castable creature card. \
         MV-0 pool entries: {:?}",
        state.momir_pool.get(&0)
    );

    // The FRONT face (Delver of Secrets) is itself an Instant, not a creature,
    // so it is correctly absent too — but the assertion that matters is the
    // back face is gone.
}

/// Generalization guard (build-for-the-class): NO face whose name is a back
/// face of a transform/flip/meld card (i.e. a creature face carrying no mana
/// cost) may appear in the pool. We approximate "no mana cost" by checking the
/// hydrated faces map: every pooled face must have a non-`NoCost` mana cost.
#[test]
fn momir_pool_contains_no_costless_creature_faces() {
    let state = build_momir_pool();

    let mut offenders: Vec<String> = Vec::new();
    for names in state.momir_pool.values() {
        for name in names {
            if let Some(face) = state.momir_pool_faces.get(&name.to_lowercase()) {
                if matches!(face.mana_cost, engine::types::mana::ManaCost::NoCost) {
                    offenders.push(name.clone());
                }
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "CR 202.1b: creature faces with no castable mana cost (transform/flip/meld \
         back faces, suspend-only cards) must not be Momir-pickable. Offenders: {offenders:?}"
    );
}
