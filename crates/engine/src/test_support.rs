#![cfg(test)]
//! Shared card-data fixture loader for inline `#[cfg(test)]` unit tests in `src/`.
//! TWIN-SYNC: keep the fixture path here in lockstep with
//! `crates/engine/tests/integration/support.rs` — both must track the fixture file.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::database::card_db::CardDatabase;

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/integration_cards.json")
}

fn export_path() -> PathBuf {
    // allow-full-card-db: FORGE_TEST_FULL_DB escape hatch only; default loads the committed fixture
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../client/public/card-data.json")
}

fn parser_fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/mtgjson/test_fixture.json")
}

/// Overlay the single parser-backed integration card after loading the normal
/// export-backed fixture. The test fixture remains the parser input authority;
/// production exports and their loading paths stay untouched.
fn overlay_parser_backed_fixture_card(mut db: CardDatabase) -> CardDatabase {
    const NAME: &str = "witherbloom apprentice";

    let parser_db = CardDatabase::from_mtgjson(&parser_fixture_path())
        .expect("parser-backed test fixture should load");
    let face = parser_db
        .face_index
        .get(NAME)
        .expect("parser-backed fixture should contain Witherbloom Apprentice")
        .clone();
    let rules = parser_db
        .cards
        .get(NAME)
        .expect("parser-backed fixture should contain Witherbloom rules")
        .clone();
    let oracle_id = face
        .scryfall_oracle_id
        .clone()
        .expect("Witherbloom fixture should carry its oracle id");

    db.cards.insert(NAME.to_string(), rules);
    db.face_index.insert(NAME.to_string(), face);

    for keys in db.oracle_id_index.values_mut() {
        keys.retain(|key| key != NAME);
    }
    db.oracle_id_index.retain(|_, keys| !keys.is_empty());
    db.oracle_id_index
        .entry(oracle_id)
        .or_default()
        .push(NAME.to_string());

    db.name_alias_index = crate::database::card_db::build_name_alias_index(db.face_index.keys());
    db.creature_type_vocabulary =
        crate::database::card_db::collect_creature_type_vocabulary(db.face_index.values());
    db
}

/// Infallible process-wide card DB for inline src tests. Loads the committed
/// fixture by default; `FORGE_TEST_FULL_DB=1` forces the full export.
pub(crate) fn shared_card_db() -> &'static CardDatabase {
    static DB: OnceLock<CardDatabase> = OnceLock::new();
    DB.get_or_init(|| {
        let db = if std::env::var_os("FORGE_TEST_FULL_DB").is_none() {
            let fixture = fixture_path();
            if fixture.exists() {
                CardDatabase::from_export(&fixture).expect("integration fixture should load")
            } else {
                CardDatabase::from_export(&export_path()).expect("card-data export should load")
            }
        } else {
            CardDatabase::from_export(&export_path()).expect("card-data export should load")
        };
        overlay_parser_backed_fixture_card(db)
    })
}

#[cfg(test)]
mod tests {
    use crate::types::ability::{Effect, PlayerFilter, QuantityExpr, TargetFilter};
    use crate::types::triggers::TriggerMode;

    use super::shared_card_db;

    #[test]
    fn parser_backed_witherbloom_is_oracle_indexed_with_fixed_magecraft() {
        let db = shared_card_db();
        let oracle_keys = db
            .oracle_id_index
            .get("696f554d-0485-48a5-9273-3f6fb7d16a5d")
            .expect("Witherbloom parser overlay must register its oracle id");
        assert_eq!(oracle_keys.len(), 1, "the parser face must be indexed once");
        assert_eq!(oracle_keys[0], "witherbloom apprentice");
        let face = db
            .get_face_by_oracle_id("696f554d-0485-48a5-9273-3f6fb7d16a5d")
            .expect("Witherbloom parser overlay must retain oracle-id lookup");
        assert_eq!(face.name, "Witherbloom Apprentice");

        let trigger = face
            .triggers
            .iter()
            .find(|trigger| trigger.mode == TriggerMode::SpellCastOrCopy)
            .expect("Witherbloom must parse Magecraft as cast-or-copy");
        let execute = trigger
            .execute
            .as_deref()
            .expect("Witherbloom Magecraft must have an effect");
        assert_eq!(execute.player_scope, Some(PlayerFilter::Opponent));
        assert!(matches!(
            &*execute.effect,
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 1 },
                target: None,
            }
        ));
        let gain = execute
            .sub_ability
            .as_deref()
            .expect("Witherbloom Magecraft must gain life after the drain");
        assert!(matches!(
            &*gain.effect,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            }
        ));
    }
}
