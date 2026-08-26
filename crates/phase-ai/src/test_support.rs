//! Shared unit-test fixtures for the plan-aware seams.
//!
//! These three helpers were private to `search.rs`'s test module until the
//! CR 601.2f sacrifice-cost cast gate (`policies::sacrifice_cost_mana_gate`)
//! became a second consumer of the same derived-plan discipline. They are moved
//! here verbatim — including the reachability doc comment below, which is the
//! contract both consumers depend on — rather than duplicated, so there stays
//! exactly one way to build a plan-carrying test context.

use std::sync::Arc;

use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use crate::config::AiConfig;
use crate::context::AiContext;
use crate::plan::PlanSnapshot;
use crate::search::build_ai_context_with_session;
use crate::session::AiSession;

/// The plan a plain midrange deck derives — land target **6**.
///
/// REACHABILITY REQUIREMENT: every `AiSession::plan` entry written **on a
/// production path** is produced by `derive_snapshot` (`session.rs`:
/// `from_game`, `from_single_deck`, `ensure_player_features`), so a
/// hand-built `PlanSnapshot` pins behaviour at a land target production
/// cannot present. These two constructors are the only reachable shapes: a
/// `wants_ramp_curve` deck targets 7, every other deck targets 6.
///
/// Scoped precisely, because the unqualified claim is false. **Tests write
/// `session.plan` directly** — `context_with_plans` below, and
/// `sole_planned_cycling_land_waits_but_remains_finite` (both now insert
/// derived snapshots). Two hand-built snapshots remain, in
/// `plan_aware_bottoming_cuts_surplus_lands_to_plan_target` and
/// `plan_aware_bottoming_protects_feature_payoff_names`; they feed
/// `plan_aware_bottom_cards`, a different consumer that reads a fixed
/// schedule index rather than a target, and one of them deliberately passes
/// an all-zero `PlanSnapshot::default()` to exercise the degenerate case.
/// `PlanSnapshot::default()` is also a *production* fallback at those two
/// bottoming call sites, so an all-zero schedule is itself reachable there.
/// What this requirement actually says is: **every fixture that reaches
/// `PlanState::realize`, i.e. the keep-tier / discard family, is derived.**
pub(crate) fn default_deck_plan() -> PlanSnapshot {
    crate::plan::derive_snapshot(&crate::features::DeckFeatures::default())
}

/// The plan a ramp deck derives — land target **7**, the only other
/// reachable target.
pub(crate) fn ramp_deck_plan() -> PlanSnapshot {
    crate::plan::derive_snapshot(&crate::features::DeckFeatures {
        mana_ramp: crate::features::ManaRampFeature {
            dork_count: 8,
            commitment: 0.96,
            ..Default::default()
        },
        ..Default::default()
    })
}

/// Build a context whose session carries a real derived plan per player.
/// `ai_player` is the seat the search optimizes for — deliberately a
/// separate parameter from the plan keys.
pub(crate) fn context_with_plans(
    state: &GameState,
    ai_player: PlayerId,
    config: &AiConfig,
    plans: &[(PlayerId, PlanSnapshot)],
) -> AiContext {
    let mut session = AiSession::default();
    for (player, plan) in plans {
        session.plan.insert(*player, plan.clone());
    }
    build_ai_context_with_session(state, ai_player, config, Arc::new(session))
}
