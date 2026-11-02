use serde::{Deserialize, Deserializer};
use std::fs;
use std::path::Path;

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Versions {
    #[serde(deserialize_with = "deserialize_version")]
    pub(crate) full: u32,
    #[serde(deserialize_with = "deserialize_version")]
    pub(crate) wire: u32,
    #[serde(deserialize_with = "deserialize_version")]
    pub(crate) lobby: u32,
    #[serde(deserialize_with = "deserialize_version")]
    pub(crate) draft: u32,
    #[serde(deserialize_with = "deserialize_version")]
    pub(crate) directory: u32,
    #[serde(rename = "minimumLobby")]
    #[serde(deserialize_with = "deserialize_version")]
    pub(crate) minimum_lobby: u32,
}

// JSON numbers follow the Node collector's integer-value contract (1.0 and 1e0
// are integers too); deserializing straight into u32 would reject those forms.
fn deserialize_version<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u32, D::Error> {
    let number = serde_json::Number::deserialize(deserializer)?;
    let value = number
        .as_f64()
        .ok_or_else(|| serde::de::Error::custom("expected u32 integer"))?;
    if value.fract() != 0.0 || value < 0.0 || value > f64::from(u32::MAX) {
        return Err(serde::de::Error::custom("expected u32 integer"));
    }
    Ok(value as u32)
}

// ECMAScript String.trim whitespace, matching the authoring collector exactly.
fn reason_is_blank(reason: &str) -> bool {
    reason.chars().all(|c| {
        matches!(c,
            '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' |
            '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' |
            '\u{205f}' | '\u{3000}' | '\u{feff}'
        )
    })
}

// Alphabetical order is the record format's canonical selection order.
#[derive(Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
enum Surface {
    Directory,
    Draft,
    Full,
    Lobby,
    Wire,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Change {
    surfaces: Vec<Surface>,
    reason: String,
}

fn is_record_name(name: &str) -> bool {
    let Some(uuid) = name.strip_suffix(".json") else {
        return false;
    };
    uuid.len() == 36
        && uuid.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            14 => byte == b'4',
            19 => matches!(byte, b'8' | b'9' | b'a' | b'b'),
            _ => byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'),
        })
}

pub(crate) fn load_versions(root: &Path) -> Result<Versions, Box<dyn std::error::Error>> {
    let baseline: serde_json::Map<String, serde_json::Value> =
        serde_json::from_slice(&fs::read(root.join("protocol/baseline.json"))?)?;
    let mut versions: Versions = serde_json::from_value(serde_json::Value::Object(baseline))?;
    if versions.minimum_lobby > versions.lobby {
        return Err("minimumLobby must not exceed lobby".into());
    }
    for entry in fs::read_dir(root.join("protocol/changes"))? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_str().ok_or("record filename must be UTF-8")?;
        if !entry.file_type()?.is_file() {
            return Err(format!("{name}: expected a regular record file").into());
        }
        if name == ".gitkeep" {
            if entry.metadata()?.len() != 0 {
                return Err(".gitkeep marker must be empty".into());
            }
            continue;
        }
        if !is_record_name(name) {
            return Err(format!("{name}: expected a lowercase UUID v4 .json filename").into());
        }
        let record: serde_json::Map<String, serde_json::Value> =
            serde_json::from_slice(&fs::read(entry.path())?)?;
        let change: Change = serde_json::from_value(serde_json::Value::Object(record))?;
        if reason_is_blank(&change.reason)
            || change.surfaces.is_empty()
            || change.surfaces.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(
                format!("{name}: expected reason and sorted unique nonempty surfaces").into(),
            );
        }
        for surface in change.surfaces {
            let value = match surface {
                Surface::Directory => &mut versions.directory,
                Surface::Draft => &mut versions.draft,
                Surface::Full => &mut versions.full,
                Surface::Lobby => &mut versions.lobby,
                Surface::Wire => &mut versions.wire,
            };
            *value = value
                .checked_add(1)
                .ok_or("protocol version exceeds u32 maximum")?;
        }
    }
    Ok(versions)
}

#[cfg(not(test))]
fn main() {
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo manifest directory");
    let root = Path::new(&manifest).join("../..");
    println!(
        "cargo:rerun-if-changed={}",
        root.join("protocol/baseline.json").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        root.join("protocol/changes").display()
    );
    let versions = load_versions(&root).expect("valid protocol registry");
    let output = format!(
        "pub const PROTOCOL_VERSION: u32 = {};\npub const LOBBY_PROTOCOL_VERSION: u32 = {};\npub const MIN_SUPPORTED_LOBBY_PROTOCOL: u32 = {};\npub const DIRECTORY_VERSION: u32 = {};\n",
        versions.full, versions.lobby, versions.minimum_lobby, versions.directory
    );
    let out_dir = std::env::var_os("OUT_DIR").expect("Cargo output directory");
    fs::write(Path::new(&out_dir).join("protocol_versions.rs"), output)
        .expect("write protocol constants");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Registry(std::path::PathBuf);
    impl Registry {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "phase-protocol-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(root.join("protocol/changes")).unwrap();
            let fixture = Self(root);
            fixture.baseline(
                r#"{"full":10,"wire":20,"lobby":3,"draft":4,"directory":5,"minimumLobby":2}"#,
            );
            fixture
        }
        fn baseline(&self, text: &str) {
            fs::write(self.0.join("protocol/baseline.json"), text).unwrap();
        }
        fn record(&self, name: &str, text: &str) {
            fs::write(self.0.join("protocol/changes").join(name), text).unwrap();
        }
    }
    impl Drop for Registry {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
    const FIRST: &str = "00000000-0000-4000-8000-000000000001.json";
    const SECOND: &str = "00000000-0000-4000-8000-000000000002.json";

    #[test]
    fn registry_counts_independent_records_on_every_axis() {
        let fixture = Registry::new();
        fixture.record(".gitkeep", "");
        fixture.record(
            FIRST,
            r#"{"surfaces":["directory","draft","full","lobby","wire"],"reason":"shape"}"#,
        );
        fixture.record(SECOND, r#"{"surfaces":["full"],"reason":"another shape"}"#);
        assert_eq!(
            load_versions(&fixture.0).unwrap(),
            Versions {
                full: 12,
                wire: 21,
                lobby: 4,
                draft: 5,
                directory: 6,
                minimum_lobby: 2,
            }
        );
        fs::remove_file(fixture.0.join("protocol/changes").join(SECOND)).unwrap();
        assert_eq!(load_versions(&fixture.0).unwrap().full, 11);
    }

    #[test]
    fn registry_requires_object_baseline() {
        let fixture = Registry::new();
        let expected = load_versions(&fixture.0).unwrap();
        fixture.baseline("[10,20,3,4,5,2]");
        assert!(load_versions(&fixture.0).is_err());
        fixture.baseline(
            r#"{"full":10,"wire":20,"lobby":3,"draft":4,"directory":5,"minimumLobby":2}"#,
        );
        assert_eq!(load_versions(&fixture.0).unwrap(), expected);
    }

    #[test]
    fn registry_requires_object_record() {
        let fixture = Registry::new();
        fixture.record(FIRST, r#"{"surfaces":["full"],"reason":"shape"}"#);
        let expected = load_versions(&fixture.0).unwrap();
        assert_eq!(expected.full, 11);
        fixture.record(FIRST, r#"[["full"],"shape"]"#);
        assert!(load_versions(&fixture.0).is_err());
        fixture.record(FIRST, r#"{"surfaces":["full"],"reason":"shape"}"#);
        assert_eq!(load_versions(&fixture.0).unwrap(), expected);
    }

    #[test]
    fn registry_matches_javascript_numeric_and_whitespace_validation() {
        let fixture = Registry::new();
        fixture.baseline(
            r#"{"full":1e1,"wire":20.0,"lobby":3,"draft":4,"directory":5,"minimumLobby":-0}"#,
        );
        let versions = load_versions(&fixture.0).unwrap();
        assert_eq!(versions.full, 10);
        assert_eq!(versions.wire, 20);
        assert_eq!(versions.minimum_lobby, 0);
        fixture.record(FIRST, r#"{"surfaces":["full"],"reason":"\ufeff"}"#);
        assert!(load_versions(&fixture.0).is_err());
        fixture.record(FIRST, r#"{"surfaces":["full"],"reason":"\u0085"}"#);
        assert_eq!(load_versions(&fixture.0).unwrap().full, 11);
    }

    #[test]
    fn registry_rejects_invalid_records() {
        for record in [
            r#"{"surfaces":[],"reason":"shape"}"#,
            r#"{"surfaces":["full","full"],"reason":"shape"}"#,
            r#"{"surfaces":["wire","full"],"reason":"shape"}"#,
            r#"{"surfaces":["game"],"reason":"shape"}"#,
            r#"{"surfaces":["full"],"reason":" "}"#,
            r#"{"surfaces":["full"],"reason":"shape","extra":1}"#,
            r#"{"surfaces":["full"]}"#,
            "not json",
        ] {
            let fixture = Registry::new();
            assert!(load_versions(&fixture.0).is_ok());
            fixture.record(FIRST, record);
            assert!(load_versions(&fixture.0).is_err(), "{record}");
        }
    }

    #[test]
    fn registry_rejects_invalid_baselines_and_overflow() {
        for baseline in [
            r#"{"full":-1,"wire":20,"lobby":3,"draft":4,"directory":5,"minimumLobby":2}"#,
            r#"{"full":1.5,"wire":20,"lobby":3,"draft":4,"directory":5,"minimumLobby":2}"#,
            r#"{"full":4294967296,"wire":20,"lobby":3,"draft":4,"directory":5,"minimumLobby":2}"#,
            r#"{"full":1,"wire":20,"lobby":1,"draft":4,"directory":5,"minimumLobby":2}"#,
            r#"{"full":1,"wire":20,"lobby":3,"draft":4,"directory":5}"#,
            r#"{"full":1,"wire":20,"lobby":3,"draft":4,"directory":5,"minimumLobby":2,"extra":0}"#,
        ] {
            let fixture = Registry::new();
            fixture.baseline(baseline);
            assert!(load_versions(&fixture.0).is_err(), "{baseline}");
        }
        let fixture = Registry::new();
        fixture.baseline(
            r#"{"full":4294967295,"wire":20,"lobby":3,"draft":4,"directory":5,"minimumLobby":2}"#,
        );
        assert!(load_versions(&fixture.0).is_ok());
        fixture.record(FIRST, r#"{"surfaces":["full"],"reason":"shape"}"#);
        assert!(load_versions(&fixture.0).is_err());
    }

    #[test]
    fn registry_rejects_invalid_entries() {
        for name in [
            "bad.json",
            "00000000-0000-1000-8000-000000000001.json",
            "00000000-0000-4000-7000-000000000001.json",
            "00000000-0000-4000-A000-000000000001.json",
            ".gitkeep",
        ] {
            let fixture = Registry::new();
            fixture.record(name, "{}");
            assert!(load_versions(&fixture.0).is_err());
        }
        let fixture = Registry::new();
        fs::create_dir(fixture.0.join("protocol/changes").join(FIRST)).unwrap();
        assert!(load_versions(&fixture.0).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn registry_rejects_symlink_records() {
        let fixture = Registry::new();
        std::os::unix::fs::symlink(
            fixture.0.join("protocol/baseline.json"),
            fixture.0.join("protocol/changes").join(FIRST),
        )
        .unwrap();
        assert!(load_versions(&fixture.0).is_err());
    }
}
