//! Commander bracket estimator. Profiles a Commander deck along four axes
//! (Game Changers, Mass Land Denial, Extra Turns, Efficient Tutors) and
//! returns a `BracketEstimate` placing the deck in bracket B1–B4.
//!
//! Pure: no game state, no I/O, no randomness. Same `(deck, db)` →
//! identical `BracketEstimate`.
//!
//! Bracket policy is **not** part of the Comprehensive Rules — it is WotC's
//! Commander Format Panel guidance. No `// CR` annotations apply.

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

impl CommanderBracketTier {
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BracketAxisCounts {
    pub game_changers: u8,
    pub mass_land_denial: u8,
    pub extra_turns: u8,
    pub efficient_tutors: u8,
}

impl BracketAxisCounts {
    /// Read the count for a given axis, by enum variant.
    pub fn count_for(&self, axis: BracketAxis) -> u8 {
        match axis {
            BracketAxis::GameChangers => self.game_changers,
            BracketAxis::MassLandDenial => self.mass_land_denial,
            BracketAxis::ExtraTurns => self.extra_turns,
            BracketAxis::EfficientTutors => self.efficient_tutors,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BracketContributingCards {
    pub game_changers: Vec<String>,
    pub mass_land_denial: Vec<String>,
    pub extra_turns: Vec<String>,
    pub efficient_tutors: Vec<String>,
}

/// One axis that forced the deck above a tier ceiling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BracketViolation {
    pub axis: BracketAxis,
    pub count: u8,
    pub prior_cap: u8,
    pub forced_floor: CommanderBracketTier,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BracketAxis {
    GameChangers,
    MassLandDenial,
    ExtraTurns,
    EfficientTutors,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BracketEstimate {
    pub tier: CommanderBracketTier,
    pub axes: BracketAxisCounts,
    pub contributing: BracketContributingCards,
    pub violations: Vec<BracketViolation>,
    /// `BracketLists.version`, passed through from the export pipeline.
    pub data_version: String,
}

/// Returns `None` when the deck has no commander.
pub fn estimate_bracket(deck: &PlayerDeckList, db: &CardDatabase) -> Option<BracketEstimate> {
    if deck.commander.is_empty() {
        return None;
    }

    let mut axes = BracketAxisCounts::default();
    let mut contributing = BracketContributingCards::default();

    let all_cards = deck.commander.iter().chain(deck.main_deck.iter());
    for name in all_cards {
        let sig = db.bracket_signals_for(name);
        if sig.game_changer {
            axes.game_changers = axes.game_changers.saturating_add(1);
            contributing.game_changers.push(name.clone());
        }
        if sig.mass_land_denial {
            axes.mass_land_denial = axes.mass_land_denial.saturating_add(1);
            contributing.mass_land_denial.push(name.clone());
        }
        if sig.extra_turn {
            axes.extra_turns = axes.extra_turns.saturating_add(1);
            contributing.extra_turns.push(name.clone());
        }
        if sig.efficient_tutor {
            axes.efficient_tutors = axes.efficient_tutors.saturating_add(1);
            contributing.efficient_tutors.push(name.clone());
        }
    }

    let (tier, violations) = decide_tier(&axes);

    Some(BracketEstimate {
        tier,
        axes,
        contributing,
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
/// floor across axes. Violations are sorted by descending `forced_floor`
/// so the UI can render "Why B3?" rows in priority order.
fn decide_tier(axes: &BracketAxisCounts) -> (CommanderBracketTier, Vec<BracketViolation>) {
    let mut floor_index: usize = 0;
    let mut violations = Vec::new();

    for (axis, caps) in CAPS {
        let count = axes.count_for(*axis);
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
            violations.push(BracketViolation {
                axis: *axis,
                count,
                prior_cap: cap,
                forced_floor,
            });
        }
    }

    violations.sort_by_key(|v| std::cmp::Reverse(v.forced_floor.as_u8()));
    (TIERS[floor_index], violations)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::bracket_lists::BracketLists;
    use crate::database::{BracketSignals, CardDatabase};
    use crate::game::deck_loading::PlayerDeckList;

    fn db_with_signals(entries: &[(&str, BracketSignals)]) -> CardDatabase {
        CardDatabase::default().with_bracket_lists(BracketLists {
            version: "test-1".to_string(),
            source: String::new(),
            game_changers: entries
                .iter()
                .filter(|(_, s)| s.game_changer)
                .map(|(n, _)| n.to_lowercase())
                .collect(),
            mass_land_denial: entries
                .iter()
                .filter(|(_, s)| s.mass_land_denial)
                .map(|(n, _)| n.to_lowercase())
                .collect(),
            extra_turns: entries
                .iter()
                .filter(|(_, s)| s.extra_turn)
                .map(|(n, _)| n.to_lowercase())
                .collect(),
            efficient_tutors: entries
                .iter()
                .filter(|(_, s)| s.efficient_tutor)
                .map(|(n, _)| n.to_lowercase())
                .collect(),
        })
    }

    fn deck(commander: Vec<&str>, main: Vec<&str>) -> PlayerDeckList {
        PlayerDeckList {
            commander: commander.into_iter().map(String::from).collect(),
            main_deck: main.into_iter().map(String::from).collect(),
            sideboard: Vec::new(),
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
        assert_eq!(e.axes, BracketAxisCounts::default());
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
        assert_eq!(e.axes.efficient_tutors, 2);
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
        assert_eq!(e.axes.efficient_tutors, 3);
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
        assert_eq!(e.axes.game_changers, 1);
        assert!(e
            .violations
            .iter()
            .any(|v| v.axis == BracketAxis::GameChangers));
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
        assert_eq!(e.axes.game_changers, 4);
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
        assert_eq!(e.axes.game_changers, 1);
        assert_eq!(e.axes.efficient_tutors, 1);
        assert_eq!(e.tier, CommanderBracketTier::Upgraded);
        assert_eq!(e.contributing.game_changers, vec!["Demonic Tutor"]);
        assert_eq!(e.contributing.efficient_tutors, vec!["Demonic Tutor"]);
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
        let db = CardDatabase::default().with_bracket_lists(BracketLists {
            version: "t".into(),
            source: String::new(),
            game_changers: entries.iter().map(|(n, _)| n.to_lowercase()).collect(),
            mass_land_denial: entries.iter().map(|(n, _)| n.to_lowercase()).collect(),
            extra_turns: entries.iter().map(|(n, _)| n.to_lowercase()).collect(),
            efficient_tutors: entries.iter().map(|(n, _)| n.to_lowercase()).collect(),
        });
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
            e.contributing.game_changers,
            vec!["Smothering Tithe", "Cyclonic Rift"]
        );
        assert_eq!(e.contributing.efficient_tutors, vec!["Demonic Tutor"]);
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
        assert_eq!(e.axes.game_changers, 1);
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
        assert_eq!(e.axes.efficient_tutors, 1);
    }

    #[test]
    fn highest_axis_wins_and_violation_is_first_in_sort() {
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
        assert_eq!(e.violations.len(), 2, "one violation per axis");
        assert_eq!(
            e.violations[0].axis,
            BracketAxis::MassLandDenial,
            "B4 violation sorts first"
        );
        assert_eq!(
            e.violations[0].forced_floor,
            CommanderBracketTier::Optimized
        );
        assert_eq!(e.violations[1].axis, BracketAxis::GameChangers);
        assert_eq!(e.violations[1].forced_floor, CommanderBracketTier::Upgraded);
    }
}
