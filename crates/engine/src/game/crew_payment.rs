//! CR 702.122a: Prepared tap-payment authority for Crew / Saddle / Station.
//!
//! Phase 1 owns this API; Phase 2A wires the tap/payment events onto it. It is the
//! "single authority for ability costs" rule applied to tap payment: the payer set
//! is validated and each payer's power contribution is captured atomically against
//! the immutable **pre-mutation** `GameState`, so threshold validation and commit
//! never re-read live power, toughness, or statics after preparation.

use crate::game::engine::EngineError;
use crate::game::static_abilities::object_crew_power_contribution;
use crate::types::game_state::GameState;
use crate::types::identifiers::{ObjectId, ObjectIncarnationRef};
use crate::types::statics::CrewAction;

/// CR 702.122a: a prepared, pre-validated Crew/Saddle/Station tap payment. Holds
/// the payer set keyed by `ObjectIncarnationRef` so a payer that changes zones
/// between preparation and commit cannot be confused with a new object at the same
/// storage id (CR 400.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedTapPayment {
    /// The permanent being crewed/saddled/stationed, at its current incarnation.
    pub vehicle: ObjectIncarnationRef,
    /// Which action is being paid for (Crew / Saddle / Station).
    pub action: CrewAction,
    /// Minimum total power required (Crew N / Saddle N / Station N).
    pub required_power: i32,
    /// The prepared payers (creatures to tap), each keyed by its incarnation.
    pub payers: Vec<ObjectIncarnationRef>,
}

impl PreparedTapPayment {
    /// Validate that the vehicle and every payer creature exist and resolve each to
    /// its current `ObjectIncarnationRef`. Performs **no** mutation (no tap, no
    /// events) — payment side effects are Phase 2A's responsibility.
    pub fn prepare(
        state: &GameState,
        vehicle_id: ObjectId,
        action: CrewAction,
        required_power: i32,
        payer_ids: &[ObjectId],
    ) -> Result<Self, EngineError> {
        let vehicle = state
            .objects
            .get(&vehicle_id)
            .map(ObjectIncarnationRef::from_object)
            .ok_or_else(|| EngineError::InvalidAction("Crew vehicle not found".to_string()))?;
        let mut payers = Vec::with_capacity(payer_ids.len());
        for &pid in payer_ids {
            let payer = state
                .objects
                .get(&pid)
                .map(ObjectIncarnationRef::from_object)
                .ok_or_else(|| EngineError::InvalidAction("Crew payer not found".to_string()))?;
            payers.push(payer);
        }
        Ok(Self {
            vehicle,
            action,
            required_power,
            payers,
        })
    }

    /// CR 608.2h: capture each payer's crew power contribution against the immutable
    /// **pre-mutation** `GameState`. Delegates entirely to
    /// `object_crew_power_contribution` — toughness substitution, accumulated power
    /// deltas, granted contribution statics, Crew-vs-Saddle action filtering, and
    /// active static/layer gates all live there; Phase 1 re-implements none of them.
    /// Each captured value is keyed by `ObjectIncarnationRef`.
    pub fn prepare_contribution_snapshot(&self, state: &GameState) -> PreparedContributionSnapshot {
        let contributions = self
            .payers
            .iter()
            .map(|&payer| {
                (
                    payer,
                    object_crew_power_contribution(state, payer.object_id, self.action),
                )
            })
            .collect();
        PreparedContributionSnapshot {
            contributions,
            required_power: self.required_power,
        }
    }
}

/// CR 702.122a: the captured per-payer contribution snapshot. Every threshold query
/// reads **only** these stored values — never `state.objects`, live power,
/// toughness, or statics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedContributionSnapshot {
    contributions: Vec<(ObjectIncarnationRef, i32)>,
    required_power: i32,
}

impl PreparedContributionSnapshot {
    /// Total captured power across all payers (pre-mutation values).
    pub fn total_contribution(&self) -> i32 {
        self.contributions.iter().map(|(_, p)| *p).sum()
    }

    /// CR 702.122a: whether the captured total meets the required power. Reads only
    /// stored values.
    pub fn meets_threshold(&self) -> bool {
        self.total_contribution() >= self.required_power
    }

    /// Consume the snapshot, yielding the prepared payer set. Infallible: no
    /// rediscovery, no revalidation, no live-state read.
    pub fn commit(self) -> Vec<ObjectIncarnationRef> {
        self.contributions
            .into_iter()
            .map(|(payer, _)| payer)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::scenario::GameScenario;
    use crate::types::player::PlayerId;

    const P0: PlayerId = PlayerId(0);

    // T-prepare: the contribution snapshot is captured against the pre-mutation
    // state; a later live-state change must NOT alter the stored value, and commit
    // re-reads nothing. Reverting `prepare_contribution_snapshot` to read live
    // power at threshold/commit time would flip the post-mutation assertion.
    #[test]
    fn contribution_snapshot_reads_pre_mutation_and_commit_reads_nothing() {
        let mut scenario = GameScenario::new();
        let vehicle = scenario.add_vanilla(P0, 0, 4);
        let payer = scenario.add_vanilla(P0, 3, 3);
        let mut runner = scenario.build();

        let prepared =
            PreparedTapPayment::prepare(runner.state(), vehicle, CrewAction::Crew, 2, &[payer])
                .expect("vehicle and payer exist");
        let snapshot = prepared.prepare_contribution_snapshot(runner.state());
        assert_eq!(
            snapshot.total_contribution(),
            3,
            "captures pre-mutation power"
        );
        assert!(snapshot.meets_threshold(), "3 power meets Crew 2");

        // Mutate live power AFTER the snapshot. A snapshot that (incorrectly)
        // re-read live state would now report 8; the stored value must stay 3.
        runner.state_mut().objects.get_mut(&payer).unwrap().power = Some(8);
        assert_eq!(
            snapshot.total_contribution(),
            3,
            "snapshot is immutable to post-preparation live-state changes"
        );

        let committed = snapshot.commit();
        assert_eq!(committed.len(), 1);
        assert_eq!(committed[0].object_id, payer);
    }

    // Sibling: a missing payer fails preparation before any capture.
    #[test]
    fn prepare_rejects_unknown_payer() {
        let mut scenario = GameScenario::new();
        let vehicle = scenario.add_vanilla(P0, 0, 4);
        let runner = scenario.build();
        let missing = crate::types::identifiers::ObjectId(999_999);
        assert!(PreparedTapPayment::prepare(
            runner.state(),
            vehicle,
            CrewAction::Crew,
            1,
            &[missing]
        )
        .is_err());
    }
}
