//! The one reader of the committed integration card fixture
//! (`crates/engine/tests/fixtures/integration_cards*`).
//!
//! Every test and tool that needs the curated card pool goes through this module, so the
//! fixture's on-disk layout is known in exactly one place. Callers get the same logical
//! content in every layout: a JSON object keyed by lowercase card name.

use std::fs::File;
use std::io::{self, Cursor, Read};
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;

use super::card_db::CardDatabase;

/// Absolute path of the fixture, independent of the caller's crate or working directory.
pub fn path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/integration_cards.json.gz")
}

/// Whether the committed fixture is present in this checkout.
pub fn exists() -> bool {
    path().exists()
}

/// The fixture as one JSON object text (`{"<key>":<card>,...}`).
pub fn json() -> io::Result<String> {
    let mut json = String::new();
    GzDecoder::new(File::open(path())?).read_to_string(&mut json)?;
    Ok(json)
}

/// The fixture's JSON object text as a reader, for APIs that take `Read`.
pub fn reader() -> io::Result<Cursor<Vec<u8>>> {
    Ok(Cursor::new(json()?.into_bytes()))
}

impl CardDatabase {
    /// Loads the committed integration fixture (eager typing, no cache).
    pub fn from_integration_fixture() -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self::from_json_str(&json()?)?)
    }

    /// Loads the committed integration fixture with faces typed lazily on first read.
    pub fn from_integration_fixture_typing_faces_on_read(
    ) -> Result<Self, Box<dyn std::error::Error>> {
        Self::from_export_text_typing_faces_on_read(json()?)
    }

    /// Loads the committed integration fixture through the per-binary decode cache.
    pub fn from_integration_fixture_cached() -> Result<Self, Box<dyn std::error::Error>> {
        Self::from_compressed_export_cached(File::open(path())?, |gz| {
            io::read_to_string(GzDecoder::new(gz))
        })
    }
}
