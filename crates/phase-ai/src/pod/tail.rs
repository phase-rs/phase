//! Integer-first lower-tail statistics for pod measurements.

use super::{PodMeasurement, PodObservation};

pub const PUBLISHED_FLOORS: &[u32] = &[9, 8, 6, 4];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailSummary {
    pub q05: Option<u32>,
    pub tail_counts: Vec<FloorTail>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FloorTail {
    pub floor: u32,
    pub decided_at_or_before: usize,
    pub informative: usize,
    pub uninformative: usize,
}

pub fn summarize(measurements: &[PodMeasurement], floors: &[u32]) -> TailSummary {
    let observations: Vec<PodObservation> = measurements
        .iter()
        .filter_map(|measurement| match measurement {
            PodMeasurement::Measured { observation, .. } => Some(*observation),
            PodMeasurement::Withheld { .. } => None,
        })
        .collect();
    let tail_counts = floors
        .iter()
        .map(|floor| count_at(&observations, *floor))
        .collect();
    TailSummary {
        q05: q05(&observations),
        tail_counts,
    }
}

pub fn summarize_published(measurements: &[PodMeasurement]) -> TailSummary {
    summarize(measurements, PUBLISHED_FLOORS)
}

pub fn count_at(observations: &[PodObservation], floor: u32) -> FloorTail {
    let mut result = FloorTail {
        floor,
        decided_at_or_before: 0,
        informative: 0,
        uninformative: 0,
    };
    for observation in observations {
        match observation {
            PodObservation::Decided { turn, .. } => {
                result.informative += 1;
                if *turn <= floor {
                    result.decided_at_or_before += 1;
                }
            }
            PodObservation::Censored { after_turn, .. } if *after_turn > floor => {
                result.informative += 1;
            }
            PodObservation::Censored { .. } => result.uninformative += 1,
        }
    }
    result
}

fn q05(observations: &[PodObservation]) -> Option<u32> {
    let mut turns: Vec<u32> = observations
        .iter()
        .filter_map(|observation| match observation {
            PodObservation::Decided { turn, .. } => Some(*turn),
            PodObservation::Censored { .. } => None,
        })
        .collect();
    turns.sort_unstable();
    turns.dedup();
    turns.into_iter().find(|turn| {
        let count = count_at(observations, *turn);
        count.uninformative == 0
            && count.informative > 0
            && count.decided_at_or_before.saturating_mul(100)
                >= count.informative.saturating_mul(5)
    })
}

/// Renders exact counts only when at most two percent are uninformative.
pub fn render_tail_count(count: FloorTail) -> Option<String> {
    let total = count.informative.saturating_add(count.uninformative);
    if total > 0 && count.uninformative.saturating_mul(100) > total.saturating_mul(2) {
        None
    } else {
        Some(format!(
            "{}/{} (uninformative={})",
            count.decided_at_or_before, count.informative, count.uninformative
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::pod::{PodEvidence, SeatCoverage, StopReason};

    fn measured(observation: PodObservation) -> PodMeasurement {
        PodMeasurement::Measured {
            observation,
            evidence: PodEvidence {
                seat_coverage: [SeatCoverage::default(); 4],
                touched_unimplemented: BTreeSet::new(),
            },
        }
    }

    #[test]
    fn censored_below_a_floor_is_uninformative_not_a_success() {
        let summary = summarize(
            &[
                measured(PodObservation::Censored {
                    after_turn: 3,
                    reason: StopReason::ActionCap,
                }),
                measured(PodObservation::Censored {
                    after_turn: 30,
                    reason: StopReason::ActionCap,
                }),
            ],
            &[4],
        );
        assert_eq!(
            summary.tail_counts[0],
            FloorTail {
                floor: 4,
                decided_at_or_before: 0,
                informative: 1,
                uninformative: 1,
            }
        );
    }

    #[test]
    fn tail_renderer_refuses_above_the_uninformative_ceiling() {
        let observations: Vec<_> = (0..100)
            .map(|index| {
                measured(PodObservation::Censored {
                    after_turn: if index < 3 { 3 } else { 30 },
                    reason: StopReason::ActionCap,
                })
            })
            .collect();
        let count = summarize(&observations, &[4]).tail_counts[0];
        assert_eq!(count.uninformative, 3);
        assert_eq!(render_tail_count(count), None);
    }
}
