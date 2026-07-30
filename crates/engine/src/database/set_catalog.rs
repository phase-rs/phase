//! MTGJSON SetList metadata used by the release-gate and legality-inference
//! pipelines. This is data-pipeline tooling, not game-rules logic.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

/// UTC calendar date (`YYYY-MM-DD`) without external date crates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReleaseDate {
    pub year: i32,
    pub month: u32,
    pub day: u32,
}

impl ReleaseDate {
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        let (year, rest) = raw.split_once('-')?;
        let (month, day) = rest.split_once('-')?;
        let year = year.parse().ok()?;
        let month = month.parse().ok()?;
        let day = day.parse().ok()?;
        if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
            return None;
        }
        Some(Self { year, month, day })
    }

    /// Whether this date is on or before `other` (inclusive).
    pub fn is_on_or_before(self, other: Self) -> bool {
        self <= other
    }
}

/// Convert a UNIX timestamp to a UTC calendar date (algorithm from Howard Hinnant).
pub fn utc_date_from_unix_secs(secs: u64) -> ReleaseDate {
    let days = secs / 86_400;
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let year = y + if m <= 2 { 1 } else { 0 };
    ReleaseDate {
        year: year as i32,
        month: m as u32,
        day: d as u32,
    }
}

/// UTC "today" for release-gate evaluation during card-data generation.
pub fn today_utc() -> ReleaseDate {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before UNIX epoch")
        .as_secs();
    utc_date_from_unix_secs(secs)
}

/// Environment variable override for the release-gate "as of" date (`YYYY-MM-DD`).
/// Used by CI (`gen-card-data.sh`) and unit tests for deterministic behavior.
pub const GATED_SETS_AS_OF_ENV: &str = "GATED_SETS_AS_OF";

/// The effective "as of" date for release-gate evaluation.
pub fn gated_sets_as_of() -> ReleaseDate {
    std::env::var(GATED_SETS_AS_OF_ENV)
        .ok()
        .and_then(|raw| ReleaseDate::parse(&raw))
        .unwrap_or_else(today_utc)
}

/// Per-set metadata extracted from MTGJSON `SetList.json`.
#[derive(Debug, Clone)]
pub struct SetMeta {
    pub code: String,
    pub name: String,
    pub release_date: Option<ReleaseDate>,
    pub set_type: Option<String>,
    pub is_online_only: bool,
    pub parent_code: Option<String>,
}

impl SetMeta {
    /// Whether the set's release date is on or before `as_of`.
    ///
    /// Sets without a recorded release date are treated as **not released** so
    /// preview gates stay conservative until MTGJSON publishes a date.
    pub fn is_released_as_of(&self, as_of: ReleaseDate) -> bool {
        self.release_date
            .is_some_and(|release| release.is_on_or_before(as_of))
    }
}

/// Index of MTGJSON set codes → metadata.
#[derive(Debug, Clone, Default)]
pub struct SetCatalog {
    pub(crate) sets: HashMap<String, SetMeta>,
}

impl SetCatalog {
    pub fn get(&self, code: &str) -> Option<&SetMeta> {
        self.sets.get(&code.to_uppercase())
    }

    pub fn len(&self) -> usize {
        self.sets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sets.is_empty()
    }

    /// Test/catalog builder helper.
    pub fn insert_test_meta(&mut self, meta: SetMeta) {
        let code = meta.code.to_uppercase();
        self.sets.insert(code, meta);
    }
}

#[derive(Deserialize)]
struct SetListFile {
    data: Vec<SetListRawEntry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetListRawEntry {
    code: String,
    name: String,
    #[serde(default)]
    release_date: Option<String>,
    #[serde(default, rename = "type")]
    set_type: Option<String>,
    #[serde(default)]
    is_online_only: bool,
    #[serde(default)]
    parent_code: Option<String>,
}

/// Load `SetList.json` from `<data_dir>/mtgjson/SetList.json`.
pub fn load_set_catalog(data_dir: &Path) -> SetCatalog {
    let path = data_dir.join("mtgjson").join("SetList.json");
    load_set_catalog_from_path(&path).unwrap_or_else(|e| {
        eprintln!(
            "warning: failed to load set catalog from {}: {e}; release-gate date filtering and legality inference will be degraded",
            path.display()
        );
        SetCatalog::default()
    })
}

pub fn load_set_catalog_from_path(path: &Path) -> Result<SetCatalog, String> {
    let contents =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let raw: SetListFile =
        serde_json::from_str(&contents).map_err(|e| format!("parse {}: {e}", path.display()))?;
    let mut sets = HashMap::new();
    for entry in raw.data {
        let code = entry.code.to_uppercase();
        sets.insert(
            code.clone(),
            SetMeta {
                code,
                name: entry.name,
                release_date: entry.release_date.as_deref().and_then(ReleaseDate::parse),
                set_type: entry.set_type,
                is_online_only: entry.is_online_only,
                parent_code: entry.parent_code.map(|c| c.to_uppercase()),
            },
        );
    }
    Ok(SetCatalog { sets })
}

/// Load from `mtgjson/sets/` parent directory (draft-pool-gen layout).
pub fn load_set_catalog_adjacent_to_sets_dir(sets_dir: &Path) -> SetCatalog {
    let data_dir = sets_dir
        .parent()
        .and_then(|p| p.parent())
        .unwrap_or_else(|| sets_dir.parent().unwrap_or(sets_dir));
    load_set_catalog(data_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_release_date_accepts_mtgjson_form() {
        let d = ReleaseDate::parse("2026-06-26").unwrap();
        assert_eq!(d.year, 2026);
        assert_eq!(d.month, 6);
        assert_eq!(d.day, 26);
    }

    #[test]
    fn release_date_ordering() {
        let pre = ReleaseDate::parse("2026-06-25").unwrap();
        let release = ReleaseDate::parse("2026-06-26").unwrap();
        let after = ReleaseDate::parse("2026-06-27").unwrap();
        assert!(pre < release);
        assert!(release < after);
        assert!(release.is_on_or_before(after));
        assert!(!after.is_on_or_before(release));
    }

    #[test]
    fn unix_epoch_maps_to_1970_01_01() {
        let d = utc_date_from_unix_secs(0);
        assert_eq!(d, ReleaseDate::parse("1970-01-01").unwrap());
    }

    #[test]
    fn msh_release_date_parses() {
        let d = ReleaseDate::parse("2026-06-26").unwrap();
        let as_of = ReleaseDate::parse("2026-06-30").unwrap();
        assert!(d.is_on_or_before(as_of));
    }

    #[test]
    fn set_meta_is_released_as_of_uses_mtgjson_date() {
        let meta = SetMeta {
            code: "MSH".into(),
            name: "Marvel Super Heroes".into(),
            release_date: ReleaseDate::parse("2026-06-26"),
            set_type: Some("expansion".into()),
            is_online_only: false,
            parent_code: None,
        };
        assert!(!meta.is_released_as_of(ReleaseDate::parse("2026-06-25").unwrap()));
        assert!(meta.is_released_as_of(ReleaseDate::parse("2026-06-26").unwrap()));
        assert!(meta.is_released_as_of(ReleaseDate::parse("2026-06-30").unwrap()));
    }
}
