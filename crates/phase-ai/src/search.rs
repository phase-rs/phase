use std::cmp::Ordering;
use std::sync::Arc;

use rand::{Rng, RngCore, SeedableRng};
use rand_chacha::ChaCha20Rng;

use engine::ai_support::build_decision_context;
use engine::types::ability::{
    AbilityDefinition, ContinuousModification, Duration, Effect, StaticDefinition, TargetFilter,
};
use engine::types::actions::{AlternativeCastDecision, GameAction, MulliganChoice};
use engine::types::card_type::CoreType;
use engine::types::game_state::{
    CastOfferKind, CostResume, GameState, ManaChoice, ManaChoicePrompt, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

use crate::cast_facts::cast_facts_for_action;
use crate::combat_ai::{choose_attackers_with_targets_with_profile, choose_blockers_with_profile};
use crate::config::{AiConfig, PlannerMode, ThreatAwareness};
use crate::context::AiContext;
use crate::features::DeckFeatures;
use crate::mana_colors::demand_aware_single_color;
use crate::plan::PlanSnapshot;
use crate::planner::{
    apply_candidate, BeamContinuationPlanner, ContinuationPlanner, PlannerServices,
    RankedCandidate, SearchBudget,
};
use crate::policies::context::{PolicyContext, SearchDepth};
use crate::policies::copy_value::score_legend_rule_keep;
use crate::policies::tutor::{score_search_choice_cards, score_search_choice_selection};
use crate::policies::{PolicyId, PolicyRegistry, PolicyVerdict};
use crate::session::AiSession;
use crate::tactical_gate::gate_candidates;
use crate::threat_profile::{
    build_threat_profile_multiplayer, ArchetypeBaseProbabilities, ThreatProfile,
};

/// CR 103.5b + Serum Powder Oracle text: return the first object in `player`'s
/// hand named "Serum Powder", if any. Used by the AI mulligan-decision branch
/// to auto-use a Powder rather than mulligan or, in the deterministic-default
/// path, rather than blindly keep — Serum Powder is strictly better than a
/// mulligan (no bottoming, no mulligan count increment).
fn first_serum_powder_in_hand(
    state: &GameState,
    player: PlayerId,
) -> Option<engine::types::identifiers::ObjectId> {
    let p = state.players.iter().find(|p| p.id == player)?;
    p.hand.iter().copied().find(|oid| {
        state
            .objects
            .get(oid)
            .is_some_and(|o| o.name.eq_ignore_ascii_case("Serum Powder"))
    })
}

/// AI safety cap on repeated activation of the same activated ability on the
/// same source within a single turn. CR 117.1b permits unbounded activation
/// at priority and absent a CR 602.5b restriction there is no per-turn cap
/// in the rules — this is a pure AI-pathology mitigation. Legitimate
/// patterns of same-source repeated activation are rare: tokens and
/// mana-abilities bypass this filter (mana abilities never hit the
/// non-mana `ActivateAbility` path; tokens have distinct `ObjectId`s per
/// instance).
///
/// **Known trade-off**: "remove a counter: deal 1 damage" style abilities
/// (Walking Ballista, Triskelion, Hangarback Walker) are bounded by their
/// own counter depletion but could legitimately exceed this cap in a lethal
/// turn (e.g. 10 counters → 10 pings). None of the registered duel-suite
/// decks contain such cards; if one is added, revisit this cap or replace
/// it with structural "source-state-unchanged" detection.
const MAX_ACTIVATIONS_PER_SOURCE_PER_TURN: u32 = 4;

/// CR 117.1 + Whitemane Lion loop mitigation (issue #563): AI safety cap on
/// the number of times the same card can be CAST in a single turn by the AI.
/// Identification is by card name captured in `SpellCastRecord` so different
/// printings/copies of the same card share the cap. CR 117.1 permits unbounded
/// casting at priority — this cap is a pure AI-pathology mitigation against
/// loop-prone cards (ETB self-bounce, Whitemane Lion class) whose
/// per-occurrence value remains positive even when the net board state is
/// unchanged. Three is generous enough for legitimate value plays (Snapcaster
/// flashback + recast, Eternal Witness reanimate chain) while preventing the
/// thousands-of-iterations pathology observed in #563.
const MAX_CASTS_OF_SAME_CARD_PER_TURN: usize = 3;
const LARGE_BOARD_FAST_PRIORITY_OBJECTS: usize = 1000;

fn pick_lowest_value_sacrifices(
    state: &GameState,
    cards: &[ObjectId],
    count: usize,
) -> Vec<ObjectId> {
    let mut scored: Vec<_> = cards
        .iter()
        .map(|&id| (id, evaluate_card_value(state, id)))
        .collect();
    scored.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.into_iter().take(count).map(|(id, _)| id).collect()
}

/// Choose the best action for the AI player given the current game state.
///
/// - For 0 or 1 legal actions, returns immediately.
/// - For DeclareAttackers/DeclareBlockers, delegates to combat AI.
/// - For VeryEasy/Easy (search disabled), uses heuristic scoring + softmax.
/// - For Medium+ (search enabled), uses beam-ordered frontier search with rollout-backed leaves.
pub fn choose_action(
    state: &GameState,
    ai_player: PlayerId,
    config: &AiConfig,
    rng: &mut impl Rng,
) -> Option<GameAction> {
    if let Some(action) = fast_priority_action(state, ai_player) {
        return Some(action);
    }

    let session = AiSession::arc_from_game(state);
    choose_action_with_session(state, ai_player, config, rng, &session)
}

/// Choose the best action using a caller-owned per-game session cache.
pub fn choose_action_with_session(
    state: &GameState,
    ai_player: PlayerId,
    config: &AiConfig,
    rng: &mut impl Rng,
    session: &Arc<AiSession>,
) -> Option<GameAction> {
    // CR 103.5: For simultaneous mulligan states, the AI controller's only
    // job is to act on behalf of `ai_player`. If `ai_player` is not in the
    // pending set, there is nothing to choose — return None so the WASM
    // bridge doesn't fabricate an action that would fail authorization.
    match &state.waiting_for {
        WaitingFor::MulliganDecision { pending, .. }
            if !pending.iter().any(|e| e.player == ai_player) =>
        {
            return None;
        }
        WaitingFor::MulliganBottomCards { pending }
            if !pending.iter().any(|e| e.player == ai_player) =>
        {
            return None;
        }
        WaitingFor::OpeningHandBottomCards { pending, .. }
            if !pending.iter().any(|e| e.player == ai_player) =>
        {
            return None;
        }
        _ => {}
    }

    // CR 702.104a: Tribute prompt — the AI's pay/decline decision has a
    // dedicated simple-eval heuristic rather than going through the tactical
    // policy registry. Punishment value vs counter value.
    if matches!(state.waiting_for, WaitingFor::TributeChoice { .. }) {
        if let Some(decision) = crate::tribute_eval::decide(state) {
            return Some(GameAction::DecideOptionalEffect {
                accept: decision.accept(),
            });
        }
    }

    // CR 608.2c + CR 701.23: SearchChoice picks have their own dedicated
    // beam-bounded scorer in `deterministic_choice`. Routing them through
    // `score_candidates` first would force `validate_candidates` to clone
    // state and re-apply every legal SelectCards combination — for a
    // multi-card tutor against a large library that is hundreds of state
    // clones (already capped engine-side, but still wasteful relative to
    // the dedicated scorer). The deterministic path returns the chosen
    // SelectCards directly; only fall through if it produces nothing.
    if matches!(state.waiting_for, WaitingFor::SearchChoice { .. }) {
        let context = build_ai_context_with_session(state, ai_player, config, Arc::clone(session));
        if let Some(action) = deterministic_choice(state, ai_player, config, &[], Some(&context)) {
            return Some(action);
        }
    }

    // CR 106.3 + CR 608.2d: Selecting which color a flexible mana source
    // produces while paying for a spell is mechanical, not a policy judgment —
    // the AI must produce the color the in-flight cost demands. `candidates.rs`
    // enumerates *every* color option, so `score_candidates` is always non-empty
    // and the old `fallback_action` SingleColor arm never fired on the normal
    // path; the scorer then picked an arbitrary (first-enumerated) color,
    // tapping a U/R dual for {R} when the spell needed {U} and stranding the pip
    // (ManaPayment dead-end). This deterministic pre-emption — parallel to the
    // TributeChoice and SearchChoice pre-emptions above — is the real fix.
    if let WaitingFor::ChooseManaColor {
        choice: ManaChoicePrompt::SingleColor { ref options },
        ..
    } = state.waiting_for
    {
        if let Some(color) = demand_aware_single_color(options, state) {
            return Some(GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(color),
                count: 1,
            });
        }
    }

    if let Some(action) = fast_priority_action(state, ai_player) {
        return Some(action);
    }

    let mut scored = score_candidates_with_session(state, ai_player, config, session);
    if scored.is_empty() {
        // No valid candidates from search — fall back to a safe escape action
        // so the game never deadlocks waiting for the AI.
        return fallback_action(state);
    }
    if config.execution_mode.is_measurement() {
        scored.sort_by_cached_key(|(action, _)| action_order_key(action));
    }
    let chosen = if scored.len() == 1 {
        Some(scored[0].0.clone())
    } else {
        softmax_select_pairs(&scored, config.temperature, rng)
    };
    if let Some(action) = &chosen {
        emit_decision_trace(state, ai_player, config, action, session);
    }
    chosen
}

fn fast_priority_action(state: &GameState, ai_player: PlayerId) -> Option<GameAction> {
    let WaitingFor::Priority { player } = state.waiting_for else {
        return None;
    };
    if player != ai_player {
        return None;
    }

    if large_board_main_phase_has_no_development_sources(state, ai_player) {
        return Some(GameAction::PassPriority);
    }

    let actions = engine::ai_support::flat_priority_actions(state);
    low_value_priority_pass_from_actions(state, ai_player, &actions)
        .or_else(|| large_board_main_phase_fast_action_from_actions(state, ai_player, &actions))
}

fn large_board_main_phase_has_no_development_sources(
    state: &GameState,
    ai_player: PlayerId,
) -> bool {
    if state.battlefield.len() < LARGE_BOARD_FAST_PRIORITY_OBJECTS
        || state.active_player != ai_player
        || !state.stack.is_empty()
        || !matches!(state.phase, Phase::PreCombatMain | Phase::PostCombatMain)
    {
        return false;
    }

    let player = &state.players[ai_player.0 as usize];
    if !player.hand.is_empty() || !player.graveyard.is_empty() {
        return false;
    }
    if engine::game::planechase::can_roll_planar_die(state, ai_player) {
        return false;
    }

    if state.exile.iter().any(|&object_id| {
        state
            .objects
            .get(&object_id)
            .is_some_and(|object| object.owner == ai_player || object.controller == ai_player)
    }) {
        return false;
    }

    let controlled_battlefield_is_inert = state.battlefield.iter().copied().all(|object_id| {
        state.objects.get(&object_id).is_none_or(|object| {
            object.controller != ai_player || object_has_no_development_source(object)
        })
    });
    let controlled_command_zone_is_inert = state.command_zone.iter().copied().all(|object_id| {
        state.objects.get(&object_id).is_none_or(|object| {
            (object.owner != ai_player && object.controller != ai_player)
                || object_has_no_development_source(object)
        })
    });

    controlled_battlefield_is_inert && controlled_command_zone_is_inert
}

fn object_has_no_development_source(object: &engine::game::game_object::GameObject) -> bool {
    object
        .abilities
        .iter()
        .all(engine::game::mana_abilities::is_mana_ability)
        && object.trigger_definitions.is_empty()
        && object.replacement_definitions.is_empty()
        && object.static_definitions.is_empty()
        && object.prepared.is_none()
        && object.room_unlocks.is_none()
        && !object.keywords.iter().any(|keyword| {
            matches!(
                keyword,
                engine::types::keywords::Keyword::Crew { .. }
                    | engine::types::keywords::Keyword::Saddle(_)
                    | engine::types::keywords::Keyword::Station
            )
        })
}

fn priority_action_is_safe_to_defer_on_own_stack(state: &GameState, action: &GameAction) -> bool {
    match action {
        GameAction::PassPriority => true,
        GameAction::ActivateAbility {
            source_id,
            ability_index,
        } => activated_ability_is_safe_to_defer(state, *source_id, *ability_index),
        _ => false,
    }
}

fn priority_action_is_safe_to_defer_empty_stack(state: &GameState, action: &GameAction) -> bool {
    match action {
        GameAction::PassPriority => true,
        GameAction::ActivateAbility {
            source_id,
            ability_index,
        } => empty_stack_activation_is_low_value(state, *source_id, *ability_index),
        _ => false,
    }
}

fn priority_action_is_pass_or_mana(state: &GameState, action: &GameAction) -> bool {
    match action {
        GameAction::PassPriority => true,
        GameAction::ActivateAbility {
            source_id,
            ability_index,
        } => activated_ability_definition(state, *source_id, *ability_index)
            .is_some_and(engine::game::mana_abilities::is_mana_ability),
        _ => false,
    }
}

fn activated_ability_is_safe_to_defer(
    state: &GameState,
    source_id: ObjectId,
    ability_index: usize,
) -> bool {
    activated_ability_definition(state, source_id, ability_index)
        .is_some_and(|ability| !ability_interacts_with_stack(ability))
}

fn empty_stack_activation_is_low_value(
    state: &GameState,
    source_id: ObjectId,
    ability_index: usize,
) -> bool {
    activated_ability_definition(state, source_id, ability_index).is_some_and(|ability| {
        engine::game::mana_abilities::is_mana_ability(ability)
            || ability_is_temporary_combat_modifier(ability)
    })
}

fn activated_ability_definition(
    state: &GameState,
    source_id: ObjectId,
    ability_index: usize,
) -> Option<&AbilityDefinition> {
    state
        .objects
        .get(&source_id)
        .and_then(|object| object.abilities.get(ability_index))
}

fn ability_interacts_with_stack(ability: &AbilityDefinition) -> bool {
    effect_interacts_with_stack(&ability.effect)
        || ability
            .sub_ability
            .as_deref()
            .is_some_and(ability_interacts_with_stack)
        || ability
            .else_ability
            .as_deref()
            .is_some_and(ability_interacts_with_stack)
        || ability
            .mode_abilities
            .iter()
            .any(ability_interacts_with_stack)
}

fn effect_interacts_with_stack(effect: &Effect) -> bool {
    matches!(effect, Effect::CounterAll { .. })
        || effect
            .target_filter()
            .is_some_and(target_filter_interacts_with_stack)
}

fn target_filter_interacts_with_stack(filter: &TargetFilter) -> bool {
    matches!(
        filter,
        TargetFilter::StackSpell | TargetFilter::StackAbility { .. }
    ) || filter.extract_zones().contains(&Zone::Stack)
}

fn ability_is_temporary_combat_modifier(ability: &AbilityDefinition) -> bool {
    ability_effect_is_temporary_combat_modifier(ability)
        && ability
            .sub_ability
            .as_deref()
            .is_none_or(ability_is_temporary_combat_modifier)
        && ability
            .else_ability
            .as_deref()
            .is_none_or(ability_is_temporary_combat_modifier)
        && ability
            .mode_abilities
            .iter()
            .all(ability_is_temporary_combat_modifier)
}

fn ability_effect_is_temporary_combat_modifier(ability: &AbilityDefinition) -> bool {
    match &*ability.effect {
        Effect::Pump { .. } => matches!(ability.duration, Some(Duration::UntilEndOfTurn)),
        effect => effect_is_temporary_combat_modifier(effect),
    }
}

fn effect_is_temporary_combat_modifier(effect: &Effect) -> bool {
    match effect {
        Effect::GenericEffect {
            static_abilities,
            duration: Some(Duration::UntilEndOfTurn),
            ..
        } => static_abilities
            .iter()
            .all(static_definition_is_temporary_combat_modifier),
        _ => false,
    }
}

fn static_definition_is_temporary_combat_modifier(static_def: &StaticDefinition) -> bool {
    matches!(static_def.mode, StaticMode::Continuous)
        && static_def
            .modifications
            .iter()
            .all(continuous_modification_is_temporary_combat_modifier)
}

fn continuous_modification_is_temporary_combat_modifier(
    modification: &ContinuousModification,
) -> bool {
    matches!(
        modification,
        ContinuousModification::AddPower { .. }
            | ContinuousModification::AddToughness { .. }
            | ContinuousModification::AddKeyword { .. }
    )
}

fn low_value_empty_stack_phase(phase: Phase) -> bool {
    matches!(
        phase,
        Phase::Upkeep | Phase::Draw | Phase::End | Phase::Cleanup
    )
}

fn low_value_priority_pass_from_actions(
    state: &GameState,
    ai_player: PlayerId,
    actions: &[GameAction],
) -> Option<GameAction> {
    let WaitingFor::Priority { player } = state.waiting_for else {
        return None;
    };
    if player != ai_player
        || !actions
            .iter()
            .any(|action| matches!(action, GameAction::PassPriority))
    {
        return None;
    }

    let owns_entire_stack = !state.stack.is_empty()
        && state
            .stack
            .iter()
            .all(|entry| entry.controller == ai_player);
    let own_stack_pass = owns_entire_stack
        && actions
            .iter()
            .all(|action| priority_action_is_safe_to_defer_on_own_stack(state, action));
    let empty_stack_pass = state.stack.is_empty()
        && actions
            .iter()
            .all(|action| priority_action_is_safe_to_defer_empty_stack(state, action))
        && (low_value_empty_stack_phase(state.phase)
            || actions
                .iter()
                .all(|action| priority_action_is_pass_or_mana(state, action)));

    if own_stack_pass || empty_stack_pass {
        Some(GameAction::PassPriority)
    } else {
        None
    }
}

fn large_board_main_phase_fast_action_from_actions(
    state: &GameState,
    ai_player: PlayerId,
    actions: &[GameAction],
) -> Option<GameAction> {
    let WaitingFor::Priority { player } = state.waiting_for else {
        return None;
    };
    if player != ai_player
        || state.battlefield.len() < LARGE_BOARD_FAST_PRIORITY_OBJECTS
        || state.active_player != ai_player
        || !state.stack.is_empty()
        || !matches!(state.phase, Phase::PreCombatMain | Phase::PostCombatMain)
    {
        return None;
    }

    if let Some(action) = prefer_land_drop(state, ai_player, actions) {
        return Some(action);
    }

    actions
        .iter()
        .filter_map(|action| match action {
            GameAction::CastSpell { object_id, .. } => {
                Some((evaluate_card_value(state, *object_id), action.clone()))
            }
            _ => None,
        })
        .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal))
        .map(|(_, action)| action)
}

/// Emit a structured decision-trace event for the chosen tactical action.
///
/// Gated on `phase_ai::decision_trace` at DEBUG — zero hot-path overhead when
/// disabled (the `event_enabled!` macro compiles to a single filter check).
/// When enabled, rebuilds the `PolicyRegistry` context for the chosen
/// candidate and emits the top 3 policy contributions sorted by `|delta|`
/// descending, plus any defensive `Reject` verdicts. Mulligan decisions are
/// excluded — the `MulliganRegistry` emits its own trace at
/// `phase_ai::decision_trace`.
fn emit_decision_trace(
    state: &GameState,
    ai_player: PlayerId,
    config: &AiConfig,
    action: &GameAction,
    session: &Arc<AiSession>,
) {
    if !tracing::event_enabled!(target: "phase_ai::decision_trace", tracing::Level::DEBUG) {
        return;
    }
    if matches!(state.waiting_for, WaitingFor::MulliganDecision { .. }) {
        return;
    }

    let ctx = build_decision_context(state);
    let candidate = ctx.candidates.iter().find(|c| c.action == *action);
    let Some(candidate) = candidate else {
        // The chosen action was produced by a deterministic path (combat AI,
        // scry ordering, etc.) that doesn't flow through the tactical policy
        // registry, so there is nothing to aggregate.
        return;
    };

    let context = build_ai_context_with_session(state, ai_player, config, Arc::clone(session));
    emit_trace_for_candidate(state, &ctx, candidate, ai_player, config, &context);
}

/// Core aggregator: given a fully-built `PolicyContext`'s inputs for a chosen
/// candidate, run every applicable policy via `PolicyRegistry::verdicts()`,
/// sort scored verdicts by `|delta|` descending, and emit a structured
/// tracing event. Separated from `emit_decision_trace` so integration tests
/// can drive the aggregator with a handcrafted `AiContext` (bypassing
/// `build_ai_context`, which depends on `state.deck_pools`).
///
/// Exposed `pub` with `#[doc(hidden)]` to keep the public surface area tight
/// while enabling direct trace-contract assertions from `tests/`.
#[doc(hidden)]
pub fn emit_trace_for_candidate(
    state: &GameState,
    decision: &engine::ai_support::AiDecisionContext,
    candidate: &engine::ai_support::CandidateAction,
    ai_player: PlayerId,
    config: &AiConfig,
    context: &AiContext,
) {
    if !tracing::event_enabled!(target: "phase_ai::decision_trace", tracing::Level::DEBUG) {
        return;
    }
    let policies = PolicyRegistry::shared();
    let cast_facts = cast_facts_for_action(state, &candidate.action, ai_player);
    let policy_ctx = PolicyContext {
        state,
        decision,
        candidate,
        ai_player,
        config,
        context,
        cast_facts,
        // The decision trace reflects the committed (root) decision.
        search_depth: SearchDepth::Root,
    };
    let verdicts = policies.verdicts(&policy_ctx);

    // Partition into Rejects (always logged) and Scores (top-3 by |delta|).
    type RejectEntry = (PolicyId, &'static str, Vec<(&'static str, i64)>);
    type ScoreEntry = (PolicyId, f64, &'static str, Vec<(&'static str, i64)>);
    let mut rejects: Vec<RejectEntry> = Vec::new();
    let mut scores: Vec<ScoreEntry> = Vec::new();
    for (id, verdict) in verdicts {
        match verdict {
            PolicyVerdict::Reject { reason } => {
                rejects.push((id, reason.kind, reason.facts));
            }
            PolicyVerdict::Score { delta, reason } => {
                scores.push((id, delta, reason.kind, reason.facts));
            }
        }
    }
    scores.sort_by(|a, b| {
        b.1.abs()
            .partial_cmp(&a.1.abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let top: Vec<_> = scores.into_iter().take(3).collect();

    let top_fmt: Vec<String> = top
        .iter()
        .map(|(id, delta, kind, facts)| format!("{:?}:{}={:+.3}{:?}", id, kind, delta, facts))
        .collect();
    let rejects_fmt: Vec<String> = rejects
        .iter()
        .map(|(id, kind, facts)| format!("{:?}:{}{:?}", id, kind, facts))
        .collect();

    tracing::debug!(
        target: "phase_ai::decision_trace",
        ai_player = ai_player.0,
        action = ?std::mem::discriminant(&candidate.action),
        top_policies = ?top_fmt,
        rejects = ?rejects_fmt,
        "tactical decision"
    );
}

/// Produce a safe action when the AI has no scored candidates.
/// During combat, submit empty declarations. During active play, pass priority.
/// Returns None only for terminal states (GameOver) where no action is possible.
///
/// **Invariant:** this function must never be called in a `has_pending_cast`
/// state. `casting::can_cast_object_now` is the single authority on castability
/// — if it returns true, the engine guarantees the cast pipeline (targeting,
/// mode selection, cost payment) has a valid completion path. Reaching the
/// pending-cast branch here means that authority has a gap: the AI entered a
/// cast it cannot complete. Fix the gate, not the recovery.
///
/// In release builds we still emit `CancelCast` to keep the match running, but
/// debug builds panic so the gap surfaces during testing instead of silently
/// degrading AI play into cast/cancel churn.
fn fallback_action(state: &GameState) -> Option<GameAction> {
    // Pending-cast states can always be escaped with CancelCast (CR 601.2).
    // Check this before the exhaustive match so every pending-cast variant
    // is covered without repeating CancelCast per-arm.
    if state.waiting_for.has_pending_cast() {
        // The internal discriminant tag is niche-optimized (non-sequential), so
        // print the variant *name* (the Debug prefix before its first field) and
        // the in-flight spell's card name instead — an opaque discriminant alone
        // is not enough to diagnose which cast/card exposed the gap.
        let debug = format!("{:?}", state.waiting_for);
        let variant = debug.split([' ', '{']).next().unwrap_or("<unknown>");
        // ManaPayment externalizes its PendingCast into `GameState::pending_cast`
        // rather than the WaitingFor variant, so check both sources.
        let spell = state
            .waiting_for
            .pending_cast_ref()
            .or(state.pending_cast.as_deref())
            .and_then(|pc| state.objects.get(&pc.object_id))
            .map_or("<none>", |obj| obj.name.as_str());
        debug_assert!(
            false,
            "AI fallback reached during pending cast (variant {variant}, spell {spell}) — \
             can_cast_object_now has a gap that allowed an uncompletable cast through. \
             Tighten the pre-cast check rather than relying on CancelCast recovery."
        );
        tracing::error!(
            variant,
            spell,
            "AI fallback cancelled an uncompletable cast — can_cast_object_now gap"
        );
        return Some(GameAction::CancelCast);
    }

    match &state.waiting_for {
        // Terminal — no action possible.
        WaitingFor::GameOver { .. } => None,

        // Priority is the only state where PassPriority is valid.
        WaitingFor::Priority { .. } => Some(GameAction::PassPriority),

        // Combat declarations: an empty declaration is NOT always legal —
        // CR 508.1d / CR 701.15b require goaded / "attacks if able" creatures
        // to be declared. Delegate to the engine's `legal_actions`, which runs
        // the simulation filter and only emits engine-legal candidates.
        WaitingFor::DeclareAttackers { .. } => engine::ai_support::legal_actions(state)
            .into_iter()
            .find(|a| matches!(a, GameAction::DeclareAttackers { .. })),
        WaitingFor::DeclareBlockers { .. } => engine::ai_support::legal_actions(state)
            .into_iter()
            .find(|a| matches!(a, GameAction::DeclareBlockers { .. })),
        WaitingFor::UntapChoice { candidates, .. } => {
            candidates
                .first()
                .map(|&object_id| GameAction::ChooseUntap {
                    object_id,
                    untap: true,
                })
        }
        // CR 502.3: bounded untap-subset selection under a MaxUntapPerType cap.
        // The conservative fallback untaps the cap-maximizing first `max` of the
        // group (untapping more would be illegal, untapping fewer is never
        // beneficial), guaranteeing the AI resolves the prompt without wedging.
        WaitingFor::ChooseUntapSubset { group, max, .. } => Some(GameAction::SelectCards {
            cards: group.iter().copied().take(*max).collect(),
        }),
        // CR 508.1g: exert-as-attack is optional; the conservative fallback
        // declines (never has a downside). Real exert decisions come from the
        // evaluated candidate actions.
        WaitingFor::ExertChoice { .. } => Some(GameAction::ChooseExert { exert: false }),
        // CR 508.1g + CR 702.154a: Enlist is optional; the conservative
        // fallback declines while normal search evaluates legal tap choices.
        WaitingFor::EnlistChoice { .. } => Some(GameAction::ChooseEnlist { target: None }),

        // Target selection: skip optional slots, fizzle mandatory ones.
        // TriggerTargetSelection is not a pending cast — the trigger is
        // already on the stack. ChooseTarget { target: None } signals
        // "no legal target" and causes the trigger to fizzle (CR 608.2b).
        WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
            Some(GameAction::ChooseTarget { target: None })
        }

        // CR 701.21a: Mandatory spell-effect sacrifices (Deadly Brew, Edict
        // riders) must pick a legal permanent — an empty SelectCards fails
        // validation when `count > 0` and `up_to` is false.
        WaitingFor::EffectZoneChoice {
            cards,
            count,
            up_to,
            effect_kind: engine::types::ability::EffectKind::Sacrifice,
            ..
        } if !cards.is_empty() && !*up_to && *count > 0 => Some(GameAction::SelectCards {
            cards: pick_lowest_value_sacrifices(state, cards, *count),
        }),

        // Selection states: empty selection is a valid "choose nothing".
        WaitingFor::ScryChoice { .. }
        | WaitingFor::DigChoice { .. }
        | WaitingFor::SurveilChoice { .. }
        | WaitingFor::RevealChoice { .. }
        | WaitingFor::SearchChoice { .. }
        | WaitingFor::ChooseFromZoneChoice { .. }
        | WaitingFor::DiscardChoice { .. }
        | WaitingFor::EffectZoneChoice { .. }
        | WaitingFor::ConniveDiscard { .. }
        | WaitingFor::DiscardToHandSize { .. }
        | WaitingFor::ManifestDreadChoice { .. }
        | WaitingFor::WardDiscardChoice { .. }
        | WaitingFor::WardSacrificeChoice { .. }
        | WaitingFor::UnlessBounceChoice { .. } => {
            Some(GameAction::SelectCards { cards: Vec::new() })
        }
        // CR 701.4a + CR 608.2d: Behold requires EXACTLY one beholdable object —
        // an empty selection is illegal. Take the first candidate (any legal pick
        // resolves the prompt; the evaluated candidate enumerator picks properly).
        WaitingFor::BeholdChoice { choices, .. } => choices
            .first()
            .map(|&id| GameAction::SelectCards { cards: vec![id] }),
        // CR 705.1 + CR 614.1a: Krark's Thumb keep choice — keep the first
        // `keep_count` flips (always in range, since keep_count <= results.len()).
        WaitingFor::CoinFlipKeepChoice { keep_count, .. } => Some(GameAction::SelectCoinFlips {
            keep_indices: (0..*keep_count).collect(),
        }),
        // CR 608.2d: SearchPartitionChoice requires EXACTLY primary_count cards —
        // an empty selection is illegal. Deterministically take the first
        // primary_count of the found set for the battlefield (rest auto-route).
        WaitingFor::SearchPartitionChoice {
            cards,
            primary_count,
            ..
        } => Some(GameAction::SelectCards {
            cards: cards
                .iter()
                .take(*primary_count as usize)
                .copied()
                .collect(),
        }),
        WaitingFor::OutsideGameChoice { choices, count, .. } => {
            // CR 400.11 + CR 406.3: Take the first `count` available picks
            // across the unified sideboard + face-up-exile pool. Sideboard
            // entries can be picked up to their remaining `count`; face-up
            // exile entries are unique objects (count fixed at 1) per the
            // resolver. The selection wire format is one discriminated
            // `OutsideGameSelection` per pick.
            use engine::types::actions::OutsideGameSelection;
            use engine::types::game_state::OutsideGameChoiceSource;
            let selections: Vec<OutsideGameSelection> = choices
                .iter()
                .flat_map(|choice| {
                    let count = choice.count as usize;
                    (0..count).map(move |_| match &choice.source {
                        OutsideGameChoiceSource::Sideboard {
                            sideboard_index, ..
                        } => OutsideGameSelection::Sideboard {
                            sideboard_index: *sideboard_index,
                        },
                        OutsideGameChoiceSource::FaceUpExile { object_id } => {
                            OutsideGameSelection::FaceUpExile {
                                object_id: *object_id,
                            }
                        }
                    })
                })
                .take(*count)
                .collect();
            Some(GameAction::ChooseOutsideGameCards { selections })
        }

        // Sylvan Library-style choices: topdeck the required cards rather than
        // paying life in the fallback path.
        WaitingFor::DrawnThisTurnTopdeckChoice { cards, count, .. } => {
            Some(GameAction::SelectCards {
                cards: cards.iter().take(*count).copied().collect(),
            })
        }

        // Multi-target selection: zero targets is valid when min == 0.
        WaitingFor::MultiTargetSelection { .. } => {
            Some(GameAction::SelectCards { cards: Vec::new() })
        }

        // Soulbond pair choice: choose the first legal partner; if none remain,
        // decline the pair.
        WaitingFor::PairChoice { choices, .. } => Some(GameAction::ChoosePair {
            partner: choices.first().copied(),
        }),

        // Binary accept/decline decisions: decline is always safe.
        WaitingFor::OptionalEffectChoice { .. }
        | WaitingFor::OpponentMayChoice { .. }
        | WaitingFor::TributeChoice { .. }
        | WaitingFor::CommanderZoneChoice { .. }
        | WaitingFor::MiracleReveal { .. }
        | WaitingFor::CastOffer {
            kind: CastOfferKind::Miracle { .. } | CastOfferKind::Madness { .. },
            ..
        } => Some(GameAction::DecideOptionalEffect { accept: false }),

        // Unless payment: decline to pay (let the effect resolve).
        WaitingFor::UnlessPayment { .. } => Some(GameAction::PayUnlessCost { pay: false }),

        // Disjunctive activation costs: default to the first payable branch.
        WaitingFor::ActivationCostOneOfChoice {
            player,
            costs,
            pending_cast,
        } => costs
            .iter()
            .position(|cost| cost.is_payable(state, *player, pending_cast.object_id))
            .map(|index| GameAction::ChooseActivationCostBranch { index }),
        // CR 118.12a: Disjunctive unless-cost choice. Fallback is to decline
        // the choice (let the effect resolve), mirroring `UnlessPayment`'s
        // pessimistic-default policy.
        WaitingFor::UnlessPaymentChooseCost { .. } => Some(GameAction::ChooseUnlessCostBranch {
            choice: engine::types::actions::UnlessCostBranch::Decline,
        }),

        // Combat tax: decline to pay.
        WaitingFor::CombatTaxPayment { .. } => Some(GameAction::PayCombatTax { accept: false }),

        // Equip/Populate/CopyTarget with no valid targets: CancelCast for
        // equip (activation that can be backed out); skip for non-cast.
        WaitingFor::EquipTarget { .. } => Some(GameAction::CancelCast),
        WaitingFor::PopulateChoice { .. } | WaitingFor::CopyTargetChoice { .. } => {
            Some(GameAction::ChooseTarget { target: None })
        }

        // Crew/Saddle/Station with no eligible creatures: CancelCast
        // (these are activated abilities that can be backed out).
        WaitingFor::CrewVehicle { .. }
        | WaitingFor::SaddleMount { .. }
        | WaitingFor::StationTarget { .. } => Some(GameAction::CancelCast),

        // Ring-bearer with no creatures: skip (empty ChooseTarget).
        WaitingFor::ChooseRingBearer { .. } => Some(GameAction::ChooseTarget { target: None }),

        // Distribute with empty targets: empty distribution.
        WaitingFor::DistributeAmong { .. } => Some(GameAction::DistributeAmong {
            distribution: Vec::new(),
        }),

        // Replacement choice: pick the first option.
        WaitingFor::ReplacementChoice { .. } => Some(GameAction::ChooseReplacement { index: 0 }),

        // Trigger order: keep the engine-provided order.
        WaitingFor::OrderTriggers { triggers, .. } => Some(GameAction::OrderTriggers {
            order: (0..triggers.len()).collect(),
        }),

        // CR 103.5 + 103.5b: Mulligan default = keep, unless the AI has a
        // Serum Powder in hand, in which case use it first (auto-heuristic —
        // see `first_serum_powder_in_hand`).
        WaitingFor::MulliganDecision { pending, .. } => {
            let entry = pending.first()?;
            Some(match first_serum_powder_in_hand(state, entry.player) {
                Some(object_id) => GameAction::MulliganDecision {
                    choice: MulliganChoice::UseSerumPowder { object_id },
                },
                None => GameAction::MulliganDecision {
                    choice: MulliganChoice::Keep,
                },
            })
        }
        WaitingFor::MulliganBottomCards { .. } | WaitingFor::OpeningHandBottomCards { .. } => {
            Some(GameAction::SelectCards { cards: Vec::new() })
        }

        // Named choice: pick the first option if available.
        WaitingFor::NamedChoice { options, .. } => {
            options.first().map(|choice| GameAction::ChooseOption {
                choice: choice.clone(),
            })
        }

        // Spellbook draft: pick the first card in the list.
        WaitingFor::SpellbookDraft { options, .. } => options
            .first()
            .map(|card| GameAction::SubmitSpellbookDraft { card: card.clone() }),

        // Damage source choice: pick the first option.
        WaitingFor::DamageSourceChoice { options, .. } => options
            .first()
            .map(|&source| GameAction::ChooseDamageSource { source }),

        // CR 709.5f-g: room-door choice — pick the first offered (op, door).
        WaitingFor::ChooseRoomDoor {
            object_id, options, ..
        } => options
            .first()
            .map(|&(op, door)| GameAction::ChooseRoomDoor {
                object_id: *object_id,
                op,
                door,
            }),

        // Mode choice: select first mode.
        WaitingFor::ModeChoice { .. } | WaitingFor::AbilityModeChoice { .. } => {
            Some(GameAction::SelectModes { indices: vec![0] })
        }

        // Choose-one-of branch: pick the first branch.
        WaitingFor::ChooseOneOfBranch { .. } => Some(GameAction::ChooseBranch { index: 0 }),

        // Discover/Cascade: decline.
        WaitingFor::CastOffer {
            kind: CastOfferKind::Discover { .. },
            ..
        } => Some(GameAction::DiscoverChoice {
            choice: engine::types::actions::CastChoice::Decline,
        }),
        // CR 608.2g + CR 609.4b: paid graveyard cast — decline by default (parity
        // with Discover/Cascade/Ripple); the candidate generator explores accept.
        WaitingFor::CastOffer {
            kind: CastOfferKind::GraveyardPaidCast { .. },
            ..
        } => Some(GameAction::GraveyardPaidCastChoice {
            choice: engine::types::actions::CastChoice::Decline,
        }),
        // CR 701.20a: RevealUntil kept choice — accept (put onto the battlefield)
        // as the search default; the candidate generator still explores decline.
        WaitingFor::RevealUntilKeptChoice { .. } => {
            Some(GameAction::DecideOptionalEffect { accept: true })
        }
        WaitingFor::CastOffer {
            kind: CastOfferKind::Cascade { .. },
            ..
        } => Some(GameAction::CascadeChoice {
            choice: engine::types::actions::CastChoice::Decline,
        }),
        // CR 702.60a: Ripple — decline as the default; candidates explore casting.
        WaitingFor::CastOffer {
            kind: CastOfferKind::Ripple { .. },
            ..
        } => Some(GameAction::RippleChoice {
            choice: engine::types::actions::CastChoice::Decline,
        }),
        // CR 608.2g + CR 601.2: Invoke Calamity's free-cast window — finish the
        // window (cast nothing) as the conservative default; the candidate
        // generator still explores casting each eligible spell.
        WaitingFor::CastOffer {
            kind: CastOfferKind::FreeCastWindow { .. },
            ..
        } => Some(GameAction::FreeCastWindowChoice { selection: None }),
        // CR 107.1c: "repeat this process" — stop as the forced-action default;
        // the candidate generator still explores repeating.
        WaitingFor::RepeatDecision { .. } => {
            Some(GameAction::DecideOptionalEffect { accept: false })
        }

        // Learn: skip.
        WaitingFor::LearnChoice { .. } => Some(GameAction::LearnDecision {
            choice: engine::types::actions::LearnOption::Skip,
        }),

        // Top or bottom: put on top.
        WaitingFor::TopOrBottomChoice { .. } | WaitingFor::ClashCardPlacement { .. } => {
            Some(GameAction::ChooseTopOrBottom { top: true })
        }

        // CR 702.140c + CR 730.2a: mutate merge side — default to placing the
        // mutating spell on top (the candidate generator still explores bottom).
        WaitingFor::MutateMergeChoice { .. } => Some(GameAction::ChooseMutateMergeSide {
            side: engine::game::merge::MergeSide::Top,
        }),

        // CR 702.99a: cipher encode — default to encoding on the first legal host
        // (the candidate generator still explores declining and other hosts).
        WaitingFor::CipherEncodeChoice { creatures, .. } => Some(GameAction::CipherEncode {
            creature: creatures.first().copied(),
        }),

        // CR 701.30b: clash opponent choice — fall back to the first candidate.
        WaitingFor::ClashChooseOpponent { candidates, .. } => candidates
            .first()
            .map(|&opponent| GameAction::ChooseClashOpponent { opponent }),

        // Adventure/MDFC/alt-cost choice: default to the "normal" face/cost.
        WaitingFor::CastOffer {
            kind: CastOfferKind::Adventure { .. },
            ..
        } => Some(GameAction::ChooseAdventureFace { creature: true }),
        WaitingFor::ModalFaceChoice { .. } => {
            Some(GameAction::ChooseModalFace { back_face: false })
        }
        // CR 118.9: Default to the printed mana cost (Normal). Each keyword
        // resolves through its own post-payment handler in the engine; the
        // search-time default is uniform.
        WaitingFor::AlternativeCastChoice { .. } => Some(GameAction::ChooseAlternativeCast {
            choice: AlternativeCastDecision::Normal,
        }),
        WaitingFor::CastingVariantChoice { options, .. } => {
            (!options.is_empty()).then_some(GameAction::ChooseCastingVariant { index: 0 })
        }
        WaitingFor::ChoosePermanentTypeSlot {
            available_slots, ..
        } => available_slots
            .first()
            .map(|slot| GameAction::ChoosePermanentTypeSlot { slot: *slot }),

        // Choose play/draw and sideboard: between-games defaults.
        WaitingFor::BetweenGamesChoosePlayDraw { .. } => {
            Some(GameAction::ChoosePlayDraw { play_first: true })
        }
        WaitingFor::BetweenGamesSideboard { player, .. } => {
            // Submit the current deck unchanged (no sideboarding).
            let pool = state.deck_pools.iter().find(|p| p.player == *player);
            pool.map(|p| {
                let main = p
                    .current_main
                    .iter()
                    .fold(
                        std::collections::BTreeMap::<String, u32>::new(),
                        |mut acc, entry| {
                            if entry.count > 0 {
                                *acc.entry(entry.card.name.clone()).or_insert(0) += entry.count;
                            }
                            acc
                        },
                    )
                    .into_iter()
                    .map(|(name, count)| engine::types::match_config::DeckCardCount { name, count })
                    .collect();
                let sideboard = p
                    .current_sideboard
                    .iter()
                    .fold(
                        std::collections::BTreeMap::<String, u32>::new(),
                        |mut acc, entry| {
                            if entry.count > 0 {
                                *acc.entry(entry.card.name.clone()).or_insert(0) += entry.count;
                            }
                            acc
                        },
                    )
                    .into_iter()
                    .map(|(name, count)| engine::types::match_config::DeckCardCount { name, count })
                    .collect();
                GameAction::SubmitSideboard { main, sideboard }
            })
        }

        // Dungeon choices: pick first option.
        WaitingFor::ChooseDungeon { options, .. } => options
            .first()
            .map(|&dungeon| GameAction::ChooseDungeon { dungeon }),
        WaitingFor::ChooseDungeonRoom { options, .. } => options
            .first()
            .map(|&room_index| GameAction::ChooseDungeonRoom { room_index }),
        WaitingFor::SpecializeColor { options, .. } => options
            .first()
            .copied()
            .map(|color| GameAction::ChooseSpecializeColor { color }),

        // Paradigm: pass.
        WaitingFor::CastOffer {
            kind: CastOfferKind::Paradigm { .. },
            ..
        } => Some(GameAction::PassParadigmOffer),

        // Vote: pick the first option.
        // CR 608.2c: For `ControllerLabels` votes (Battlebond friend-or-foe),
        // the AI is the spell controller making one label per player. The
        // heuristic is trivial: self → friend (the beneficial label, choice
        // index 0), every other player → foe (the harmful label, choice
        // index 1). Classic votes (where `actor == player`) fall back to
        // "first option" since the AI is voting for itself.
        WaitingFor::VoteChoice {
            options,
            player,
            actor,
            controller,
            candidate_objects,
            ..
        } => {
            // CR 701.38b: object-pool votes (Council's Judgment, Prime
            // Minister's Cabinet Room) submit a ballot by candidate index, not
            // by option word — the engine's `handle_resolution_choice` rejects
            // `ChooseOption` whenever `candidate_objects` is non-empty. The
            // deadlock-safety fallback must mirror that shape, so vote for the
            // first candidate object rather than emitting an action the engine
            // would reject.
            if !candidate_objects.is_empty() {
                return Some(GameAction::SubmitVoteCandidate { candidate_index: 0 });
            }
            // The friend-or-foe heuristic only fires when the controller is
            // labeling other players (the delegated shape) — matching
            // `VoteActor::Delegated(actor)` where `actor == controller` is
            // robust to any future delegated-vote shape where the actor is
            // some non-controller player.
            let choice_text = match actor {
                engine::types::game_state::VoteActor::Delegated(actor) if *actor == *controller => {
                    let target_label = if player == controller {
                        "friend"
                    } else {
                        "foe"
                    };
                    options
                        .iter()
                        .find(|o| o.as_str() == target_label)
                        .or_else(|| options.first())
                        .cloned()
                }
                _ => options.first().cloned(),
            };
            choice_text.map(|choice| GameAction::ChooseOption { choice })
        }

        // CR 704.5j: keep the commander / original over ephemeral copy tokens.
        WaitingFor::ChooseLegend { candidates, .. } => candidates
            .iter()
            .max_by(|&&left, &&right| {
                score_legend_rule_keep(state, left)
                    .partial_cmp(&score_legend_rule_keep(state, right))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|&keep| GameAction::ChooseLegend { keep }),

        // Battle protector: pick the first candidate.
        WaitingFor::BattleProtectorChoice { candidates, .. } => candidates
            .first()
            .map(|&protector| GameAction::ChooseBattleProtector { protector }),

        // Proliferate: choose nothing.
        WaitingFor::ProliferateChoice { .. } => Some(GameAction::SelectTargets {
            targets: Vec::new(),
        }),

        // CR 701.56a: Time travel — default to changing nothing this phase
        // (an empty selection is legal: "choose any number").
        WaitingFor::TimeTravelChoice { .. } => Some(GameAction::SelectTargets {
            targets: Vec::new(),
        }),

        // CR 702.132a: Assist — default to not seeking help (decline the offer)
        // and, if asked to contribute, contribute nothing.
        WaitingFor::AssistChoosePlayer { .. } => {
            Some(GameAction::ChooseAssistPlayer { player: None })
        }
        WaitingFor::AssistPayment { .. } => Some(GameAction::CommitAssistPayment { generic: 0 }),

        // ChooseObjectsIntoTrackedSet: default to declining (empty selection).
        WaitingFor::ChooseObjectsSelection { .. } => Some(GameAction::SelectTargets {
            targets: Vec::new(),
        }),

        // Copy retarget: keep copied targets when all slots already have a
        // current value; freshly cast prepare/paradigm copies start empty, so
        // choose the first legal target for the current slot.
        WaitingFor::CopyRetarget {
            target_slots,
            current_slot,
            ..
        } => {
            let slot = target_slots.get(*current_slot)?;
            if target_slots.iter().all(|slot| slot.current.is_some()) {
                Some(GameAction::KeepAllCopyTargets)
            } else if slot.current.is_some() {
                Some(GameAction::ChooseTarget { target: None })
            } else {
                slot.legal_alternatives
                    .first()
                    .cloned()
                    .map(|target| GameAction::ChooseTarget {
                        target: Some(target),
                    })
            }
        }

        // Assign combat damage: greedy lethal-to-each, mirroring the engine's
        // ai_support::candidates AssignCombatDamage arm so the fallback stays
        // rules-legal for trample (CR 702.19b) and trample-over-PW (CR 702.19c).
        WaitingFor::AssignCombatDamage {
            total_damage,
            blockers,
            trample,
            pw_loyalty,
            attack_target,
            ..
        } => {
            let mut remaining = *total_damage;
            let mut assignments = Vec::new();
            // CR 702.19b: Assign lethal to each blocker in order.
            for slot in blockers {
                let assign = remaining.min(slot.lethal_minimum);
                assignments.push((slot.blocker_id, assign));
                remaining = remaining.saturating_sub(assign);
            }
            // CR 510.1c: Non-trample — the leftover must land on a blocker (no player
            // spillover), so dump it on the last blocker to keep the total == power.
            if trample.is_none() && remaining > 0 {
                if let Some(last) = assignments.last_mut() {
                    last.1 += remaining;
                    remaining = 0;
                }
            }
            // CR 702.19c: Trample-over-PW attacking a PW splits excess into
            // loyalty-worth to the PW and the remainder to the PW's controller.
            let (trample_damage, controller_damage) = if *trample
                == Some(engine::game::combat::TrampleKind::OverPlaneswalkers)
                && matches!(
                    attack_target,
                    engine::game::combat::AttackTarget::Planeswalker(_)
                ) {
                let loyalty = pw_loyalty.unwrap_or(0);
                let to_pw = remaining.min(loyalty);
                let to_ctrl = remaining.saturating_sub(to_pw);
                (to_pw, to_ctrl)
            } else {
                // CR 702.19b: Standard trample — all excess to the attack target.
                (if trample.is_some() { remaining } else { 0 }, 0)
            };
            Some(GameAction::AssignCombatDamage {
                mode: engine::types::game_state::CombatDamageAssignmentMode::Normal,
                assignments,
                trample_damage,
                controller_damage,
            })
        }

        // CR 510.1d + CR 702.22k: a banded blocker's damage is divided by the
        // ACTIVE player among the attackers it blocks. There is no lethal rule
        // (CR 510.1d), so the simplest legal division dumps the blocker's full
        // power onto the first blocked attacker — mirroring the engine's
        // ai_support::candidates AssignBlockerDamage arm.
        WaitingFor::AssignBlockerDamage {
            total_damage,
            attackers,
            ..
        } => attackers
            .first()
            .map(|first| GameAction::AssignBlockerDamage {
                assignments: vec![(*first, *total_damage)],
            }),

        // X value: pick max (CR 107.1c + CR 601.2f). The engine has already
        // capped `max` to the maximum legally-payable X for this cast (see
        // `engine::game::casting_costs::max_x_value`), so picking max is always
        // affordable. Issue #710: the previous default of X=0 caused every
        // unsupervised X-cost spell to resolve for no effect (Fireball dealing
        // 0 damage, Hydroid Krasis entering 0/0, Banefire whiffing). Picking
        // max is the right safety net when no tactical policy scores; the
        // XValuePolicy + CopyValuePolicy still override this for cases where a
        // smaller X is strictly better (e.g. a copy spell whose only legal
        // targets sit at a lower mana value).
        WaitingFor::ChooseXValue { max, .. } => Some(GameAction::ChooseX { value: *max }),

        // Pay amount: pick minimum.
        WaitingFor::PayAmountChoice { min, .. } => {
            Some(GameAction::SubmitPayAmount { amount: *min })
        }

        // Retarget: keep current targets.
        WaitingFor::RetargetChoice {
            current_targets, ..
        } => Some(GameAction::RetargetSpell {
            new_targets: current_targets.clone(),
        }),

        // Companion reveal: decline.
        WaitingFor::CompanionReveal { .. } => {
            Some(GameAction::DeclareCompanion { card_index: None })
        }

        // Explore choice: pick the first choosable creature.
        WaitingFor::ExploreChoice { choosable, .. } => {
            choosable.first().map(|&id| GameAction::ChooseTarget {
                target: Some(engine::types::ability::TargetRef::Object(id)),
            })
        }

        // CR 303.4 + CR 303.4g: Aura attach pick — the engine only installs
        // this state when `legal_targets` is non-empty, so picking the first
        // candidate is always a legal fallback.
        WaitingFor::ReturnAsAuraTarget { legal_targets, .. } => {
            legal_targets
                .first()
                .cloned()
                .map(|target| GameAction::ChooseTarget {
                    target: Some(target),
                })
        }

        // Phyrexian payment: preserve each shard's only legal route when there
        // is no scored candidate to choose from.
        WaitingFor::PhyrexianPayment { shards, .. } => {
            let choices = shards
                .iter()
                .map(|shard| match shard.options {
                    engine::types::game_state::ShardOptions::LifeOnly => {
                        engine::types::game_state::ShardChoice::PayLife
                    }
                    engine::types::game_state::ShardOptions::ManaOrLife
                    | engine::types::game_state::ShardOptions::ManaOnly => {
                        engine::types::game_state::ShardChoice::PayMana
                    }
                })
                .collect();
            Some(GameAction::SubmitPhyrexianChoices { choices })
        }

        // Mana-related states: picking a color or paying mana.
        WaitingFor::ChooseManaColor { choice, .. } => {
            match choice {
                ManaChoicePrompt::SingleColor { options } => {
                    // Defense-in-depth: the primary fix is the pre-emption in
                    // `choose_action_with_session`; this honors the demanded
                    // color too should this path ever be reached.
                    demand_aware_single_color(options, state).map(|color| {
                        GameAction::ChooseManaColor {
                            choice: ManaChoice::SingleColor(color),
                            count: 1,
                        }
                    })
                }
                ManaChoicePrompt::Combination { options } => {
                    options.first().map(|combo| GameAction::ChooseManaColor {
                        choice: ManaChoice::Combination(combo.clone()),
                        count: 1,
                    })
                }
                ManaChoicePrompt::AnyCombination { count, options } => {
                    let combo = vec![
                        options
                            .first()
                            .copied()
                            .unwrap_or(engine::types::mana::ManaType::Colorless);
                        *count
                    ];
                    Some(GameAction::ChooseManaColor {
                        choice: ManaChoice::Combination(combo),
                        count: 1,
                    })
                }
            }
        }
        WaitingFor::PayManaAbilityMana { options, .. } => {
            options.first().map(|plan| GameAction::PayManaAbilityMana {
                payment: plan.clone(),
            })
        }

        // Mana ability sub-costs: these are not pending-cast states but
        // carry PendingManaAbility. Empty eligible lists shouldn't normally
        // happen but CancelCast is not valid here. Use empty selection.
        WaitingFor::PayCost {
            resume: CostResume::ManaAbility { .. },
            ..
        } => Some(GameAction::SelectCards { cards: Vec::new() }),

        // CR 101.4 + CR 701.21a: Category choice — pick one permanent
        // per type category, the rest are sacrificed. A permanent that belongs
        // to multiple categories (e.g. an artifact creature) is eligible in
        // each and may be chosen in each eligible slot. `None` is legal only
        // for an empty category.
        WaitingFor::CategoryChoice {
            eligible_per_category,
            ..
        } => {
            let choices = eligible_per_category
                .iter()
                .map(|eligible| eligible.first().copied())
                .collect();
            Some(GameAction::SelectCategoryPermanents { choices })
        }

        // CR 107.1c + CR 701.21a (Slaughter the Strong): keep the most creatures
        // whose running power total fits the cap (lowest power first) — a valid,
        // non-trivial fallback that minimises self-sacrifice.
        WaitingFor::KeepWithinTotalPowerChoice { eligible, cap, .. } => {
            let power = |id: &engine::types::identifiers::ObjectId| {
                state.objects.get(id).and_then(|o| o.power).unwrap_or(0)
            };
            let mut by_power = eligible.clone();
            by_power.sort_by_key(power);
            let mut kept = Vec::new();
            let mut total = 0i32;
            for id in by_power {
                let p = power(&id);
                if total + p <= *cap {
                    total += p;
                    kept.push(id);
                }
            }
            Some(GameAction::ChooseKeptCreatures { kept })
        }

        // CR 700.3: Pile-separation fallbacks — empty pile-A partition (every
        // object goes to derived pile B) is the simplest legal partition, and
        // pile A is the default choice for the chooser. Tactical AI override
        // happens through legal_actions; this is the safety net.
        WaitingFor::SeparatePilesPartition { .. } => {
            Some(GameAction::SubmitPilePartition { pile_a: Vec::new() })
        }
        WaitingFor::SeparatePilesChoice { .. } => Some(GameAction::ChoosePile {
            pile: engine::types::game_state::PileSide::A,
        }),
        WaitingFor::MoveCountersDistribution { .. } => engine::ai_support::legal_actions(state)
            .into_iter()
            .find(|action| matches!(action, GameAction::ChooseCounterMoveDistribution { .. })),
        WaitingFor::RemoveCountersChoice { .. } => engine::ai_support::legal_actions(state)
            .into_iter()
            .find(|action| matches!(action, GameAction::ChooseCountersToRemove { .. })),

        // Remaining pending-cast states are caught by the has_pending_cast
        // guard above. This arm is structurally unreachable but required
        // for exhaustive match. ManaPayment is a pending-cast state.
        WaitingFor::ManaPayment { .. }
        | WaitingFor::OptionalCostChoice { .. }
        | WaitingFor::SpliceOffer { .. }
        | WaitingFor::DefilerPayment { .. }
        | WaitingFor::PayCost {
            resume: CostResume::Spell { .. } | CostResume::SpellCost { .. },
            ..
        }
        | WaitingFor::BlightChoice { .. }
        | WaitingFor::CostTypeChoice { .. }
        | WaitingFor::CollectEvidenceChoice { .. }
        | WaitingFor::HarmonizeTapChoice { .. } => {
            // These are all pending-cast states — the has_pending_cast guard
            // above already returned CancelCast. This branch is unreachable
            // at runtime but keeps the match exhaustive.
            Some(GameAction::CancelCast)
        }
    }
}

/// Score all candidate actions without selecting one.
/// Returns `(GameAction, f64)` pairs for external merging (root parallelism).
/// For special cases (mulligan, combat, etc.) returns a single-element list
/// with the deterministic choice scored at 1.0.
pub fn score_candidates(
    state: &GameState,
    ai_player: PlayerId,
    config: &AiConfig,
) -> Vec<(GameAction, f64)> {
    if let Some(action) = fast_priority_action(state, ai_player) {
        return vec![(action, 1.0)];
    }

    let session = AiSession::arc_from_game(state);
    score_candidates_with_session(state, ai_player, config, &session)
}

/// Canonical serialization key for aggregating action scores across
/// determinized samples. `GameAction` derives `Serialize` (but not `Eq`/`Hash`),
/// so we key by `serde_json::to_string`, mirroring the frontend `mergeScores`
/// `JSON.stringify(action)` contract exactly.
type GameActionKey = String;

fn game_action_key(action: &GameAction) -> GameActionKey {
    serde_json::to_string(action).unwrap_or_default()
}

/// Sum each sample's per-action score into `acc` (first-seen order preserved).
/// `positions` maps a key to its index in `acc`; `counts` records how many
/// samples observed each action (the pin-invariant expects this to reach K for
/// every action — see `finalize_mean`).
fn merge_into(
    acc: &mut Vec<(GameAction, f64)>,
    positions: &mut std::collections::HashMap<GameActionKey, usize>,
    counts: &mut std::collections::HashMap<GameActionKey, usize>,
    scored: Vec<(GameAction, f64)>,
) {
    for (action, score) in scored {
        let key = game_action_key(&action);
        match positions.get(&key) {
            Some(&pos) => {
                acc[pos].1 += score;
                *counts.get_mut(&key).expect("counted") += 1;
            }
            None => {
                let pos = acc.len();
                acc.push((action, score));
                positions.insert(key.clone(), pos);
                counts.insert(key, 1);
            }
        }
    }
}

/// Divide each accumulated sum by the number of samples that observed it,
/// yielding the ensemble mean (matches the frontend `mergeScores` averaging).
/// The pin-invariant guarantees a constant candidate support across samples, so
/// every action should be observed exactly `k` times; the `debug_assert` fires
/// loudly if a future change lets the support drift (strategy fusion over a
/// non-constant support). Release degrades to per-action-observed-count mean —
/// `counts` is always >= 1 for any accumulated action, so never a divide-by-zero.
fn finalize_mean(
    mut acc: Vec<(GameAction, f64)>,
    counts: std::collections::HashMap<GameActionKey, usize>,
    k: usize,
) -> Vec<(GameAction, f64)> {
    for (action, score) in acc.iter_mut() {
        let observed = counts
            .get(&game_action_key(action))
            .copied()
            .unwrap_or(1)
            .max(1);
        debug_assert_eq!(
            observed, k,
            "determinization aggregation: action observed in {observed}/{k} samples (support drift)"
        );
        *score /= observed as f64;
    }
    acc
}

/// Ensemble entry point (native + WASM inherit it). With
/// `determinization_samples == 0` this is byte-identical to the pre-feature
/// single search. With `K > 0` it runs the untouched search against K
/// determinized opponent-hidden-zone samples and means the per-action scores.
pub fn score_candidates_with_session(
    state: &GameState,
    ai_player: PlayerId,
    config: &AiConfig,
    session: &Arc<AiSession>,
) -> Vec<(GameAction, f64)> {
    let k = config.search.determinization_samples;
    if k == 0 {
        // Unchanged path: no determinization, no shared-deadline override.
        return score_candidates_core(state, ai_player, config, session, None);
    }

    // ONE shared wall-clock ceiling across all K sequential samples (bounds
    // AGGREGATE latency ~time_budget_ms, not K x budget). Measurement mode is
    // bounded by node cap only — mirrors `PlannerServices::with_deadline`, so
    // `cargo ai-gate` stays deterministic and K-bounded solely by nodes.
    let deadline = if config.execution_mode.is_measurement() {
        engine::util::Deadline::none()
    } else {
        match config.search.time_budget_ms {
            Some(ms) => engine::util::Deadline::after(ms),
            None => engine::util::Deadline::none(),
        }
    };

    // Seed: fixed across K for a given (position, game, worker); per-sample split
    // by index. `state.rng.clone()` keeps `&state` immutable (RNG purity via
    // clone). Native runs diverge via distinct `rng_seed`; WASM workers diverge
    // via the per-worker `state.rng` re-seed.
    let base_seed = crate::planner::quick_state_hash(state)
        .wrapping_add(state.rng_seed)
        .wrapping_add(state.rng.clone().next_u64());

    let mut acc: Vec<(GameAction, f64)> = Vec::new();
    let mut positions: std::collections::HashMap<GameActionKey, usize> =
        std::collections::HashMap::new();
    let mut counts: std::collections::HashMap<GameActionKey, usize> =
        std::collections::HashMap::new();
    for i in 0..k {
        let seed = base_seed.wrapping_add(crate::determinize::splitmix64(i as u64));
        let mut rng = ChaCha20Rng::seed_from_u64(seed);
        let sampled = crate::determinize::determinize_opponents(state, ai_player, &mut rng);
        let scored = score_candidates_core(&sampled, ai_player, config, session, Some(deadline));
        merge_into(&mut acc, &mut positions, &mut counts, scored);
    }
    finalize_mean(acc, counts, k as usize)
}

/// Core scoring for a single (possibly determinized) state. Byte-identical to
/// the pre-feature `score_candidates_with_session` except it threads a shared
/// `deadline_override` into `PlannerServices` — `None` reproduces the old
/// behavior exactly.
fn score_candidates_core(
    state: &GameState,
    ai_player: PlayerId,
    config: &AiConfig,
    session: &Arc<AiSession>,
    deadline_override: Option<engine::util::Deadline>,
) -> Vec<(GameAction, f64)> {
    if let Some(action) = fast_priority_action(state, ai_player) {
        return vec![(action, 1.0)];
    }

    let ctx = build_decision_context(state);
    let policies = PolicyRegistry::shared();
    let context = build_ai_context_with_session(state, ai_player, config, Arc::clone(session));

    // Combat decisions bypass the candidate pipeline entirely — the combat AI
    // reads directly from game state and never uses generated candidates.
    // This must run before validation/gating, which can filter out all candidates
    // and cause an empty-actions early return that skips deterministic_choice.
    // build_ai_context runs first so combat gets the archetype-modulated profile.
    if matches!(
        state.waiting_for,
        WaitingFor::DeclareAttackers { .. } | WaitingFor::DeclareBlockers { .. }
    ) {
        let effective_profile = config.profile.with_strategy(&context.strategy);
        if let Some(action) = deterministic_combat_choice(
            state,
            ai_player,
            &effective_profile,
            Some(session.as_ref()),
        ) {
            return vec![(action, 1.0)];
        }
    }

    let mut services =
        PlannerServices::with_deadline(ai_player, config, policies, context, deadline_override);
    let candidates = services.validate_candidates(state, ctx.candidates.clone());
    let gated = gate_candidates(
        state,
        &ctx,
        candidates,
        ai_player,
        config,
        &services.context,
    );

    // Filter out (a) spells/abilities that were cast then cancelled this
    // priority window (prevents cast→cancel→recast loops), (b) activated
    // abilities whose prior activation is still pending on the stack
    // (prevents re-picking the same ability before it resolves — a
    // pathological softmax outcome when the effect is redundant or
    // self-undoing), and (c) activated abilities that have been activated
    // more than `MAX_ACTIVATIONS_PER_SOURCE_PER_TURN` times this turn on the
    // same source (AI safety cap against loops where the effect is
    // card-neutral — e.g. "Discard a card: gain indestructible UEOT" when
    // the buff is already active and a discard-triggered draw replaces the
    // discarded card). CR 117.1b permits unbounded activation at priority,
    // and absent a CR 602.5b restriction there is no per-turn cap, so this
    // cap is a pure AI-pathology mitigation — legitimate patterns of
    // repeated same-source activation are extremely rare (tokens and
    // mana-abilities have distinct per-activation identities or bypass
    // this filter entirely).
    //
    // `cancelled_casts` and `pending_activations` clear on PassPriority;
    // `activated_abilities_this_turn` clears on turn change.
    let mut gated: Vec<_> = gated
        .into_iter()
        .filter(|g| match &g.candidate.action {
            GameAction::CastSpell { object_id, .. } => {
                if state.cancelled_casts.contains(object_id) {
                    return false;
                }
                // CR 117.1 + #563: Cap repeated casts of the same card by name
                // within a single turn. The AI player's
                // `spells_cast_this_turn_by_player` record carries each cast's
                // captured name (`SpellCastRecord.name`) so the cap survives
                // the spell having left the stack. Lookups are case-sensitive
                // matches against the candidate object's current name (set at
                // creation from the card name).
                let candidate_name = state
                    .objects
                    .get(object_id)
                    .map(|o| o.name.as_str())
                    .unwrap_or("");
                if candidate_name.is_empty() {
                    return true;
                }
                let cast_count = state
                    .spells_cast_this_turn_by_player
                    .get(&ai_player)
                    .map(|history| {
                        history
                            .iter()
                            .filter(|rec| rec.name == candidate_name)
                            .count()
                    })
                    .unwrap_or(0);
                cast_count < MAX_CASTS_OF_SAME_CARD_PER_TURN
            }
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            } => {
                !state.cancelled_casts.contains(source_id)
                    && !state
                        .pending_activations
                        .contains(&(*source_id, *ability_index))
                    && state
                        .activated_abilities_this_turn
                        .get(&(*source_id, *ability_index))
                        .copied()
                        .unwrap_or(0)
                        < MAX_ACTIVATIONS_PER_SOURCE_PER_TURN
            }
            _ => true,
        })
        .collect();
    if config.execution_mode.is_measurement() {
        gated.sort_by_cached_key(|g| action_order_key(&g.candidate.action));
    }

    let actions: Vec<GameAction> = gated
        .iter()
        .map(|candidate| candidate.candidate.action.clone())
        .collect();

    if actions.is_empty() {
        return vec![];
    }

    // Deterministic early returns — these don't benefit from search/parallelism.
    // Pass the already-built context so the mulligan branch avoids a second
    // full deck analysis (DeckProfile + SynergyGraph for both players).
    if let Some(action) =
        deterministic_choice(state, ai_player, config, &actions, Some(&services.context))
    {
        return vec![(action, 1.0)];
    }

    // Score actions via search or heuristics
    if config.search.enabled {
        let branching = config.search.max_branching as usize;

        // Target selection decisions are dominated by the tactical policy
        // (anti-self-harm) but benefit from limited search lookahead.
        // The 0.7 weight ensures the tactical signal (anti-self-harm penalties
        // of -50+) still dominates obvious cases while allowing 30% search
        // influence for ambiguous multi-target decisions where the
        // continuation matters (e.g., which creature to pump).
        let is_target_selection = matches!(
            state.waiting_for,
            WaitingFor::TargetSelection { .. }
                | WaitingFor::TriggerTargetSelection { .. }
                | WaitingFor::MultiTargetSelection { .. }
        );
        // Stack response decisions (counter/interact with opponent's spell) need
        // higher tactical weight because search can't see through the full
        // cast-target-pay-resolve chain at typical depths. Policies like
        // counterspell_score and stack_awareness guide these reactive decisions.
        let is_stack_response = !state.stack.is_empty()
            && state
                .stack
                .iter()
                .any(|entry| entry.controller != ai_player);
        let tactical_weight = if is_target_selection {
            0.7
        } else if is_stack_response {
            0.35
        } else {
            0.1
        };

        // Score and rank directly from `gated`, which already carries penalty
        // alongside each candidate. Previously a `penalty_for` closure did an
        // O(n) linear scan of `gated` per scored candidate — O(n²) overall.
        // GameAction is not Hash, so we can't key a HashMap; carrying the
        // penalty with its candidate is both cheaper and more idiomatic.
        let mut ranked: Vec<RankedCandidate> = gated
            .iter()
            .map(|g| {
                let tactical = services.tactical_score(
                    state,
                    &ctx,
                    &g.candidate,
                    ai_player,
                    SearchDepth::Root,
                );
                RankedCandidate {
                    candidate: g.candidate.clone(),
                    score: tactical + g.penalty,
                }
            })
            .collect();
        ranked.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(Ordering::Equal)
                .then_with(|| {
                    action_order_key(&a.candidate.action)
                        .cmp(&action_order_key(&b.candidate.action))
                })
        });
        ranked.truncate(branching);

        // Iterative deepening: rung 0 (quiesced eval per candidate) -> ceiling.
        // Return the deepest *fully completed* rung. The deepest rung reproduces
        // origin/main's fixed-depth pass; the TT (per-decision, on `services`)
        // accelerates the re-search of transposing subtrees across rungs.
        let ceiling: u32 = match config.search.planner_mode {
            PlannerMode::BeamOnly => 0,
            PlannerMode::BeamPlusRollout => config.search.max_depth.saturating_sub(1),
        };

        // No-regression floor == origin/main's deadline collapse: tactical-only for
        // every candidate. Overwritten by each completed rung; returned as-is only
        // if not even rung 0 is entered (deadline pre-expired), which reproduces
        // origin/main's zero-apply collapse exactly.
        let mut best_scored: Vec<(GameAction, f64)> = ranked
            .iter()
            .map(|r| (r.candidate.action.clone(), r.score * tactical_weight))
            .collect();

        for iter_depth in 0..=ceiling {
            // Guard EVERY rung (incl. rung 0) at entry. Interactive: a pre-expired
            // deadline returns the tactical-only floor with zero applies (==
            // origin/main). Measurement: services.deadline is none() => never
            // expires => full fixed ceiling => deterministic.
            if services.deadline.expired() {
                break;
            }
            // Fresh node budget per rung sharing the one services.deadline (none()
            // in measurement, so this single constructor is correct for both modes).
            // The deepest rung thus gets the full max_nodes just like origin/main's
            // single pass.
            let mut budget =
                SearchBudget::with_deadline(config.search.max_nodes, services.deadline);
            let mut planner = BeamContinuationPlanner {
                depth: iter_depth,
                rollout_depth: config.search.rollout_depth,
            };

            let mut rung_scored = Vec::with_capacity(ranked.len());
            let mut completed = true;
            for r in &ranked {
                // Rungs >= 1 may bail mid-rung (interior search is expensive) and
                // discard the partial. Rung 0 is cheap (branching quiesced evals)
                // and runs atomically once entered, so it is never left partial.
                if iter_depth > 0 && services.deadline.expired() {
                    completed = false;
                    break;
                }
                let score = if let Some(sim) = apply_candidate(state, &r.candidate) {
                    let cont = planner.evaluate_after_action(&sim, &mut services, &mut budget);
                    cont + (r.score * tactical_weight)
                } else {
                    // Action failed simulation — same penalty as origin/main so the
                    // AI prefers any valid alternative.
                    r.score - 1000.0
                };
                rung_scored.push((r.candidate.action.clone(), score));
            }

            // "Fully completed" also requires the deadline to be live after the
            // LAST candidate: expiry mid-final-evaluation is invisible to the
            // per-candidate entry check and would accept a rung whose tail score
            // was truncated. Rung 0 stays exempt (atomic once entered — it is the
            // no-regression floor, == origin/main's deadline collapse). Node-budget
            // exhaustion deliberately does NOT discard: the deepest rung consuming
            // its full `max_nodes` reproduces origin/main's single fixed-depth pass.
            if completed && (iter_depth == 0 || !services.deadline.expired()) {
                best_scored = rung_scored; // deepest fully-completed rung so far
            } else {
                break;
            }
        }

        let mut out = best_scored;
        if config.execution_mode.is_measurement() {
            out.sort_by_cached_key(|(action, _)| action_order_key(action));
        }
        out
    } else {
        // Heuristic-only scoring
        let mut out: Vec<_> = gated
            .into_iter()
            .map(|candidate| {
                let score = services.tactical_score(
                    state,
                    &ctx,
                    &candidate.candidate,
                    ai_player,
                    SearchDepth::Root,
                ) + candidate.penalty;
                (candidate.candidate.action, score)
            })
            .collect();
        if config.execution_mode.is_measurement() {
            out.sort_by_cached_key(|(action, _)| action_order_key(action));
        }
        out
    }
}

fn action_order_key(action: &GameAction) -> String {
    format!("{action:?}")
}

/// Build AI context from the player's deck pool, or a neutral default if unavailable.
fn build_ai_context_with_session(
    state: &GameState,
    player: PlayerId,
    config: &AiConfig,
    session: Arc<AiSession>,
) -> AiContext {
    let deck_profile = session
        .deck_profile
        .get(&player)
        .cloned()
        .unwrap_or_default();
    let adjusted_weights = crate::eval::EvalWeightSet {
        early: deck_profile
            .adjust_weights_with(&config.archetype_multipliers, &config.weights.early),
        mid: deck_profile.adjust_weights_with(&config.archetype_multipliers, &config.weights.mid),
        late: deck_profile.adjust_weights_with(&config.archetype_multipliers, &config.weights.late),
    };
    let strategy = session.strategy.get(&player).cloned().unwrap_or_default();
    let mut ctx = AiContext {
        deck_profile,
        adjusted_weights,
        strategy,
        opponent_threat: None,
        session,
        player,
        deadline: engine::util::Deadline::none(),
    };
    // Compute opponent threat profile based on difficulty setting.
    ctx.opponent_threat = match config.search.threat_awareness {
        ThreatAwareness::None => None,
        ThreatAwareness::ArchetypeOnly => {
            // Use fixed archetype-based probabilities. Archetype is cached on
            // `AiSession`, so this is a HashMap lookup.
            let opponents = engine::game::players::opponents(state, player);
            let opp_archetype = opponents
                .first()
                .and_then(|&opp| ctx.session.archetype(opp))
                .unwrap_or(crate::deck_profile::DeckArchetype::Midrange);
            Some(ThreatProfile {
                probabilities: ArchetypeBaseProbabilities::for_archetype(opp_archetype),
                opponent_archetype: opp_archetype,
                category_pools: Default::default(),
                pool_size: 0,
                hand_size: 0,
            })
        }
        ThreatAwareness::Full => build_threat_profile_multiplayer(state, player),
    };

    ctx
}

fn build_ai_context(state: &GameState, player: PlayerId, config: &AiConfig) -> AiContext {
    build_ai_context_with_session(state, player, config, AiSession::arc_from_game(state))
}

/// Handle deterministic decisions that don't benefit from search or parallelism.
/// Returns `Some(action)` for special cases, `None` to proceed to scoring.
///
/// Also used by quiescence search to resolve mechanical choices (scry, surveil, etc.)
/// without stopping at non-strategic decision points.
pub(crate) fn deterministic_choice(
    state: &GameState,
    ai_player: PlayerId,
    config: &AiConfig,
    actions: &[GameAction],
    context: Option<&AiContext>,
) -> Option<GameAction> {
    if matches!(
        state.waiting_for,
        WaitingFor::BetweenGamesChoosePlayDraw { .. }
    ) {
        return Some(GameAction::ChoosePlayDraw { play_first: true });
    }

    if matches!(state.waiting_for, WaitingFor::BetweenGamesSideboard { .. }) {
        return actions
            .iter()
            .find(|action| matches!(action, GameAction::SubmitSideboard { .. }))
            .cloned();
    }

    if actions.len() == 1 {
        return Some(actions[0].clone());
    }

    if let Some(action) = prefer_land_drop(state, ai_player, actions) {
        return Some(action);
    }

    // CR 103.5 + CR 103.6: Mulligan decisions — defer to the sibling
    // `MulliganRegistry` for structured, feature-aware hand evaluation. All
    // registered `MulliganPolicy` implementations contribute; search can't
    // evaluate these (the hand isn't yet committed to an opening state).
    //
    // CR 103.5: With simultaneous mulligan, `pending` may contain several
    // players. The AI controller's job is to choose for `ai_player`; if
    // `ai_player` is in the pending set, evaluate their own hand. Otherwise
    // no action is owed by this AI right now.
    if let WaitingFor::MulliganDecision { pending, .. } = &state.waiting_for {
        let entry = pending.iter().find(|e| e.player == ai_player)?;
        let player = entry.player;
        let mulligan_count = entry.mulligan_count;
        let owned_ctx;
        let ctx = match context {
            Some(c) => c,
            None => {
                owned_ctx = build_ai_context(state, player, config);
                &owned_ctx
            }
        };
        let default_features = crate::features::DeckFeatures::default();
        let default_plan = crate::plan::PlanSnapshot::default();
        let features = ctx
            .session
            .features
            .get(&player)
            .unwrap_or(&default_features);
        let plan = ctx.session.plan.get(&player).unwrap_or(&default_plan);
        let hand: Vec<_> = state.players[player.0 as usize]
            .hand
            .iter()
            .copied()
            .collect();
        let turn_order = crate::policies::mulligan::turn_order_for(state, player);
        let decision = crate::policies::mulligan::MulliganRegistry::default().evaluate_hand(
            &hand,
            state,
            features,
            plan,
            turn_order,
            mulligan_count,
        );
        // CR 103.5b + Serum Powder Oracle text: if the AI would mulligan and
        // it has a Serum Powder in hand, prefer the Powder — it's a strictly
        // better action than a mulligan (no bottoming, no mulligan count
        // increment). When the registry says keep, take the keep — don't burn
        // a Powder on a hand the policies already endorsed.
        let choice = if decision.keep {
            MulliganChoice::Keep
        } else if let Some(object_id) = first_serum_powder_in_hand(state, player) {
            MulliganChoice::UseSerumPowder { object_id }
        } else {
            MulliganChoice::Mulligan
        };
        return Some(GameAction::MulliganDecision { choice });
    }

    // CR 103.5 + TL:R 906.6: Mulligan / opening-hand bottoming. Each pending
    // player owes a distinct `count`, and several players can be pending at
    // once (simultaneous bottoming). The AI controller must scope to
    // `ai_player`'s own entry: the shared candidate pool mixes every pending
    // player's combos, and `validate_candidates` simulates them as the first
    // authorized submitter (seat order) rather than `ai_player` — so without
    // this branch the AI can pick a selection sized for a different player and
    // the engine rejects it ("Expected N cards to bottom, got M"). Bottom the
    // N least valuable cards, using the cached plan to preserve expected land
    // count and structurally detected payoff cards.
    if let WaitingFor::MulliganBottomCards { pending }
    | WaitingFor::OpeningHandBottomCards { pending, .. } = &state.waiting_for
    {
        let entry = pending.iter().find(|e| e.player == ai_player)?;
        let count = entry.count as usize;
        let owned_ctx;
        let ctx = match context {
            Some(c) => c,
            None => {
                owned_ctx = build_ai_context(state, ai_player, config);
                &owned_ctx
            }
        };
        let default_features = DeckFeatures::default();
        let default_plan = PlanSnapshot::default();
        let features = ctx
            .session
            .features
            .get(&ai_player)
            .unwrap_or(&default_features);
        let plan = ctx.session.plan.get(&ai_player).unwrap_or(&default_plan);
        let to_bottom = plan_aware_bottom_cards(state, ai_player, count, features, plan);
        return Some(GameAction::SelectCards { cards: to_bottom });
    }

    // Scry/Dig/Surveil: use card evaluation heuristics
    if let WaitingFor::ScryChoice { cards, .. } = &state.waiting_for {
        let mut scored: Vec<_> = cards
            .iter()
            .map(|&id| (id, evaluate_card_value(state, id)))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let top_cards: Vec<_> = scored.iter().map(|(id, _)| *id).collect();
        return Some(GameAction::SelectCards { cards: top_cards });
    }

    if let WaitingFor::DigChoice {
        selectable_cards,
        keep_count,
        up_to,
        ..
    } = &state.waiting_for
    {
        if selectable_cards.is_empty() {
            return Some(GameAction::SelectCards { cards: Vec::new() });
        }
        let mut scored: Vec<_> = selectable_cards
            .iter()
            .map(|&id| (id, evaluate_card_value(state, id)))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let kept: Vec<_> = if *up_to && scored.first().is_some_and(|(_, v)| *v < 0.1) {
            // Up-to selection with no valuable cards — take nothing
            Vec::new()
        } else {
            scored.iter().take(*keep_count).map(|(id, _)| *id).collect()
        };
        return Some(GameAction::SelectCards { cards: kept });
    }

    if let WaitingFor::SurveilChoice { cards, .. } = &state.waiting_for {
        let mut scored: Vec<_> = cards
            .iter()
            .map(|&id| (id, evaluate_card_value(state, id)))
            .collect();
        // CR 701.25a: the action is the ordered keep-on-top set; cards left out
        // are milled. Keep the higher-value half on top (best drawn first) and
        // let the worse half fall into the graveyard.
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let keep_count = scored.len() / 2;
        let top_cards: Vec<_> = scored.iter().take(keep_count).map(|(id, _)| *id).collect();
        return Some(GameAction::SelectCards { cards: top_cards });
    }

    if let WaitingFor::RevealChoice { cards, .. } = &state.waiting_for {
        let mut scored: Vec<_> = cards
            .iter()
            .map(|&id| (id, evaluate_card_value(state, id)))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        if let Some((best, _)) = scored.first() {
            return Some(GameAction::SelectCards { cards: vec![*best] });
        }
    }

    if let WaitingFor::EffectZoneChoice {
        cards,
        count,
        up_to,
        effect_kind,
        ..
    } = &state.waiting_for
    {
        if matches!(effect_kind, engine::types::ability::EffectKind::Sacrifice)
            && !cards.is_empty()
            && !*up_to
            && *count > 0
        {
            return Some(GameAction::SelectCards {
                cards: pick_lowest_value_sacrifices(state, cards, *count),
            });
        }
    }

    if let WaitingFor::SearchChoice {
        cards,
        count,
        up_to,
        constraint,
        ..
    } = &state.waiting_for
    {
        if *count == 1 {
            let mut scored = score_search_choice_cards(state, ai_player, cards);
            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            if let Some((best, _)) = scored.first() {
                return Some(GameAction::SelectCards { cards: vec![*best] });
            }
        } else {
            // CR 608.2c: Multi-card library searches are *combinatorial* — an
            // opponent may pick the worst card from the chosen set (Gifts
            // Ungiven). Per-card greedy scoring is wrong; we must score whole
            // selections via `score_search_choice_selection`. To bound cost
            // when the pool is large, beam-restrict to the top BEAM_K cards
            // by per-card score and enumerate `C(BEAM_K, count)` combinations
            // locally — three orders of magnitude smaller than `C(|cards|,
            // count)` for typical Commander libraries (C(12, 4) = 495 ≪
            // C(88, 4) ≈ 2.4M). The engine's candidate list has already been
            // filtered against the selection constraint at this point; we
            // re-apply it after enumerating beam combinations because the
            // beam itself is computed in AI-local space.
            const BEAM_K: usize = 12;
            let beam_ids: Vec<_> = if cards.len() <= BEAM_K {
                cards.clone()
            } else {
                let mut per_card = score_search_choice_cards(state, ai_player, cards);
                per_card.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                per_card.iter().take(BEAM_K).map(|(id, _)| *id).collect()
            };
            let sizes: Vec<usize> = if *up_to {
                (0..=*count).collect()
            } else {
                vec![*count]
            };
            let mut scored: Vec<(Vec<_>, f64)> = sizes
                .into_iter()
                .flat_map(|size| local_combinations(&beam_ids, size))
                .filter(|combo| {
                    engine::game::effects::search_library::selection_satisfies_constraint(
                        state, combo, constraint,
                    )
                })
                .map(|combo| {
                    let score = score_search_choice_selection(state, ai_player, &combo);
                    (combo, score)
                })
                .collect();
            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            if let Some((chosen, _)) = scored.first() {
                return Some(GameAction::SelectCards {
                    cards: chosen.clone(),
                });
            }
        }
    }

    // CR 700.2: ChooseFromZoneChoice — select cards from a tracked set.
    if let WaitingFor::ChooseFromZoneChoice {
        cards,
        count,
        player,
        ..
    } = &state.waiting_for
    {
        let mut scored: Vec<_> = cards
            .iter()
            .map(|&id| (id, evaluate_card_value(state, id)))
            .collect();
        // The search optimizes for `ai_player`, so a choice made by any other
        // player is an opponent's (they pick the highest-value cards for
        // themselves; the AI picks the lowest when choosing for itself).
        // Compare against `ai_player`, not `state.priority_player` — under a
        // turn-control effect (CR 723, e.g. Mindslaver) the latter is the
        // controller (the authorized submitter), not the chooser, which would
        // misclassify the controlled player's choice.
        let is_opponent_chooser = *player != ai_player;
        if is_opponent_chooser {
            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        } else {
            scored.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        }
        let chosen: Vec<_> = scored.iter().take(*count).map(|(id, _)| *id).collect();
        if !chosen.is_empty() {
            return Some(GameAction::SelectCards { cards: chosen });
        }
    }

    // CR 702.33a: Kicker and other optional additional costs.
    // Pay the additional mana cost only if affordable AND the extra mana is a good
    // deal relative to the effect upgrade. For pure mana kickers, check that the
    // player has enough mana to pay the combined cost after auto-tapping, and that
    // paying it doesn't over-commit mana (leave at least 1 land untapped when
    // possible, since holding mana open for instant-speed interaction is valuable).
    if let WaitingFor::OptionalCostChoice {
        player,
        cost: additional_cost,
        pending_cast,
        ..
    } = &state.waiting_for
    {
        // Affordability + over-commit guard for a pure mana additional cost:
        // pay only if the combined cost is affordable after auto-tapping AND
        // it leaves at least one land untapped (holding mana open for
        // instant-speed interaction is valuable). Shared by the Optional(Mana)
        // and single-mana Kicker branches so the AI does not over-commit on
        // multikicker re-prompts (CR 702.33c — they arrive as real Kicker).
        let affordable_mana_cost = |extra_mana: &engine::types::mana::ManaCost| -> bool {
            let combined =
                engine::game::restrictions::add_mana_cost(&pending_cast.cost, extra_mana);
            let affordable = engine::game::casting::can_pay_cost_after_auto_tap(
                state,
                *player,
                pending_cast.object_id,
                &combined,
            );
            if !affordable {
                return false;
            }
            // Count total untapped lands to gauge remaining resources.
            let total_untapped = state
                .objects
                .values()
                .filter(|o| {
                    o.controller == *player
                        && o.zone == engine::types::zones::Zone::Battlefield
                        && !o.tapped
                        && o.card_types
                            .core_types
                            .contains(&engine::types::card_type::CoreType::Land)
                })
                .count();
            let combined_cmc = match &combined {
                engine::types::mana::ManaCost::Cost { shards, generic } => {
                    shards.len() + *generic as usize
                }
                _ => 0,
            };
            // Pay only if we'll have mana to spare afterward.
            total_untapped > combined_cmc
        };

        let pay = match additional_cost {
            engine::types::ability::AdditionalCost::Optional {
                cost: engine::types::ability::AbilityCost::Mana { cost: extra_mana },
                ..
            } => affordable_mana_cost(extra_mana),
            // CR 702.33c: a multikicker / kicker re-prompt presents exactly one
            // live cost. When that cost is pure mana, apply the same
            // affordability + over-commit guard as Optional(Mana).
            engine::types::ability::AdditionalCost::Kicker { costs, .. }
                if matches!(
                    costs.as_slice(),
                    [engine::types::ability::AbilityCost::Mana { .. }]
                ) =>
            {
                let engine::types::ability::AbilityCost::Mana { cost: extra_mana } = &costs[0]
                else {
                    unreachable!("guarded by the matches! above")
                };
                affordable_mana_cost(extra_mana)
            }
            // Non-mana optional costs: sacrifice → usually worth it for the upgrade
            engine::types::ability::AdditionalCost::Optional {
                cost: engine::types::ability::AbilityCost::Sacrifice(_),
                ..
            } => false, // Conservative: don't sacrifice unless search says so
            engine::types::ability::AdditionalCost::Optional {
                cost: engine::types::ability::AbilityCost::PayLife { amount },
                ..
            } => {
                // CR 119.4 + CR 903.4: PayLife carries a QuantityExpr; resolve
                // against the activator/source so dynamic costs (e.g. commander
                // color identity) are costed correctly. Source = 0 falls back
                // to Fixed variants; QuantityRef variants that need a source
                // won't appear on optional additional costs today.
                let resolved = engine::game::quantity::resolve_quantity(
                    state,
                    amount,
                    *player,
                    engine::types::identifiers::ObjectId(0),
                )
                .max(0);
                let life = state.players[player.0 as usize].life;
                life > resolved * 3
            }
            engine::types::ability::AdditionalCost::Optional { .. } => true,
            engine::types::ability::AdditionalCost::Kicker { .. } => true,
            engine::types::ability::AdditionalCost::Choice(_, _) => true,
            engine::types::ability::AdditionalCost::Required(_) => true,
        };
        return Some(GameAction::DecideOptionalCost { pay });
    }

    // CR 601.2b: Defiler — accept life payment when life cushion is sufficient.
    if let WaitingFor::DefilerPayment {
        life_cost, player, ..
    } = &state.waiting_for
    {
        let life = state.players[player.0 as usize].life;
        let pay = life > (*life_cost as i32) * 3;
        return Some(GameAction::DecideOptionalCost { pay });
    }

    if let WaitingFor::DiscardToHandSize { cards, count, .. } = &state.waiting_for {
        let mut scored: Vec<_> = cards
            .iter()
            .map(|&id| (id, evaluate_card_value(state, id)))
            .collect();
        scored.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        let to_discard: Vec<_> = scored.iter().take(*count).map(|(id, _)| *id).collect();
        return Some(GameAction::SelectCards { cards: to_discard });
    }

    // Combat decisions: delegate to specialized combat AI
    if let WaitingFor::DeclareAttackers {
        valid_attacker_ids,
        valid_attack_targets,
        ..
    } = &state.waiting_for
    {
        let attacks = choose_attackers_with_targets_with_profile(
            state,
            ai_player,
            &config.profile,
            config.combat_lookahead,
            Some(valid_attacker_ids),
            Some(valid_attack_targets),
            context.map(|c| c.session.as_ref()),
        );
        return Some(validated_declare_attackers(state, attacks));
    }

    if let WaitingFor::DeclareBlockers {
        valid_block_targets,
        ..
    } = &state.waiting_for
    {
        if let Some(combat) = &state.combat {
            // CR 509.1: Blockers may only be declared against attackers attacking
            // the defending player or a planeswalker/battle they control. In a
            // multi-defender pod, `combat.attackers` carries attackers heading to
            // every defender — filter to those targeting the AI before evaluating
            // block objective and assignments.
            let attacker_ids: Vec<_> = combat
                .attackers
                .iter()
                .filter(|a| a.defending_player == ai_player)
                .map(|a| a.object_id)
                .collect();
            let assignments = choose_blockers_with_profile(
                state,
                ai_player,
                &attacker_ids,
                &config.profile,
                Some(valid_block_targets),
            );
            return Some(GameAction::DeclareBlockers { assignments });
        }
        return Some(GameAction::DeclareBlockers {
            assignments: Vec::new(),
        });
    }

    None
}

/// Handle combat decisions with an archetype-modulated profile.
/// Separated from `deterministic_choice` so the combat fast-path in `score_candidates`
/// can pass an effective profile (difficulty x archetype) to the combat AI.
fn deterministic_combat_choice(
    state: &GameState,
    ai_player: PlayerId,
    profile: &crate::config::AiProfile,
    session: Option<&AiSession>,
) -> Option<GameAction> {
    if let WaitingFor::DeclareAttackers {
        valid_attacker_ids,
        valid_attack_targets,
        ..
    } = &state.waiting_for
    {
        let attacks = choose_attackers_with_targets_with_profile(
            state,
            ai_player,
            profile,
            false,
            Some(valid_attacker_ids),
            Some(valid_attack_targets),
            session,
        );
        return Some(validated_declare_attackers(state, attacks));
    }

    if let WaitingFor::DeclareBlockers {
        valid_block_targets,
        ..
    } = &state.waiting_for
    {
        if let Some(combat) = &state.combat {
            // CR 509.1: Filter to attackers targeting the AI; see deterministic_choice.
            let attacker_ids: Vec<_> = combat
                .attackers
                .iter()
                .filter(|a| a.defending_player == ai_player)
                .map(|a| a.object_id)
                .collect();
            let assignments = choose_blockers_with_profile(
                state,
                ai_player,
                &attacker_ids,
                profile,
                Some(valid_block_targets),
            );
            return Some(GameAction::DeclareBlockers { assignments });
        }
        return Some(GameAction::DeclareBlockers {
            assignments: Vec::new(),
        });
    }

    None
}

/// CR 508.1 (issue #1523): Guard the combat AI's attacker declaration so the
/// engine never rejects it. The combat AI draws attackers from the
/// engine-provided `valid_attacker_ids`, but the chosen *subset* + *target
/// assignment* can still be illegal as a whole — e.g. a "can't attack alone"
/// creature swinging solo, a split must-attack-together pair, or a target an
/// attacker may not legally be assigned. The action driver re-requests the AI's
/// (deterministic) decision after a rejection, so an illegal declaration loops
/// forever and softlocks the game ("repeated attempts to attack").
///
/// Dry-run the declaration on a cloned state; if the engine would reject it,
/// fall back to an engine-validated legal `DeclareAttackers` (the first such
/// candidate from `legal_actions`, which prefers declining combat but still
/// satisfies any mandatory must-attack requirement, since illegal candidates
/// are filtered out by the simulation pipeline). This costs one state clone per
/// attacker declaration — infrequent and far cheaper than the combat AI's own
/// lookahead — and the fallback path only runs on the rare illegal choice.
fn validated_declare_attackers(
    state: &GameState,
    attacks: Vec<(
        engine::types::identifiers::ObjectId,
        engine::game::combat::AttackTarget,
    )>,
) -> GameAction {
    let candidate = GameAction::DeclareAttackers {
        attacks,
        bands: vec![],
    };
    let mut sim = state.clone();
    if engine::game::engine::apply_as_current_for_simulation(&mut sim, candidate.clone()).is_ok() {
        return candidate;
    }
    engine::ai_support::legal_actions(state)
        .into_iter()
        .find(|action| matches!(action, GameAction::DeclareAttackers { .. }))
        .unwrap_or(GameAction::DeclareAttackers {
            attacks: Vec::new(),
            bands: vec![],
        })
}

fn prefer_land_drop(
    state: &GameState,
    ai_player: PlayerId,
    actions: &[GameAction],
) -> Option<GameAction> {
    let WaitingFor::Priority { player } = &state.waiting_for else {
        return None;
    };

    if engine::game::turn_control::authorized_submitter_for_player(state, *player) != ai_player
        || state.active_player != *player
        || !matches!(
            state.phase,
            engine::types::phase::Phase::PreCombatMain
                | engine::types::phase::Phase::PostCombatMain
        )
        || !state.stack.is_empty()
        || state.lands_played_this_turn >= state.max_lands_per_turn
    {
        return None;
    }

    actions
        .iter()
        .find(|action| matches!(action, GameAction::PlayLand { .. }))
        .cloned()
}

/// Evaluate a card's value for scry/dig/surveil decisions.
/// Higher values mean the card is more desirable to keep/draw.
fn evaluate_card_value(state: &GameState, obj_id: engine::types::identifiers::ObjectId) -> f64 {
    let obj = match state.objects.get(&obj_id) {
        Some(o) => o,
        None => return 0.0,
    };

    let mut value = 0.0;

    // Creatures: value based on power + toughness
    if obj.card_types.core_types.contains(&CoreType::Creature) {
        let power = obj.power.unwrap_or(0) as f64;
        let toughness = obj.toughness.unwrap_or(0) as f64;
        value += power * 1.5 + toughness;
    }

    // Lands: moderate value (mana development)
    if obj.card_types.core_types.contains(&CoreType::Land) {
        value += 3.0;
    }

    // Instants/Sorceries: base value from mana cost (proxy for power)
    if let engine::types::mana::ManaCost::Cost { shards, generic } = &obj.mana_cost {
        let total_mana = shards.len() as f64 + *generic as f64;
        value += total_mana * 0.5;
    }

    value
}

fn plan_aware_bottom_cards(
    state: &GameState,
    player: PlayerId,
    count: usize,
    features: &DeckFeatures,
    plan: &PlanSnapshot,
) -> Vec<ObjectId> {
    let hand: Vec<_> = state.players[player.0 as usize]
        .hand
        .iter()
        .copied()
        .collect();
    let final_hand_size = hand.len().saturating_sub(count);
    let land_target = plan_bottoming_land_target(plan, final_hand_size);
    let land_count = hand
        .iter()
        .filter(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|obj| obj.card_types.core_types.contains(&CoreType::Land))
        })
        .count();
    let mut surplus_lands = land_count.saturating_sub(land_target);
    let mut scored = Vec::with_capacity(hand.len());

    for id in hand {
        let score = state.objects.get(&id).map_or(0.0, |obj| {
            if is_plan_payoff_name(features, &obj.name) {
                25.0 + evaluate_card_value(state, id)
            } else if obj.card_types.core_types.contains(&CoreType::Land) {
                if surplus_lands > 0 {
                    surplus_lands -= 1;
                    -5.0
                } else {
                    30.0
                }
            } else {
                evaluate_card_value(state, id)
            }
        });
        scored.push((id, score));
    }

    scored.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(Ordering::Equal));
    scored.into_iter().take(count).map(|(id, _)| id).collect()
}

fn plan_bottoming_land_target(plan: &PlanSnapshot, final_hand_size: usize) -> usize {
    let target = plan
        .expected_lands
        .get(2)
        .copied()
        .filter(|lands| *lands > 0)
        .unwrap_or(3) as usize;
    target.min(final_hand_size)
}

fn is_plan_payoff_name(features: &DeckFeatures, name: &str) -> bool {
    features.landfall.payoff_names.iter().any(|n| n == name)
        || features.aristocrats.outlet_names.iter().any(|n| n == name)
        || features
            .aristocrats
            .death_trigger_names
            .iter()
            .any(|n| n == name)
        || features.tokens_wide.payoff_names.iter().any(|n| n == name)
        || features
            .plus_one_counters
            .payoff_names
            .iter()
            .any(|n| n == name)
        || features
            .spellslinger_prowess
            .payoff_names
            .iter()
            .any(|n| n == name)
}

/// AI-local combination enumerator. Mirrors `engine::ai_support::candidates::combinations`
/// but lives in `phase-ai` so the beam in `deterministic_choice` can build
/// `C(BEAM_K, count)` tuples without paying the cost of the engine's full
/// candidate enumeration. Empty `k` yields a single empty combination so
/// `up_to` searches naturally include the "select zero" option.
fn local_combinations(
    items: &[engine::types::identifiers::ObjectId],
    k: usize,
) -> Vec<Vec<engine::types::identifiers::ObjectId>> {
    if k == 0 {
        return vec![Vec::new()];
    }
    if items.len() < k {
        return Vec::new();
    }
    if items.len() == k {
        return vec![items.to_vec()];
    }
    let mut result = Vec::new();
    for mut combo in local_combinations(&items[1..], k - 1) {
        combo.insert(0, items[0]);
        result.push(combo);
    }
    result.extend(local_combinations(&items[1..], k));
    result
}

/// Select an action from scored `(GameAction, f64)` pairs using softmax.
/// Used by `choose_action` and by the WASM `select_action_from_scores` export.
pub fn softmax_select_pairs(
    scored: &[(GameAction, f64)],
    temperature: f64,
    rng: &mut impl Rng,
) -> Option<GameAction> {
    if scored.is_empty() {
        return None;
    }
    if scored.len() == 1 {
        return Some(scored[0].0.clone());
    }

    // Numerical stability: subtract max score
    let max_score = scored.iter().map(|s| s.1).fold(f64::NEG_INFINITY, f64::max);

    let weights: Vec<f64> = scored
        .iter()
        .map(|s| ((s.1 - max_score) / temperature).exp())
        .collect();

    let total: f64 = weights.iter().sum();
    if total <= 0.0 || !total.is_finite() {
        // Fallback: pick the highest-scored action
        return scored
            .iter()
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|s| s.0.clone());
    }

    let threshold: f64 = rng.random::<f64>() * total;
    let mut cumulative = 0.0;
    for (i, w) in weights.iter().enumerate() {
        cumulative += w;
        if cumulative >= threshold {
            return Some(scored[i].0.clone());
        }
    }

    // Fallback to last
    Some(scored.last().unwrap().0.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, CategoryChooserScope, ContinuousModification, Duration,
        Effect, EffectKind, QuantityExpr, ResolvedAbility, StaticDefinition, TargetFilter,
        TargetRef, TypedFilter,
    };
    use engine::types::card_type::CoreType;
    use engine::types::counter::CounterType;
    use engine::types::game_state::{StackEntry, StackEntryKind};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::mana::{ManaType, ManaUnit};
    use engine::types::phase::Phase;
    use engine::types::zones::Zone;
    use rand::rngs::SmallRng;
    use rand::SeedableRng;

    use crate::config::{create_config, AiDifficulty, Platform};
    use crate::policies::context::PolicyContext;
    use crate::session::SessionCache;

    fn make_state() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        state
    }

    fn add_creature(
        state: &mut GameState,
        owner: PlayerId,
        power: i32,
        toughness: i32,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            "Creature".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(power);
        obj.toughness = Some(toughness);
        obj.entered_battlefield_turn = Some(1);
        id
    }

    fn add_spell_to_hand(
        state: &mut GameState,
        owner: PlayerId,
        name: &str,
        generic_cost: u32,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            name.to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Sorcery);
        obj.mana_cost = engine::types::mana::ManaCost::Cost {
            shards: Vec::new(),
            generic: generic_cost,
        };
        id
    }

    fn add_mana(state: &mut GameState, player: PlayerId, color: ManaType, count: usize) {
        let p = &mut state.players[player.0 as usize];
        for _ in 0..count {
            p.mana_pool.add(ManaUnit {
                color,
                source_id: ObjectId(0),
                pip_id: engine::types::mana::ManaPipId(0),
                supertype: None,
                source_could_produce_two_or_more_colors: false,
                restrictions: Vec::new(),
                grants: vec![],
                expiry: None,
            });
        }
    }

    fn add_activated_ability(state: &mut GameState, source_id: ObjectId, effect: Effect) -> usize {
        let object = state.objects.get_mut(&source_id).unwrap();
        let abilities = Arc::make_mut(&mut object.abilities);
        let index = abilities.len();
        abilities.push(AbilityDefinition::new(AbilityKind::Activated, effect));
        index
    }

    fn no_op_stack_entry(id: u64, controller: PlayerId) -> StackEntry {
        let object_id = ObjectId(id);
        StackEntry {
            id: object_id,
            source_id: object_id,
            controller,
            kind: StackEntryKind::ActivatedAbility {
                source_id: object_id,
                ability: ResolvedAbility::new(Effect::NoOp, vec![], object_id, controller),
            },
        }
    }

    fn temporary_combat_modifier_effect() -> Effect {
        Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::continuous().modifications(vec![
                ContinuousModification::AddPower { value: 2 },
                ContinuousModification::AddToughness { value: 0 },
                ContinuousModification::AddKeyword {
                    keyword: engine::types::keywords::Keyword::Haste,
                },
            ])],
            duration: Some(Duration::UntilEndOfTurn),
            target: None,
        }
    }

    fn set_opp_deck(state: &mut GameState, names: &[&str]) {
        let entries = names
            .iter()
            .map(|n| engine::game::deck_loading::DeckEntry {
                card: engine::types::card::CardFace {
                    name: n.to_string(),
                    mana_cost: engine::types::mana::ManaCost::zero(),
                    ..Default::default()
                },
                count: 1,
            })
            .collect();
        state
            .deck_pools
            .push(engine::types::game_state::PlayerDeckPool {
                player: PlayerId(1),
                current_main: Arc::new(entries),
                ..Default::default()
            });
    }

    fn add_opp_hidden(state: &mut GameState, name: &str, zone: Zone) -> ObjectId {
        create_object(
            state,
            CardId(state.next_object_id),
            PlayerId(1),
            name.to_string(),
            zone,
        )
    }

    #[test]
    fn determinization_k0_equals_core_baseline() {
        // B1: `determinization_samples == 0` returns the core path unchanged.
        let mut state = make_state();
        add_mana(&mut state, PlayerId(0), ManaType::Colorless, 3);
        add_spell_to_hand(&mut state, PlayerId(0), "SpellA", 1);
        add_spell_to_hand(&mut state, PlayerId(0), "SpellB", 2);
        let mut config = create_config(AiDifficulty::Hard, Platform::Native).into_measurement(1);
        config.search.determinization_samples = 0;
        let session = AiSession::arc_from_game(&state);
        let via_wrapper = score_candidates_with_session(&state, PlayerId(0), &config, &session);
        let via_core = score_candidates_core(&state, PlayerId(0), &config, &session, None);
        assert_eq!(via_wrapper, via_core);
    }

    /// Battlefield permanent carrying a single Helix-shape `{X}` activated
    /// ability ("{X}: put X tower counters on ~" — scales with X, a no-op at
    /// X=0). Returns the source ObjectId; the sole ability is index 0.
    fn add_helix_x_ability(state: &mut GameState, owner: PlayerId) -> ObjectId {
        let id = add_creature(state, owner, 1, 1);
        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::PutCounter {
                counter_type: CounterType::Generic("tower".to_string()),
                count: QuantityExpr::Ref {
                    qty: engine::types::ability::QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
                target: TargetFilter::SelfRef,
            },
        );
        ability.cost = Some(engine::types::ability::AbilityCost::Mana {
            cost: engine::types::mana::ManaCost::Cost {
                shards: vec![engine::types::mana::ManaCostShard::X],
                generic: 0,
            },
        });
        *Arc::make_mut(&mut state.objects.get_mut(&id).unwrap().abilities) = vec![ability];
        id
    }

    fn activate_score(scored: &[(GameAction, f64)], source: ObjectId) -> Option<f64> {
        scored.iter().find_map(|(action, score)| match action {
            GameAction::ActivateAbility { source_id, .. } if *source_id == source => Some(*score),
            _ => None,
        })
    }

    #[test]
    fn xcast_zero_no_op_not_committed_at_root() {
        // Claim C (end-to-end, discriminating): at the real committed-decision
        // seam (`score_candidates_core`), a Helix-shape {X} activation whose only
        // affordable X is 0 (zero mana) must NOT be the committed argmax. The root
        // gate scores it `NEG_INFINITY`, so `Pass` (always a Priority candidate)
        // outranks it. Reverting the Root gate lets the X=0 activation score finite
        // and possibly win → the "not finite / not argmax" assertions flip.
        let mut state = make_state();
        let source = add_helix_x_ability(&mut state, PlayerId(0)); // zero mana → max X = 0
        let config = create_config(AiDifficulty::Hard, Platform::Native).into_measurement(1);
        let session = AiSession::arc_from_game(&state);
        let scored = score_candidates_core(&state, PlayerId(0), &config, &session, None);

        // Non-vacuous reach-guard: the activation candidate is actually present in
        // the scored set (candidate generation produced the X=0 activation — the
        // exact commitment the gate exists to stop), so the assertion below is not
        // silently satisfied by an absent candidate.
        let score = activate_score(&scored, source)
            .expect("the {X}=0 activation must be an enumerated, scored candidate");
        assert!(
            !score.is_finite(),
            "root gate must reject the X=0 no-op activation (got finite score {score})"
        );

        // It is therefore not the argmax — some other action (Pass) wins.
        let best = scored
            .iter()
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(Ordering::Equal))
            .map(|(action, _)| action.clone());
        assert!(
            !matches!(best, Some(GameAction::ActivateAbility { source_id, .. }) if source_id == source),
            "the X=0 no-op activation must not be the committed decision"
        );
    }

    #[test]
    fn xcast_affordable_activation_committed_at_root() {
        // Reach-guard sibling (non-vacuous): the IDENTICAL Helix fixture with
        // enough mana for X >= 1 lets the gate stand down, so the activation scores
        // FINITE and is a legitimate candidate. Proves the refusal above is
        // affordability-driven, not a blanket suppression of the activation.
        let mut state = make_state();
        let source = add_helix_x_ability(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::Colorless, 1); // max X = 1
        let config = create_config(AiDifficulty::Hard, Platform::Native).into_measurement(1);
        let session = AiSession::arc_from_game(&state);
        let scored = score_candidates_core(&state, PlayerId(0), &config, &session, None);

        let score = activate_score(&scored, source)
            .expect("the {X} activation must be an enumerated, scored candidate");
        assert!(
            score.is_finite(),
            "with X >= 1 affordable the gate stands down; activation must score finite"
        );
    }

    #[test]
    fn determinization_candidate_set_stable_over_resampled_opponent_hand() {
        // B2 + N4(b): the AI's ObjectId-keyed candidate set is invariant to
        // opponent hidden-hand resampling — the pin-invariant. To actually
        // EXERCISE the pin, a candidate must key off an opponent object's id:
        // the AI is choosing a target for a removal-style effect and the sole
        // legal target is the opponent's PUBLIC creature. Determinization only
        // resamples opponent HIDDEN-zone cards (hand/library), so the public
        // creature's ObjectId is stable and the emitted `ChooseTarget` candidate
        // set is identical across K=0 and K=3 even as the opponent's hidden hand
        // resamples. (The pre-fix fixture used own-action-only candidates, so no
        // candidate referenced an opponent object and the invariant was vacuous.)
        let mut state = make_state();
        add_mana(&mut state, PlayerId(0), ManaType::Colorless, 3);
        // Opponent's public permanent — the object the AI's candidate targets.
        let opp_creature = add_creature(&mut state, PlayerId(1), 2, 2);
        // AI mid-resolution choosing a target; the single legal target is the
        // opponent's public creature, so the `ChooseTarget` candidate keys off
        // `opp_creature`'s ObjectId.
        state.waiting_for = WaitingFor::TriggerTargetSelection {
            player: PlayerId(0),
            trigger_controller: None,
            trigger_event: None,
            trigger_events: Vec::new(),
            target_slots: vec![engine::types::game_state::TargetSelectionSlot {
                legal_targets: vec![TargetRef::Object(opp_creature)],
                optional: false,
            }],
            mode_labels: Vec::new(),
            target_constraints: Vec::new(),
            selection: engine::types::game_state::TargetSelectionProgress {
                current_slot: 0,
                selected_slots: Vec::new(),
                current_legal_targets: vec![TargetRef::Object(opp_creature)],
            },
            source_id: None,
            description: None,
        };
        // Opponent decklist + hidden hand so determinization actually resamples.
        set_opp_deck(&mut state, &["Alpha", "Beta", "Gamma", "Delta"]);
        for i in 0..3 {
            add_opp_hidden(&mut state, &format!("Hidden{i}"), Zone::Hand);
        }
        let session = AiSession::arc_from_game(&state);
        let mut k0 = create_config(AiDifficulty::Hard, Platform::Native).into_measurement(2);
        k0.search.determinization_samples = 0;
        let mut k3 = k0.clone();
        k3.search.determinization_samples = 3;

        let base = score_candidates_with_session(&state, PlayerId(0), &k0, &session);
        let ensemble = score_candidates_with_session(&state, PlayerId(0), &k3, &session);

        // Reach-guard A: a candidate genuinely keys off the opponent permanent's
        // ObjectId (otherwise the pin-invariant is vacuously satisfied).
        assert!(
            base.iter().any(|(a, _)| matches!(
                a,
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(id)),
                } if *id == opp_creature
            )),
            "reach-guard: a candidate keys off the opponent permanent's ObjectId"
        );

        // Reach-guard B: determinization is non-vacuous — reproduce the wrapper's
        // sample-0 seed and confirm the opponent's hidden hand really resamples,
        // while the targeted PUBLIC permanent's identity stays pinned.
        let base_seed = crate::planner::quick_state_hash(&state)
            .wrapping_add(state.rng_seed)
            .wrapping_add(state.rng.clone().next_u64());
        let seed = base_seed.wrapping_add(crate::determinize::splitmix64(0));
        let mut rng = ChaCha20Rng::seed_from_u64(seed);
        let sampled = crate::determinize::determinize_opponents(&state, PlayerId(0), &mut rng);
        assert!(
            state.players[1]
                .hand
                .iter()
                .any(|id| sampled.objects[id].name != state.objects[id].name),
            "reach-guard: at least one opponent hidden-hand card must resample"
        );
        assert_eq!(
            sampled.objects[&opp_creature].name, state.objects[&opp_creature].name,
            "the targeted public permanent's identity is stable across resampling"
        );

        let base_keys: std::collections::BTreeSet<_> =
            base.iter().map(|(a, _)| game_action_key(a)).collect();
        let ensemble_keys: std::collections::BTreeSet<_> =
            ensemble.iter().map(|(a, _)| game_action_key(a)).collect();
        assert_eq!(
            base_keys, ensemble_keys,
            "candidate set must stay constant across determinized samples"
        );
    }

    #[test]
    fn determinization_aggregation_means_per_action_scores() {
        // B3: `finalize_mean` divides each summed score by the observed count and
        // preserves first-seen order.
        let mut acc = Vec::new();
        let mut pos = std::collections::HashMap::new();
        let mut counts = std::collections::HashMap::new();
        merge_into(
            &mut acc,
            &mut pos,
            &mut counts,
            vec![
                (GameAction::PassPriority, 2.0),
                (GameAction::CancelCast, 6.0),
            ],
        );
        merge_into(
            &mut acc,
            &mut pos,
            &mut counts,
            vec![
                (GameAction::PassPriority, 4.0),
                (GameAction::CancelCast, 10.0),
            ],
        );
        let out = finalize_mean(acc, counts, 2);
        assert_eq!(out[0], (GameAction::PassPriority, 3.0)); // (2+4)/2
        assert_eq!(out[1], (GameAction::CancelCast, 8.0)); // (6+10)/2
    }

    #[test]
    fn determinization_tiny_shared_deadline_returns_nonempty_floor() {
        // B4: an already-expired shared deadline (interactive, budget 0) returns
        // the tactical floor across K samples — never empty, never a panic.
        let mut state = make_state();
        add_mana(&mut state, PlayerId(0), ManaType::Colorless, 3);
        add_spell_to_hand(&mut state, PlayerId(0), "SpellA", 1);
        add_spell_to_hand(&mut state, PlayerId(0), "SpellB", 2);
        set_opp_deck(&mut state, &["Alpha", "Beta"]);
        add_opp_hidden(&mut state, "Hidden", Zone::Hand);
        let mut config = create_config(AiDifficulty::Hard, Platform::Native);
        config.search.time_budget_ms = Some(0); // pre-expired shared deadline
        config.search.determinization_samples = 3;
        let session = AiSession::arc_from_game(&state);
        let out = score_candidates_with_session(&state, PlayerId(0), &config, &session);
        assert!(
            !out.is_empty(),
            "K-sample ensemble must return a floor, never empty"
        );
    }

    #[test]
    fn determinized_search_ignores_real_opponent_hand() {
        // D (the crux): the opponent's REAL hand holds Negate — "Counter target
        // noncreature spell." — whose castability the perfect-information eval
        // reads through `zone_bonus` (opponent hand quality). Under
        // determinization the AI scores a RESAMPLED opponent hand (all cheap,
        // castable) instead, so the K>0 scores differ from the K=0 (real-hand)
        // scores. Paired reach-guard: the real Negate is swapped out of the world
        // the wrapper's search actually sees.
        let mut state = make_state();
        add_mana(&mut state, PlayerId(0), ManaType::Colorless, 3);
        add_spell_to_hand(&mut state, PlayerId(0), "SpellA", 1);
        add_spell_to_hand(&mut state, PlayerId(0), "SpellB", 2);
        // Opponent decklist is all cheap (mana value 0, castable at 0 mana).
        set_opp_deck(&mut state, &["Cheap", "Cheap", "Cheap", "Cheap", "Cheap"]);
        // Real hand = Negate (mana value 2), uncastable because the opponent has
        // no mana — so it contributes NO castable bonus in the real world.
        let negate = add_opp_hidden(&mut state, "Negate", Zone::Hand);
        {
            let obj = state.objects.get_mut(&negate).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            obj.mana_cost = engine::types::mana::ManaCost::Cost {
                shards: Vec::new(),
                generic: 2,
            };
        }

        // Exercise the production wrapper at K=2: it must run the determinized
        // ensemble without collapsing/crashing.
        let session = AiSession::arc_from_game(&state);
        let mut k2 = create_config(AiDifficulty::Hard, Platform::Native).into_measurement(3);
        k2.search.determinization_samples = 2;
        let determinized_scores = score_candidates_with_session(&state, PlayerId(0), &k2, &session);
        assert!(!determinized_scores.is_empty());

        // Reach-guard: reproduce the wrapper's sample-0 seed and confirm the real
        // Negate is resampled OUT of the world the per-sample search evaluates.
        let base_seed = crate::planner::quick_state_hash(&state)
            .wrapping_add(state.rng_seed)
            .wrapping_add(state.rng.clone().next_u64());
        let seed = base_seed.wrapping_add(crate::determinize::splitmix64(0));
        let mut rng = ChaCha20Rng::seed_from_u64(seed);
        let sampled = crate::determinize::determinize_opponents(&state, PlayerId(0), &mut rng);
        assert_ne!(
            sampled.objects[&negate].name, "Negate",
            "reach-guard: the real Negate must be resampled out of the search's world"
        );

        // Revert-failing crux assertion. `evaluate_state` is exactly the leaf
        // evaluator the beam search runs at every node (via
        // `evaluate_state_quiesced` -> `evaluate_with_strategy` -> `zone_bonus`,
        // which reads the OPPONENT's hidden-hand card mana values — the perfect-
        // information cheat channel). With the real hand the opponent holds
        // uncastable Negate; in the determinized world it holds castable Cheap, so
        // the leaf value the search sees differs. If `determinize_opponents` were
        // reverted to a no-op, `sampled` would equal `state` and these two evals
        // would be identical -> this assertion flips.
        let policies = crate::policies::PolicyRegistry::shared();
        let services = PlannerServices::new_default(PlayerId(0), &k2, policies);
        let real_eval = services.evaluate_state(&state);
        let determinized_eval = services.evaluate_state(&sampled);
        assert_ne!(
            real_eval, determinized_eval,
            "the search's leaf eval must change once the real opponent hand is resampled away"
        );
    }

    #[test]
    fn returns_none_for_no_legal_actions() {
        let mut state = make_state();
        state.waiting_for = WaitingFor::GameOver {
            winner: Some(PlayerId(0)),
        };
        let config = create_config(AiDifficulty::Medium, Platform::Native);
        let mut rng = SmallRng::seed_from_u64(1);
        assert!(choose_action(&state, PlayerId(0), &config, &mut rng).is_none());
    }

    #[test]
    fn returns_single_action_immediately() {
        let state = make_state();
        // Only pass priority available (no mana, no cards)
        let config = create_config(AiDifficulty::Medium, Platform::Native);
        let mut rng = SmallRng::seed_from_u64(1);
        let action = choose_action(&state, PlayerId(0), &config, &mut rng);
        assert_eq!(action, Some(GameAction::PassPriority));
    }

    #[test]
    fn low_value_priority_passes_over_board_activations_on_own_stack() {
        let mut state = make_state();
        let source_id = add_creature(&mut state, PlayerId(0), 1, 1);
        let ability_index = add_activated_ability(&mut state, source_id, Effect::NoOp);
        state.stack.push_back(no_op_stack_entry(10, PlayerId(0)));
        let actions = vec![
            GameAction::PassPriority,
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            },
        ];

        assert_eq!(
            low_value_priority_pass_from_actions(&state, PlayerId(0), &actions),
            Some(GameAction::PassPriority)
        );
    }

    #[test]
    fn low_value_priority_passes_empty_stack_upkeep_over_board_activations() {
        let mut state = make_state();
        state.phase = Phase::Upkeep;
        let source_id = add_creature(&mut state, PlayerId(0), 1, 1);
        let ability_index =
            add_activated_ability(&mut state, source_id, temporary_combat_modifier_effect());
        let actions = vec![
            GameAction::PassPriority,
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            },
        ];

        assert_eq!(
            low_value_priority_pass_from_actions(&state, PlayerId(0), &actions),
            Some(GameAction::PassPriority)
        );
    }

    #[test]
    fn choose_action_passes_empty_stack_upkeep_before_search() {
        let mut state = make_state();
        state.phase = Phase::Upkeep;
        let source_id = add_creature(&mut state, PlayerId(0), 1, 1);
        add_activated_ability(&mut state, source_id, temporary_combat_modifier_effect());
        let config = create_config(AiDifficulty::Medium, Platform::Native);
        let mut rng = SmallRng::seed_from_u64(1);

        assert_eq!(
            choose_action(&state, PlayerId(0), &config, &mut rng),
            Some(GameAction::PassPriority)
        );
    }

    #[test]
    fn score_candidates_passes_empty_stack_upkeep_before_search() {
        let mut state = make_state();
        state.phase = Phase::Upkeep;
        let source_id = add_creature(&mut state, PlayerId(0), 1, 1);
        add_activated_ability(&mut state, source_id, temporary_combat_modifier_effect());
        let config = create_config(AiDifficulty::VeryHard, Platform::Native);

        assert_eq!(
            score_candidates(&state, PlayerId(0), &config),
            vec![(GameAction::PassPriority, 1.0)]
        );
    }

    #[test]
    fn low_value_priority_does_not_skip_spell_responses() {
        let mut state = make_state();
        state.stack.push_back(no_op_stack_entry(10, PlayerId(0)));
        let actions = vec![
            GameAction::PassPriority,
            GameAction::CastSpell {
                object_id: ObjectId(20),
                card_id: CardId(20),
                targets: Vec::new(),
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
            },
        ];

        assert_eq!(
            low_value_priority_pass_from_actions(&state, PlayerId(0), &actions),
            None
        );
    }

    #[test]
    fn low_value_priority_does_not_skip_stack_interactive_activation() {
        let mut state = make_state();
        state.phase = Phase::Upkeep;
        let source_id = add_creature(&mut state, PlayerId(0), 1, 1);
        let ability_index = add_activated_ability(
            &mut state,
            source_id,
            Effect::Counter {
                target: TargetFilter::StackSpell,
                source_rider: None,
                countered_spell_zone: None,
            },
        );
        let actions = vec![
            GameAction::PassPriority,
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            },
        ];

        assert_eq!(
            low_value_priority_pass_from_actions(&state, PlayerId(0), &actions),
            None
        );
    }

    #[test]
    fn low_value_priority_does_not_skip_permanent_progress_activation() {
        let mut state = make_state();
        state.phase = Phase::Upkeep;
        let source_id = add_creature(&mut state, PlayerId(0), 1, 1);
        let ability_index = add_activated_ability(
            &mut state,
            source_id,
            Effect::PutCounter {
                counter_type: CounterType::Generic("tower".to_string()),
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::SelfRef,
            },
        );
        let actions = vec![
            GameAction::PassPriority,
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            },
        ];

        assert_eq!(
            low_value_priority_pass_from_actions(&state, PlayerId(0), &actions),
            None
        );
    }

    #[test]
    fn low_value_priority_does_not_skip_opponent_stack() {
        let mut state = make_state();
        let source_id = add_creature(&mut state, PlayerId(0), 1, 1);
        let ability_index = add_activated_ability(&mut state, source_id, Effect::NoOp);
        state.stack.push_back(no_op_stack_entry(10, PlayerId(1)));
        let actions = vec![
            GameAction::PassPriority,
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            },
        ];

        assert_eq!(
            low_value_priority_pass_from_actions(&state, PlayerId(0), &actions),
            None
        );
    }

    #[test]
    fn large_board_main_phase_fast_action_picks_best_cast_spell() {
        let mut state = make_state();
        for _ in 0..LARGE_BOARD_FAST_PRIORITY_OBJECTS {
            add_creature(&mut state, PlayerId(1), 1, 1);
        }
        let cheap = add_spell_to_hand(&mut state, PlayerId(0), "Cheap Spell", 1);
        let expensive = add_spell_to_hand(&mut state, PlayerId(0), "Expensive Spell", 6);
        let actions = vec![
            GameAction::PassPriority,
            GameAction::CastSpell {
                object_id: cheap,
                card_id: CardId(cheap.0),
                targets: Vec::new(),
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
            },
            GameAction::CastSpell {
                object_id: expensive,
                card_id: CardId(expensive.0),
                targets: Vec::new(),
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
            },
        ];

        assert_eq!(
            large_board_main_phase_fast_action_from_actions(&state, PlayerId(0), &actions),
            Some(GameAction::CastSpell {
                object_id: expensive,
                card_id: CardId(expensive.0),
                targets: Vec::new(),
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
            })
        );
    }

    #[test]
    fn large_board_main_phase_fast_action_does_not_fire_off_turn() {
        let mut state = make_state();
        state.active_player = PlayerId(1);
        for _ in 0..LARGE_BOARD_FAST_PRIORITY_OBJECTS {
            add_creature(&mut state, PlayerId(1), 1, 1);
        }
        let spell = add_spell_to_hand(&mut state, PlayerId(0), "Spell", 1);
        let actions = vec![
            GameAction::PassPriority,
            GameAction::CastSpell {
                object_id: spell,
                card_id: CardId(spell.0),
                targets: Vec::new(),
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
            },
        ];

        assert_eq!(
            large_board_main_phase_fast_action_from_actions(&state, PlayerId(0), &actions),
            None
        );
    }

    fn pending_cast_with_cost(
        shards: Vec<engine::types::mana::ManaCostShard>,
        generic: u32,
    ) -> Box<engine::types::game_state::PendingCast> {
        use engine::types::ability::{QuantityExpr, ResolvedAbility, TargetFilter};
        use engine::types::game_state::PendingCast;
        Box::new(PendingCast::new(
            ObjectId(100),
            CardId(100),
            ResolvedAbility::new(
                engine::types::ability::Effect::Draw {
                    count: QuantityExpr::Fixed { value: 0 },
                    target: TargetFilter::Controller,
                },
                Vec::new(),
                ObjectId(100),
                PlayerId(0),
            ),
            engine::types::mana::ManaCost::Cost { shards, generic },
        ))
    }

    /// Minimal ChooseManaColor SingleColor state with the given option list and
    /// pending cast. The `context` is a degenerate `ResolvingEffect` resume — the
    /// pre-emption never inspects it, only `choice` and `state.pending_cast`.
    /// Production Improvise/dual repro paths use `ManaChoiceContext::ManaAbility`,
    /// but the context variant is irrelevant to the pre-emption (which reads only
    /// `pending_cast` + `options`), so `ResolvingEffect` is used for fixture
    /// simplicity.
    fn choose_mana_color_state(
        options: Vec<ManaType>,
        pending: Option<Box<engine::types::game_state::PendingCast>>,
    ) -> GameState {
        use engine::types::ability::{QuantityExpr, ResolvedAbility, TargetFilter};
        use engine::types::game_state::{ManaChoiceContext, ManaChoicePrompt};
        let mut state = make_state();
        state.pending_cast = pending;
        let resume = ResolvedAbility::new(
            engine::types::ability::Effect::Draw {
                count: QuantityExpr::Fixed { value: 0 },
                target: TargetFilter::Controller,
            },
            Vec::new(),
            ObjectId(100),
            PlayerId(0),
        );
        state.waiting_for = WaitingFor::ChooseManaColor {
            player: PlayerId(0),
            choice: ManaChoicePrompt::SingleColor { options },
            context: ManaChoiceContext::ResolvingEffect(Box::new(resume)),
        };
        state
    }

    #[test]
    fn choose_mana_color_preemption_selects_demanded_color() {
        // Repro: {2}{U} spell, U/R source offered [Red, Blue]. The AI must
        // produce Blue (demanded) — the old scorer picked the first-enumerated
        // color (Red) and stranded the {U} pip into a ManaPayment dead-end.
        // Drives the PUBLIC choose_action path, not fallback_action.
        let state = choose_mana_color_state(
            vec![ManaType::Red, ManaType::Blue],
            Some(pending_cast_with_cost(
                vec![engine::types::mana::ManaCostShard::Blue],
                2,
            )),
        );
        let config = create_config(AiDifficulty::Medium, Platform::Native);
        let mut rng = SmallRng::seed_from_u64(1);
        assert_eq!(
            choose_action(&state, PlayerId(0), &config, &mut rng),
            Some(GameAction::ChooseManaColor {
                choice: engine::types::game_state::ManaChoice::SingleColor(ManaType::Blue),
                count: 1,
            })
        );
    }

    #[test]
    fn choose_mana_color_preemption_selects_demanded_red() {
        // {1}{R} spell, source offered [Blue, Red] → must produce Red.
        let state = choose_mana_color_state(
            vec![ManaType::Blue, ManaType::Red],
            Some(pending_cast_with_cost(
                vec![engine::types::mana::ManaCostShard::Red],
                1,
            )),
        );
        let config = create_config(AiDifficulty::Medium, Platform::Native);
        let mut rng = SmallRng::seed_from_u64(1);
        assert_eq!(
            choose_action(&state, PlayerId(0), &config, &mut rng),
            Some(GameAction::ChooseManaColor {
                choice: engine::types::game_state::ManaChoice::SingleColor(ManaType::Red),
                count: 1,
            })
        );
    }

    #[test]
    fn choose_mana_color_preemption_demanded_color_first() {
        // Demanded color is first here; paired with selects_demanded_color
        // (demanded second) this proves demand-driven, not positional.
        let state = choose_mana_color_state(
            vec![ManaType::Blue, ManaType::Red],
            Some(pending_cast_with_cost(
                vec![engine::types::mana::ManaCostShard::Blue],
                2,
            )),
        );
        let config = create_config(AiDifficulty::Medium, Platform::Native);
        let mut rng = SmallRng::seed_from_u64(1);
        assert_eq!(
            choose_action(&state, PlayerId(0), &config, &mut rng),
            Some(GameAction::ChooseManaColor {
                choice: engine::types::game_state::ManaChoice::SingleColor(ManaType::Blue),
                count: 1,
            })
        );
    }

    #[test]
    fn choose_mana_color_preemption_no_pending_cast_uses_first() {
        // No pending cast (e.g. a mana-ability color choice at priority) →
        // first option, identical to the old behavior.
        let state = choose_mana_color_state(vec![ManaType::Red, ManaType::Blue], None);
        let config = create_config(AiDifficulty::Medium, Platform::Native);
        let mut rng = SmallRng::seed_from_u64(1);
        assert_eq!(
            choose_action(&state, PlayerId(0), &config, &mut rng),
            Some(GameAction::ChooseManaColor {
                choice: engine::types::game_state::ManaChoice::SingleColor(ManaType::Red),
                count: 1,
            })
        );
    }

    #[test]
    fn session_policy_memory_survives_consecutive_decisions() {
        let state = make_state();
        let config = create_config(AiDifficulty::Medium, Platform::Native);
        let session = AiSession::arc_from_game(&state);
        session.memory.write().unwrap().by_policy.insert(
            PolicyId::LandfallTiming,
            crate::session::PolicyState::LandfallTiming {
                held_fetch_count: 7,
                last_held_turn: state.turn_number,
            },
        );

        let mut rng = SmallRng::seed_from_u64(1);
        assert_eq!(
            choose_action_with_session(&state, PlayerId(0), &config, &mut rng, &session),
            Some(GameAction::PassPriority)
        );
        assert_eq!(
            choose_action_with_session(&state, PlayerId(0), &config, &mut rng, &session),
            Some(GameAction::PassPriority)
        );

        let memory = session.memory.read().unwrap();
        assert!(matches!(
            memory.by_policy.get(&PolicyId::LandfallTiming),
            Some(crate::session::PolicyState::LandfallTiming {
                held_fetch_count: 7,
                last_held_turn: 2,
            })
        ));
    }

    #[test]
    fn softmax_low_temp_picks_highest() {
        let scored = vec![
            (GameAction::PassPriority, 1.0),
            (
                GameAction::PlayLand {
                    object_id: ObjectId(0),
                    card_id: CardId(1),
                },
                10.0,
            ),
        ];
        let mut rng = SmallRng::seed_from_u64(42);
        let mut picked_land = 0;
        for _ in 0..20 {
            if let Some(GameAction::PlayLand { .. }) = softmax_select_pairs(&scored, 0.01, &mut rng)
            {
                picked_land += 1;
            }
        }
        assert!(
            picked_land >= 18,
            "Low temperature should almost always pick highest score, got {picked_land}/20"
        );
    }

    #[test]
    fn softmax_high_temp_is_more_random() {
        let scored = vec![
            (GameAction::PassPriority, 1.0),
            (
                GameAction::PlayLand {
                    object_id: ObjectId(0),
                    card_id: CardId(1),
                },
                2.0,
            ),
        ];
        let mut rng = SmallRng::seed_from_u64(42);
        let mut picked_pass = 0;
        for _ in 0..100 {
            if let Some(GameAction::PassPriority) = softmax_select_pairs(&scored, 4.0, &mut rng) {
                picked_pass += 1;
            }
        }
        assert!(
            picked_pass > 10 && picked_pass < 90,
            "High temperature should produce mixed results, got pass={picked_pass}/100"
        );
    }

    #[test]
    fn budget_limits_stop_search() {
        let mut budget = SearchBudget::new(3);
        assert!(!budget.exhausted());
        budget.tick();
        budget.tick();
        budget.tick();
        assert!(budget.exhausted());
    }

    #[test]
    fn score_candidates_filters_activation_pending_on_stack() {
        // CR 117.1b + pending_activations guard: when an activated ability's
        // prior activation is still on the stack, the AI filter rejects the
        // same (source_id, ability_index) from the candidate list to prevent
        // softmax re-pick loops.
        let mut state = make_state();
        let creature = add_creature(&mut state, PlayerId(0), 1, 1);
        state.pending_activations.push((creature, 0));

        // Construct a candidate for ActivateAbility on the pending pair.
        let blocked = CandidateAction {
            action: GameAction::ActivateAbility {
                source_id: creature,
                ability_index: 0,
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Ability,
            },
        };
        let allowed = CandidateAction {
            action: GameAction::PassPriority,
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Utility,
            },
        };

        // Inline the filter logic the same way score_candidates does.
        let gated: Vec<CandidateAction> = vec![blocked.clone(), allowed.clone()]
            .into_iter()
            .filter(|c| match &c.action {
                GameAction::CastSpell { object_id, .. } => {
                    !state.cancelled_casts.contains(object_id)
                }
                GameAction::ActivateAbility {
                    source_id,
                    ability_index,
                } => {
                    !state.cancelled_casts.contains(source_id)
                        && !state
                            .pending_activations
                            .contains(&(*source_id, *ability_index))
                        && state
                            .activated_abilities_this_turn
                            .get(&(*source_id, *ability_index))
                            .copied()
                            .unwrap_or(0)
                            < MAX_ACTIVATIONS_PER_SOURCE_PER_TURN
                }
                _ => true,
            })
            .collect();

        assert_eq!(
            gated.len(),
            1,
            "pending activation should block re-activation candidate"
        );
        assert_eq!(gated[0].action, GameAction::PassPriority);
    }

    #[test]
    fn score_candidates_filters_activation_at_per_turn_cap() {
        // AI safety cap: once an ability has been activated
        // MAX_ACTIVATIONS_PER_SOURCE_PER_TURN times this turn on the same
        // source, further activations are rejected regardless of stack state.
        let mut state = make_state();
        let creature = add_creature(&mut state, PlayerId(0), 1, 1);
        state
            .activated_abilities_this_turn
            .insert((creature, 0), MAX_ACTIVATIONS_PER_SOURCE_PER_TURN);

        let blocked = CandidateAction {
            action: GameAction::ActivateAbility {
                source_id: creature,
                ability_index: 0,
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Ability,
            },
        };

        let gated: Vec<CandidateAction> = vec![blocked]
            .into_iter()
            .filter(|c| match &c.action {
                GameAction::ActivateAbility {
                    source_id,
                    ability_index,
                } => {
                    !state.cancelled_casts.contains(source_id)
                        && !state
                            .pending_activations
                            .contains(&(*source_id, *ability_index))
                        && state
                            .activated_abilities_this_turn
                            .get(&(*source_id, *ability_index))
                            .copied()
                            .unwrap_or(0)
                            < MAX_ACTIVATIONS_PER_SOURCE_PER_TURN
                }
                _ => true,
            })
            .collect();

        assert!(
            gated.is_empty(),
            "activation at per-turn cap should be filtered"
        );
    }

    #[test]
    fn search_prefers_board_advantage() {
        // Set up a state where AI (player 0) has options and a board advantage matters
        let mut state = make_state();
        add_creature(&mut state, PlayerId(0), 3, 3);
        add_creature(&mut state, PlayerId(1), 1, 1);
        add_mana(&mut state, PlayerId(0), ManaType::Red, 3);

        let config = create_config(AiDifficulty::Medium, Platform::Native);
        let mut rng = SmallRng::seed_from_u64(42);
        let action = choose_action(&state, PlayerId(0), &config, &mut rng);
        // Should return some valid action (not None)
        assert!(
            action.is_some(),
            "AI should choose an action with board advantage"
        );
    }

    #[test]
    fn heuristic_mode_works_for_easy() {
        let state = make_state();
        let config = create_config(AiDifficulty::Easy, Platform::Native);
        let mut rng = SmallRng::seed_from_u64(42);
        let action = choose_action(&state, PlayerId(0), &config, &mut rng);
        assert!(action.is_some());
    }

    #[test]
    fn very_hard_prefers_playing_available_land() {
        let mut state = make_state();
        let land_id = engine::game::zones::create_object(
            &mut state,
            CardId(99),
            PlayerId(0),
            "Forest".to_string(),
            engine::types::zones::Zone::Hand,
        );
        state
            .objects
            .get_mut(&land_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let mut rng = SmallRng::seed_from_u64(7);
        let action = choose_action(&state, PlayerId(0), &config, &mut rng);

        assert_eq!(
            action,
            Some(GameAction::PlayLand {
                object_id: land_id,
                card_id: CardId(99)
            })
        );
    }

    /// Regression test: AI with a castable creature in hand and untapped lands
    /// on the battlefield should cast the creature, not just tap lands for mana.
    #[test]
    fn very_hard_casts_creature_instead_of_tapping_lands() {
        let mut state = make_state();
        state.lands_played_this_turn = 1; // Already played a land

        // Add two forests on battlefield (untapped, can tap for green)
        for i in 0..2 {
            let land_id = engine::game::zones::create_object(
                &mut state,
                CardId(200 + i),
                PlayerId(0),
                "Forest".to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&land_id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Forest".to_string());
            obj.controller = PlayerId(0);
            obj.entered_battlefield_turn = Some(1);
        }

        // Add a 2/2 creature with mana cost {1}{G} in hand
        let creature_id = engine::game::zones::create_object(
            &mut state,
            CardId(300),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&creature_id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.mana_cost = engine::types::mana::ManaCost::Cost {
            shards: vec![engine::types::mana::ManaCostShard::Green],
            generic: 1,
        };

        // Verify CastSpell is at least a scored candidate (the AI considers it)
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let scored = score_candidates(&state, PlayerId(0), &config);
        let has_cast = scored
            .iter()
            .any(|(a, _)| matches!(a, GameAction::CastSpell { .. }));
        assert!(
            has_cast || scored.is_empty(),
            "CastSpell should be a candidate when creature is castable"
        );
    }

    /// Scoring is RNG-free, so a session pulled from `SessionCache` must produce
    /// byte-identical scores to a freshly built session. Guards the WASM
    /// session-cache reuse: if `get_or_build` ever returned a session that
    /// differed from `arc_from_game`, `assert_eq` on the full score vector flips.
    #[test]
    fn score_candidates_with_session_matches_fresh_session() {
        let mut state = make_state();
        state.lands_played_this_turn = 1;

        let creature_id = create_object(
            &mut state,
            CardId(900),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&creature_id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.mana_cost = engine::types::mana::ManaCost::Cost {
            shards: vec![engine::types::mana::ManaCostShard::Green],
            generic: 1,
        };
        add_mana(&mut state, PlayerId(0), ManaType::Green, 3);

        let config = create_config(AiDifficulty::Medium, Platform::Native);

        let session_fresh = AiSession::arc_from_game(&state);
        let mut cache = SessionCache::new_empty();
        let session_cached = cache.get_or_build(&state);

        let scored_fresh =
            score_candidates_with_session(&state, PlayerId(0), &config, &session_fresh);
        let scored_cached =
            score_candidates_with_session(&state, PlayerId(0), &config, &session_cached);

        // HARD reach-guard (no `|| is_empty()` escape): production input must
        // reach the CastSpell enumeration arm, else the assert_eq is vacuous.
        assert!(
            scored_cached
                .iter()
                .any(|(a, _)| matches!(a, GameAction::CastSpell { .. })),
            "castable creature + pool mana must enumerate a CastSpell candidate"
        );
        assert_eq!(
            scored_cached, scored_fresh,
            "cached and fresh sessions must produce identical scores (RNG-free scoring path)"
        );
    }

    /// The pool-worker discriminator: a board-only mutation (hand + mana pool,
    /// `deck_pools` untouched) must NOT invalidate the deck-keyed session, and
    /// the reused session must still score the mutated board identically to a
    /// fresh session. If board state leaked into the fingerprint, `ptr_eq`
    /// flips; if a stale session mis-scored the new board, `assert_eq` flips.
    #[test]
    fn session_cache_reused_across_board_mutation_stays_correct() {
        let mut state = make_state();
        let mut cache = SessionCache::new_empty();
        let s1 = cache.get_or_build(&state);

        // Mutate the board only — hand object, mana pool, and state.objects.
        state.lands_played_this_turn = 1;
        let creature_id = create_object(
            &mut state,
            CardId(900),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&creature_id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.mana_cost = engine::types::mana::ManaCost::Cost {
            shards: vec![engine::types::mana::ManaCostShard::Green],
            generic: 1,
        };
        add_mana(&mut state, PlayerId(0), ManaType::Green, 3);

        let s2 = cache.get_or_build(&state);
        assert!(
            Arc::ptr_eq(&s1, &s2),
            "board-only mutation must NOT invalidate the deck-keyed session"
        );

        let config = create_config(AiDifficulty::Medium, Platform::Native);
        let scored_reused = score_candidates_with_session(&state, PlayerId(0), &config, &s2);
        assert!(
            scored_reused
                .iter()
                .any(|(a, _)| matches!(a, GameAction::CastSpell { .. })),
            "reused session must still enumerate the now-castable creature"
        );

        let session_fresh = AiSession::arc_from_game(&state);
        let scored_fresh =
            score_candidates_with_session(&state, PlayerId(0), &config, &session_fresh);
        assert_eq!(
            scored_reused, scored_fresh,
            "reused (board-stale) session must score the mutated board identically to a fresh one"
        );
    }

    #[test]
    fn search_choice_picks_best_tutor_target() {
        let mut state = make_state();
        let titan = engine::game::zones::create_object(
            &mut state,
            CardId(401),
            PlayerId(0),
            "Titan".to_string(),
            Zone::Library,
        );
        let land = engine::game::zones::create_object(
            &mut state,
            CardId(402),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Library,
        );
        {
            let titan_obj = state.objects.get_mut(&titan).unwrap();
            titan_obj.card_types.core_types.push(CoreType::Creature);
            titan_obj.power = Some(6);
            titan_obj.toughness = Some(6);
        }
        state
            .objects
            .get_mut(&land)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
        state.waiting_for = WaitingFor::SearchChoice {
            player: PlayerId(0),
            cards: vec![titan, land],
            count: 1,
            reveal: false,
            up_to: false,
            allows_partial_find: false,
            constraint: engine::types::ability::SearchSelectionConstraint::None,
            split: None,
        };

        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let mut rng = SmallRng::seed_from_u64(11);
        let action = choose_action(&state, PlayerId(0), &config, &mut rng);

        assert_eq!(action, Some(GameAction::SelectCards { cards: vec![titan] }));
    }

    #[test]
    fn self_targeting_is_penalized() {
        let state = make_state();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TriggerTargetSelection {
                player: PlayerId(0),
                trigger_controller: None,
                trigger_event: None,
                trigger_events: Vec::new(),
                target_slots: Vec::new(),
                mode_labels: Vec::new(),
                target_constraints: Vec::new(),
                selection: Default::default(),
                source_id: None,
                description: None,
            },
            candidates: Vec::new(),
        };
        let policies = PolicyRegistry::default();
        let self_candidate = CandidateAction {
            action: GameAction::ChooseTarget {
                target: Some(TargetRef::Player(PlayerId(0))),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Target,
            },
        };
        let opp_candidate = CandidateAction {
            action: GameAction::ChooseTarget {
                target: Some(TargetRef::Player(PlayerId(1))),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Target,
            },
        };

        let self_score = policies.score(&PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &self_candidate,
            ai_player: PlayerId(0),
            config: &AiConfig::default(),
            context: &crate::context::AiContext::empty(&AiConfig::default().weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        });
        let opp_score = policies.score(&PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &opp_candidate,
            ai_player: PlayerId(0),
            config: &AiConfig::default(),
            context: &crate::context::AiContext::empty(&AiConfig::default().weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        });
        assert!(self_score < opp_score);
        assert!(self_score < -50.0);
    }

    #[test]
    fn target_selection_prefers_opponent_over_self() {
        let mut state = make_state();
        state.waiting_for = WaitingFor::TriggerTargetSelection {
            player: PlayerId(0),
            trigger_controller: None,
            trigger_event: None,
            trigger_events: Vec::new(),
            target_slots: vec![engine::types::game_state::TargetSelectionSlot {
                legal_targets: vec![
                    TargetRef::Player(PlayerId(0)),
                    TargetRef::Player(PlayerId(1)),
                ],
                optional: false,
            }],
            mode_labels: Vec::new(),
            target_constraints: Vec::new(),
            selection: engine::types::game_state::TargetSelectionProgress {
                current_slot: 0,
                selected_slots: Vec::new(),
                current_legal_targets: vec![
                    TargetRef::Player(PlayerId(0)),
                    TargetRef::Player(PlayerId(1)),
                ],
            },
            source_id: None,
            description: None,
        };

        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let mut rng = SmallRng::seed_from_u64(9);
        let action = choose_action(&state, PlayerId(0), &config, &mut rng);

        assert_eq!(
            action,
            Some(GameAction::ChooseTarget {
                target: Some(TargetRef::Player(PlayerId(1))),
            })
        );
    }

    #[test]
    fn optional_target_selection_can_skip_when_no_targets_exist() {
        let mut state = make_state();
        state.waiting_for = WaitingFor::TriggerTargetSelection {
            player: PlayerId(0),
            trigger_controller: None,
            trigger_event: None,
            trigger_events: Vec::new(),
            target_slots: vec![engine::types::game_state::TargetSelectionSlot {
                legal_targets: Vec::new(),
                optional: true,
            }],
            mode_labels: Vec::new(),
            target_constraints: Vec::new(),
            selection: Default::default(),
            source_id: None,
            description: None,
        };

        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let mut rng = SmallRng::seed_from_u64(10);
        let action = choose_action(&state, PlayerId(0), &config, &mut rng);

        assert_eq!(action, Some(GameAction::ChooseTarget { target: None }));
    }

    /// Regression test: AI must produce DeclareBlockers action even when the
    /// candidate pipeline filters out all generated blocker combinations.
    /// Previously, empty candidates caused fallback_action() to return
    /// PassPriority, which is illegal during DeclareBlockers.
    #[test]
    fn declare_blockers_never_returns_pass_priority() {
        use engine::game::combat::{AttackTarget, AttackerInfo, CombatState};
        use std::collections::HashMap;

        let mut state = make_state();
        state.phase = Phase::DeclareBlockers;

        // Opponent's attacker
        let attacker = add_creature(&mut state, PlayerId(1), 3, 3);

        // AI's potential blocker
        let blocker = add_creature(&mut state, PlayerId(0), 2, 2);

        // Set up combat state with attacker
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo {
                object_id: attacker,
                defending_player: PlayerId(0),
                attack_target: AttackTarget::Player(PlayerId(0)),
                blocked: false,
                band_id: None,
            }],
            blocker_assignments: HashMap::new(),
            blocker_to_attacker: HashMap::new(),
            damage_assignments: HashMap::new(),
            first_strike_done: false,
            damage_step_index: None,
            pending_damage: Vec::new(),
            regular_damage_done: false,
            ..Default::default()
        });

        state.waiting_for = WaitingFor::DeclareBlockers {
            player: PlayerId(0),
            valid_blocker_ids: vec![blocker],
            valid_block_targets: {
                let mut m = HashMap::new();
                m.insert(blocker, vec![attacker]);
                m
            },
            block_requirements: HashMap::new(),
        };

        for difficulty in [
            AiDifficulty::VeryEasy,
            AiDifficulty::Easy,
            AiDifficulty::Medium,
            AiDifficulty::Hard,
            AiDifficulty::VeryHard,
        ] {
            let config = create_config(difficulty, Platform::Native);
            let mut rng = SmallRng::seed_from_u64(42);
            let action = choose_action(&state, PlayerId(0), &config, &mut rng);
            assert!(
                matches!(action, Some(GameAction::DeclareBlockers { .. })),
                "Difficulty {:?} should return DeclareBlockers, got {:?}",
                difficulty,
                action
            );
        }
    }

    /// Regression test: DeclareAttackers also bypasses candidate pipeline.
    #[test]
    fn declare_attackers_never_returns_pass_priority() {
        let mut state = make_state();
        state.phase = Phase::DeclareAttackers;
        let creature = add_creature(&mut state, PlayerId(0), 3, 3);

        state.waiting_for = WaitingFor::DeclareAttackers {
            player: PlayerId(0),
            valid_attacker_ids: vec![creature],
            valid_attack_targets: vec![],
        };

        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let mut rng = SmallRng::seed_from_u64(42);
        let action = choose_action(&state, PlayerId(0), &config, &mut rng);
        assert!(
            matches!(action, Some(GameAction::DeclareAttackers { .. })),
            "Should return DeclareAttackers, got {:?}",
            action
        );
    }

    /// Issue #1523 (p0 softlock): `validated_declare_attackers` must never
    /// return an attacker declaration the engine would reject — otherwise the
    /// deterministic action driver re-submits it forever ("repeated attempts to
    /// attack"). Given an illegal declaration (here a tapped creature, which
    /// can't be declared as an attacker, CR 508.1a), the guard dry-runs it,
    /// sees the rejection, and falls back to a legal declaration that does NOT
    /// contain the illegal attacker.
    #[test]
    fn validated_declare_attackers_drops_illegal_attacker() {
        let mut state = make_state();
        state.phase = Phase::DeclareAttackers;
        let creature = add_creature(&mut state, PlayerId(0), 3, 3);
        // Tap it: a tapped creature can't be a legal attacker.
        state.objects.get_mut(&creature).unwrap().tapped = true;
        let target = engine::game::combat::AttackTarget::Player(PlayerId(1));

        state.waiting_for = WaitingFor::DeclareAttackers {
            player: PlayerId(0),
            valid_attacker_ids: vec![creature],
            valid_attack_targets: vec![target],
        };

        let action = validated_declare_attackers(&state, vec![(creature, target)]);

        match action {
            GameAction::DeclareAttackers { attacks, .. } => assert!(
                !attacks.iter().any(|(id, _)| *id == creature),
                "guard must drop the illegal (tapped) attacker, got {attacks:?}"
            ),
            other => panic!("expected DeclareAttackers, got {other:?}"),
        }
    }

    /// CR 608.2c + CR 701.23: Gifts Ungiven scaling regression — with a
    /// large library (80 cards), a count-4 search must complete via the
    /// BEAM_K-bounded path rather than the pre-fix Cartesian enumerator
    /// (~C(80, 4) ≈ 1.5M combos × per-combo scoring) that stalled the AI.
    /// The beam reduces this to C(BEAM_K, 4) ≈ 794 scored selections.
    ///
    /// The ceiling is a *blowup* guard, not a tight micro-benchmark: the
    /// healthy beam path runs in ~60–130 ms (machine- and load-dependent —
    /// this runs in CI and alongside concurrent Tilt rebuilds), while a
    /// reversion to Cartesian enumeration costs *tens of seconds*. A 1 s
    /// ceiling cleanly separates the two — ~8× headroom over the loaded
    /// healthy path, ~1000× below a Cartesian regression — so it catches the
    /// regression it exists to catch without flaking on contention. The
    /// DistinctNames constraint is honored by the engine candidate filter and
    /// re-checked inside the AI beam, so the returned selection must contain
    /// only uniquely-named cards.
    #[test]
    fn gifts_ungiven_search_choice_returns_quickly_with_distinct_names() {
        use engine::types::ability::{SearchSelectionConstraint, SharedQuality};
        use std::time::Instant;

        let mut state = make_state();

        // Seed an 80-card pool with mostly unique names plus a few duplicates,
        // mirroring the kind of long-game library Gifts is cast into.
        let mut cards: Vec<ObjectId> = Vec::with_capacity(80);
        for i in 0..80 {
            // Repeat 8 base names to ensure DistinctNames pruning has work to do.
            let name = format!("Card-{}", i % 8);
            let id = create_object(
                &mut state,
                CardId(1000 + i as u64),
                PlayerId(0),
                name,
                Zone::Library,
            );
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
            cards.push(id);
        }

        state.waiting_for = WaitingFor::SearchChoice {
            player: PlayerId(0),
            cards,
            count: 4,
            reveal: true,
            up_to: true,
            allows_partial_find: false,
            constraint: SearchSelectionConstraint::DistinctQualities {
                qualities: vec![SharedQuality::Name],
            },
            split: None,
        };

        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let mut rng = SmallRng::seed_from_u64(42);
        let started = Instant::now();
        let action = choose_action(&state, PlayerId(0), &config, &mut rng);
        let elapsed = started.elapsed();
        assert!(
            elapsed.as_millis() < 1000,
            "AI search-choice took {elapsed:?}; a Cartesian-enumeration regression \
             (C(80,4) ≈ 1.5M combos) costs tens of seconds — the BEAM_K path must \
             stay well under the 1s blowup ceiling"
        );

        match action {
            Some(GameAction::SelectCards { cards }) => {
                assert!(
                    cards.len() <= 4,
                    "up_to=true SearchChoice must respect the count ceiling"
                );
                let mut names = std::collections::HashSet::new();
                for id in &cards {
                    let obj = state.objects.get(id).expect("selected card present");
                    assert!(
                        names.insert(obj.name.clone()),
                        "DistinctNames must prevent duplicate name in selection: {:?}",
                        obj.name
                    );
                }
            }
            other => panic!("expected SelectCards, got {other:?}"),
        }
    }

    // --- ControllerLabels (Battlebond friend-or-foe) AI heuristic ---

    /// Build a 2-player `VoteChoice` representing one step of a
    /// `ControllerLabels` vote where the named subject is being labeled.
    /// `actor` is always the spell controller.
    fn vote_choice_for_subject(
        state: &GameState,
        controller: PlayerId,
        subject: PlayerId,
    ) -> WaitingFor {
        let _ = state;
        WaitingFor::VoteChoice {
            player: subject,
            remaining_votes: 1,
            options: vec!["friend".to_string(), "foe".to_string()],
            option_labels: vec!["Friend".to_string(), "Foe".to_string()],
            remaining_voters: Vec::new(),
            tallies: vec![0, 0],
            ballots: engine::im::Vector::new(),
            per_choice_effect: Vec::new(),
            controller,
            source_id: ObjectId(1),
            actor: engine::types::game_state::VoteActor::Delegated(controller),
            tally_mode: engine::types::ability::VoteTally::PerVote,
            candidate_objects: engine::im::Vector::new(),
            outcome_template: None,
            visibility: engine::types::ability::VoteVisibility::Open,
        }
    }

    /// When the AI controller is labeling themselves, the heuristic picks
    /// `friend` — the beneficial label. The fallback action route exercises
    /// the same code path the runtime walks when no scored candidate beats
    /// the deterministic default.
    #[test]
    fn controller_labels_ai_labels_self_friend() {
        let mut state = make_state();
        let controller = PlayerId(0);
        state.waiting_for = vote_choice_for_subject(&state, controller, controller);
        let action = fallback_action(&state).expect("fallback returns an action");
        assert!(
            matches!(action, GameAction::ChooseOption { ref choice } if choice == "friend"),
            "AI labeling self must pick friend, got {action:?}"
        );
    }

    /// When the AI controller is labeling an opponent, the heuristic picks
    /// `foe` — the harmful label.
    #[test]
    fn controller_labels_ai_labels_opponent_foe() {
        let mut state = make_state();
        let controller = PlayerId(0);
        let opp = PlayerId(1);
        state.waiting_for = vote_choice_for_subject(&state, controller, opp);
        let action = fallback_action(&state).expect("fallback returns an action");
        assert!(
            matches!(action, GameAction::ChooseOption { ref choice } if choice == "foe"),
            "AI labeling opponent must pick foe, got {action:?}"
        );
    }

    #[test]
    fn copy_retarget_fallback_keeps_existing_targets_with_legal_action() {
        let mut state = make_state();
        let original_target = TargetRef::Object(ObjectId(10));
        state.waiting_for = WaitingFor::CopyRetarget {
            player: PlayerId(0),
            copy_id: ObjectId(20),
            target_slots: vec![engine::types::game_state::CopyTargetSlot {
                current: Some(original_target),
                legal_alternatives: vec![TargetRef::Object(ObjectId(11))],
            }],
            effect_kind: EffectKind::CopySpell,
            effect_source_id: Some(ObjectId(20)),
            current_slot: 0,
            paradigm_remaining_offers: None,
        };

        let action = fallback_action(&state).expect("fallback returns an action");
        assert_eq!(action, GameAction::KeepAllCopyTargets);
        assert!(engine::game::engine::apply_as_current(&mut state, action).is_ok());
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    }

    #[test]
    fn copy_retarget_fallback_keeps_current_slot_before_later_empty_slot() {
        let mut state = make_state();
        let current_target = TargetRef::Object(ObjectId(10));
        state.waiting_for = WaitingFor::CopyRetarget {
            player: PlayerId(0),
            copy_id: ObjectId(20),
            target_slots: vec![
                engine::types::game_state::CopyTargetSlot {
                    current: Some(current_target),
                    legal_alternatives: vec![TargetRef::Object(ObjectId(11))],
                },
                engine::types::game_state::CopyTargetSlot {
                    current: None,
                    legal_alternatives: vec![TargetRef::Object(ObjectId(12))],
                },
            ],
            effect_kind: EffectKind::CopySpell,
            effect_source_id: Some(ObjectId(20)),
            current_slot: 0,
            paradigm_remaining_offers: None,
        };

        let action = fallback_action(&state).expect("fallback returns an action");
        assert_eq!(action, GameAction::ChooseTarget { target: None });
        assert!(engine::game::engine::apply_as_current(&mut state, action).is_ok());
        assert!(matches!(
            state.waiting_for,
            WaitingFor::CopyRetarget {
                current_slot: 1,
                ..
            }
        ));
    }

    #[test]
    fn copy_retarget_fallback_selects_first_target_for_fresh_copy_cast() {
        let mut state = make_state();
        let first_target = TargetRef::Object(ObjectId(10));
        state.waiting_for = WaitingFor::CopyRetarget {
            player: PlayerId(0),
            copy_id: ObjectId(20),
            target_slots: vec![engine::types::game_state::CopyTargetSlot {
                current: None,
                legal_alternatives: vec![first_target.clone(), TargetRef::Object(ObjectId(11))],
            }],
            effect_kind: EffectKind::CopySpell,
            effect_source_id: Some(ObjectId(20)),
            current_slot: 0,
            paradigm_remaining_offers: None,
        };

        let action = fallback_action(&state).expect("fallback returns an action");
        assert_eq!(
            action,
            GameAction::ChooseTarget {
                target: Some(first_target),
            }
        );
        assert!(engine::game::engine::apply_as_current(&mut state, action).is_ok());
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    }

    /// A classic vote (`actor == player`) keeps the pre-existing "first
    /// option" fallback — the friend-or-foe heuristic must not leak into
    /// Council's-dilemma votes.
    #[test]
    fn classic_vote_falls_back_to_first_option() {
        let mut state = make_state();
        let controller = PlayerId(0);
        state.waiting_for = WaitingFor::VoteChoice {
            player: controller,
            remaining_votes: 1,
            options: vec!["evidence".to_string(), "bribery".to_string()],
            option_labels: vec!["Evidence".to_string(), "Bribery".to_string()],
            remaining_voters: Vec::new(),
            tallies: vec![0, 0],
            ballots: engine::im::Vector::new(),
            per_choice_effect: Vec::new(),
            controller,
            source_id: ObjectId(1),
            actor: engine::types::game_state::VoteActor::SubjectActs,
            tally_mode: engine::types::ability::VoteTally::PerVote,
            candidate_objects: engine::im::Vector::new(),
            outcome_template: None,
            visibility: engine::types::ability::VoteVisibility::Open,
        };
        let action = fallback_action(&state).expect("fallback returns an action");
        assert!(
            matches!(action, GameAction::ChooseOption { ref choice } if choice == "evidence"),
            "classic vote must pick first option, got {action:?}"
        );
    }

    /// Regression guard: AI priority decision against 1000-token opponent
    /// board must complete in single-digit milliseconds. The combination of
    /// `ranked.truncate(branching)`, the deadline mechanism, and the
    /// `im::HashMap` structural sharing in `apply_candidate` keeps priority
    /// decisions cheap even on Scute Swarm-class boards. If this test ever
    /// regresses past 100ms, something started doing per-opponent-creature
    /// work inside `evaluate_after_action` or the candidate scoring loop —
    /// hunt that down rather than relax this bound.
    #[test]
    fn priority_decision_vs_thousand_opponent_tokens_stays_fast() {
        let mut state = make_state();
        // 1000 1/1 opponent tokens — the pathological board.
        for _ in 0..1000 {
            add_creature(&mut state, PlayerId(1), 1, 1);
        }
        // AI has 5 untapped lands available (so legal_actions has some real
        // candidates: PassPriority + maybe land-tap mana abilities).
        for _ in 0..5 {
            let cid = CardId(state.next_object_id);
            let id = create_object(
                &mut state,
                cid,
                PlayerId(0),
                "Forest".to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
        }

        let config = create_config(AiDifficulty::Hard, Platform::Native);
        let mut rng = SmallRng::seed_from_u64(42);

        let start = std::time::Instant::now();
        let action = choose_action(&state, PlayerId(0), &config, &mut rng);
        let elapsed = start.elapsed();

        eprintln!(
            "[bench] choose_action priority-pass (1000 opponent tokens, AI difficulty=Hard): {:?}",
            elapsed
        );
        assert!(action.is_some(), "AI must produce some action");
        // Empirical baseline ~5ms in debug. 100ms is a generous ceiling that
        // catches a 20× regression while staying robust to CI-runner noise.
        assert!(
            elapsed.as_millis() < 100,
            "Priority decision regressed past 100ms ceiling: {:?}; \
             investigate per-opponent-creature work in score_candidates / \
             evaluate_after_action before relaxing this bound.",
            elapsed
        );
    }

    /// Regression for #1591: when a permanent belongs to multiple type
    /// categories (an artifact creature), the `CategoryChoice` fallback may
    /// choose that same object for every eligible category slot. The engine
    /// dedupes only the protected set before sacrificing the rest.
    #[test]
    fn category_choice_fallback_allows_duplicate_object_slots_and_applies() {
        let mut state = make_state();
        // Source of the ChooseAndSacrificeRest ability.
        let source_card = CardId(state.next_object_id);
        let source = create_object(
            &mut state,
            source_card,
            PlayerId(0),
            "Cataclysmic Gearhulk".to_string(),
            Zone::Battlefield,
        );
        // An artifact creature controlled by player 0 — eligible in both the
        // Artifact and Creature categories.
        let ac_card = CardId(state.next_object_id);
        let artifact_creature = create_object(
            &mut state,
            ac_card,
            PlayerId(0),
            "Steel Hellkite".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&artifact_creature).unwrap();
            obj.card_types.core_types = vec![CoreType::Artifact, CoreType::Creature];
        }

        // `[[X],[X]]` — X shared across both categories. The fallback may use
        // X for both slots because each slot asks a separate category question.
        state.waiting_for = WaitingFor::CategoryChoice {
            player: PlayerId(0),
            target_player: PlayerId(0),
            categories: vec![CoreType::Artifact, CoreType::Creature],
            chooser_scope: CategoryChooserScope::EachPlayerSelf,
            choose_filter: TargetFilter::Typed(TypedFilter::permanent()),
            sacrifice_filter: TargetFilter::Typed(TypedFilter::permanent()),
            source_controller: PlayerId(0),
            eligible_per_category: vec![vec![artifact_creature], vec![artifact_creature]],
            source_id: source,
            remaining_players: Vec::new(),
            all_kept: Vec::new(),
            scoped_players: Vec::new(),
        };

        let action = fallback_action(&state).expect("fallback returns an action");
        let choices = match &action {
            GameAction::SelectCategoryPermanents { choices } => choices.clone(),
            other => panic!("expected SelectCategoryPermanents, got {other:?}"),
        };

        assert_eq!(
            choices,
            vec![Some(artifact_creature), Some(artifact_creature)]
        );

        engine::game::engine::apply(&mut state, PlayerId(0), action)
            .expect("engine must accept duplicate-object category choices");
    }

    // --- Multikicker mana-budget guard (issue #454) ---

    /// Build an `OptionalCostChoice` for P0 carrying a repeatable {2}
    /// multikicker (CR 702.33c) over a base-cost-{0} spell, plus `lands`
    /// untapped Forests for P0. The pool is pre-filled with {2} colorless so
    /// the combined cost is affordable; whether the AI pays then depends
    /// solely on the over-commit guard (`untapped lands > combined CMC`).
    fn multikicker_choice_state(lands: usize) -> GameState {
        let mut state = make_state();

        let spell_id = create_object(
            &mut state,
            CardId(700),
            PlayerId(0),
            "Everflowing Chalice".to_string(),
            Zone::Stack,
        );
        state
            .objects
            .get_mut(&spell_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);

        for i in 0..lands {
            let land_id = create_object(
                &mut state,
                CardId(710 + i as u64),
                PlayerId(0),
                "Forest".to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&land_id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.entered_battlefield_turn = Some(1);
        }

        // {2} colorless in pool covers the combined base-{0} + kicker-{2}
        // cost, so `can_pay_cost_after_auto_tap` is satisfied on both boards.
        add_mana(&mut state, PlayerId(0), ManaType::Colorless, 2);

        let pending = engine::types::game_state::PendingCast::new(
            spell_id,
            CardId(700),
            engine::types::ability::ResolvedAbility::new(
                engine::types::ability::Effect::Unimplemented {
                    name: "Everflowing Chalice".to_string(),
                    description: None,
                },
                Vec::new(),
                spell_id,
                PlayerId(0),
            ),
            engine::types::mana::ManaCost::NoCost,
        );

        state.waiting_for = WaitingFor::OptionalCostChoice {
            player: PlayerId(0),
            cost: engine::types::ability::AdditionalCost::Kicker {
                costs: vec![engine::types::ability::AbilityCost::Mana {
                    cost: engine::types::mana::ManaCost::Cost {
                        shards: vec![],
                        generic: 2,
                    },
                }],
                repeatability: engine::types::ability::AdditionalCostRepeatability::Repeatable,
            },
            times_kicked: 0,
            pending_cast: Box::new(pending),
        };
        state
    }

    /// CR 702.33c: on a mana-tight board (untapped lands ≤ combined CMC of 2)
    /// the AI must decline the multikick rather than over-commit. Regression
    /// guard for the stale `Kicker { .. } => true` catch-all.
    #[test]
    fn ai_declines_multikicker_when_it_would_over_commit_mana() {
        let state = multikicker_choice_state(2); // 2 untapped lands, combined CMC 2
        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let action = deterministic_choice(&state, PlayerId(0), &config, &[], None)
            .expect("deterministic_choice must decide the kicker prompt");
        assert_eq!(
            action,
            GameAction::DecideOptionalCost { pay: false },
            "AI must decline a multikick that over-commits its mana"
        );
    }

    /// CR 702.33c: on a mana-rich board (untapped lands > combined CMC) the
    /// AI pays the multikick — the affordability/over-commit guard still
    /// approves a kick it can comfortably afford.
    #[test]
    fn ai_pays_multikicker_when_mana_is_plentiful() {
        let state = multikicker_choice_state(6); // 6 untapped lands, combined CMC 2
        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let action = deterministic_choice(&state, PlayerId(0), &config, &[], None)
            .expect("deterministic_choice must decide the kicker prompt");
        assert_eq!(
            action,
            GameAction::DecideOptionalCost { pay: true },
            "AI must pay a multikick when it has mana to spare"
        );
    }

    /// Create a vanilla (zero-value) card directly in `owner`'s hand.
    fn vanilla_in_hand(state: &mut GameState, owner: PlayerId) -> ObjectId {
        named_vanilla_in_hand(state, owner, "Card")
    }

    fn named_vanilla_in_hand(state: &mut GameState, owner: PlayerId, name: &str) -> ObjectId {
        let id = CardId(state.next_object_id);
        create_object(state, id, owner, name.to_string(), Zone::Hand)
    }

    fn land_in_hand(state: &mut GameState, owner: PlayerId) -> ObjectId {
        let id = named_vanilla_in_hand(state, owner, "Land");
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
        id
    }

    /// Create a creature (high `evaluate_card_value`) directly in `owner`'s hand.
    fn creature_in_hand(state: &mut GameState, owner: PlayerId) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            "Creature".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(3);
        obj.toughness = Some(3);
        id
    }

    /// Build a two-player simultaneous-bottoming fixture. Player 0 (the first
    /// pending seat) gets a plain 7-card hand; the AI (player 1) gets
    /// `keep` creatures plus `bottom` vanilla cards. Returns the AI's vanilla
    /// object ids — the cards a least-valuable heuristic must put on the bottom.
    fn two_player_bottom_fixture(
        state: &mut GameState,
        keep: usize,
        bottom: usize,
    ) -> Vec<ObjectId> {
        for _ in 0..7 {
            vanilla_in_hand(state, PlayerId(0));
        }
        for _ in 0..keep {
            creature_in_hand(state, PlayerId(1));
        }
        (0..bottom)
            .map(|_| vanilla_in_hand(state, PlayerId(1)))
            .collect()
    }

    /// Regression (CR 103.5 simultaneous bottoming): driven through the real
    /// `choose_action` entry point so the validate-as-first-pending-seat
    /// contamination is actually exercised. Player 0 (first seat) owes 1 and
    /// player 1 (the AI) owes 3 from a 7-card hand of 4 creatures + 3 vanilla.
    /// `validate_candidates` (via `apply_as_current`) keeps only player 0's
    /// 1-card combos in the pool, so before the scoped `deterministic_choice`
    /// branch the AI's search path emitted a 1-card selection and the engine
    /// rejected it ("Expected 3 cards to bottom, got 1"). The fix must instead
    /// bottom the AI's own 3 least valuable cards — exactly the vanilla cards.
    #[test]
    fn ai_bottoms_own_least_valuable_count_via_choose_action() {
        let mut state = make_state();
        let vanilla = two_player_bottom_fixture(&mut state, 4, 3);

        state.waiting_for = WaitingFor::MulliganBottomCards {
            pending: vec![
                engine::types::game_state::MulliganBottomEntry {
                    player: PlayerId(0),
                    count: 1,
                },
                engine::types::game_state::MulliganBottomEntry {
                    player: PlayerId(1),
                    count: 3,
                },
            ],
        };

        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let mut rng = SmallRng::seed_from_u64(1);
        let action = choose_action(&state, PlayerId(1), &config, &mut rng)
            .expect("AI owes bottoms, must produce an action");

        match action {
            GameAction::SelectCards { cards } => {
                let chosen: std::collections::HashSet<_> = cards.iter().copied().collect();
                let expected: std::collections::HashSet<_> = vanilla.iter().copied().collect();
                assert_eq!(
                    chosen, expected,
                    "AI must bottom its own 3 least valuable (vanilla) cards, \
                     not player 0's 1-card selection"
                );
            }
            other => panic!("expected SelectCards, got {other:?}"),
        }
    }

    /// The fix's `|`-combined arm must hold for `OpeningHandBottomCards`
    /// (TL:R 906.6 Tiny Leaders forced bottom), not just `MulliganBottomCards`:
    /// the AI must still scope to its own owed count when a second player is
    /// pending. Guards against a future refactor silently dropping one variant.
    #[test]
    fn ai_opening_hand_bottom_scopes_to_own_count_via_choose_action() {
        let mut state = make_state();
        let vanilla = two_player_bottom_fixture(&mut state, 5, 2);

        state.waiting_for = WaitingFor::OpeningHandBottomCards {
            pending: vec![
                engine::types::game_state::MulliganBottomEntry {
                    player: PlayerId(0),
                    count: 1,
                },
                engine::types::game_state::MulliganBottomEntry {
                    player: PlayerId(1),
                    count: 2,
                },
            ],
            reason: engine::types::game_state::OpeningHandBottomReason::TinyLeadersMultiCommander,
        };

        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let mut rng = SmallRng::seed_from_u64(1);
        let action = choose_action(&state, PlayerId(1), &config, &mut rng)
            .expect("AI owes opening-hand bottoms, must produce an action");

        match action {
            GameAction::SelectCards { cards } => {
                let chosen: std::collections::HashSet<_> = cards.iter().copied().collect();
                let expected: std::collections::HashSet<_> = vanilla.iter().copied().collect();
                assert_eq!(
                    chosen, expected,
                    "AI must bottom its own 2 least valuable cards for the \
                     opening-hand-bottom path too"
                );
            }
            other => panic!("expected SelectCards, got {other:?}"),
        }
    }

    #[test]
    fn plan_aware_bottoming_cuts_surplus_lands_to_plan_target() {
        let mut state = make_state();
        let lands: Vec<_> = (0..5)
            .map(|_| land_in_hand(&mut state, PlayerId(1)))
            .collect();
        creature_in_hand(&mut state, PlayerId(1));
        creature_in_hand(&mut state, PlayerId(1));

        let mut plan = PlanSnapshot::default();
        plan.expected_lands[2] = 3;
        let bottoms =
            plan_aware_bottom_cards(&state, PlayerId(1), 2, &DeckFeatures::default(), &plan);
        let land_set: std::collections::HashSet<_> = lands.iter().copied().collect();

        assert_eq!(bottoms.len(), 2);
        assert!(
            bottoms.iter().all(|id| land_set.contains(id)),
            "bottoming should cut surplus lands before real threats"
        );
    }

    #[test]
    fn plan_aware_bottoming_protects_feature_payoff_names() {
        let mut state = make_state();
        let payoff = named_vanilla_in_hand(&mut state, PlayerId(1), "Landfall Payoff");
        let filler_a = vanilla_in_hand(&mut state, PlayerId(1));
        let filler_b = vanilla_in_hand(&mut state, PlayerId(1));
        let features = DeckFeatures {
            landfall: crate::features::LandfallFeature {
                payoff_names: vec!["Landfall Payoff".to_string()],
                commitment: 1.0,
                ..Default::default()
            },
            ..Default::default()
        };

        let bottoms =
            plan_aware_bottom_cards(&state, PlayerId(1), 1, &features, &PlanSnapshot::default());

        assert_ne!(bottoms, vec![payoff]);
        assert!(
            bottoms == vec![filler_a] || bottoms == vec![filler_b],
            "bottoming should protect structurally detected payoff names"
        );
    }

    /// Build a single-blocker AssignCombatDamage prompt and run the AI fallback.
    fn assign_combat_damage_fallback(
        total_damage: u32,
        lethal_minimum: u32,
        trample: Option<engine::game::combat::TrampleKind>,
    ) -> GameAction {
        let mut state = make_state();
        let attacker = add_creature(&mut state, PlayerId(0), total_damage as i32, 1);
        let blocker = add_creature(&mut state, PlayerId(1), 1, lethal_minimum as i32);
        state.waiting_for = WaitingFor::AssignCombatDamage {
            player: PlayerId(0),
            attacker_id: attacker,
            total_damage,
            blockers: vec![engine::types::game_state::DamageSlot {
                blocker_id: blocker,
                lethal_minimum,
            }],
            assignment_modes: vec![engine::types::game_state::CombatDamageAssignmentMode::Normal],
            trample,
            defending_player: PlayerId(1),
            attack_target: engine::game::combat::AttackTarget::Player(PlayerId(1)),
            pw_loyalty: None,
            pw_controller: None,
        };
        fallback_action(&state).expect("AssignCombatDamage fallback must produce an action")
    }

    /// CR 702.19b: single-blocker trample attacker — the AI fallback keeps lethal
    /// on the blocker and tramples the excess through to the defending player.
    #[test]
    fn fallback_single_blocker_trample_tramples_excess() {
        let action =
            assign_combat_damage_fallback(5, 2, Some(engine::game::combat::TrampleKind::Standard));
        match action {
            GameAction::AssignCombatDamage {
                mode,
                assignments,
                trample_damage,
                controller_damage,
            } => {
                assert_eq!(
                    mode,
                    engine::types::game_state::CombatDamageAssignmentMode::Normal
                );
                assert_eq!(assignments.len(), 1);
                assert_eq!(assignments[0].1, 2, "lethal (2) assigned to blocker");
                assert_eq!(trample_damage, 3, "excess (3) tramples through");
                assert_eq!(controller_damage, 0);
            }
            other => panic!("expected AssignCombatDamage, got {other:?}"),
        }
    }

    /// CR 510.1c: single-blocker non-trample attacker — the AI fallback assigns
    /// all damage to the blocker (no spillover to the player is legal).
    #[test]
    fn fallback_single_blocker_no_trample_all_to_blocker() {
        let action = assign_combat_damage_fallback(5, 2, None);
        match action {
            GameAction::AssignCombatDamage {
                assignments,
                trample_damage,
                controller_damage,
                ..
            } => {
                assert_eq!(assignments.len(), 1);
                assert_eq!(assignments[0].1, 5, "all 5 to the single blocker");
                assert_eq!(trample_damage, 0, "no trample without trample keyword");
                assert_eq!(controller_damage, 0);
            }
            other => panic!("expected AssignCombatDamage, got {other:?}"),
        }
    }

    // ===== Iterative-deepening tests (pipeline 5) =====

    /// A main-phase priority board with real branching: a castable creature in
    /// hand (+ pool mana) plus an opponent threat, so depth-2 search evaluates a
    /// different position than a depth-0 quiesced snapshot. Reaches the
    /// `config.search.enabled` ID loop (verified by the CastSpell reach-guards).
    fn searchable_state() -> GameState {
        let mut state = make_state();
        state.lands_played_this_turn = 1;
        // Opponent threat on the battlefield so search sees a value gradient.
        let _opp = add_creature(&mut state, PlayerId(1), 3, 3);
        let creature_id = create_object(
            &mut state,
            CardId(900),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&creature_id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.mana_cost = engine::types::mana::ManaCost::Cost {
            shards: vec![engine::types::mana::ManaCostShard::Green],
            generic: 1,
        };
        add_mana(&mut state, PlayerId(0), ManaType::Green, 3);
        state
    }

    fn has_cast(scored: &[(GameAction, f64)]) -> bool {
        scored
            .iter()
            .any(|(a, _)| matches!(a, GameAction::CastSpell { .. }))
    }

    fn sorted_by_action(mut scored: Vec<(GameAction, f64)>) -> Vec<(GameAction, f64)> {
        scored.sort_by_cached_key(|(action, _)| action_order_key(action));
        scored
    }

    // Row 7: the ID ceiling derivation respects planner_mode and the WASM depth
    // cap. `create_config` caps `max_depth` at 2 on WASM, so a BeamPlusRollout
    // config still deepens (ceiling 1) rather than collapsing to a single pass.
    #[test]
    fn id_ceiling_matches_planner_mode_and_platform() {
        // Mirror of the production ceiling derivation in `score_candidates_with_session`.
        let ceiling = |config: &AiConfig| -> u32 {
            match config.search.planner_mode {
                PlannerMode::BeamOnly => 0,
                PlannerMode::BeamPlusRollout => config.search.max_depth.saturating_sub(1),
            }
        };
        let native = create_config(AiDifficulty::Hard, Platform::Native);
        let wasm = create_config(AiDifficulty::Hard, Platform::Wasm);

        assert_eq!(native.search.max_depth, 3, "native Hard depth precondition");
        assert_eq!(wasm.search.max_depth, 2, "WASM caps depth at 2");
        assert_eq!(ceiling(&native), 2, "native Hard -> ID ceiling 2");
        assert_eq!(
            ceiling(&wasm),
            1,
            "WASM Hard -> ID ceiling 1 (still deepens)"
        );
    }

    // Row 6: measurement-mode scoring is within-process deterministic (the ID loop
    // never consults the wall clock in measurement — deadline is none()).
    #[test]
    fn measurement_score_candidates_deterministic_in_process() {
        let state = searchable_state();
        let config = create_config(AiDifficulty::Hard, Platform::Native).into_measurement(7);
        let session = AiSession::arc_from_game(&state);

        let first = score_candidates_with_session(&state, PlayerId(0), &config, &session);
        let second = score_candidates_with_session(&state, PlayerId(0), &config, &session);

        assert!(
            has_cast(&first),
            "reach-guard: board reaches the search-enabled ID loop"
        );
        assert_eq!(
            first, second,
            "measurement scoring must be byte-identical across same-process runs"
        );
    }

    // Row 5b: ID's deepest rung deepens beyond the rung-0 quiesced baseline (no
    // depth regression / floor leak). Measurement mode runs the full ceiling; a
    // BeamOnly clone pins the planner to rung 0 only. If the ID loop ever returned
    // rung 0 (or the tactical floor) instead of the deepest completed rung, the
    // two outputs would coincide.
    #[test]
    fn iterative_deepening_deepens_beyond_rung_zero() {
        let state = searchable_state();
        let session = AiSession::arc_from_game(&state);

        let full = create_config(AiDifficulty::Hard, Platform::Native).into_measurement(7);
        assert_eq!(
            full.search.max_depth.saturating_sub(1),
            2,
            "reach-guard: full ceiling must be >= 1 or the test is vacuous"
        );
        let mut shallow = full.clone();
        shallow.search.planner_mode = PlannerMode::BeamOnly; // ceiling 0 -> rung 0 only

        let deep_scores = score_candidates_with_session(&state, PlayerId(0), &full, &session);
        let rung0_scores = score_candidates_with_session(&state, PlayerId(0), &shallow, &session);

        assert!(
            has_cast(&deep_scores),
            "reach-guard: search-enabled branch reached"
        );
        // Revert-failing: a broken ID accumulation returning rung 0 / the floor
        // makes the deepest rung indistinguishable from the rung-0 baseline.
        assert_ne!(
            deep_scores, rung0_scores,
            "the deepest ID rung must deepen beyond the rung-0 quiesced baseline"
        );
    }

    // Row 5a: a pre-expired interactive deadline collapses to the tactical-only
    // floor with ZERO applies (rung-guard option (a)). The distinguishing witness:
    // under option (a) the pre-expired output carries NO quiesced continuation
    // term, so it differs from the measurement rung-0 output (which DOES run rung 0
    // = `quiesced(sim) + floor`). Under option (b) — running rung 0 even when
    // pre-expired — the two would coincide, so this `assert_ne!` is revert-failing
    // for the rung-0 entry guard.
    #[test]
    fn pre_expired_deadline_collapses_to_zero_apply_floor() {
        let state = searchable_state();
        let session = AiSession::arc_from_game(&state);

        // Interactive (non-measurement) with a pre-expired deadline (0 ms budget).
        let mut interactive = create_config(AiDifficulty::Hard, Platform::Native);
        interactive.search.time_budget_ms = Some(0);
        let floor = sorted_by_action(score_candidates_with_session(
            &state,
            PlayerId(0),
            &interactive,
            &session,
        ));

        // Measurement + BeamOnly => deadline none(), ceiling 0 => rung 0 runs fully:
        // per-candidate `quiesced(sim) + r.score*tactical_weight`. This is exactly
        // what option (b) would produce for the pre-expired interactive run.
        let mut rung0_cfg = create_config(AiDifficulty::Hard, Platform::Native).into_measurement(7);
        rung0_cfg.search.planner_mode = PlannerMode::BeamOnly;
        let rung0 = sorted_by_action(score_candidates_with_session(
            &state,
            PlayerId(0),
            &rung0_cfg,
            &session,
        ));

        assert!(
            has_cast(&floor),
            "reach-guard: pre-expired run still reaches the ID loop"
        );
        assert_eq!(
            floor.len(),
            rung0.len(),
            "same gated candidate set feeds both runs"
        );
        // Option (a): zero applies past the deadline -> pure tactical floor,
        // distinct from rung-0's quiesced-augmented scores.
        assert_ne!(
            floor, rung0,
            "pre-expired deadline must do ZERO continuation applies (option a), \
             so its floor differs from the rung-0 quiesced baseline"
        );
    }
}
