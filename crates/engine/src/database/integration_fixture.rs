//! The one reader of the committed integration card fixture
//! (`crates/engine/tests/fixtures/integration_cards/`).
//!
//! Every test and tool that needs the curated card pool goes through this module, so the
//! fixture's on-disk layout is known in exactly one place. Callers get a JSON object keyed by
//! lowercase card name, the same text the export has always had.
//!
//! The fixture is a directory of JSON Lines shards, one file per first letter of the key
//! (`a.jsonl` .. `z.jsonl`, `_digits.jsonl` for other ASCII, `_other.jsonl` for non-ASCII). Each
//! line is one complete record, `{"key":"abandon hope","card":{...}}`, sorted by key, so pull
//! requests that touch different cards touch different lines and merge without conflict. The
//! writer is `scripts/gen-test-fixture.py` (format in `scripts/lib/fixture_shards.py`).

use std::collections::HashSet;
use std::fs;
use std::io::{self, Cursor};
use std::path::{Path, PathBuf};

use super::card_db::CardDatabase;

/// Suffix of every shard file. Other files in the directory (editor leftovers, `.DS_Store`) are
/// not shards and are ignored by the loader; the layout test rejects them in the committed tree.
pub const SHARD_SUFFIX: &str = ".jsonl";

/// Absolute path of the fixture directory, independent of the caller's crate or working directory.
pub fn path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/integration_cards")
}

/// Whether the committed fixture is present in this checkout.
pub fn exists() -> bool {
    path().is_dir()
}

/// One shard file: its name and its text.
pub struct Shard {
    pub name: String,
    pub text: String,
}

/// Every shard of the fixture directory `dir`, in file-name order (never directory order).
pub fn read_shards(dir: &Path) -> io::Result<Vec<Shard>> {
    let mut shards = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(SHARD_SUFFIX) && entry.file_type()?.is_file() {
            shards.push(Shard {
                text: fs::read_to_string(entry.path())?,
                name,
            });
        }
    }
    shards.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(shards)
}

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// Splits one record line `{"key":<json string>,"card":<json value>}` into the raw key literal
/// (quotes included) and the raw card value, without parsing either.
fn split_record(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix(r#"{"key":""#)?;
    let mut escaped = false;
    let key_end = rest.bytes().position(|byte| {
        let closes = !escaped && byte == b'"';
        escaped = !escaped && byte == b'\\';
        closes
    })?;
    // `key` keeps its quotes: the opening one is the last byte of the stripped prefix.
    let key = &line[line.len() - rest.len() - 1..line.len() - rest.len() + key_end + 1];
    let card = rest[key_end + 1..].strip_prefix(r#","card":"#)?;
    Some((key, card.trim_end().strip_suffix('}')?))
}

/// Joins shards into one JSON object text (`{"<key>":<card>,...}`). A malformed record or a key
/// that appears twice is an error naming the shard and line.
pub fn splice(shards: &[Shard]) -> io::Result<String> {
    let mut json = String::with_capacity(shards.iter().map(|shard| shard.text.len()).sum());
    let mut seen: HashSet<&str> = HashSet::new();
    json.push('{');
    let mut first = true;
    for shard in shards {
        let text = shard.text.strip_prefix('\u{feff}').unwrap_or(&shard.text);
        for (index, line) in text.split('\n').enumerate() {
            let line = line.strip_suffix('\r').unwrap_or(line);
            if line.is_empty() {
                continue;
            }
            let at = || format!("{}:{}", shard.name, index + 1);
            let (key, card) = split_record(line).ok_or_else(|| {
                invalid(format!("{}: not a {{\"key\":..,\"card\":..}} record", at()))
            })?;
            if !seen.insert(key) {
                return Err(invalid(format!("{}: duplicate key {key}", at())));
            }
            if !first {
                json.push(',');
            }
            first = false;
            json.push_str(key);
            json.push(':');
            json.push_str(card);
        }
    }
    json.push('}');
    Ok(json)
}

/// The fixture as one JSON object text (`{"<key>":<card>,...}`).
pub fn json() -> io::Result<String> {
    splice(&read_shards(&path())?)
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

    /// Loads the committed integration fixture through the per-binary decode cache. The cache is
    /// keyed by the shards' raw bytes, so the object text is spliced only on a miss.
    pub fn from_integration_fixture_cached() -> Result<Self, Box<dyn std::error::Error>> {
        let shards = read_shards(&path())?;
        Self::from_parts_cached(
            shards
                .iter()
                .map(|shard| (shard.name.as_str(), shard.text.as_bytes())),
            || splice(&shards),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shard(name: &str, text: &str) -> Shard {
        Shard {
            name: name.to_string(),
            text: text.to_string(),
        }
    }

    #[test]
    fn splice_joins_records_into_one_object() {
        let shards = [
            shard(
                "a.jsonl",
                "{\"key\":\"ab\",\"card\":{\"n\":1}}\n{\"key\":\"ac\",\"card\":[1,2]}\n",
            ),
            shard("b.jsonl", "{\"key\":\"b\",\"card\":null}"),
        ];
        assert_eq!(
            splice(&shards).unwrap(),
            r#"{"ab":{"n":1},"ac":[1,2],"b":null}"#
        );
    }

    #[test]
    fn splice_of_nothing_is_the_empty_object() {
        assert_eq!(splice(&[]).unwrap(), "{}");
        assert_eq!(splice(&[shard("a.jsonl", "\n")]).unwrap(), "{}");
    }

    #[test]
    fn splice_tolerates_crlf_bom_and_trailing_space() {
        let shards = [shard(
            "a.jsonl",
            "\u{feff}{\"key\":\"x\",\"card\":1} \r\n{\"key\":\"y\",\"card\":2}\r\n",
        )];
        assert_eq!(splice(&shards).unwrap(), r#"{"x":1,"y":2}"#);
    }

    #[test]
    fn splice_keeps_escaped_keys_and_card_text_that_looks_like_the_separator() {
        let shards = [shard(
            "a.jsonl",
            "{\"key\":\"a\\\"b\\\\\",\"card\":{\"t\":\",\\\"card\\\":\"}}\n",
        )];
        let object: serde_json::Value = serde_json::from_str(&splice(&shards).unwrap()).unwrap();
        assert_eq!(object["a\"b\\"]["t"], ",\"card\":");
    }

    #[test]
    fn splice_names_the_shard_and_line_of_a_bad_record() {
        let shards = [shard(
            "c.jsonl",
            "{\"key\":\"c\",\"card\":1}\nnot a record\n",
        )];
        let error = splice(&shards).unwrap_err().to_string();
        assert!(error.starts_with("c.jsonl:2:"), "{error}");
    }

    #[test]
    fn splice_rejects_a_key_in_two_places() {
        let shards = [
            shard("a.jsonl", "{\"key\":\"a\",\"card\":1}\n"),
            shard("b.jsonl", "{\"key\":\"a\",\"card\":2}\n"),
        ];
        let error = splice(&shards).unwrap_err().to_string();
        assert!(
            error.contains("b.jsonl:1") && error.contains("duplicate key"),
            "{error}"
        );
    }

    #[test]
    fn read_shards_orders_by_name_and_ignores_other_files() {
        let dir = tempfile::tempdir().unwrap();
        for (name, text) in [
            ("b.jsonl", "b"),
            ("_digits.jsonl", "d"),
            ("a.jsonl", "a"),
            (".DS_Store", "x"),
        ] {
            fs::write(dir.path().join(name), text).unwrap();
        }
        let names: Vec<String> = read_shards(dir.path())
            .unwrap()
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, ["_digits.jsonl", "a.jsonl", "b.jsonl"]);
    }

    #[test]
    fn the_committed_fixture_splices_into_a_populated_object() {
        let object: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&json().unwrap()).unwrap();
        assert!(object.len() > 1000, "{} entries", object.len());
        assert!(object.contains_key("llanowar elves"));
    }
}
