//! Curated coverage classification, independent of parsed rules and gameplay.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

use super::card::CardFace;

/// The census's semantic-defect categories, not parser or resolver diagnoses.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, strum::EnumIter,
)]
#[serde(rename_all = "snake_case")]
pub enum MisparseClass {
    DroppedQualifier,
    WrongPlayer,
    WrongObject,
    WrongCount,
    WrongDuration,
    WrongZone,
    MissingEffect,
    ExtraEffect,
    CostWrong,
    ConditionWrong,
    TriggerWrong,
    TargetWrong,
    Other,
}

impl MisparseClass {
    /// Public coverage handler; no clause prose is included in runtime gaps.
    pub fn handler(self) -> &'static str {
        match self {
            Self::DroppedQualifier => "Misparse:dropped_qualifier",
            Self::WrongPlayer => "Misparse:wrong_player",
            Self::WrongObject => "Misparse:wrong_object",
            Self::WrongCount => "Misparse:wrong_count",
            Self::WrongDuration => "Misparse:wrong_duration",
            Self::WrongZone => "Misparse:wrong_zone",
            Self::MissingEffect => "Misparse:missing_effect",
            Self::ExtraEffect => "Misparse:extra_effect",
            Self::CostWrong => "Misparse:cost_wrong",
            Self::ConditionWrong => "Misparse:condition_wrong",
            Self::TriggerWrong => "Misparse:trigger_wrong",
            Self::TargetWrong => "Misparse:target_wrong",
            Self::Other => "Misparse:other",
        }
    }
}

/// One exact exported face and its census-cited broken clauses.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfirmedMisparse {
    pub key: String,
    pub class: MisparseClass,
    pub clauses: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MisparseFile {
    face: Vec<ConfirmedMisparse>,
}

/// Deterministically ordered, exact-key membership authority.
#[derive(Debug, Default)]
pub struct ConfirmedMisparses {
    faces: BTreeMap<String, ConfirmedMisparse>,
}

/// Export diagnostics only: absent keys do not invalidate the population.
#[derive(Debug, PartialEq, Eq)]
pub struct MisparseStamp<'a> {
    pub marked: usize,
    pub unmatched_keys: Vec<&'a str>,
}

impl ConfirmedMisparses {
    pub fn parse(raw: &str) -> Result<Self, String> {
        let file: MisparseFile = toml::from_str(raw).map_err(|error| error.to_string())?;
        let mut faces = BTreeMap::new();
        for face in file.face {
            match faces.entry(face.key.clone()) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(face);
                }
                std::collections::btree_map::Entry::Occupied(entry) => {
                    return Err(format!("duplicate confirmed-misparse key: {}", entry.key()));
                }
            }
        }
        Ok(Self { faces })
    }

    pub fn committed() -> &'static Self {
        static LIST: LazyLock<ConfirmedMisparses> = LazyLock::new(|| {
            ConfirmedMisparses::parse(include_str!("../../data/confirmed-misparses.toml"))
                .expect("committed confirmed-misparses.toml must be valid")
        });
        &LIST
    }

    pub fn empty() -> Self {
        Self::default()
    }

    pub fn class_of(&self, key: &str) -> Option<MisparseClass> {
        self.faces.get(key).map(|face| face.class)
    }

    pub fn iter(&self) -> impl Iterator<Item = &ConfirmedMisparse> {
        self.faces.values()
    }

    pub fn len(&self) -> usize {
        self.faces.len()
    }

    pub fn is_empty(&self) -> bool {
        self.faces.is_empty()
    }

    /// Replace stamps using final export keys, never the face's printed name.
    /// An empty population clears any previous marker without changing rules.
    pub fn stamp_all<'list, 'face>(
        &'list self,
        faces: impl IntoIterator<Item = (&'face str, &'face mut CardFace)>,
    ) -> MisparseStamp<'list> {
        let mut unmatched: BTreeSet<&str> = self.faces.keys().map(String::as_str).collect();
        let mut marked = 0;
        for (key, face) in faces {
            face.metadata.confirmed_misparse = self.class_of(key);
            if face.metadata.confirmed_misparse.is_some() {
                marked += 1;
                unmatched.remove(key);
            }
        }
        MisparseStamp {
            marked,
            unmatched_keys: unmatched.into_iter().collect(),
        }
    }
}
