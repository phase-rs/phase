#![cfg(test)]
//! Shared card-data fixture loader for inline `#[cfg(test)]` unit tests in `src/`.
//! The fixture itself is read through `crate::database::integration_fixture`, the one
//! reader shared with `crates/engine/tests/integration/support.rs` and the other crates.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::database::card_db::CardDatabase;
use crate::database::integration_fixture;
use crate::database::mtgjson::load_atomic_cards;
use crate::database::oracle_loader::load_from_atomic_cards;

const PARSER_BACKED_CARD: &str = "witherbloom apprentice";

pub(crate) fn load_fixture() -> CardDatabase {
    CardDatabase::from_integration_fixture_typing_faces_on_read()
        .expect("integration fixture should load")
}

fn load_fixture_cached() -> CardDatabase {
    CardDatabase::from_integration_fixture_cached().expect("integration fixture should load")
}

fn export_path() -> PathBuf {
    // allow-full-card-db: FORGE_TEST_FULL_DB escape hatch only; default loads the committed fixture
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../client/public/card-data.json")
}

fn parser_fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/mtgjson/test_fixture.json")
}

fn parser_backed_card_db() -> CardDatabase {
    let mut atomic =
        load_atomic_cards(&parser_fixture_path()).expect("parser-backed test fixture should load");
    atomic
        .data
        .retain(|name, _| name.eq_ignore_ascii_case(PARSER_BACKED_CARD));
    load_from_atomic_cards(atomic)
}

/// Asserts every field of two databases is equal, after checking `expected` is populated enough
/// for that equality to mean something.
pub(crate) fn assert_same_database(expected: &CardDatabase, actual: &CardDatabase) {
    let CardDatabase {
        cards,
        face_index,
        name_alias_index,
        oracle_id_index,
        face_order_index,
        search_face_keys,
        layout_index,
        legalities,
        printings_index,
        rulings_index,
        errors,
        bracket_lists,
        bracket_signals_by_name,
        creature_type_vocabulary,
        valid_subtypes: _,
    } = expected;
    assert!(!face_index.is_empty(), "face_index is empty");
    assert!(!name_alias_index.is_empty(), "name_alias_index is empty");
    assert!(!oracle_id_index.is_empty(), "oracle_id_index is empty");
    assert!(!face_order_index.is_empty(), "face_order_index is empty");
    assert!(!search_face_keys.is_empty(), "search_face_keys is empty");
    assert!(!layout_index.is_empty(), "layout_index is empty");
    assert!(!legalities.is_empty(), "legalities is empty");
    assert!(!printings_index.is_empty(), "printings_index is empty");
    assert!(!rulings_index.is_empty(), "rulings_index is empty");
    assert!(
        !creature_type_vocabulary.is_empty(),
        "creature_type_vocabulary is empty"
    );
    assert!(
        bracket_signals_by_name
            .values()
            .any(|signals| *signals != crate::database::bracket_lists::BracketSignals::default()),
        "bracket_signals_by_name carries no signal"
    );

    assert!(*cards == actual.cards, "cards differs");
    assert!(*face_index == actual.face_index, "face_index differs");
    assert!(
        *name_alias_index == actual.name_alias_index,
        "name_alias_index differs"
    );
    assert!(
        *oracle_id_index == actual.oracle_id_index,
        "oracle_id_index differs"
    );
    assert!(
        *face_order_index == actual.face_order_index,
        "face_order_index differs"
    );
    assert!(
        *search_face_keys == actual.search_face_keys,
        "search_face_keys differs"
    );
    assert!(*layout_index == actual.layout_index, "layout_index differs");
    assert!(*legalities == actual.legalities, "legalities differs");
    assert!(
        *printings_index == actual.printings_index,
        "printings_index differs"
    );
    assert!(
        *rulings_index == actual.rulings_index,
        "rulings_index differs"
    );
    assert!(*errors == actual.errors, "errors differs");
    assert!(
        *bracket_lists == actual.bracket_lists,
        "bracket_lists differs"
    );
    assert!(
        *bracket_signals_by_name == actual.bracket_signals_by_name,
        "bracket_signals_by_name differs"
    );
    assert!(
        *creature_type_vocabulary == actual.creature_type_vocabulary,
        "creature_type_vocabulary differs"
    );
}

/// Overlay the single parser-backed integration card after loading the normal
/// export-backed fixture. The test fixture remains the parser input authority;
/// production exports and their loading paths stay untouched.
fn overlay_parser_backed_fixture_card(
    mut db: CardDatabase,
    parser_db: &CardDatabase,
) -> CardDatabase {
    let face = parser_db
        .face_index
        .get(PARSER_BACKED_CARD)
        .expect("parser-backed fixture should contain Witherbloom Apprentice")
        .clone();
    let rules = parser_db
        .cards
        .get(PARSER_BACKED_CARD)
        .expect("parser-backed fixture should contain Witherbloom rules")
        .clone();
    let oracle_id = face
        .scryfall_oracle_id
        .clone()
        .expect("Witherbloom fixture should carry its oracle id");

    db.cards.insert(PARSER_BACKED_CARD.to_string(), rules);
    db.face_index.insert(PARSER_BACKED_CARD.to_string(), face);

    for keys in db.oracle_id_index.values_mut() {
        keys.retain(|key| key != PARSER_BACKED_CARD);
    }
    db.oracle_id_index.retain(|_, keys| !keys.is_empty());
    db.oracle_id_index
        .entry(oracle_id)
        .or_default()
        .push(PARSER_BACKED_CARD.to_string());

    db.name_alias_index = crate::database::card_db::build_name_alias_index(db.face_index.keys());
    db.creature_type_vocabulary =
        crate::database::card_db::collect_creature_type_vocabulary(db.face_index.card_types());
    db
}

/// Infallible process-wide card DB for inline src tests. Loads the committed
/// fixture by default; `FORGE_TEST_FULL_DB=1` forces the full export.
pub(crate) fn shared_card_db() -> &'static CardDatabase {
    static DB: OnceLock<CardDatabase> = OnceLock::new();
    DB.get_or_init(|| {
        let db = if std::env::var_os("FORGE_TEST_FULL_DB").is_none() {
            if integration_fixture::exists() {
                load_fixture_cached()
            } else {
                CardDatabase::from_export(&export_path()).expect("card-data export should load")
            }
        } else {
            CardDatabase::from_export(&export_path()).expect("card-data export should load")
        };
        overlay_parser_backed_fixture_card(db, &parser_backed_card_db())
    })
}

#[cfg(test)]
mod tests {
    use crate::types::ability::{Effect, PlayerFilter, QuantityExpr, TargetFilter};
    use crate::types::triggers::TriggerMode;

    use super::{
        assert_same_database, load_fixture, overlay_parser_backed_fixture_card,
        parser_backed_card_db, parser_fixture_path, shared_card_db,
    };
    use crate::database::card_db::CardDatabase;

    #[test]
    fn lone_card_overlay_matches_the_full_parser_fixture_overlay() {
        let reference = overlay_parser_backed_fixture_card(
            load_fixture(),
            &CardDatabase::from_mtgjson(&parser_fixture_path()).unwrap(),
        );
        let candidate =
            overlay_parser_backed_fixture_card(load_fixture(), &parser_backed_card_db());
        assert_same_database(&reference, &candidate);
    }

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
