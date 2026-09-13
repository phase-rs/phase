//! Commander bracket estimator. Profiles a Commander deck along four axes
//! (Game Changers, Mass Land Denial, Extra Turns, Efficient Tutors) and
//! returns a `BracketEstimate` placing the deck in bracket B1–B4.
//!
//! Pure: no game state, no I/O, no randomness. Same `(deck, db)` →
//! identical `BracketEstimate`.
//!
//! Bracket policy is **not** part of the Comprehensive Rules — it is WotC's
//! Commander Format Panel guidance. No `// CR` annotations apply.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::database::CardDatabase;
use crate::game::deck_loading::PlayerDeckList;

/// Commander bracket tier. The estimator never returns `Cedh` — that is a
/// meta self-declaration kept on the frontend's existing manual picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommanderBracketTier {
    Exhibition, // B1
    Core,       // B2
    Upgraded,   // B3
    Optimized,  // B4
    Cedh,       // B5 (manual-declaration only; estimator never returns this)
}

impl Default for CommanderBracketTier {
    /// Default to `Core` (B2) — the most common casual tier and the
    /// value used by all legacy construction sites that predate the
    /// `bracket_tier` field on `PlayerDeckPool`.
    fn default() -> Self {
        Self::Core
    }
}

impl std::fmt::Display for CommanderBracketTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Exhibition => "Exhibition",
            Self::Core => "Core",
            Self::Upgraded => "Upgraded",
            Self::Optimized => "Optimized",
            Self::Cedh => "Cedh",
        };
        write!(f, "{name}")
    }
}

/// Every label [`CommanderBracketTier::from_label`] accepts.
pub const ACCEPTED_BRACKET_LABELS: &[&str] =
    &["Exhibition", "Core", "Upgraded", "Optimized", "Cedh"];

impl CommanderBracketTier {
    /// Parses a case-insensitive, whitespace-trimmed bracket label.
    pub fn from_label(label: &str) -> Option<Self> {
        match label.trim().to_lowercase().as_str() {
            "exhibition" => Some(Self::Exhibition),
            "core" => Some(Self::Core),
            "upgraded" => Some(Self::Upgraded),
            "optimized" => Some(Self::Optimized),
            "cedh" => Some(Self::Cedh),
            _ => None,
        }
    }

    /// Numeric bracket level (B1..=B5 → 1..=5). Used for ordered
    /// comparisons (e.g., sorting violations by tier).
    pub fn as_u8(self) -> u8 {
        match self {
            Self::Exhibition => 1,
            Self::Core => 2,
            Self::Upgraded => 3,
            Self::Optimized => 4,
            Self::Cedh => 5,
        }
    }
}

/// One axis that forced the deck above a tier ceiling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BracketViolation {
    pub axis: BracketAxis,
    pub count: u8,
    pub prior_cap: u8,
    pub forced_floor: CommanderBracketTier,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, strum::EnumIter,
)]
#[serde(rename_all = "snake_case")]
pub enum BracketAxis {
    GameChangers,
    MassLandDenial,
    ExtraTurns,
    EfficientTutors,
}

/// One bracket axis's reading for a deck.
///
/// Bracket policy is WotC Commander Format Panel guidance, not the
/// Comprehensive Rules, so no rules annotation applies.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AxisReading {
    pub count: u8,
    /// `None` means no cap on this axis at the resolved tier. This must remain
    /// an explicit JSON `null` so the frontend can validate every reading.
    pub cap_at_tier: Option<u8>,
    /// Cards that counted toward this axis, in deck order (commander first).
    pub contributing: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BracketEstimate {
    pub tier: CommanderBracketTier,
    /// Every `BracketAxis` is present, including zero-count axes.
    pub axes: BTreeMap<BracketAxis, AxisReading>,
    /// At most one violation per axis — keyed by `BracketAxis` so the
    /// invariant is expressed in the type. Iterate in `BracketAxis`
    /// declaration order (BTreeMap) or sort by `forced_floor` on the
    /// consumer side.
    pub violations: BTreeMap<BracketAxis, BracketViolation>,
    /// `BracketLists.version`, passed through from the export pipeline.
    pub data_version: String,
}

/// Returns `None` when the deck has no commander.
pub fn estimate_bracket(deck: &PlayerDeckList, db: &CardDatabase) -> Option<BracketEstimate> {
    use strum::IntoEnumIterator;

    if deck.commander.is_empty() {
        return None;
    }

    let mut axes: BTreeMap<BracketAxis, AxisReading> = BracketAxis::iter()
        .map(|axis| (axis, AxisReading::default()))
        .collect();

    let all_cards = deck.commander.iter().chain(deck.main_deck.iter());
    for name in all_cards {
        for axis in db.bracket_signals_for(name).axes() {
            let reading = axes.entry(axis).or_default();
            reading.count = reading.count.saturating_add(1);
            reading.contributing.push(name.clone());
        }
    }

    let (tier, violations) = decide_tier(&axes);
    for (axis, reading) in &mut axes {
        reading.cap_at_tier = cap_for(*axis, tier);
    }

    Some(BracketEstimate {
        tier,
        axes,
        violations,
        data_version: db.bracket_lists.version.clone(),
    })
}

/// Per-axis caps for each tier. `u8::MAX` represents an effectively infinite
/// cap (no upper bound at that tier).
///
/// Table layout: each row is `(axis, [B1_cap, B2_cap, B3_cap, B4_cap])`.
/// The estimator raises the tier floor whenever `axis_count > cap[tier_idx]`.
const CAPS: &[(BracketAxis, [u8; 4])] = &[
    (BracketAxis::GameChangers, [0, 0, 3, u8::MAX]),
    (BracketAxis::MassLandDenial, [0, 0, 0, u8::MAX]),
    (BracketAxis::ExtraTurns, [0, 0, u8::MAX, u8::MAX]),
    (BracketAxis::EfficientTutors, [0, 2, u8::MAX, u8::MAX]),
];

const TIERS: [CommanderBracketTier; 4] = [
    CommanderBracketTier::Exhibition,
    CommanderBracketTier::Core,
    CommanderBracketTier::Upgraded,
    CommanderBracketTier::Optimized,
];

/// Walks `axes` against the per-axis cap table. For each axis whose count
/// exceeds at least one tier ceiling, emits exactly one `BracketViolation`
/// recording the highest ceiling crossed. The returned tier is the max
/// floor across axes. Violations are keyed by `BracketAxis` (at most one
/// per axis — the type expresses this invariant). Callers that need display
/// ordering should sort by `forced_floor` on their side.
fn decide_tier(
    axes: &BTreeMap<BracketAxis, AxisReading>,
) -> (
    CommanderBracketTier,
    BTreeMap<BracketAxis, BracketViolation>,
) {
    let mut floor_index: usize = 0;
    let mut violations: BTreeMap<BracketAxis, BracketViolation> = BTreeMap::new();

    for (axis, caps) in CAPS {
        let count = axes.get(axis).map_or(0, |reading| reading.count);
        let mut highest_crossed: Option<(u8, CommanderBracketTier)> = None;
        for (tier_idx, cap) in caps.iter().enumerate() {
            if count > *cap {
                let new_floor = (tier_idx + 1).min(TIERS.len() - 1);
                if new_floor > floor_index {
                    floor_index = new_floor;
                }
                highest_crossed = Some((*cap, TIERS[new_floor]));
            }
        }
        if let Some((cap, forced_floor)) = highest_crossed {
            violations.insert(
                *axis,
                BracketViolation {
                    axis: *axis,
                    count,
                    prior_cap: cap,
                    forced_floor,
                },
            );
        }
    }

    (TIERS[floor_index], violations)
}

/// Cap for one axis at one tier. `None` means uncapped (`u8::MAX` in `CAPS`).
fn cap_for(axis: BracketAxis, tier: CommanderBracketTier) -> Option<u8> {
    let tier_idx = match tier {
        CommanderBracketTier::Exhibition => 0,
        CommanderBracketTier::Core => 1,
        CommanderBracketTier::Upgraded => 2,
        // cEDH caps mirror B4 (no caps at this tier).
        CommanderBracketTier::Optimized | CommanderBracketTier::Cedh => 3,
    };
    CAPS.iter()
        .find(|(candidate, _)| *candidate == axis)
        .and_then(|(_, caps)| match caps[tier_idx] {
            u8::MAX => None,
            value => Some(value),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::bracket_lists::{BracketCardClass, BracketLists};
    use crate::database::{BracketSignals, CardDatabase};
    use crate::game::deck_loading::PlayerDeckList;

    fn db_with_signals(entries: &[(&str, BracketSignals)]) -> CardDatabase {
        let mass_land_denial: Vec<&str> = entries
            .iter()
            .filter(|(_, signals)| signals.mass_land_denial)
            .map(|(name, _)| *name)
            .collect();
        let extra_turns: Vec<&str> = entries
            .iter()
            .filter(|(_, signals)| signals.extra_turn)
            .map(|(name, _)| *name)
            .collect();
        let efficient_tutors: Vec<&str> = entries
            .iter()
            .filter(|(_, signals)| signals.efficient_tutor)
            .map(|(name, _)| *name)
            .collect();
        let mut db = CardDatabase::default().with_bracket_lists(BracketLists::from_pairs(
            "test-1",
            &[
                (
                    BracketCardClass::MassLandSweepers,
                    mass_land_denial.as_slice(),
                ),
                (BracketCardClass::ExtraTurns, extra_turns.as_slice()),
                (
                    BracketCardClass::EfficientTutors,
                    efficient_tutors.as_slice(),
                ),
            ],
        ));
        db.bracket_signals_by_name = entries
            .iter()
            .map(|(name, signals)| (name.to_lowercase(), *signals))
            .collect();
        db
    }

    fn deck(commander: Vec<&str>, main: Vec<&str>) -> PlayerDeckList {
        PlayerDeckList {
            commander: commander.into_iter().map(String::from).collect(),
            main_deck: main.into_iter().map(String::from).collect(),
            sideboard: Vec::new(),
            ..Default::default()
        }
    }

    #[test]
    fn empty_deck_returns_none() {
        let db = CardDatabase::default();
        let d = deck(vec![], vec![]);
        assert!(estimate_bracket(&d, &db).is_none());
    }

    #[test]
    fn no_commander_returns_none() {
        let db = CardDatabase::default();
        let d = deck(vec![], vec!["Forest", "Island"]);
        assert!(estimate_bracket(&d, &db).is_none());
    }

    #[test]
    fn clean_deck_is_b1_exhibition() {
        let db = db_with_signals(&[]);
        let d = deck(vec!["Atraxa, Praetors' Voice"], vec!["Forest", "Island"]);
        let e = estimate_bracket(&d, &db).unwrap();
        assert_eq!(e.tier, CommanderBracketTier::Exhibition);
        assert!(e
            .axes
            .values()
            .all(|reading| reading.count == 0 && reading.contributing.is_empty()));
        assert!(e.violations.is_empty());
    }

    #[test]
    fn one_or_two_tutors_only_is_b2_core() {
        let db = db_with_signals(&[
            (
                "Demonic Tutor",
                BracketSignals {
                    efficient_tutor: true,
                    ..Default::default()
                },
            ),
            (
                "Vampiric Tutor",
                BracketSignals {
                    efficient_tutor: true,
                    ..Default::default()
                },
            ),
        ]);
        let d = deck(
            vec!["Atraxa, Praetors' Voice"],
            vec!["Demonic Tutor", "Vampiric Tutor", "Forest"],
        );
        let e = estimate_bracket(&d, &db).unwrap();
        assert_eq!(e.tier, CommanderBracketTier::Core);
        assert_eq!(e.axes[&BracketAxis::EfficientTutors].count, 2);
    }

    #[test]
    fn three_tutors_only_is_b3_upgraded() {
        let db = db_with_signals(&[
            (
                "Demonic Tutor",
                BracketSignals {
                    efficient_tutor: true,
                    ..Default::default()
                },
            ),
            (
                "Vampiric Tutor",
                BracketSignals {
                    efficient_tutor: true,
                    ..Default::default()
                },
            ),
            (
                "Mystical Tutor",
                BracketSignals {
                    efficient_tutor: true,
                    ..Default::default()
                },
            ),
        ]);
        let d = deck(
            vec!["Atraxa, Praetors' Voice"],
            vec!["Demonic Tutor", "Vampiric Tutor", "Mystical Tutor"],
        );
        let e = estimate_bracket(&d, &db).unwrap();
        assert_eq!(e.tier, CommanderBracketTier::Upgraded);
        assert_eq!(e.axes[&BracketAxis::EfficientTutors].count, 3);
    }

    #[test]
    fn one_game_changer_forces_b3() {
        let db = db_with_signals(&[(
            "Smothering Tithe",
            BracketSignals {
                game_changer: true,
                ..Default::default()
            },
        )]);
        let d = deck(vec!["Atraxa, Praetors' Voice"], vec!["Smothering Tithe"]);
        let e = estimate_bracket(&d, &db).unwrap();
        assert_eq!(e.tier, CommanderBracketTier::Upgraded);
        assert_eq!(e.axes[&BracketAxis::GameChangers].count, 1);
        assert!(
            e.violations.contains_key(&BracketAxis::GameChangers),
            "GameChangers violation must be present"
        );
    }

    #[test]
    fn four_game_changers_forces_b4() {
        let sig = BracketSignals {
            game_changer: true,
            ..Default::default()
        };
        let db = db_with_signals(&[("A", sig), ("B", sig), ("C", sig), ("D", sig)]);
        let d = deck(vec!["Cmdr"], vec!["A", "B", "C", "D"]);
        let e = estimate_bracket(&d, &db).unwrap();
        assert_eq!(e.tier, CommanderBracketTier::Optimized);
        assert_eq!(e.axes[&BracketAxis::GameChangers].count, 4);
    }

    #[test]
    fn any_mass_land_denial_forces_b4() {
        let db = db_with_signals(&[(
            "Armageddon",
            BracketSignals {
                mass_land_denial: true,
                ..Default::default()
            },
        )]);
        let d = deck(vec!["Cmdr"], vec!["Armageddon"]);
        let e = estimate_bracket(&d, &db).unwrap();
        assert_eq!(e.tier, CommanderBracketTier::Optimized);
    }

    #[test]
    fn any_extra_turn_forces_b3() {
        let db = db_with_signals(&[(
            "Time Warp",
            BracketSignals {
                extra_turn: true,
                ..Default::default()
            },
        )]);
        let d = deck(vec!["Cmdr"], vec!["Time Warp"]);
        let e = estimate_bracket(&d, &db).unwrap();
        assert_eq!(e.tier, CommanderBracketTier::Upgraded);
    }

    #[test]
    fn overlapping_lists_register_on_every_matching_axis() {
        let db = db_with_signals(&[(
            "Demonic Tutor",
            BracketSignals {
                game_changer: true,
                efficient_tutor: true,
                ..Default::default()
            },
        )]);
        let d = deck(vec!["Cmdr"], vec!["Demonic Tutor"]);
        let e = estimate_bracket(&d, &db).unwrap();
        assert_eq!(e.axes[&BracketAxis::GameChangers].count, 1);
        assert_eq!(e.axes[&BracketAxis::EfficientTutors].count, 1);
        assert_eq!(e.tier, CommanderBracketTier::Upgraded);
        assert_eq!(
            e.axes[&BracketAxis::GameChangers].contributing,
            vec!["Demonic Tutor"]
        );
        assert_eq!(
            e.axes[&BracketAxis::EfficientTutors].contributing,
            vec!["Demonic Tutor"]
        );
    }

    #[test]
    fn estimator_never_returns_cedh() {
        let sig = BracketSignals {
            game_changer: true,
            mass_land_denial: true,
            extra_turn: true,
            efficient_tutor: true,
        };
        let entries: Vec<(String, BracketSignals)> =
            (0..40).map(|i| (format!("Card{i}"), sig)).collect();
        let entry_refs: Vec<(&str, BracketSignals)> = entries
            .iter()
            .map(|(name, signals)| (name.as_str(), *signals))
            .collect();
        let db = db_with_signals(&entry_refs);
        let main: Vec<&str> = entries.iter().map(|(n, _)| n.as_str()).collect();
        let d = deck(vec!["Cmdr"], main);
        let e = estimate_bracket(&d, &db).unwrap();
        assert_eq!(
            e.tier,
            CommanderBracketTier::Optimized,
            "estimator caps at B4"
        );
    }

    #[test]
    fn contributing_cards_listed_per_axis() {
        let db = db_with_signals(&[
            (
                "Smothering Tithe",
                BracketSignals {
                    game_changer: true,
                    ..Default::default()
                },
            ),
            (
                "Cyclonic Rift",
                BracketSignals {
                    game_changer: true,
                    ..Default::default()
                },
            ),
            (
                "Demonic Tutor",
                BracketSignals {
                    efficient_tutor: true,
                    ..Default::default()
                },
            ),
        ]);
        let d = deck(
            vec!["Cmdr"],
            vec!["Smothering Tithe", "Cyclonic Rift", "Demonic Tutor"],
        );
        let e = estimate_bracket(&d, &db).unwrap();
        assert_eq!(
            e.axes[&BracketAxis::GameChangers].contributing,
            vec!["Smothering Tithe", "Cyclonic Rift"]
        );
        assert_eq!(
            e.axes[&BracketAxis::EfficientTutors].contributing,
            vec!["Demonic Tutor"]
        );
    }

    #[test]
    fn determinism_same_inputs_same_estimate() {
        let db = db_with_signals(&[(
            "Demonic Tutor",
            BracketSignals {
                efficient_tutor: true,
                ..Default::default()
            },
        )]);
        let d = deck(vec!["Cmdr"], vec!["Demonic Tutor"]);
        let a = estimate_bracket(&d, &db).unwrap();
        let b = estimate_bracket(&d, &db).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn data_version_is_passed_through() {
        let db = db_with_signals(&[]);
        let d = deck(vec!["Cmdr"], vec!["Forest"]);
        let e = estimate_bracket(&d, &db).unwrap();
        assert_eq!(e.data_version, "test-1");
    }

    #[test]
    fn signal_on_commander_card_is_counted() {
        let db = db_with_signals(&[(
            "Sol Ring",
            BracketSignals {
                game_changer: true,
                ..Default::default()
            },
        )]);
        let d = deck(vec!["Sol Ring"], vec!["Forest", "Forest"]);
        let e = estimate_bracket(&d, &db).unwrap();
        assert_eq!(e.axes[&BracketAxis::GameChangers].count, 1);
        assert_eq!(e.tier, CommanderBracketTier::Upgraded);
    }

    #[test]
    fn one_tutor_is_b2_core() {
        let db = db_with_signals(&[(
            "Demonic Tutor",
            BracketSignals {
                efficient_tutor: true,
                ..Default::default()
            },
        )]);
        let d = deck(vec!["Cmdr"], vec!["Demonic Tutor"]);
        let e = estimate_bracket(&d, &db).unwrap();
        assert_eq!(e.tier, CommanderBracketTier::Core);
        assert_eq!(e.axes[&BracketAxis::EfficientTutors].count, 1);
    }

    #[test]
    fn highest_axis_wins_and_violations_recorded_per_axis() {
        let db = db_with_signals(&[
            (
                "Smothering Tithe",
                BracketSignals {
                    game_changer: true,
                    ..Default::default()
                },
            ),
            (
                "Armageddon",
                BracketSignals {
                    mass_land_denial: true,
                    ..Default::default()
                },
            ),
        ]);
        let d = deck(vec!["Cmdr"], vec!["Smothering Tithe", "Armageddon"]);
        let e = estimate_bracket(&d, &db).unwrap();
        assert_eq!(e.tier, CommanderBracketTier::Optimized, "MLD pushes to B4");
        assert_eq!(e.violations.len(), 2, "one violation per crossed axis");
        assert_eq!(
            e.violations[&BracketAxis::MassLandDenial].forced_floor,
            CommanderBracketTier::Optimized
        );
        assert_eq!(
            e.violations[&BracketAxis::GameChangers].forced_floor,
            CommanderBracketTier::Upgraded
        );
    }

    #[test]
    fn caps_are_defined_for_every_axis_at_every_tier() {
        use strum::IntoEnumIterator;

        let expected = [
            (
                CommanderBracketTier::Exhibition,
                [Some(0), Some(0), Some(0), Some(0)],
            ),
            (
                CommanderBracketTier::Core,
                [Some(0), Some(0), Some(0), Some(2)],
            ),
            (
                CommanderBracketTier::Upgraded,
                [Some(3), Some(0), None, None],
            ),
            (CommanderBracketTier::Optimized, [None, None, None, None]),
        ];
        for (tier, caps) in expected {
            for (axis, expected_cap) in BracketAxis::iter().zip(caps) {
                assert_eq!(cap_for(axis, tier), expected_cap, "{tier:?} {axis:?}");
            }
        }
    }

    #[test]
    fn every_axis_is_present_even_at_zero() {
        use strum::IntoEnumIterator;

        let estimate =
            estimate_bracket(&deck(vec!["Cmdr"], vec!["Forest"]), &db_with_signals(&[])).unwrap();
        assert_eq!(estimate.axes.len(), BracketAxis::iter().count());
        for axis in BracketAxis::iter() {
            let reading = &estimate.axes[&axis];
            assert_eq!(reading.count, 0);
            assert!(reading.contributing.is_empty());
        }
    }

    #[test]
    fn estimate_serializes_as_an_axis_keyed_reading_map() {
        let db = db_with_signals(&[(
            "Smothering Tithe",
            BracketSignals {
                game_changer: true,
                ..Default::default()
            },
        )]);
        let estimate =
            estimate_bracket(&deck(vec!["Cmdr"], vec!["Smothering Tithe"]), &db).unwrap();
        let value = serde_json::to_value(estimate).unwrap();

        assert_eq!(value["axes"]["game_changers"]["count"], 1);
        assert_eq!(
            value["axes"]["game_changers"]["contributing"][0],
            "Smothering Tithe"
        );
        assert!(value["axes"]["extra_turns"]["cap_at_tier"].is_null());
        assert!(value.get("axis_caps_at_tier").is_none());
        assert!(value.get("contributing").is_none());
    }
}
