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

/// Infallible process-wide card DB for inline src tests. Loads the committed
/// fixture by default; `FORGE_TEST_FULL_DB=1` forces the full export.
pub(crate) fn shared_card_db() -> &'static CardDatabase {
    static DB: OnceLock<CardDatabase> = OnceLock::new();
    DB.get_or_init(|| {
        if std::env::var_os("FORGE_TEST_FULL_DB").is_none() {
            let fixture = fixture_path();
            if fixture.exists() {
                return CardDatabase::from_export(&fixture)
                    .expect("integration fixture should load");
            }
        }
        CardDatabase::from_export(&export_path()).expect("card-data export should load")
    })
}
