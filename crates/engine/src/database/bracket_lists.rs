//! Bracket list data — dated, sourced curated card-name lists that drive the
//! Commander bracket estimator's axes.
//!
//! This is NOT a Comprehensive Rules artifact — the bracket system is WotC's
//! Commander Format Panel policy, not part of the CR. No rules annotation
//! belongs on anything in this module.
//!
//! TWO LAYERS, deliberately separate:
//!   * `BracketCardClass` (here) — what a card does; one curated list per class.
//!   * `BracketAxis` (`game/bracket_estimate.rs`) — what WotC counts; unchanged.
//!     The mapping is many-to-one and partial: two classes feed mass land denial,
//!     and extra combats feed no axis at all.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

/// **Frozen wire DTO.** Per-card bracket signal flags stamped onto every
/// `CardExportEntry`. Its field names, order, defaults, and derives preserve
/// the existing `card-data.json` and AI-worker subset representation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BracketSignals {
    #[serde(default)]
    pub game_changer: bool,
    #[serde(default)]
    pub mass_land_denial: bool,
    #[serde(default)]
    pub extra_turn: bool,
    #[serde(default)]
    pub efficient_tutor: bool,
}

impl BracketSignals {
    /// Convert the legacy wire booleans into the engine's axis-keyed domain set.
    pub fn axes(self) -> BTreeSet<crate::game::bracket_estimate::BracketAxis> {
        use crate::game::bracket_estimate::BracketAxis;
        use strum::IntoEnumIterator;

        BracketAxis::iter()
            .filter(|axis| match axis {
                BracketAxis::GameChangers => self.game_changer,
                BracketAxis::MassLandDenial => self.mass_land_denial,
                BracketAxis::ExtraTurns => self.extra_turn,
                BracketAxis::EfficientTutors => self.efficient_tutor,
            })
            .collect()
    }

    /// Convert an axis-keyed domain set back to the frozen wire DTO.
    pub fn from_axes(axes: &BTreeSet<crate::game::bracket_estimate::BracketAxis>) -> Self {
        use crate::game::bracket_estimate::BracketAxis;

        Self {
            game_changer: axes.contains(&BracketAxis::GameChangers),
            mass_land_denial: axes.contains(&BracketAxis::MassLandDenial),
            extra_turn: axes.contains(&BracketAxis::ExtraTurns),
            efficient_tutor: axes.contains(&BracketAxis::EfficientTutors),
        }
    }

    pub fn is_clean(self) -> bool {
        !self.game_changer && !self.mass_land_denial && !self.extra_turn && !self.efficient_tutor
    }
}

/// A curated card-behaviour class. One independently dated list per variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BracketCardClass {
    /// WotC's published Game Changers list, authoritative over MTGJSON when present.
    GameChangers,
    /// One-shot mass destruction, exile, or bounce of lands.
    MassLandSweepers,
    /// Persistent land tap-down or mana-type changes.
    MassManaDenial,
    /// Effects that grant an extra turn.
    ExtraTurns,
    /// Effects that add a combat phase. Evidence only; feeds no policy axis.
    ExtraCombats,
    /// Superseded tutor guidance, retained as evidence.
    EfficientTutors,
}

impl BracketCardClass {
    pub const ALL: [Self; 6] = [
        Self::GameChangers,
        Self::MassLandSweepers,
        Self::MassManaDenial,
        Self::ExtraTurns,
        Self::ExtraCombats,
        Self::EfficientTutors,
    ];
}

/// Where a curated list was read from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicySource {
    pub url: String,
    pub published: String,
    pub retrieved: String,
    #[serde(default)]
    pub local_copy: Option<String>,
}

/// One dated, sourced curated list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CuratedList {
    pub source: PolicySource,
    #[serde(default)]
    pub superseded_by: Option<PolicySource>,
    #[serde(default)]
    pub note: Option<String>,
    pub names: Vec<String>,
}

/// The class-keyed curated lists and the revision tag of this data file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BracketLists {
    pub version: String,
    lists: BTreeMap<BracketCardClass, LoadedList>,
}

/// Validated in-memory form of a curated list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedList {
    pub source: PolicySource,
    pub superseded_by: Option<PolicySource>,
    pub note: Option<String>,
    /// Lowercased for deterministic, case-insensitive lookup.
    pub names: BTreeSet<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBracketLists {
    version: String,
    #[serde(default)]
    lists: BTreeMap<BracketCardClass, CuratedList>,
}

impl BracketLists {
    pub fn from_json_path(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let raw = std::fs::read_to_string(path)?;
        Self::from_json_str(&raw)
    }

    pub fn from_json_str(raw: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let parsed: RawBracketLists = serde_json::from_str(raw)?;
        if parsed.version.trim().is_empty() {
            return Err(validation_error("version must not be empty"));
        }

        let mut lists = BTreeMap::new();
        for (class, list) in parsed.lists {
            validate_source(&list.source, "source")?;
            if let Some(source) = &list.superseded_by {
                validate_source(source, "superseded_by")?;
            }
            if list.names.is_empty() {
                return Err(validation_error(format!(
                    "{class:?}.names must not be empty"
                )));
            }

            let original_len = list.names.len();
            let names: BTreeSet<String> = list
                .names
                .into_iter()
                .map(|name| name.to_lowercase())
                .collect();
            if names.len() != original_len {
                return Err(validation_error(format!(
                    "{class:?}.names contains a duplicate after case folding"
                )));
            }

            lists.insert(
                class,
                LoadedList {
                    source: list.source,
                    superseded_by: list.superseded_by,
                    note: list.note,
                    names,
                },
            );
        }

        Ok(Self {
            version: parsed.version,
            lists,
        })
    }

    /// Test/builder constructor that avoids hand-writing schema JSON.
    pub fn from_pairs(version: &str, pairs: &[(BracketCardClass, &[&str])]) -> Self {
        let lists = pairs
            .iter()
            .map(|(class, names)| {
                (
                    *class,
                    LoadedList {
                        source: PolicySource {
                            url: String::new(),
                            published: String::new(),
                            retrieved: String::new(),
                            local_copy: None,
                        },
                        superseded_by: None,
                        note: None,
                        names: names.iter().map(|name| name.to_lowercase()).collect(),
                    },
                )
            })
            .collect();
        Self {
            version: version.to_string(),
            lists,
        }
    }

    pub fn get(&self, class: BracketCardClass) -> Option<&LoadedList> {
        self.lists.get(&class)
    }

    pub fn has(&self, class: BracketCardClass) -> bool {
        self.lists.contains_key(&class)
    }

    /// Every class this name belongs to.
    pub fn classes_for(&self, name: &str) -> BTreeSet<BracketCardClass> {
        let key = name.to_lowercase();
        self.lists
            .iter()
            .filter_map(|(class, list)| list.names.contains(&key).then_some(*class))
            .collect()
    }

    /// Names on both lists, lowercased.
    pub fn overlap(&self, a: BracketCardClass, b: BracketCardClass) -> BTreeSet<&str> {
        let (Some(a), Some(b)) = (self.get(a), self.get(b)) else {
            return BTreeSet::new();
        };
        a.names.intersection(&b.names).map(String::as_str).collect()
    }

    /// Look up the bracket signals for a card name (case-insensitive).
    pub fn signals_for(&self, name: &str) -> BracketSignals {
        let mut signals = BracketSignals::default();
        for class in self.classes_for(name) {
            match class {
                BracketCardClass::GameChangers => signals.game_changer = true,
                BracketCardClass::MassLandSweepers | BracketCardClass::MassManaDenial => {
                    signals.mass_land_denial = true;
                }
                BracketCardClass::ExtraTurns => signals.extra_turn = true,
                BracketCardClass::ExtraCombats => {}
                BracketCardClass::EfficientTutors => signals.efficient_tutor = true,
            }
        }
        signals
    }

    /// True when `name` appears on any curated list.
    pub fn contains(&self, name: &str) -> bool {
        !self.classes_for(name).is_empty()
    }

    /// Iterate every distinct lowercased card name across all six lists.
    pub fn all_names(&self) -> impl Iterator<Item = &str> {
        self.lists
            .values()
            .flat_map(|list| list.names.iter().map(String::as_str))
            .collect::<BTreeSet<_>>()
            .into_iter()
    }
}

fn validate_source(source: &PolicySource, field: &str) -> Result<(), Box<dyn std::error::Error>> {
    for (date_field, date) in [
        ("published", source.published.as_str()),
        ("retrieved", source.retrieved.as_str()),
    ] {
        if !is_date_shaped(date) {
            return Err(validation_error(format!(
                "{field}.{date_field} must have YYYY-MM-DD shape"
            )));
        }
    }
    Ok(())
}

fn is_date_shaped(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes[0..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[7] == b'-'
        && bytes[8..10].iter().all(u8::is_ascii_digit)
}

fn validation_error(message: impl Into<String>) -> Box<dyn std::error::Error> {
    Box::new(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        message.into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "version": "test-1",
        "lists": {
            "game_changers": {
                "source": {
                    "url": "https://example.test/game-changers",
                    "published": "2026-02-09",
                    "retrieved": "2026-09-12"
                },
                "names": ["Demonic Tutor"]
            },
            "mass_land_sweepers": {
                "source": {
                    "url": "https://example.test/land",
                    "published": "2025-02-11",
                    "retrieved": "2026-09-12"
                },
                "names": ["Armageddon"]
            },
            "mass_mana_denial": {
                "source": {
                    "url": "https://example.test/mana",
                    "published": "2025-02-11",
                    "retrieved": "2026-09-12"
                },
                "names": ["Winter Orb"]
            },
            "extra_turns": {
                "source": {
                    "url": "https://example.test/turns",
                    "published": "2025-02-11",
                    "retrieved": "2026-09-12"
                },
                "names": ["Time Warp"]
            },
            "extra_combats": {
                "source": {
                    "url": "phase.rs engine taxonomy",
                    "published": "2026-09-12",
                    "retrieved": "2026-09-12"
                },
                "names": ["Aggravated Assault"]
            },
            "efficient_tutors": {
                "source": {
                    "url": "https://example.test/tutors",
                    "published": "2025-02-11",
                    "retrieved": "2026-09-12"
                },
                "names": ["Demonic Tutor", "Vampiric Tutor"]
            }
        }
    }"#;

    #[test]
    fn schema_parses_class_keyed_lists() {
        let lists = BracketLists::from_json_str(SAMPLE).unwrap();
        assert_eq!(lists.version, "test-1");
        assert_eq!(
            lists
                .get(BracketCardClass::EfficientTutors)
                .unwrap()
                .names
                .len(),
            2
        );
    }

    #[test]
    fn class_is_usable_as_a_json_map_key() {
        let source = PolicySource {
            url: "https://example.test".into(),
            published: "2026-02-09".into(),
            retrieved: "2026-09-12".into(),
            local_copy: None,
        };
        let map = BTreeMap::from([(
            BracketCardClass::GameChangers,
            CuratedList {
                source,
                superseded_by: None,
                note: None,
                names: vec!["Farewell".into()],
            },
        )]);
        let json = serde_json::to_string(&map).unwrap();
        let round_trip: BTreeMap<BracketCardClass, CuratedList> =
            serde_json::from_str(&json).unwrap();
        assert_eq!(map, round_trip);
    }

    #[test]
    fn unknown_field_in_source_is_an_error() {
        let malformed = SAMPLE.replace(
            "\"published\": \"2026-02-09\"",
            "\"sourceDate\": \"2026-02-09\"",
        );
        assert!(BracketLists::from_json_str(&malformed).is_err());
    }

    #[test]
    fn duplicate_name_within_a_list_is_an_error() {
        let malformed = SAMPLE.replace(
            "\"names\": [\"Time Warp\"]",
            "\"names\": [\"Time Warp\", \"TIME WARP\"]",
        );
        assert!(BracketLists::from_json_str(&malformed).is_err());
    }

    #[test]
    fn empty_list_is_an_error() {
        let malformed = SAMPLE.replace("\"names\": [\"Time Warp\"]", "\"names\": []");
        assert!(BracketLists::from_json_str(&malformed).is_err());
    }

    #[test]
    fn empty_version_is_an_error() {
        let malformed = SAMPLE.replace("\"version\": \"test-1\"", "\"version\": \"\"");
        assert!(BracketLists::from_json_str(&malformed).is_err());
    }

    #[test]
    fn malformed_source_date_is_an_error() {
        let malformed = SAMPLE.replace("2026-02-09", "2026-2-9");
        assert!(BracketLists::from_json_str(&malformed).is_err());
    }

    #[test]
    fn signals_for_unions_both_mass_classes() {
        let lists = BracketLists::from_json_str(SAMPLE).unwrap();
        assert!(lists.signals_for("Armageddon").mass_land_denial);
        assert!(lists.signals_for("Winter Orb").mass_land_denial);
    }

    #[test]
    fn extra_combats_membership_sets_no_signal() {
        let lists = BracketLists::from_json_str(SAMPLE).unwrap();
        assert!(lists.signals_for("Aggravated Assault").is_clean());
    }

    #[test]
    fn classes_for_sees_evidence_only_classes() {
        let lists = BracketLists::from_json_str(SAMPLE).unwrap();
        assert_eq!(
            lists.classes_for("Aggravated Assault"),
            BTreeSet::from([BracketCardClass::ExtraCombats])
        );
    }

    #[test]
    fn game_changer_signal_comes_from_the_curated_list() {
        let lists = BracketLists::from_json_str(SAMPLE).unwrap();
        assert!(lists.signals_for("Demonic Tutor").game_changer);
    }

    #[test]
    fn overlap_is_computed_not_stored() {
        let lists = BracketLists::from_json_str(SAMPLE).unwrap();
        assert_eq!(
            lists.overlap(
                BracketCardClass::GameChangers,
                BracketCardClass::EfficientTutors
            ),
            BTreeSet::from(["demonic tutor"])
        );
    }

    #[test]
    fn all_names_dedups_across_six_lists() {
        let lists = BracketLists::from_json_str(SAMPLE).unwrap();
        assert_eq!(lists.all_names().count(), 6);
    }

    #[test]
    fn bracket_signals_wire_shape_is_byte_stable() {
        use crate::game::bracket_estimate::BracketAxis;

        let signals = BracketSignals::from_axes(&BTreeSet::from([BracketAxis::GameChangers]));
        assert_eq!(
            serde_json::to_string(&signals).unwrap(),
            r#"{"game_changer":true,"mass_land_denial":false,"extra_turn":false,"efficient_tutor":false}"#
        );
    }

    #[test]
    fn legacy_partial_signals_object_deserializes_with_signals_intact() {
        use crate::game::bracket_estimate::BracketAxis;

        let signals: BracketSignals = serde_json::from_str(r#"{"game_changer":true}"#).unwrap();
        assert_eq!(signals.axes(), BTreeSet::from([BracketAxis::GameChangers]));
        assert!(serde_json::from_str::<BracketSignals>("{}")
            .unwrap()
            .axes()
            .is_empty());
    }

    #[test]
    fn signals_axes_round_trip_over_every_axis() {
        use crate::game::bracket_estimate::BracketAxis;
        use strum::IntoEnumIterator;

        for axis in BracketAxis::iter() {
            let axes = BTreeSet::from([axis]);
            assert_eq!(BracketSignals::from_axes(&axes).axes(), axes);
        }
        let all = BracketAxis::iter().collect();
        assert_eq!(BracketSignals::from_axes(&all).axes(), all);
    }
}
