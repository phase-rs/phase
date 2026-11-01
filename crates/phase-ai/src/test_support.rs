//! Shared unit-test fixtures for the plan-aware seams and the stack readers.
//!
//! The three plan helpers were private to `search.rs`'s test module until the
//! CR 601.2f sacrifice-cost cast gate (`policies::sacrifice_cost_mana_gate`)
//! became a second consumer of the same derived-plan discipline. They are moved
//! here verbatim — including the reachability doc comment below, which is the
//! contract both consumers depend on — rather than duplicated, so there stays
//! exactly one way to build a plan-carrying test context.

use std::sync::Arc;

use engine::game::ability_utils::build_resolved_from_def_with_targets;
use engine::game::scenario::{GameScenario, P0};
use engine::game::zones::create_object;
use engine::types::ability::{AbilityDefinition, Effect};
use engine::types::game_state::{GameState, StackEntry, StackEntryKind};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

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

/// Scute Swarm's Oracle text.
pub(crate) const SCUTE_SWARM: &str = "Landfall — Whenever a land you control enters, create a 1/1 green Insect creature token. If you control six or more lands, create a token that's a copy of this creature instead.";

/// Scute Swarm's landfall ability, parsed from its Oracle text: a token, or a
/// copy of the creature instead. It acts on nothing a stack reader judges.
fn scute_swarm_landfall() -> AbilityDefinition {
    let mut scenario = GameScenario::new();
    let swarm = scenario
        .add_creature_from_oracle(P0, "Scute Swarm", 1, 1, SCUTE_SWARM)
        .id();
    let runner = scenario.build();
    let landfall = runner.state().objects[&swarm].trigger_definitions[0]
        .definition
        .execute
        .as_deref()
        .cloned()
        .expect("the landfall trigger has an effect");
    assert!(
        matches!(*landfall.effect, Effect::Token { .. })
            && landfall
                .sub_ability
                .as_deref()
                .is_some_and(|sub| matches!(*sub.effect, Effect::CopyTokenOf { .. })),
        "reach guard: the landfall trigger makes a token, or a copy instead"
    );
    landfall
}

/// `board` with a Scute Swarm landfall trigger of `controller`'s below every
/// stack entry, between each two and on top: the stack a landfall turn builds
/// around the entries a reader judges.
pub(crate) fn with_landfall_triggers(board: &GameState, controller: PlayerId) -> GameState {
    let landfall = scute_swarm_landfall();
    let mut state = board.clone();
    for index in (0..=state.stack.len()).rev() {
        let card_id = CardId(state.next_object_id);
        let source = create_object(
            &mut state,
            card_id,
            controller,
            "Scute Swarm".to_string(),
            Zone::Battlefield,
        );
        let id = ObjectId(state.next_object_id);
        state.next_object_id += 1;
        state.stack.insert(
            index,
            StackEntry {
                id,
                source_id: source,
                controller,
                kind: StackEntryKind::TriggeredAbility {
                    source_id: source,
                    ability: Box::new(build_resolved_from_def_with_targets(
                        &landfall,
                        source,
                        controller,
                        Vec::new(),
                    )),
                    condition: None,
                    trigger_event: None,
                    description: None,
                    source_name: "Scute Swarm".to_string(),
                    subject_match_count: None,
                    die_result: None,
                    provenance: None,
                },
            },
        );
    }
    state
}

/// What `read` returns, paired with how many stack entries' reach
/// (`stack_reach::stack_entry_node_reach`) it built.
pub(crate) fn reading_with_reach_count<T>(read: impl FnOnce() -> T) -> (T, u64) {
    engine::game::perf_counters::reset();
    let value = read();
    (
        value,
        engine::game::perf_counters::stack_reach_snapshot().entry_reaches,
    )
}
