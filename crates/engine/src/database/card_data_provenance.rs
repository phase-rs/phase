use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::process;

use serde::{Deserialize, Serialize};

/// The exact input and output bytes used for one generated card-data export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CardDataProvenance {
    pub source_corpus_sha256: String,
    pub card_data_sha256: String,
}

impl CardDataProvenance {
    /// Read a provenance sidecar from disk.
    pub fn read(path: &Path) -> io::Result<Self> {
        let bytes = fs::read(path)?;
        serde_json::from_slice(&bytes).map_err(io::Error::other)
    }

    /// Check that both recorded identities are lowercase SHA-256 strings.
    pub fn hashes_are_well_formed(&self) -> bool {
        [
            self.source_corpus_sha256.as_str(),
            self.card_data_sha256.as_str(),
        ]
        .into_iter()
        .all(|hash| {
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    }

    /// Write the sidecar through a same-directory temporary file and rename.
    pub fn write_atomic(&self, path: &Path) -> io::Result<()> {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let file_name = path.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "provenance output has no file name",
            )
        })?;
        let temp = parent.join(format!(
            ".{}.{}.tmp",
            file_name.to_string_lossy(),
            process::id()
        ));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            serde_json::to_writer_pretty(&mut file, self).map_err(io::Error::other)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            fs::rename(&temp, path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }
}
