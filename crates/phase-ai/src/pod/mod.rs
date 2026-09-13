//! Deterministic measurement types for four-player Commander AI pods.

use std::collections::BTreeSet;

use engine::game::UnsupportedCard;
use engine::types::player::PlayerId;
use serde::{Deserialize, Serialize};

use crate::auto_play::{AiActionsStop, DriverExit};

pub mod feed;
pub mod report;
pub mod tail;

/// Why a driver abandoned a game before `WaitingFor::GameOver`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    ActionCap,
    WallTimeout,
    StalledSameTurn,
    NoLegalActions,
    AiChoseNoAction,
    ActionRejected,
    MissingAiConfig,
    ActionSafetyCap,
}

impl StopReason {
    /// Stable label used by existing Commander reports.
    pub fn label(self) -> &'static str {
        match self {
            Self::ActionCap => "action_cap",
            Self::WallTimeout => "wall_timeout",
            Self::StalledSameTurn => "stalled_same_turn",
            Self::NoLegalActions => "no_legal_actions",
            Self::AiChoseNoAction => "ai_chose_no_action",
            Self::ActionRejected => "action_rejected",
            Self::MissingAiConfig => "missing_ai_config",
            Self::ActionSafetyCap => "action_safety_cap",
        }
    }
}

/// One run's contribution to a decision-turn distribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PodObservation {
    Decided { turn: u32, winner: Option<PlayerId> },
    Censored { after_turn: u32, reason: StopReason },
}

/// Maps a driver exit into a right-censored observation.
pub fn censored_observation(after_turn: u32, exit: &DriverExit) -> PodObservation {
    let reason = match exit {
        DriverExit::CapReached => StopReason::ActionCap,
        DriverExit::BatchBreak(reason) => match reason {
            AiActionsStop::NoEligibleAiActor | AiActionsStop::ActionBudgetReached { .. } => {
                StopReason::NoLegalActions
            }
            AiActionsStop::MissingAiConfig { .. } => StopReason::MissingAiConfig,
            AiActionsStop::ChooseActionNone { .. } => StopReason::AiChoseNoAction,
            AiActionsStop::ApplyFailed { .. } => StopReason::ActionRejected,
            AiActionsStop::ActionSafetyCapReached { .. } => StopReason::ActionSafetyCap,
        },
    };
    PodObservation::Censored { after_turn, reason }
}

/// A simulated result is either reportable or withheld in full.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum PodMeasurement {
    Measured {
        observation: PodObservation,
        evidence: PodEvidence,
    },
    Withheld {
        shortfall: FidelityShortfall,
    },
}

/// The stage at which measurement fidelity failed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum FidelityShortfall {
    PreScreen {
        seat: PlayerId,
        unknown: Vec<String>,
        unsupported: Vec<UnsupportedCard>,
    },
    Runtime {
        oracle_ids: BTreeSet<String>,
    },
}

/// Integer-only coverage evidence for a measured run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PodEvidence {
    pub seat_coverage: [SeatCoverage; 4],
    pub touched_unimplemented: BTreeSet<String>,
}

/// Integer-only coverage summary for one seat.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatCoverage {
    pub total_unique: usize,
    pub supported_unique: usize,
    pub total_copies: usize,
    pub supported_copies: usize,
    pub unknown_names: usize,
}

/// Applies the unconditional runtime fidelity gate.
pub fn finish_measurement(
    observation: PodObservation,
    mut evidence: PodEvidence,
    touched_unimplemented: BTreeSet<String>,
) -> PodMeasurement {
    if touched_unimplemented.is_empty() {
        evidence.touched_unimplemented = touched_unimplemented;
        PodMeasurement::Measured {
            observation,
            evidence,
        }
    } else {
        PodMeasurement::Withheld {
            shortfall: FidelityShortfall::Runtime {
                oracle_ids: touched_unimplemented,
            },
        }
    }
}

/// Constructs an unconditional pre-screen refusal.
pub fn withhold_pre_screen(
    seat: PlayerId,
    unknown: Vec<String>,
    unsupported: Vec<UnsupportedCard>,
) -> PodMeasurement {
    PodMeasurement::Withheld {
        shortfall: FidelityShortfall::PreScreen {
            seat,
            unknown,
            unsupported,
        },
    }
}

/// Applies the pre-screen fidelity policy using integer copy counts.
pub fn pre_screen_measurement(
    seat: PlayerId,
    unknown: Vec<String>,
    unsupported: Vec<UnsupportedCard>,
    supported_copies: usize,
    total_copies: usize,
    coverage_floor: Option<u8>,
) -> Option<PodMeasurement> {
    let below_floor = coverage_floor.is_some_and(|floor| {
        total_copies > 0
            && supported_copies.saturating_mul(100)
                < total_copies.saturating_mul(usize::from(floor))
    });
    (!unknown.is_empty() || below_floor).then(|| withhold_pre_screen(seat, unknown, unsupported))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_evidence() -> PodEvidence {
        PodEvidence {
            seat_coverage: [SeatCoverage::default(); 4],
            touched_unimplemented: BTreeSet::new(),
        }
    }

    #[test]
    fn censored_run_is_never_reported_as_a_decided_turn() {
        let observation = censored_observation(41, &DriverExit::CapReached);
        let measurement = finish_measurement(observation, empty_evidence(), BTreeSet::new());
        assert!(matches!(
            &measurement,
            PodMeasurement::Measured {
                observation: PodObservation::Censored { after_turn: 41, .. },
                ..
            }
        ));
        assert_eq!(tail::summarize(&[measurement], &[41]).q05, None);
    }

    #[test]
    fn runtime_touch_always_withholds_regardless_of_coverage_floor() {
        let mut ids = BTreeSet::new();
        ids.insert("oracle-id".to_string());
        let measurement = finish_measurement(
            PodObservation::Decided {
                turn: 9,
                winner: Some(PlayerId(0)),
            },
            empty_evidence(),
            ids,
        );
        assert!(matches!(
            &measurement,
            PodMeasurement::Withheld {
                shortfall: FidelityShortfall::Runtime { .. }
            }
        ));
        assert_eq!(
            tail::summarize(&[measurement], &[9]).tail_counts[0].informative,
            0
        );
    }

    #[test]
    fn unresolvable_name_withholds_even_at_zero_floor() {
        let measurement = pre_screen_measurement(
            PlayerId(2),
            vec!["Missing Card".to_string()],
            Vec::new(),
            99,
            100,
            Some(0),
        )
        .expect("unknown name always withholds");
        assert!(matches!(
            &measurement,
            PodMeasurement::Withheld {
                shortfall: FidelityShortfall::PreScreen {
                    seat: PlayerId(2),
                    ..
                }
            }
        ));
        assert_eq!(
            tail::summarize(&[measurement], &[4]).tail_counts[0].informative,
            0
        );
    }
}
