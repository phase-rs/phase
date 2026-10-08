//! CR 603.3b + CR 704.5j: an SBA-owned player choice parks its own trigger
//! batch, which must join the answer's ordering window. An ordinary deferred
//! context queued beside it — a construction, terminal, or cost-trigger tail —
//! belongs to its own window and must NOT be absorbed into the answer's.
//!
//! These are crate-internal rather than integration tests for one reason:
//! `PendingTriggerContext::batch_origin` is `pub(crate)`, so only from inside
//! the crate can a test both seed a genuinely ordinary context and assert which
//! origin each queued context carries. The scenario itself is still driven
//! through the production `apply()` pipeline.

use super::*;
use crate::game::scenario::{GameScenario, P0};
use crate::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter};
use crate::types::actions::GameAction;
use crate::types::game_state::{StackEntryKind, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::phase::Phase;
use crate::types::player::PlayerId;

const DIES_GAIN_LIFE: &str = "When this creature dies, you gain 3 life.";

/// Source id for the seeded ordinary context, deliberately outside the range
/// the scenario allocates so stack entries are unambiguously attributable.
const ORDINARY_SOURCE: ObjectId = ObjectId(9001);

/// A distinguishable ordinary deferred context, standing in for the
/// construction / terminal / cost-trigger tails that legitimately sit in
/// `deferred_triggers` while an SBA-owned choice is open.
fn ordinary_context() -> PendingTriggerContext {
    PendingTriggerContext::single(PendingTrigger {
        source_id: ORDINARY_SOURCE,
        controller: PlayerId(0),
        condition: None,
        ability: Box::new(ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            Vec::new(),
            ORDINARY_SOURCE,
            PlayerId(0),
        )),
        timestamp: 0,
        target_constraints: Vec::new(),
        distribute: None,
        trigger_event: None,
        modal: None,
        mode_abilities: Vec::new(),
        description: None,
        may_trigger_origin: None,
        subject_match_count: None,
        die_result: None,
        provenance: None,
    })
}

/// Drive the production pipeline to a `ChooseLegend` prompt whose deferred queue
/// holds BOTH a seeded ordinary context and an SBA-parked one.
///
/// The legend-rule violation and a 1/0 creature are both present when the first
/// state-based-action pass runs (CR 704.3 checks all SBAs simultaneously), so
/// that single pass puts the creature into the graveyard — producing a real
/// dies-trigger event to park — and then opens the legend choice, which parks it.
fn runner_at_mixed_sba_choice() -> (crate::game::scenario::GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Library Card"]);

    let keep = scenario
        .add_creature(P0, "Moonlit Sentinel", 2, 2)
        .as_legendary()
        .id();
    scenario
        .add_creature(P0, "Moonlit Sentinel", 2, 2)
        .as_legendary()
        .id();
    let doomed = scenario
        .add_creature_from_oracle(P0, "Doomed Herald", 1, 0, DIES_GAIN_LIFE)
        .id();

    let mut runner = scenario.build();
    runner
        .state_mut()
        .deferred_triggers
        .push(ordinary_context());
    let _ = runner.act(GameAction::PassPriority);

    // Reach-guard: the whole point of these tests is the MIXED queue. If the
    // fixture ever stops producing one, every assertion below would pass
    // vacuously, so prove both origins are present before going further.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ChooseLegend { .. }),
        "fixture must park on the legend-rule SBA choice, got {:?}",
        runner.state().waiting_for
    );
    let ordinary = runner
        .state()
        .deferred_triggers
        .iter()
        .filter(|c| {
            matches!(c.batch_origin, DeferredTriggerBatchOrigin::Ordinary)
                && c.pending.source_id == ORDINARY_SOURCE
        })
        .count();
    let sba = runner
        .state()
        .deferred_triggers
        .iter()
        .filter(|c| {
            matches!(
                c.batch_origin,
                DeferredTriggerBatchOrigin::StateBasedActionChoice
            )
        })
        .count();
    assert_eq!(
        ordinary, 1,
        "fixture must queue exactly one ordinary context"
    );
    assert_eq!(sba, 1, "fixture must park exactly one SBA-choice context");

    (runner, keep, doomed)
}

/// The production-pipeline regression. Answering the SBA choice must dispatch
/// ONLY the parked SBA batch alongside the answer's delayed trigger; the
/// ordinary context keeps its own ordering window.
///
/// Before the partition fix this produced a single `OrderTriggers` prompt
/// listing both the ordinary context and the answer's dies trigger — one
/// ordering window covering two, which is the defect.
#[test]
fn sba_choice_answer_does_not_absorb_an_ordinary_deferred_context() {
    let (mut runner, keep, doomed) = runner_at_mixed_sba_choice();

    let _ = runner.act(GameAction::ChooseLegend { keep });

    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }),
        "the ordinary context must not be ordered together with the answer's \
         delayed trigger; got a combined ordering prompt: {:?}",
        runner.state().waiting_for
    );

    let stacked: Vec<ObjectId> = runner
        .state()
        .stack
        .iter()
        .filter_map(|entry| match &entry.kind {
            StackEntryKind::TriggeredAbility { source_id, .. } => Some(*source_id),
            _ => None,
        })
        .collect();
    assert_eq!(
        stacked,
        vec![doomed, ORDINARY_SOURCE],
        "the SBA batch owns the answer's window and goes on the stack first; the \
         ordinary context follows in its own window"
    );
}

/// The unit contract the dispatch above depends on: the take is a PARTITION.
/// Only `StateBasedActionChoice` contexts leave the queue, and the ordinary
/// remainder stays queued for its ordinary drain policy.
#[test]
fn take_sba_choice_trigger_batch_partitions_by_origin() {
    let (mut runner, _keep, doomed) = runner_at_mixed_sba_choice();

    // Boundary = the whole queue: both contexts predate the answer, exactly as
    // the pipeline sees it at the top of the answering pass.
    let preexisting = runner.state().deferred_triggers.len();
    let taken = take_sba_choice_trigger_batch(runner.state_mut(), preexisting);

    assert_eq!(
        taken
            .iter()
            .map(|c| c.pending.source_id)
            .collect::<Vec<_>>(),
        vec![doomed],
        "only the SBA-parked context may be taken for the answer's batch"
    );
    assert!(
        taken
            .iter()
            .all(|c| matches!(c.batch_origin, DeferredTriggerBatchOrigin::Ordinary)),
        "the taken batch's ownership marker is consumed, so a re-park is ordinary work"
    );
    assert_eq!(
        runner
            .state()
            .deferred_triggers
            .iter()
            .map(|c| c.pending.source_id)
            .collect::<Vec<_>>(),
        vec![ORDINARY_SOURCE],
        "the ordinary context must remain queued for its own drain policy"
    );
}
