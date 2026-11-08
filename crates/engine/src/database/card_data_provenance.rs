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

#[cfg(test)]
mod tests {
    use super::*;

    fn provenance(source: &str, card_data: &str) -> CardDataProvenance {
        CardDataProvenance {
            source_corpus_sha256: source.to_string(),
            card_data_sha256: card_data.to_string(),
        }
    }

    #[test]
    fn well_formed_requires_two_lowercase_sha256_hashes() {
        let a = "a".repeat(64);
        let digits = "0123456789abcdef".repeat(4);
        assert!(provenance(&a, &digits).hashes_are_well_formed());

        // Each identity is validated independently.
        assert!(!provenance(&"A".repeat(64), &digits).hashes_are_well_formed());
        assert!(!provenance(&a, &"a".repeat(63)).hashes_are_well_formed());
        assert!(!provenance(&a, &"a".repeat(65)).hashes_are_well_formed());
        assert!(!provenance(&"g".repeat(64), &a).hashes_are_well_formed());
        assert!(!provenance("", &a).hashes_are_well_formed());
    }

    #[test]
    fn write_atomic_round_trips_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("card-data.provenance.json");
        let written = provenance(&"a".repeat(64), &"b".repeat(64));

        written.write_atomic(&path).unwrap();
        assert_eq!(CardDataProvenance::read(&path).unwrap(), written);

        // Rewriting replaces the sidecar in place.
        let rewritten = provenance(&"c".repeat(64), &"d".repeat(64));
        rewritten.write_atomic(&path).unwrap();
        assert_eq!(CardDataProvenance::read(&path).unwrap(), rewritten);

        let entries: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(
            entries,
            vec![std::ffi::OsString::from("card-data.provenance.json")]
        );
    }

    #[test]
    fn read_rejects_unknown_and_missing_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("card-data.provenance.json");
        let hash = "a".repeat(64);

        fs::write(
            &path,
            format!(r#"{{"source_corpus_sha256":"{hash}","card_data_sha256":"{hash}","extra":1}}"#),
        )
        .unwrap();
        assert!(CardDataProvenance::read(&path).is_err());

        fs::write(&path, format!(r#"{{"source_corpus_sha256":"{hash}"}}"#)).unwrap();
        assert!(CardDataProvenance::read(&path).is_err());
    }

    #[test]
    fn read_reports_missing_sidecar_as_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let error = CardDataProvenance::read(&dir.path().join("absent.json")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }
}
