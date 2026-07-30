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
    CastOfferKind, CompanionDeclaration, CostResume, GameState, ManaChoice, ManaChoicePrompt,
    MulliganDecisionPhase, PendingMulliganAction, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

use crate::card_value::{cmp_keep, intrinsic_value, keep_key};
use crate::cast_facts::cast_facts_for_action;
use crate::combat_ai::{choose_attackers_with_targets_with_profile, choose_blockers_with_profile};
use crate::config::{AiConfig, PlannerMode, ThreatAwareness};
use crate::context::AiContext;
use crate::features::DeckFeatures;
use crate::plan::{PlanSnapshot, PlanState};
use crate::planner::{
    apply_candidate, prepare_payment_candidates, BeamContinuationPlanner, ContinuationPlanner,
    PlannerServices, PreparedCandidate, RankedCandidate, RungStat, SearchBudget,
};
use crate::policies::context::{PolicyContext, SearchDepth};
use crate::policies::copy_value::score_legend_rule_keep;
use crate::policies::strategy_helpers::{cmp_sacrifice, sacrifice_key};
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
// Iterative deepening repeatedly serializes and simulates the whole game state.
// A token-heavy battlefield is already expensive well before a thousand objects,
// so keep normal search for ordinary games while routing pathological boards
// through the bounded, policy-scored priority path.
const LARGE_BOARD_FAST_PRIORITY_BATTLEFIELD_OBJECTS: usize = 128;

fn has_large_battlefield(state: &GameState) -> bool {
    state.battlefield.len() >= LARGE_BOARD_FAST_PRIORITY_BATTLEFIELD_OBJECTS
}

/// CR 701.21a: choose which permanents to sacrifice for a mandatory
/// spell-effect sacrifice.
///
/// `strategy_helpers::sacrifice_cost` is the single battlefield give-up
/// authority — the same one `SacrificeValuePolicy` uses. Scoring these with the
/// zone-agnostic card scalar instead made this path land-blind and, because
/// `deterministic_choice` short-circuits the policy registry, the land-blind
/// answer won.
///
/// The ordering is **total**, via `sacrifice_key` / `cmp_sacrifice`: the
/// land-vs-nonland axis is a tier, not a scalar gap. `sort_by` is stable, so
/// ranking on the bare `f64` left equal scores to be decided by enumeration
/// order — and `sacrifice_land_penalty` is CMA-ES-trained, so a trained profile
/// could restore that tie under the noncreature cap at any time. Within a tier
/// the scalar still decides. This mirrors `card_value::cmp_keep` at the cleanup
/// discard seam, which is the identical problem.
fn pick_lowest_value_sacrifices(
    state: &GameState,
    cards: &[ObjectId],
    count: usize,
    penalties: &crate::config::PolicyPenalties,
) -> Vec<ObjectId> {
    let mut scored: Vec<_> = cards
        .iter()
        .map(|&id| (id, sacrifice_key(state, id, penalties)))
        .collect();
    scored.sort_by(|a, b| cmp_sacrifice(&a.1, &b.1));
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
        WaitingFor::OpeningHandBottomCards { pending, .. }
            if !pending.iter().any(|e| e.player == ai_player) =>
        {
            return None;
        }
        _ => {}
    }

    if let Some(action) = random_card_predicate_guess(state, ai_player, rng) {
        return Some(action);
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

    // CR 608.2d (hidden information): the guesser has no legal access to the
    // committed value / chosen-card identity — it is genuinely a guess. The AI
    // MUST NOT score guess branches via `score_candidates` (eval/search runs on
    // the UNFILTERED GameState and would read the secret, always guessing
    // correctly). Uniform random is rules-fair and the information-theoretic
    // optimum, and uses the caller-owned RNG so seeded measurement runs remain
    // reproducible. Parallel to the TributeChoice / SearchChoice / ChooseManaColor
    // pre-emptions above.
    if let WaitingFor::OpponentGuess { ref options, .. } = state.waiting_for {
        use rand::seq::IndexedRandom;
        if let Some(choice) = options.choose(rng) {
            return Some(GameAction::ChooseOption {
                choice: choice.clone(),
            });
        }
    }

    if let Some(action) = fast_priority_action(state, ai_player, config, session) {
        return Some(action);
    }

    let mut scored = score_candidates_with_session(state, ai_player, config, session);
    if scored.is_empty() {
        // No valid candidates from search — fall back to a safe escape action
        // so the game never deadlocks waiting for the AI.
        return fallback_action(state, config);
    }
    // Issue #4878: total order before softmax so equal scores never depend on
    // HashSet/HashMap allocation order.
    scored.sort_by(|a, b| a.0.cmp_stable(&b.0));
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

fn random_card_predicate_guess(
    state: &GameState,
    ai_player: PlayerId,
    rng: &mut impl Rng,
) -> Option<GameAction> {
    let WaitingFor::NamedChoice {
        player,
        choice_type,
        options,
        source: Some(source),
        persist_player: _,
    } = &state.waiting_for
    else {
        return None;
    };
    if *player != ai_player || !choice_type.is_card_predicate_guess() {
        return None;
    }
    if source.prompt.controller == ai_player || options.is_empty() {
        return None;
    }
    let index = rng.random_range(0..options.len());
    let choice = options[index].clone();
    tracing::info!(
        target: "phase_ai::choice",
        ai_player = ai_player.0,
        source_id = source.prompt.identity.reference.object_id.0,
        source_name = %source.prompt.display_name,
        guess = %choice,
        "AI randomly guessed card predicate"
    );
    Some(GameAction::ChooseOption { choice })
}

fn fast_priority_action(
    state: &GameState,
    ai_player: PlayerId,
    config: &AiConfig,
    session: &Arc<AiSession>,
) -> Option<GameAction> {
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
    low_value_priority_pass_from_actions(state, ai_player, &actions).or_else(|| {
        large_board_main_phase_fast_action_from_actions(state, ai_player, &actions, config, session)
    })
}

fn large_board_main_phase_has_no_development_sources(
    state: &GameState,
    ai_player: PlayerId,
) -> bool {
    if !has_large_battlefield(state)
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
    config: &AiConfig,
    session: &Arc<AiSession>,
) -> Option<GameAction> {
    let WaitingFor::Priority { player } = state.waiting_for else {
        return None;
    };
    if player != ai_player
        || !has_large_battlefield(state)
        || state.active_player != ai_player
        || !state.stack.is_empty()
        || !matches!(state.phase, Phase::PreCombatMain | Phase::PostCombatMain)
    {
        return None;
    }

    // Deep search over a token-heavy own main phase is not a bounded operation.
    // Retain the fast path, but score the exact engine-legal candidates through
    // the tactical registry so land sequencing and other safety policies still
    // participate. Spell mana value remains the deterministic baseline that
    // this shortcut historically used; policies may adjust or reject it.
    let decision = build_decision_context(state);
    let context = build_ai_context_with_session(state, ai_player, config, Arc::clone(session));
    let policies = PolicyRegistry::shared();

    let candidates = decision
        .candidates
        .iter()
        // `flat_priority_actions` is the engine's complete legal-action set;
        // retain only those candidates before applying the same tactical and
        // loop-safety gates that the normal scoring path uses.
        .filter(|candidate| actions.contains(&candidate.action))
        .cloned()
        .collect();
    let candidates = gate_candidates(state, &decision, candidates, ai_player, config, &context);

    candidates
        .into_iter()
        .filter(|candidate| {
            priority_action_is_allowed_by_loop_guards(state, ai_player, &candidate.candidate.action)
        })
        .map(|candidate| {
            let penalty = candidate.penalty;
            let candidate = candidate.candidate;
            let baseline = match &candidate.action {
                GameAction::CastSpell { object_id, .. } => intrinsic_value(state, *object_id),
                _ => 0.0,
            };
            let tactical = policies.score(&PolicyContext {
                state,
                decision: &decision,
                candidate: &candidate,
                ai_player,
                config,
                context: &context,
                cast_facts: cast_facts_for_action(state, &candidate.action, ai_player),
                search_depth: SearchDepth::Root,
            });
            (candidate.action, baseline + tactical + penalty)
        })
        .filter(|(_, score)| score.is_finite())
        .max_by(|(left_action, left_score), (right_action, right_score)| {
            left_score
                .partial_cmp(right_score)
                .unwrap_or(Ordering::Equal)
                .then_with(|| right_action.cmp_stable(left_action))
        })
        .map(|(action, _)| action)
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
/// Deadlock-safe escape hatch when tactical scoring cannot produce an action.
/// The WASM bridge exposes this for client AI-controller escape — callers must
/// not invent actions from legal-action enumeration order (#6393).
///
/// `config` supplies policy penalties used by selection escapes (e.g. sacrifice
/// value ordering); difficulty/search knobs are unused here.
pub fn fallback_action(state: &GameState, config: &AiConfig) -> Option<GameAction> {
    // CR 601.2c: A spell's target step must use the engine's current legal
    // target list. `target_slots` is a historical snapshot and can be stale
    // after earlier selections; if no current legal action remains, abort the
    // in-flight cast rather than fabricating an illegal required-target skip.
    if matches!(state.waiting_for, WaitingFor::TargetSelection { .. }) {
        return engine::ai_support::legal_actions(state)
            .into_iter()
            .find(|action| matches!(action, GameAction::ChooseTarget { .. }))
            .or(Some(GameAction::CancelCast));
    }

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

        // CR 732.2a: if tactical scoring found no choice, take the conservative legal escape
        // from the engine's candidate set. The AI is never forced to propose a shortcut.
        WaitingFor::LoopShortcut { .. } => engine::ai_support::legal_actions(state)
            .into_iter()
            .find(|action| matches!(action, GameAction::DeclineShortcut)),
        // CR 732.2a: the finite pre-cast family has the same conservative
        // proposer fallback as the legacy shortcut. Ask the engine for its
        // issued decline capability instead of fabricating a route response.
        WaitingFor::PrecastCopyShortcutOffer { .. } => engine::ai_support::legal_actions(state)
            .into_iter()
            .find(|action| {
                matches!(
                    action,
                    GameAction::PrecastCopyShortcut {
                        response: engine::types::actions::PrecastCopyShortcutResponse::Decline,
                        ..
                    }
                )
            }),
        // PR-7 Phase 4c (LOW-2): self-preservation via the single-authority
        // `smart_shortcut_response` — Shorten when the polled player has a meaningful
        // way to break the loop, else Accept.
        WaitingFor::RespondToShortcut { player, .. } => Some(GameAction::RespondToShortcut {
            response: engine::ai_support::smart_shortcut_response(state, *player),
        }),
        // CR 732.2b/c: use the same meaningful-priority probe as the legacy
        // responder. A finite route can only shorten at its engine-issued
        // breakpoint, so translate a legacy-style Shorten to that concrete
        // capability; if none is issued, accepting is the only legal fallback.
        WaitingFor::RespondToPrecastCopyShortcut {
            player,
            epoch,
            breakpoint_ids,
            ..
        } => {
            let response = match engine::ai_support::smart_shortcut_response(state, *player) {
                engine::analysis::loop_check::ShortcutResponse::Shorten { .. } => {
                    breakpoint_ids.first().map_or(
                        engine::types::actions::PrecastCopyShortcutResponse::Accept,
                        |breakpoint_id| {
                            engine::types::actions::PrecastCopyShortcutResponse::Shorten {
                                breakpoint_id: *breakpoint_id,
                            }
                        },
                    )
                }
                engine::analysis::loop_check::ShortcutResponse::Accept => {
                    engine::types::actions::PrecastCopyShortcutResponse::Accept
                }
            };
            Some(GameAction::PrecastCopyShortcut {
                epoch: *epoch,
                response,
            })
        }

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

        // CR 701.42b / CR 508.4: deadlock-safe deterministic fallbacks. Normal
        // public `choose_action` evaluates these legal actions through search;
        // when time expires, preserve the engine's canonical physical-pair
        // authority before falling back to the first legal live-name choice.
        WaitingFor::MeldPairChoice { choices, .. } => choices
            .iter()
            .find(|choice| engine::game::meld::is_canonical_physical_meld_pair(state, choice))
            .or_else(|| choices.first())
            .map(|choice| GameAction::ChooseMeldPair {
                source_id: choice.source_id,
                partner_id: choice.partner_id,
            }),
        WaitingFor::MeldAttackTargetChoice { valid_targets, .. } => valid_targets
            .first()
            .copied()
            .map(|target| GameAction::ChooseEntryAttackTarget { target }),

        // TargetSelection returned from the early current-legal-target branch.
        WaitingFor::TargetSelection { .. } => unreachable!("handled before fallback match"),

        // TriggerTargetSelection is not a pending cast — the trigger is
        // already on the stack. ChooseTarget { target: None } signals
        // "no legal target" and causes the trigger to fizzle (CR 608.2b).
        WaitingFor::TriggerTargetSelection { .. } => {
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
            cards: pick_lowest_value_sacrifices(state, cards, *count, &config.policy_penalties),
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

        // CR 901.15: Planar deck arrange requires exactly `keep_on_top` cards
        // on top — pick the highest-valued looked-at planes.
        WaitingFor::ArrangePlanarDeckTopChoice {
            cards, keep_on_top, ..
        } => {
            let mut scored: Vec<_> = cards
                .iter()
                .map(|&id| (id, intrinsic_value(state, id)))
                .collect();
            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            Some(GameAction::SelectCards {
                cards: scored
                    .iter()
                    .take(*keep_on_top)
                    .map(|(id, _)| *id)
                    .collect(),
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

        // CR 103.5 + 103.5b: Mulligan default. In `Declare`, keep unless the AI
        // has a Serum Powder in hand, in which case use it first (auto-heuristic
        // — see `first_serum_powder_in_hand`). In `BottomCards`, submit an empty
        // `SelectCards` as the deadlock-safe escape hatch.
        WaitingFor::MulliganDecision { pending, .. } => {
            let entry = pending.first()?;
            match &entry.phase {
                MulliganDecisionPhase::Declare => {
                    Some(match first_serum_powder_in_hand(state, entry.player) {
                        Some(object_id) => GameAction::MulliganDecision {
                            choice: MulliganChoice::UseSerumPowder { object_id },
                        },
                        None => GameAction::MulliganDecision {
                            choice: MulliganChoice::Keep,
                        },
                    })
                }
                MulliganDecisionPhase::BottomCards { .. } => {
                    Some(GameAction::SelectCards { cards: Vec::new() })
                }
            }
        }
        WaitingFor::OpeningHandBottomCards { .. } => {
            Some(GameAction::SelectCards { cards: Vec::new() })
        }

        // Named choice: prefer an engine-legal ChooseOption. CardName prompts
        // intentionally keep `options` empty and synthesize candidates from
        // `all_card_names` (#6248); reading `options.first()` softlocks after
        // restore when rehydrate succeeded but options stayed empty (#6393).
        WaitingFor::NamedChoice { .. } => engine::ai_support::legal_actions(state)
            .into_iter()
            .find(|a| matches!(a, GameAction::ChooseOption { .. })),

        // CR 608.2d: opponent-guess fallback — any printed guess is legal. The
        // hidden-info determinization in `choose_action` already pre-empts this
        // for the live AI; this is only the deadlock-safe escape hatch.
        WaitingFor::OpponentGuess { options, .. } => {
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
        // CR 119.7 + CR 119.8: option 0 is always the identity ("keep current totals")
        // assignment and always legal — a safe deterministic fallback.
        WaitingFor::RedistributeLifeTotals { .. } => {
            Some(GameAction::SubmitLifeRedistribution { option_index: 0 })
        }

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

        // CR 608.2d: "an opponent chooses …" — the controller picks which
        // opponent makes the zone choice; fall back to the first candidate.
        WaitingFor::ChooseFromZoneOpponentChooser { candidates, .. } => candidates
            .first()
            .map(|&opponent| GameAction::ChooseZoneOpponentChooser { opponent }),

        // CR 601.2c + CR 115.1: "of an opponent's choice" announcer — the
        // controller picks which opponent announces; fall back to the first.
        WaitingFor::ChooseAnnouncingOpponent { candidates, .. } => candidates
            .first()
            .map(|&opponent| GameAction::ChooseAnnouncingOpponent { opponent }),

        // CR 702.174a: Gift recipient — fall back to the first candidate.
        WaitingFor::ChooseGiftRecipient { candidates, .. } => candidates
            .first()
            .map(|&opponent| GameAction::ChooseGiftRecipient { opponent }),

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

        // CR 101.4 + CR 707.2: EachPlayerCopyChosen selection — an empty pick is
        // illegal (min >= 1), so pick the first `min` eligible objects.
        WaitingFor::EachPlayerCopyChosenSelection { eligible, min, .. } => {
            let targets: Vec<_> = eligible
                .iter()
                .take((*min).max(1) as usize)
                .cloned()
                .collect();
            if targets.is_empty() {
                None
            } else {
                Some(GameAction::SelectTargets { targets })
            }
        }

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
        WaitingFor::CompanionReveal { .. } => Some(GameAction::DeclareCompanion {
            choice: CompanionDeclaration::Decline,
        }),

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
        WaitingFor::ChooseManaColor { choice, .. } => match choice {
            ManaChoicePrompt::SingleColor { options } => {
                options
                    .first()
                    .copied()
                    .map(|color| GameAction::ChooseManaColor {
                        choice: ManaChoice::SingleColor(color),
                        count: 1,
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
        },
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
        WaitingFor::PayCost {
            resume: CostResume::Resolution,
            ..
        } => engine::ai_support::legal_actions(state)
            .into_iter()
            .find(|action| matches!(action, GameAction::SelectCards { .. })),

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

        // CR 101.4 + CR 701.21a: choose a valid exact-size baseline subset.
        WaitingFor::KeepExactPermanentsChoice {
            eligible,
            required_count,
            ..
        } => {
            let kept = eligible.iter().copied().take(*required_count).collect();
            Some(GameAction::ChooseKeptPermanents { kept })
        }

        // CR 700.3: Pile-separation fallbacks — empty pile-A partition (every
        // object goes to derived pile B) is the simplest legal partition, and
        // pile A is the default choice for the chooser. Tactical AI override
        // happens through legal_actions; this is the safety net.
        WaitingFor::SeparatePilesChooseOpponent { candidates, .. } => candidates
            .first()
            .map(|&opp| GameAction::ChoosePileOpponent { opponent: opp }),
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
    let mut out = finalize_mean(acc, counts, k as usize);
    // Issue #4878: canonical order after K-sample merge (measurement + play).
    out.sort_by(|a, b| a.0.cmp_stable(&b.0));
    out
}

/// Reject repeatable priority actions that would re-enter known AI loops.
///
/// `cancelled_casts` and `pending_activations` clear on PassPriority;
/// `activated_abilities_this_turn` clears on turn change. CR 117.1b permits
/// unbounded activation at priority, so the activation and same-card cast caps
/// are AI-pathology safeguards rather than game rules.
fn priority_action_is_allowed_by_loop_guards(
    state: &GameState,
    ai_player: PlayerId,
    action: &GameAction,
) -> bool {
    match action {
        GameAction::CastSpell { object_id, .. } => {
            if state.cancelled_casts.contains(object_id) {
                return false;
            }
            // CR 117.1 + #563: `SpellCastRecord.name` preserves the card name
            // after its object left the stack, so identical cards share the cap.
            let candidate_name = state
                .objects
                .get(object_id)
                .map(|object| object.name.as_str())
                .unwrap_or("");
            candidate_name.is_empty()
                || state
                    .spells_cast_this_turn_by_player
                    .get(&ai_player)
                    .map(|history| {
                        history
                            .iter()
                            .filter(|record| record.name == candidate_name)
                            .count()
                    })
                    .unwrap_or(0)
                    < MAX_CASTS_OF_SAME_CARD_PER_TURN
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
    }
}

/// Rank the root beam after validation and gating, retaining an affiliated
/// payment candidate's already-witnessed reducer successor through width
/// truncation. This is the single production seam for root payment ranking;
/// tests exercise it directly to prove the enabled-search beam boundary.
fn rank_root_payment_candidates(
    state: &GameState,
    decision: &engine::ai_support::AiDecisionContext,
    prepared: &[PreparedCandidate],
    gated: &[crate::tactical_gate::GatedCandidate],
    services: &PlannerServices<'_>,
    max_branching: usize,
) -> Vec<RankedCandidate> {
    let mut ranked: Vec<RankedCandidate> = gated
        .iter()
        .map(|gated_candidate| {
            let score = services.tactical_score(
                state,
                decision,
                &gated_candidate.candidate,
                services.ai_player,
                SearchDepth::Root,
            ) + gated_candidate.penalty;
            prepared
                .iter()
                .find(|prepared_candidate| {
                    prepared_candidate.candidate.action == gated_candidate.candidate.action
                })
                .and_then(|prepared_candidate| prepared_candidate.payment_successor.clone())
                .map_or_else(
                    || RankedCandidate::new(gated_candidate.candidate.clone(), score),
                    |successor| {
                        RankedCandidate::with_payment_successor(
                            gated_candidate.candidate.clone(),
                            score,
                            successor,
                        )
                    },
                )
        })
        .collect();
    ranked.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.candidate.action.cmp_stable(&right.candidate.action))
    });
    ranked.truncate(max_branching);
    ranked
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
    if let Some(action) = fast_priority_action(state, ai_player, config, session) {
        return vec![(action, 1.0)];
    }

    let ctx = build_decision_context(state);
    #[cfg(test)]
    let policies = session
        .policy_registry_override
        .as_deref()
        .unwrap_or_else(|| PolicyRegistry::shared());
    #[cfg(not(test))]
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
    let prepared = prepare_payment_candidates(state, ctx.candidates.clone());
    let candidates = services.validate_candidates(
        state,
        prepared
            .iter()
            .map(|candidate| candidate.candidate.clone())
            .collect(),
    );
    let gated = gate_candidates(
        state,
        &ctx,
        candidates,
        ai_player,
        config,
        &services.context,
    );

    let mut gated: Vec<_> = gated
        .into_iter()
        .filter(|candidate| {
            priority_action_is_allowed_by_loop_guards(state, ai_player, &candidate.candidate.action)
        })
        .collect();
    // Issue #4878: deterministic candidate order before scoring / search.
    gated.sort_by(|a, b| a.candidate.action.cmp_stable(&b.candidate.action));

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
    if matches!(
        engine::ai_support::classify_payment_continuation(state),
        engine::ai_support::PaymentContinuationState::NotAffiliated
    ) {
        if let Some(action) =
            deterministic_choice(state, ai_player, config, &actions, Some(&services.context))
        {
            return vec![(action, 1.0)];
        }
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
        let ranked =
            rank_root_payment_candidates(state, &ctx, &prepared, &gated, &services, branching);

        run_iterative_deepening(state, ranked, tactical_weight, config, &mut services)
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
        out.sort_by(|a, b| a.0.cmp_stable(&b.0));
        out
    }
}

/// Runs rung-0..=ceiling iterative deepening over the pre-ranked root beam.
/// Extracted from `score_candidates_core` so tests can construct
/// `PlannerServices`, run the loop, and inspect witness state (`rung_stats`,
/// killers, counters) — mirroring how `tt_hits` is observable via direct
/// `search_value` calls. The pre-rung tactical-only floor, the rung loop, and
/// the acceptance logic all live here; `score_candidates_core` just delegates.
///
/// PV threading (D2) and the rung witness (D3) are the only additions over the
/// pre-extraction behavior; both are no-ops for `rung_stats`/ordering when the
/// beam is a single candidate or the killers are empty.
fn run_iterative_deepening(
    state: &GameState,
    mut ranked: Vec<RankedCandidate>,
    tactical_weight: f64,
    config: &AiConfig,
    services: &mut PlannerServices<'_>,
) -> Vec<(GameAction, f64)> {
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
        let mut budget = SearchBudget::with_deadline(config.search.max_nodes, services.deadline);
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
            let score = if let Some(sim) = r
                .payment_successor
                .clone()
                .or_else(|| apply_candidate(state, &r.candidate))
            {
                let cont = planner.evaluate_after_action(&sim, services, &mut budget);
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
        let accepted = completed && (iter_depth == 0 || !services.deadline.expired());

        // D3: one witness per executed rung (completion + node headroom). A
        // pre-expired deadline breaks at the entry guard above, so zero rungs
        // execute and `rung_stats` stays empty — the honest "no search" trace.
        services.rung_stats.push(RungStat {
            depth: iter_depth,
            completed: accepted,
            nodes_used: budget.nodes_evaluated,
            max_nodes: budget.max_nodes,
        });

        if accepted {
            // D2: thread the principal variation into the NEXT rung. Gated to
            // searched rungs (`iter_depth >= 1`): rung 0's argmax mixes quiesced
            // eval with the tactical term, so rotating on it would change rung
            // 1's order vs today. Rung 1 therefore provably sees today's
            // ordering; divergence begins at rung 2, where it is a legitimate
            // budget-allocation improvement (see `pv_argmax`).
            if iter_depth >= 1 {
                if let Some(pv) = pv_argmax(&rung_scored) {
                    rotate_pv_to_front(&mut ranked, pv);
                }
            }
            best_scored = rung_scored; // deepest fully-completed rung so far
        } else {
            break;
        }
    }

    tracing::debug!(
        rungs = services.rung_stats.len(),
        completed = services.rung_stats.iter().filter(|r| r.completed).count(),
        deepest = services.rung_stats.last().map_or(0, |r| r.depth),
        nodes_used = services
            .rung_stats
            .iter()
            .map(|r| r.nodes_used)
            .sum::<u32>(),
        beta_cutoffs = services.beta_cutoffs,
        killer_orderings = services.killer_orderings,
        "iterative deepening rung summary"
    );

    let mut out = best_scored;
    out.sort_by(|a, b| a.0.cmp_stable(&b.0));
    out
}

/// Deterministic principal-variation selection over a completed rung's scores.
/// Budget-allocation policy, not alpha-beta: root siblings share one per-rung
/// `SearchBudget` (constructed once per rung in `run_iterative_deepening`) and
/// each opens a fresh `(-inf, +inf)` window, so PV-first spends the shared pool
/// on the strongest known candidate before the tail starves — no alpha carries
/// between root siblings.
///
/// NaN-safe: `unwrap_or(Equal)` defers to the `cmp_stable` total order so ties
/// and non-finite scores resolve deterministically, never a bare
/// `max_by(|a, b| a.partial_cmp(b).unwrap())`.
fn pv_argmax(rung_scored: &[(GameAction, f64)]) -> Option<&GameAction> {
    rung_scored
        .iter()
        .max_by(|a, b| {
            a.1.partial_cmp(&b.1)
                .unwrap_or(Ordering::Equal)
                .then_with(|| b.0.cmp_stable(&a.0)) // ties: cmp_stable decides
        })
        .map(|(action, _)| action)
}

/// Stable-rotate the candidate whose action equals `pv` to the front of
/// `ranked`, preserving the relative order of every other candidate. No-op when
/// `pv` is absent (e.g. it was the `-1000.0`-penalized illegal candidate that a
/// later rung will re-validate anyway).
fn rotate_pv_to_front(ranked: &mut Vec<RankedCandidate>, pv: &GameAction) {
    if let Some(idx) = ranked.iter().position(|r| &r.candidate.action == pv) {
        let pv_candidate = ranked.remove(idx);
        ranked.insert(0, pv_candidate);
    }
}

/// Build AI context from the player's deck pool, or a neutral default if unavailable.
/// `pub(crate)` so `crate::test_support::context_with_plans` — the single shared
/// builder for plan-carrying test contexts — can reach it.
pub(crate) fn build_ai_context_with_session(
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

        match &entry.phase {
            // CR 103.5: This player's entry owes bottoms at their own declare
            // point. Bottom the N least valuable cards, using the cached plan
            // to preserve expected land count and structurally detected payoff
            // cards. The earmarked Serum Powder (if `then` is `UseSerumPowder`)
            // is excluded from the selection pool — it's committed to its own
            // activation.
            MulliganDecisionPhase::BottomCards { count, then } => {
                let exclude = match then {
                    PendingMulliganAction::UseSerumPowder { object_id } => Some(*object_id),
                    PendingMulliganAction::Keep => None,
                };
                let to_bottom = plan_aware_bottom_cards(
                    state,
                    player,
                    *count as usize,
                    features,
                    plan,
                    exclude,
                );
                return Some(GameAction::SelectCards { cards: to_bottom });
            }
            MulliganDecisionPhase::Declare => {
                let hand: Vec<_> = state.players[player.0 as usize]
                    .hand
                    .iter()
                    .copied()
                    .collect();
                let turn_order = crate::policies::mulligan::turn_order_for(state, player);
                let decision = crate::policies::mulligan::MulliganRegistry::default()
                    .evaluate_hand(&hand, state, features, plan, turn_order, mulligan_count);
                // CR 103.5b + Serum Powder Oracle text: if the AI would mulligan
                // and it has a Serum Powder in hand, prefer the Powder — it's a
                // strictly better action than a mulligan (no mulligan count
                // increment). When the registry says keep, take the keep — don't
                // burn a Powder on a hand the policies already endorsed.
                let choice = if decision.keep {
                    MulliganChoice::Keep
                } else if let Some(object_id) = first_serum_powder_in_hand(state, player) {
                    MulliganChoice::UseSerumPowder { object_id }
                } else {
                    MulliganChoice::Mulligan
                };
                return Some(GameAction::MulliganDecision { choice });
            }
        }
    }

    // TL:R 906.6: Opening-hand forced bottoming. Each pending player owes a
    // distinct `count`, and several players can be pending at once. The AI
    // controller must scope to `ai_player`'s own entry: the shared candidate
    // pool mixes every pending player's combos, and `validate_candidates`
    // simulates them as the first authorized submitter (seat order) rather than
    // `ai_player` — so without this branch the AI can pick a selection sized for
    // a different player and the engine rejects it. Bottom the N least valuable
    // cards, using the cached plan to preserve expected land count and
    // structurally detected payoff cards.
    if let WaitingFor::OpeningHandBottomCards { pending, .. } = &state.waiting_for {
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
        let to_bottom = plan_aware_bottom_cards(state, ai_player, count, features, plan, None);
        return Some(GameAction::SelectCards { cards: to_bottom });
    }

    // Scry/Dig/Surveil: use card evaluation heuristics
    if let WaitingFor::ScryChoice { cards, .. } = &state.waiting_for {
        let mut scored: Vec<_> = cards
            .iter()
            .map(|&id| (id, intrinsic_value(state, id)))
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
            .map(|&id| (id, intrinsic_value(state, id)))
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
            .map(|&id| (id, intrinsic_value(state, id)))
            .collect();
        // CR 701.25a: the action is the ordered keep-on-top set; cards left out
        // are milled. Keep the higher-value half on top (best drawn first) and
        // let the worse half fall into the graveyard.
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let keep_count = scored.len() / 2;
        let top_cards: Vec<_> = scored.iter().take(keep_count).map(|(id, _)| *id).collect();
        return Some(GameAction::SelectCards { cards: top_cards });
    }

    if let WaitingFor::ArrangePlanarDeckTopChoice {
        cards, keep_on_top, ..
    } = &state.waiting_for
    {
        let mut scored: Vec<_> = cards
            .iter()
            .map(|&id| (id, intrinsic_value(state, id)))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let top_cards: Vec<_> = scored
            .iter()
            .take(*keep_on_top)
            .map(|(id, _)| *id)
            .collect();
        return Some(GameAction::SelectCards { cards: top_cards });
    }

    if let WaitingFor::RevealChoice { cards, .. } = &state.waiting_for {
        let mut scored: Vec<_> = cards
            .iter()
            .map(|&id| (id, intrinsic_value(state, id)))
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
                cards: pick_lowest_value_sacrifices(state, cards, *count, &config.policy_penalties),
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

    // CR 608.2d: ChooseFromZoneChoice — select cards from a tracked set.
    if let WaitingFor::ChooseFromZoneChoice {
        cards,
        count,
        player,
        ..
    } = &state.waiting_for
    {
        let mut scored: Vec<_> = cards
            .iter()
            .map(|&id| (id, intrinsic_value(state, id)))
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

    // CR 514.1 + CR 701.9a: cleanup discard. The give-up order is
    // `card_value::cmp_keep`, not the raw scalar — a mana source the discarding
    // player's own plan still needs must not be pitched ahead of a spell that
    // merely scores lower.
    //
    // The authority key is the `WaitingFor`'s own `player` — the decision
    // subject is the *discarding* player, not `ai_player` (they can diverge
    // under CR 723 turn control). The plan lookup and the land count MUST use
    // the same id or the tier compares one player's schedule against another's
    // board.
    //
    // `context == None` (the shape `planner/mod.rs`'s quiescence loop passes on
    // every rollout step) yields `plan == None`, every card `Ordinary`, and the
    // tuple comparator degenerates to the scalar comparator — the fail-safe.
    if let WaitingFor::DiscardToHandSize {
        cards,
        count,
        player,
    } = &state.waiting_for
    {
        let plan_state = context
            .and_then(|c| c.session.plan.get(player))
            .map(|plan| PlanState::realize(state, *player, plan));
        let mut scored: Vec<_> = cards
            .iter()
            .map(|&id| (id, keep_key(state, id, plan_state)))
            .collect();
        // CR 723.5: while controlling another player, a player makes all the
        // choices and decisions that player is told to make — this discard
        // among them. The AI is then deciding FOR an opponent, so it minimizes
        // *their* position rather than serving it: the comparator is reversed,
        // which surrenders the mana sources their own plan still needs first,
        // their best remaining spell next, and leaves their surplus lands in
        // hand. Keying the tier on the discarding player (above) chooses WHOSE
        // schedule is read; this chooses WHOSE interest is served, and both
        // halves are needed — reading their schedule while serving their
        // interest is strictly worse than the pre-change behaviour.
        //
        // The gate is the engine's own submitter authority rather than the
        // coarser `*player != ai_player` the `ChooseFromZoneChoice` sibling
        // uses. `deterministic_choice` is also driven from the rollout
        // quiescence loop (`planner/mod.rs`), which passes the *acting* player
        // as the optimizing seat precisely so each simulated player is modelled
        // playing WELL for themselves; a bare seat comparison would flip to
        // sabotage the moment a caller passed the real AI seat while simulating
        // someone else's decision. Turn control is the only shape where the AI
        // legitimately submits for a seat that is not its own.
        let decide_against_the_discarder = *player != ai_player
            && engine::game::turn_control::authorized_submitter_for_player(state, *player)
                == ai_player;
        if decide_against_the_discarder {
            scored.sort_by(|a, b| cmp_keep(&b.1, &a.1));
        } else {
            scored.sort_by(|a, b| cmp_keep(&a.1, &b.1));
        }
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
            return Some(engine::game::combat::complete_blocker_proposal(
                state,
                ai_player,
                &assignments,
            ));
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
            return Some(engine::game::combat::complete_blocker_proposal(
                state,
                ai_player,
                &assignments,
            ));
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
    // CR 508.1d: the AI's heuristic assignment is a PROPOSAL. The engine-owned
    // completion returns it unchanged when it is hard-legal, meets the maximum
    // requirement score, and incurs no tax; otherwise it returns the deterministic
    // tax-free maximum-legal witness. This replaces the old clone-apply +
    // first-generic-legal-action fallback with the single engine legality authority
    // (no second combat validator, no repeat-tax loop).
    engine::game::combat::complete_attacker_proposal(state, &attacks, &[])
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

    // This is a latency shortcut only when the land play is unambiguous. A
    // first-match choice bypasses `LandSequencingPolicy`, which must compare
    // self-bouncing lands with their ordinary-land siblings. Let scoring make
    // every ambiguous land choice; this applies equally to the large-board
    // priority shortcut that calls this helper.
    let mut land_actions = actions
        .iter()
        .filter(|action| matches!(action, GameAction::PlayLand { .. }));
    let only_land = land_actions.next()?;
    land_actions.next().is_none().then(|| only_land.clone())
}

fn plan_aware_bottom_cards(
    state: &GameState,
    player: PlayerId,
    count: usize,
    features: &DeckFeatures,
    plan: &PlanSnapshot,
    exclude: Option<ObjectId>,
) -> Vec<ObjectId> {
    // The full hand — including any earmarked-Serum-Powder `exclude` object —
    // drives the hand-size and land-target arithmetic, because the earmarked
    // card is still physically in hand until its effect runs.
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

    // Only the candidate selection POOL excludes the earmarked object.
    for id in hand.into_iter().filter(|id| Some(*id) != exclude) {
        let score = state.objects.get(&id).map_or(0.0, |obj| {
            if is_plan_payoff_name(features, &obj.name) {
                25.0 + intrinsic_value(state, id)
            } else if obj.card_types.core_types.contains(&CoreType::Land) {
                if surplus_lands > 0 {
                    surplus_lands -= 1;
                    -5.0
                } else {
                    30.0
                }
            } else {
                intrinsic_value(state, id)
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
        // Fallback: pick the highest-scored action (tie-break by action key —
        // issue #4878).
        return scored
            .iter()
            .max_by(|a, b| {
                a.1.partial_cmp(&b.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.0.cmp_stable(&b.0))
            })
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
    use engine::game::scenario::{GameScenario, P0};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, CategoryChooserScope, ContinuousModification,
        Duration, Effect, EffectKind, ManaProduction, QuantityExpr, ResolvedAbility,
        StaticDefinition, TargetFilter, TargetRef, TypedFilter,
    };
    use engine::types::ability::{ChoiceType, ChosenAttribute};
    use engine::types::card_type::CoreType;
    use engine::types::counter::CounterType;
    use engine::types::game_state::{
        NamedChoiceSource, NamedChoiceSourceBinding, OpponentGuessOwner, OpponentGuessSource,
        PromptSourceBinding, StackEntry, StackEntryKind,
    };
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::keywords::Keyword;
    use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
    use engine::types::phase::Phase;
    use engine::types::zones::Zone;
    use rand::rngs::SmallRng;
    use rand::SeedableRng;

    use crate::config::{create_config, AiDifficulty, Platform};
    use crate::policies::context::PolicyContext;
    use crate::policies::{DecisionKind, PolicyReason, TacticalPolicy};
    use crate::session::SessionCache;
    use crate::test_support::{context_with_plans, default_deck_plan, ramp_deck_plan};

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

    /// `fallback_action` under the default policy penalties. These tests assert
    /// the *shape* of an escape action and do not vary penalties; the
    /// land-aware sacrifice path threaded through `config` is covered
    /// separately by `fallback_sacrifice_prefers_creature_over_land`.
    fn fallback_action_default(state: &GameState) -> Option<GameAction> {
        fallback_action(
            state,
            &create_config(AiDifficulty::VeryHard, Platform::Native),
        )
    }

    fn resolution_choice_source(state: &GameState, object_id: ObjectId) -> NamedChoiceSource {
        let context = engine::game::triggers::trigger_source_context_for_latch(
            state,
            state.objects.get(&object_id).unwrap(),
        );
        NamedChoiceSource::from_trigger_source(context, NamedChoiceSourceBinding::ResolutionContext)
    }

    #[test]
    fn loop_shortcut_fallback_selects_legal_decline() {
        let mut state = make_state();
        state.waiting_for = WaitingFor::LoopShortcut {
            proposer: PlayerId(0),
            predicted_winner: Some(PlayerId(1)),
            certificate: engine::analysis::loop_check::LoopCertificate {
                unbounded: vec![],
                win_kind: engine::analysis::loop_check::WinKind::LethalDamage,
                mandatory: false,
                residual_board_delta: engine::analysis::resource::BoardDelta::default(),
            },
            schema: engine::analysis::decision_template::ShortcutDecisionSchema::default(),
        };

        assert_eq!(
            fallback_action_default(&state),
            Some(GameAction::DeclineShortcut),
            "the no-score fallback must select DeclineShortcut from engine legal actions"
        );
    }

    /// CR 701.42b: the public search path prefers the physical canonical meld
    /// pair over an earlier live-name impostor that would exile both selected
    /// objects without producing the result permanent. This proves the choice
    /// is handled by ordinary simulation/evaluation, not bespoke name scoring.
    #[test]
    fn choose_action_simulates_meld_pair_outcomes() {
        use engine::types::ability::{PermanentEntryMode, PtValue};
        use engine::types::card::CardFace;
        use engine::types::game_state::{MeldPairRecord, MeldSelection};

        const SOURCE: &str = "AI Meld Source";
        const PARTNER: &str = "AI Meld Partner";
        const RESULT: &str = "AI Meld Result";

        let mut state = make_state();
        let impostor_source = add_creature(&mut state, PlayerId(0), 3, 3);
        let impostor_partner = add_creature(&mut state, PlayerId(0), 3, 3);
        let real_source = add_creature(&mut state, PlayerId(0), 3, 3);
        let real_partner = add_creature(&mut state, PlayerId(0), 3, 3);
        for (id, live_name, base_name) in [
            (impostor_source, SOURCE, "Printed Impostor Source"),
            (impostor_partner, PARTNER, "Printed Impostor Partner"),
            (real_source, SOURCE, SOURCE),
            (real_partner, PARTNER, PARTNER),
        ] {
            let object = state.objects.get_mut(&id).unwrap();
            object.name = live_name.to_string();
            object.base_name = base_name.to_string();
        }
        let mut result = CardFace {
            name: RESULT.to_string(),
            power: Some(PtValue::Fixed(9)),
            toughness: Some(PtValue::Fixed(9)),
            ..CardFace::default()
        };
        result.card_type.core_types.push(CoreType::Creature);
        Arc::make_mut(&mut state.card_face_registry).insert(RESULT.to_lowercase(), result);
        Arc::make_mut(&mut state.meld_pair_registry).insert(
            format!("{}\0{}", SOURCE.to_lowercase(), PARTNER.to_lowercase()),
            MeldPairRecord {
                source: SOURCE.to_string(),
                partner: PARTNER.to_string(),
                result: RESULT.to_string(),
            },
        );
        let selection = |source_id, partner_id| MeldSelection {
            source_id,
            partner_id,
            controller: PlayerId(0),
            expected_source: SOURCE.to_string(),
            expected_partner: PARTNER.to_string(),
            result: RESULT.to_string(),
            entry: PermanentEntryMode::Normal,
        };
        state.waiting_for = WaitingFor::MeldPairChoice {
            player: PlayerId(0),
            choices: vec![
                selection(impostor_source, impostor_partner),
                selection(real_source, real_partner),
            ],
        };

        let config = create_config(AiDifficulty::Medium, Platform::Native).into_measurement(9);
        let mut rng = SmallRng::seed_from_u64(9);
        assert_eq!(
            choose_action(&state, PlayerId(0), &config, &mut rng),
            Some(GameAction::ChooseMeldPair {
                source_id: real_source,
                partner_id: real_partner,
            })
        );
    }

    /// CR 701.42b: even when search cannot run, the deterministic fallback
    /// prefers the canonical physical pair over an earlier live-name impostor.
    #[test]
    fn meld_pair_fallback_prefers_canonical_pair_in_hostile_order() {
        use engine::types::ability::PermanentEntryMode;
        use engine::types::game_state::{MeldPairRecord, MeldSelection};

        const SOURCE: &str = "Fallback Meld Source";
        const PARTNER: &str = "Fallback Meld Partner";
        const RESULT: &str = "Fallback Meld Result";

        let mut state = make_state();
        let impostor_source = add_creature(&mut state, PlayerId(0), 3, 3);
        let impostor_partner = add_creature(&mut state, PlayerId(0), 3, 3);
        let real_source = add_creature(&mut state, PlayerId(0), 3, 3);
        let real_partner = add_creature(&mut state, PlayerId(0), 3, 3);
        for (id, base_name) in [
            (impostor_source, "Printed Impostor Source"),
            (impostor_partner, "Printed Impostor Partner"),
            (real_source, SOURCE),
            (real_partner, PARTNER),
        ] {
            state.objects.get_mut(&id).unwrap().base_name = base_name.to_string();
        }
        Arc::make_mut(&mut state.meld_pair_registry).insert(
            format!("{}\0{}", SOURCE.to_lowercase(), PARTNER.to_lowercase()),
            MeldPairRecord {
                source: SOURCE.to_string(),
                partner: PARTNER.to_string(),
                result: RESULT.to_string(),
            },
        );
        let selection = |source_id, partner_id| MeldSelection {
            source_id,
            partner_id,
            controller: PlayerId(0),
            expected_source: SOURCE.to_string(),
            expected_partner: PARTNER.to_string(),
            result: RESULT.to_string(),
            entry: PermanentEntryMode::Normal,
        };
        state.waiting_for = WaitingFor::MeldPairChoice {
            player: PlayerId(0),
            choices: vec![
                selection(impostor_source, impostor_partner),
                selection(real_source, real_partner),
            ],
        };

        assert_eq!(
            fallback_action_default(&state),
            Some(GameAction::ChooseMeldPair {
                source_id: real_source,
                partner_id: real_partner,
            })
        );
    }

    /// Issue #4878: the degenerate-weight fallback in `softmax_select_pairs`
    /// must break score ties with `GameAction::cmp_stable`, not fall back to the
    /// input-list order. Here every score is `-inf` (weights become `NaN`, so
    /// the fallback branch runs). `PassPriority` (discriminant 0) sorts before
    /// `PlayLand` (discriminant 1), so the `cmp_stable`-maximum is the `PlayLand`
    /// listed FIRST. Removing the `then_with(cmp_stable)` tie-break makes
    /// `max_by` return the last equally-maximal element (`PassPriority`) instead,
    /// flipping this assertion.
    #[test]
    fn softmax_fallback_tiebreak_is_cmp_stable_deterministic() {
        let scored = vec![
            (
                GameAction::PlayLand {
                    object_id: ObjectId(5),
                    card_id: CardId(1),
                },
                f64::NEG_INFINITY,
            ),
            (GameAction::PassPriority, f64::NEG_INFINITY),
        ];
        // Reach guard: `PlayLand` must outrank `PassPriority` under cmp_stable so
        // the expected pick is the first (non-last) element, distinguishing the
        // tie-break from `max_by`'s last-on-ties behavior.
        assert_eq!(
            scored[0].0.cmp_stable(&scored[1].0),
            std::cmp::Ordering::Greater,
            "precondition: PlayLand > PassPriority under cmp_stable"
        );

        let mut rng = SmallRng::seed_from_u64(0);
        let chosen = softmax_select_pairs(&scored, 1.0, &mut rng)
            .expect("non-empty scored list must select an action");
        assert_eq!(
            chosen, scored[0].0,
            "degenerate-weight fallback must pick the cmp_stable-max action"
        );
    }

    /// Issue #4878: the candidate sort was previously gated behind measurement
    /// mode. A *normal* (non-measurement) config must still emit candidates in
    /// the canonical `cmp_stable` order. Reverting the always-on
    /// `out.sort_by(cmp_stable)` returns candidates in score / enumeration order,
    /// which is not `cmp_stable`-sorted for this set, flipping the assertion.
    #[test]
    fn score_candidates_non_measurement_order_is_cmp_stable_canonical() {
        let mut state = make_state();
        add_mana(&mut state, PlayerId(0), ManaType::Colorless, 6);
        add_spell_to_hand(&mut state, PlayerId(0), "SpellA", 1);
        add_spell_to_hand(&mut state, PlayerId(0), "SpellB", 2);
        add_spell_to_hand(&mut state, PlayerId(0), "SpellC", 3);
        // Normal config: NOT measurement mode (the guard this test protects only
        // ever sorted under measurement before #4878).
        let config = create_config(AiDifficulty::Hard, Platform::Native);
        let session = AiSession::arc_from_game(&state);

        let scored = score_candidates_with_session(&state, PlayerId(0), &config, &session);
        let actions: Vec<GameAction> = scored.iter().map(|(a, _)| a.clone()).collect();
        // Reach guard: several distinct candidates (3 castable spells + Pass)
        // so the order is non-trivial.
        assert!(
            actions.len() >= 3,
            "expected several scored candidates, got {}",
            actions.len()
        );

        let mut expected = actions.clone();
        expected.sort_by(|a, b| a.cmp_stable(b));
        assert_eq!(
            actions, expected,
            "non-measurement scoring must emit cmp_stable-canonical order"
        );
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
                ability: Box::new(ResolvedAbility::new(
                    Effect::NoOp,
                    vec![],
                    object_id,
                    controller,
                )),
            },
        }
    }
    fn add_cycler_to_hand(
        state: &mut GameState,
        core_type: CoreType,
        keyword: engine::types::keywords::Keyword,
    ) -> ObjectId {
        let card_id = CardId(state.next_object_id);
        let id = create_object(
            state,
            card_id,
            PlayerId(0),
            "Cycler".to_string(),
            Zone::Hand,
        );
        let ability = engine::database::synthesis::cycling_ability_for_keyword(&keyword)
            .expect("cycling keyword must synthesize an activated ability");
        let object = state.objects.get_mut(&id).unwrap();
        object.card_types.core_types.push(core_type);
        object.base_card_types = object.card_types.clone();
        Arc::make_mut(&mut object.abilities).push(ability);
        id
    }

    fn add_plain_land(state: &mut GameState, zone: Zone) -> ObjectId {
        let card_id = CardId(state.next_object_id);
        let id = create_object(state, card_id, PlayerId(0), "Land".to_string(), zone);
        let object = state.objects.get_mut(&id).unwrap();
        object.card_types.core_types.push(CoreType::Land);
        object.base_card_types = object.card_types.clone();
        id
    }

    fn priority_on_opponent_end_step(state: &mut GameState) {
        state.phase = Phase::End;
        state.active_player = PlayerId(1);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
    }

    fn action_score(scored: &[(GameAction, f64)], expected: &GameAction) -> f64 {
        scored
            .iter()
            .find_map(|(action, score)| (action == expected).then_some(*score))
            .unwrap_or_else(|| panic!("expected scored action {expected:?}"))
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
            end_cost: None,
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
    fn ordinary_cycling_is_finite_and_scored_below_pass_at_root() {
        // Production regression for the generic "always cycle" report. Cycling
        // replaces itself, so without the registered patience policy its generic
        // activation prior beats Pass at this otherwise-neutral end-step window.
        let mut state = make_state();
        priority_on_opponent_end_step(&mut state);
        add_mana(&mut state, PlayerId(0), ManaType::Colorless, 2);
        create_object(
            &mut state,
            CardId(9_000),
            PlayerId(0),
            "Replacement".to_string(),
            Zone::Library,
        );
        let cycler = add_cycler_to_hand(
            &mut state,
            CoreType::Creature,
            engine::types::keywords::Keyword::Cycling(engine::types::keywords::CyclingCost::Mana(
                engine::types::mana::ManaCost::generic(2),
            )),
        );
        let activation = GameAction::ActivateAbility {
            source_id: cycler,
            ability_index: 0,
        };

        let config = create_config(AiDifficulty::VeryHard, Platform::Native).into_measurement(1);
        let session = AiSession::arc_from_game(&state);
        let scored = score_candidates_core(&state, PlayerId(0), &config, &session, None);
        let cycling_score = action_score(&scored, &activation);
        let pass_score = action_score(&scored, &GameAction::PassPriority);

        assert!(
            cycling_score.is_finite(),
            "cycling must remain a finite option"
        );
        assert!(pass_score.is_finite(), "Pass must reach registered scoring");
        assert!(
            cycling_score < pass_score,
            "registered cycling patience must make neutral cycling wait: cycle={cycling_score}, pass={pass_score}"
        );
    }

    #[test]
    fn printed_typecycling_is_not_rejected_by_self_cost_policy() {
        // Nonland Typecycling searches rather than draws. SelfCostValue used to
        // classify that SearchLibrary payoff as trivial and hard-reject the
        // discard; the exact Cycling tag now delegates to finite patience.
        let mut state = make_state();
        priority_on_opponent_end_step(&mut state);
        add_mana(&mut state, PlayerId(0), ManaType::Colorless, 1);
        let cycler = add_cycler_to_hand(
            &mut state,
            CoreType::Creature,
            engine::types::keywords::Keyword::Typecycling {
                cost: engine::types::mana::ManaCost::generic(1),
                subtype: "Wizard".to_string(),
            },
        );
        let activation = GameAction::ActivateAbility {
            source_id: cycler,
            ability_index: 0,
        };

        let config = create_config(AiDifficulty::VeryHard, Platform::Native).into_measurement(2);
        let session = AiSession::arc_from_game(&state);
        let scored = score_candidates_core(&state, PlayerId(0), &config, &session, None);

        assert!(
            action_score(&scored, &activation).is_finite(),
            "printed Typecycling must reach finite registered scoring"
        );
    }

    #[test]
    fn sole_planned_cycling_land_waits_but_remains_finite() {
        let mut state = make_state();
        priority_on_opponent_end_step(&mut state);
        for _ in 0..5 {
            add_plain_land(&mut state, Zone::Battlefield);
        }
        add_mana(&mut state, PlayerId(0), ManaType::Colorless, 2);
        create_object(
            &mut state,
            CardId(9_001),
            PlayerId(0),
            "Replacement".to_string(),
            Zone::Library,
        );
        let cycler = add_cycler_to_hand(
            &mut state,
            CoreType::Land,
            engine::types::keywords::Keyword::Cycling(engine::types::keywords::CyclingCost::Mana(
                engine::types::mana::ManaCost::generic(2),
            )),
        );
        let activation = GameAction::ActivateAbility {
            source_id: cycler,
            ability_index: 0,
        };

        let mut ai_session = AiSession::empty();
        // Derived, not hand-built: `[1,2,3,4,5,6,6,…]` was written out by hand
        // here, and `derive_snapshot` produces exactly that for a default deck
        // (pinned by `cycling_discipline::tests::
        // derived_plans_match_the_schedules_they_replaced`) while also filling
        // the mana and threat schedules a hand-built snapshot left at zero.
        ai_session.plan.insert(
            PlayerId(0),
            crate::plan::derive_snapshot(&crate::features::DeckFeatures::default()),
        );
        let session = Arc::new(ai_session);
        let config = create_config(AiDifficulty::VeryHard, Platform::Native).into_measurement(3);
        let scored = score_candidates_core(&state, PlayerId(0), &config, &session, None);
        let cycling_score = action_score(&scored, &activation);
        let pass_score = action_score(&scored, &GameAction::PassPriority);

        assert!(
            cycling_score.is_finite(),
            "needed-land patience is not a veto"
        );
        assert!(
            cycling_score < pass_score,
            "the sole next planned land must wait: cycle={cycling_score}, pass={pass_score}"
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
                chooser: None,
                effect_kind: EffectKind::NoOp,
                effect_detail: engine::types::game_state::TargetEffectDetail::None,
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
    fn large_board_main_phase_fast_action_uses_bounded_policy_scoring() {
        let mut state = make_state();
        for _ in 0..LARGE_BOARD_FAST_PRIORITY_BATTLEFIELD_OBJECTS {
            add_creature(&mut state, PlayerId(1), 1, 1);
        }
        assert_eq!(
            state.battlefield.len(),
            LARGE_BOARD_FAST_PRIORITY_BATTLEFIELD_OBJECTS,
            "the fixture must cross the fast-path battlefield threshold explicitly"
        );
        assert!(has_large_battlefield(&state));
        let cheap = add_spell_to_hand(&mut state, PlayerId(0), "Cheap Spell", 1);
        let expensive = add_spell_to_hand(&mut state, PlayerId(0), "Expensive Spell", 6);
        add_mana(&mut state, PlayerId(0), ManaType::Colorless, 6);
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
        let config = AiConfig::default();
        let session = AiSession::arc_from_game(&state);

        assert_eq!(
            large_board_main_phase_fast_action_from_actions(
                &state,
                PlayerId(0),
                &actions,
                &config,
                &session,
            ),
            Some(GameAction::CastSpell {
                object_id: expensive,
                card_id: CardId(expensive.0),
                targets: Vec::new(),
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
            }),
            "large-board action selection must remain bounded while retaining tactical scoring"
        );
    }

    #[test]
    fn large_board_main_phase_fast_action_requires_battlefield_threshold() {
        let mut state = make_state();
        for _ in 0..(LARGE_BOARD_FAST_PRIORITY_BATTLEFIELD_OBJECTS - 1) {
            add_creature(&mut state, PlayerId(1), 1, 1);
        }
        assert_eq!(
            state.battlefield.len(),
            LARGE_BOARD_FAST_PRIORITY_BATTLEFIELD_OBJECTS - 1,
            "the fixture must remain below the fast-path battlefield threshold"
        );
        assert!(!has_large_battlefield(&state));
        let spell = add_spell_to_hand(&mut state, PlayerId(0), "Spell", 1);
        add_spell_to_hand(&mut state, PlayerId(0), "Filler", 1);
        assert!(
            state.objects.len() >= LARGE_BOARD_FAST_PRIORITY_BATTLEFIELD_OBJECTS,
            "the object count alone must not admit the bounded path"
        );
        let actions = vec![
            GameAction::PassPriority,
            GameAction::CastSpell {
                object_id: spell,
                card_id: CardId(spell.0),
                targets: Vec::new(),
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
            },
        ];
        let config = AiConfig::default();
        let session = AiSession::arc_from_game(&state);

        assert_eq!(
            large_board_main_phase_fast_action_from_actions(
                &state,
                PlayerId(0),
                &actions,
                &config,
                &session,
            ),
            None,
            "ordinary boards must continue through normal candidate scoring"
        );
    }

    #[test]
    fn large_board_main_phase_fast_action_honors_loop_guards() {
        let mut state = make_state();
        for _ in 0..LARGE_BOARD_FAST_PRIORITY_BATTLEFIELD_OBJECTS {
            add_creature(&mut state, PlayerId(1), 1, 1);
        }
        let cheap = add_spell_to_hand(&mut state, PlayerId(0), "Cheap Spell", 1);
        let cancelled = add_spell_to_hand(&mut state, PlayerId(0), "Cancelled Spell", 6);
        add_mana(&mut state, PlayerId(0), ManaType::Colorless, 6);
        state.cancelled_casts.push(cancelled);
        let actions = vec![
            GameAction::PassPriority,
            GameAction::CastSpell {
                object_id: cheap,
                card_id: CardId(cheap.0),
                targets: Vec::new(),
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
            },
            GameAction::CastSpell {
                object_id: cancelled,
                card_id: CardId(cancelled.0),
                targets: Vec::new(),
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
            },
        ];
        let config = AiConfig::default();
        let session = AiSession::arc_from_game(&state);

        assert_eq!(
            large_board_main_phase_fast_action_from_actions(
                &state,
                PlayerId(0),
                &actions,
                &config,
                &session,
            ),
            Some(GameAction::CastSpell {
                object_id: cheap,
                card_id: CardId(cheap.0),
                targets: Vec::new(),
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
            }),
            "the bounded path must not re-cast a cancelled spell"
        );
    }

    #[test]
    fn large_board_main_phase_fast_action_does_not_fire_off_turn() {
        let mut state = make_state();
        state.active_player = PlayerId(1);
        for _ in 0..LARGE_BOARD_FAST_PRIORITY_BATTLEFIELD_OBJECTS {
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
        let config = AiConfig::default();
        let session = AiSession::arc_from_game(&state);

        assert_eq!(
            large_board_main_phase_fast_action_from_actions(
                &state,
                PlayerId(0),
                &actions,
                &config,
                &session,
            ),
            None
        );
    }

    fn spell_target_selection_state(
        current_legal_targets: Vec<TargetRef>,
        stale_slot_targets: Vec<TargetRef>,
        optional: bool,
    ) -> GameState {
        let mut state = make_state();
        let spell_id = add_spell_to_hand(&mut state, PlayerId(0), "Targeting Spell", 0);
        let mut ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Any,
                damage_source: None,
                excess: None,
            },
            Vec::new(),
            spell_id,
            PlayerId(0),
        );
        ability.optional_targeting = optional;
        let pending_cast = engine::types::game_state::PendingCast::new(
            spell_id,
            CardId(spell_id.0),
            ability,
            engine::types::mana::ManaCost::NoCost,
        );

        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(spell_id.0),
                ability: None,
                casting_variant: engine::types::game_state::CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });
        state.waiting_for = WaitingFor::TargetSelection {
            player: PlayerId(0),
            pending_cast: Box::new(pending_cast),
            target_slots: vec![engine::types::game_state::TargetSelectionSlot {
                legal_targets: stale_slot_targets,
                optional,
                chooser: None,
                effect_kind: EffectKind::NoOp,
                effect_detail: engine::types::game_state::TargetEffectDetail::None,
            }],
            mode_labels: Vec::new(),
            selection: engine::types::game_state::TargetSelectionProgress {
                current_slot: 0,
                selected_slots: Vec::new(),
                current_legal_targets,
            },
        };
        state
    }

    /// Minimal non-payment mana-color prompt. The production payment path uses
    /// a live mana-ability carrier below; this fixture is deliberately outside
    /// that authority so it pins the established first-option fallback.
    fn non_affiliated_choose_mana_color_state(options: Vec<ManaType>) -> GameState {
        use engine::types::ability::{QuantityExpr, ResolvedAbility, TargetFilter};
        use engine::types::game_state::{ManaChoiceContext, ManaChoicePrompt};
        let mut state = make_state();
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
    fn non_affiliated_choose_mana_color_uses_first_option() {
        let state = non_affiliated_choose_mana_color_state(vec![ManaType::Red, ManaType::Blue]);
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

    fn flexible_mana_payment_state() -> GameState {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Flexible AI Witness", true, "Draw a card.")
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 1,
            })
            .id();
        let source = scenario.add_creature(P0, "Flexible AI Source", 1, 1).id();
        let mut runner = scenario.build();
        let ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyCombination {
                    count: QuantityExpr::Fixed { value: 2 },
                    color_options: vec![ManaColor::Blue, ManaColor::Red],
                },
                restrictions: Vec::new(),
                grants: Vec::new(),
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Tap);
        let source_object = runner.state_mut().objects.get_mut(&source).unwrap();
        Arc::make_mut(&mut source_object.abilities).push(ability);
        let card_id = runner.state().objects[&spell].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: spell,
                card_id,
                targets: Vec::new(),
                payment_mode: engine::types::game_state::CastPaymentMode::Manual,
            })
            .expect("the test spell reaches manual payment");
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
            })
            .expect("the real mana ability opens its colour prompt");
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseManaColor { .. }
        ));
        runner.state().clone()
    }

    fn mana_product(colors: &[ManaType]) -> GameAction {
        GameAction::ChooseManaColor {
            choice: engine::types::game_state::ManaChoice::Combination(colors.to_vec()),
            count: 1,
        }
    }

    /// Reach a live CR 702.126a Improvise payment carrier, then leave its last
    /// mana open for the red-first flexible mana ability. `improvise_taps`
    /// deliberately differs between the coloured and generic control so each
    /// still needs exactly one colour allocation after its artifact payment.
    fn red_first_improvise_payment_state(cost: ManaCost, improvise_taps: usize) -> GameState {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let spell = {
            let mut builder = scenario.add_spell_to_hand_from_oracle(
                P0,
                "Improvise Payment Witness",
                true,
                "Draw a card.",
            );
            builder.with_mana_cost(cost);
            builder.with_keyword(Keyword::Improvise);
            builder.id()
        };
        let artifacts: Vec<_> = (0..improvise_taps)
            .map(|index| {
                let mut builder =
                    scenario.add_creature(P0, &format!("Improvise Artifact {index}"), 0, 1);
                builder.as_artifact();
                builder.id()
            })
            .collect();
        let source = scenario
            .add_creature(P0, "Red First Mana Source", 1, 1)
            .id();
        let mut runner = scenario.build();
        let ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyCombination {
                    count: QuantityExpr::Fixed { value: 1 },
                    color_options: vec![ManaColor::Red, ManaColor::Blue],
                },
                restrictions: Vec::new(),
                grants: Vec::new(),
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Tap);
        Arc::make_mut(
            &mut runner
                .state_mut()
                .objects
                .get_mut(&source)
                .unwrap()
                .abilities,
        )
        .push(ability);
        let card_id = runner.state().objects[&spell].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: spell,
                card_id,
                targets: Vec::new(),
                payment_mode: engine::types::game_state::CastPaymentMode::Manual,
            })
            .expect("the Improvise spell reaches manual payment");
        for artifact in artifacts {
            runner
                .act(GameAction::TapForConvoke {
                    object_id: artifact,
                    mana_type: ManaType::Colorless,
                })
                .expect("each artifact pays one generic mana through Improvise");
        }
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
            })
            .expect("the real mana ability opens its red-first colour prompt");
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseManaColor { .. }
        ));
        runner.state().clone()
    }

    struct FlexibleManaPolicy(Arc<std::sync::atomic::AtomicUsize>);

    impl TacticalPolicy for FlexibleManaPolicy {
        fn id(&self) -> PolicyId {
            PolicyId::PaymentSelection
        }

        fn decision_kinds(&self) -> &'static [DecisionKind] {
            &[DecisionKind::ActivateAbility]
        }

        fn activation(
            &self,
            _: &crate::features::DeckFeatures,
            _: &GameState,
            _: PlayerId,
        ) -> Option<f32> {
            Some(1.0)
        }

        fn verdict(&self, context: &PolicyContext<'_>) -> PolicyVerdict {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            match &context.candidate.action {
                GameAction::ChooseManaColor {
                    choice: engine::types::game_state::ManaChoice::Combination(colors),
                    ..
                } if colors == &[ManaType::Blue, ManaType::Red] => {
                    PolicyVerdict::critical(15.0, PolicyReason::new("flexible_mana_test"))
                }
                GameAction::ChooseManaColor {
                    choice: engine::types::game_state::ManaChoice::Combination(colors),
                    ..
                } if colors == &[ManaType::Red, ManaType::Blue] => {
                    PolicyVerdict::strong(5.0, PolicyReason::new("flexible_mana_test"))
                }
                _ => PolicyVerdict::neutral(PolicyReason::new("flexible_mana_test")),
            }
        }
    }

    fn flexible_mana_session(
        state: &GameState,
        calls: Arc<std::sync::atomic::AtomicUsize>,
    ) -> Arc<AiSession> {
        let mut session = AiSession::from_game(state);
        session.policy_registry_override =
            Some(Arc::new(PolicyRegistry::for_tests(vec![Box::new(
                FlexibleManaPolicy(calls),
            )])));
        Arc::new(session)
    }

    #[test]
    fn affiliated_flexible_mana_uses_witnessed_support_in_public_and_enabled_beam_paths() {
        let state = flexible_mana_payment_state();
        let all = engine::ai_support::legal_actions(&state);
        let expected_all = vec![
            mana_product(&[ManaType::Blue, ManaType::Blue]),
            mana_product(&[ManaType::Blue, ManaType::Red]),
            mana_product(&[ManaType::Red, ManaType::Blue]),
            mana_product(&[ManaType::Red, ManaType::Red]),
        ];
        assert_eq!(
            all, expected_all,
            "live AnyCombination exposes all four products"
        );
        let witnessed: Vec<_> = all
            .iter()
            .filter_map(|action| engine::ai_support::witness_payment_continuation(&state, action))
            .collect();
        let expected = expected_all[..3].to_vec();
        assert_eq!(
            witnessed
                .iter()
                .map(|accepted| accepted.action.clone())
                .collect::<Vec<_>>(),
            expected,
            "only the three products that can finish the announced root survive"
        );

        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let session = flexible_mana_session(&state, Arc::clone(&calls));
        let mut disabled = create_config(AiDifficulty::Hard, Platform::Native).into_measurement(43);
        disabled.search.enabled = false;
        disabled.temperature = 0.25;
        let scored = score_candidates_with_session(&state, P0, &disabled, &session);
        assert_eq!(
            scored
                .iter()
                .map(|(action, _)| action.clone())
                .collect::<Vec<_>>(),
            expected,
            "the public scorer retains every witnessed successor and rejects RR"
        );
        assert!(calls.load(std::sync::atomic::Ordering::Relaxed) >= 3);
        assert_eq!(score_of(&scored, &expected[0]), 0.45);
        assert_eq!(score_of(&scored, &expected[1]), 15.45);
        assert_eq!(score_of(&scored, &expected[2]), 5.45);

        let max_score = scored
            .iter()
            .map(|(_, score)| *score)
            .fold(f64::NEG_INFINITY, f64::max);
        let weights: Vec<_> = scored
            .iter()
            .map(|(_, score)| ((*score - max_score) / disabled.temperature).exp())
            .collect();
        let total: f64 = weights.iter().sum();
        let mut threshold_rng = SmallRng::seed_from_u64(0);
        let threshold = threshold_rng.random::<f64>() * total;
        assert!(
            weights[0] < threshold && threshold <= weights[0] + weights[1],
            "the seeded full-support softmax threshold lies in BR's interval"
        );
        let mut direct_rng = SmallRng::seed_from_u64(0);
        assert_eq!(
            softmax_select_pairs(&scored, disabled.temperature, &mut direct_rng),
            Some(expected[1].clone()),
            "the full accepted support selects BR, not stable-first BB"
        );
        calls.store(0, std::sync::atomic::Ordering::Relaxed);
        let mut chooser_rng = SmallRng::seed_from_u64(0);
        assert_eq!(
            choose_action_with_session(&state, P0, &disabled, &mut chooser_rng, &session),
            Some(expected[1].clone()),
            "the disabled public chooser uses the ordinary full-support softmax path"
        );
        assert!(
            calls.load(std::sync::atomic::Ordering::Relaxed) > 0,
            "the public chooser reaches tactical policy scoring after its counter reset"
        );

        calls.store(0, std::sync::atomic::Ordering::Relaxed);
        let mut enabled = create_config(AiDifficulty::Hard, Platform::Native).into_measurement(43);
        enabled.search.enabled = true;
        enabled.search.max_branching = 3;
        enabled.search.planner_mode = PlannerMode::BeamOnly;
        enabled.search.determinization_samples = 0;
        let context = build_ai_context_with_session(&state, P0, &enabled, Arc::clone(&session));
        let policies = session.policy_registry_override.as_deref().unwrap();
        let services = PlannerServices::with_deadline(P0, &enabled, policies, context, None);
        let decision = build_decision_context(&state);
        let prepared = prepare_payment_candidates(&state, decision.candidates.clone());
        let validated = services.validate_candidates(
            &state,
            prepared
                .iter()
                .map(|candidate| candidate.candidate.clone())
                .collect(),
        );
        let gated = gate_candidates(
            &state,
            &decision,
            validated,
            P0,
            &enabled,
            &services.context,
        );
        let beam = rank_root_payment_candidates(&state, &decision, &prepared, &gated, &services, 3);
        assert_eq!(
            beam.iter()
                .map(|candidate| candidate.candidate.action.clone())
                .collect::<Vec<_>>(),
            vec![
                expected[1].clone(),
                expected[2].clone(),
                expected[0].clone()
            ],
            "the enabled root beam ranks BR > RB > BB and retains width three"
        );
        assert!(beam
            .iter()
            .all(|candidate| candidate.payment_successor.is_some()));

        calls.store(0, std::sync::atomic::Ordering::Relaxed);
        let enabled_scored = score_candidates_with_session(&state, P0, &enabled, &session);
        assert_eq!(enabled_scored.len(), 3);
        assert!(enabled_scored
            .iter()
            .all(|(action, _)| expected.contains(action)));
        assert!(calls.load(std::sync::atomic::Ordering::Relaxed) > 0);

        calls.store(0, std::sync::atomic::Ordering::Relaxed);
        let mut rng = SmallRng::seed_from_u64(0);
        let chosen = choose_action_with_session(&state, P0, &enabled, &mut rng, &session);
        assert!(chosen
            .as_ref()
            .is_some_and(|action| expected.contains(action)));
        assert!(calls.load(std::sync::atomic::Ordering::Relaxed) > 0);
    }

    #[test]
    fn improvise_mana_only_strands_mandatory_blue() {
        let coloured = red_first_improvise_payment_state(
            ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 2,
            },
            2,
        );
        let generic = red_first_improvise_payment_state(ManaCost::generic(2), 1);
        let red = mana_product(&[ManaType::Red]);
        let blue = mana_product(&[ManaType::Blue]);

        let coloured_actions = engine::ai_support::legal_actions(&coloured);
        assert!(
            coloured_actions.contains(&red),
            "the live Metallic-Rebuke-style carrier offers a red allocation"
        );
        assert!(
            coloured_actions.contains(&blue),
            "the live Metallic-Rebuke-style carrier offers a blue allocation"
        );
        assert!(matches!(
            engine::ai_support::classify_payment_continuation(&coloured),
            engine::ai_support::PaymentContinuationState::Affiliated(_)
        ));
        assert!(
            engine::ai_support::witness_payment_continuation(&coloured, &red).is_none(),
            "red cannot pay the mandatory blue shard after Improvise covers only generic mana"
        );
        assert!(
            engine::ai_support::witness_payment_continuation(&coloured, &blue).is_some(),
            "blue finalizes the coloured Improvise cast"
        );
        let generic_actions = engine::ai_support::legal_actions(&generic);
        assert!(
            generic_actions.contains(&red),
            "the paired generic control offers a red allocation"
        );
        assert!(
            generic_actions.contains(&blue),
            "the paired generic control offers a blue allocation"
        );
        assert!(
            engine::ai_support::witness_payment_continuation(&generic, &red).is_some(),
            "red remains a valid final allocation when no mandatory blue shard exists"
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
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Ability),
        };
        let allowed = CandidateAction {
            action: GameAction::PassPriority,
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Utility),
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
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Ability),
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
        engine::game::engine::apply(&mut state, PlayerId(0), action.unwrap())
            .expect("the production controller's unique-land choice must be engine-legal");
        assert!(state.battlefield.contains(&land_id));
    }

    #[test]
    fn land_fast_path_only_accepts_a_unique_legal_land() {
        let state = make_state();
        let land = GameAction::PlayLand {
            object_id: ObjectId(1),
            card_id: CardId(1),
        };
        assert_eq!(
            prefer_land_drop(
                &state,
                PlayerId(0),
                &[GameAction::PassPriority, land.clone()]
            ),
            Some(land.clone()),
            "a single legal land may use the fast path"
        );
        assert_eq!(
            prefer_land_drop(
                &state,
                PlayerId(0),
                &[
                    GameAction::PassPriority,
                    land,
                    GameAction::PlayLand {
                        object_id: ObjectId(2),
                        card_id: CardId(2),
                    },
                ],
            ),
            None,
            "competing land plays must reach policy scoring"
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
            library_owner: None,
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
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Target),
        };
        let opp_candidate = CandidateAction {
            action: GameAction::ChooseTarget {
                target: Some(TargetRef::Player(PlayerId(1))),
            },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Target),
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
                chooser: None,
                effect_kind: EffectKind::NoOp,
                effect_detail: engine::types::game_state::TargetEffectDetail::None,
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
                chooser: None,
                effect_kind: EffectKind::NoOp,
                effect_detail: engine::types::game_state::TargetEffectDetail::None,
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

    #[test]
    fn fallback_spell_target_selection_uses_current_legal_target_when_slot_is_stale() {
        let target = TargetRef::Player(PlayerId(1));
        let mut state = spell_target_selection_state(
            vec![target.clone()],
            vec![TargetRef::Player(PlayerId(0))],
            false,
        );

        let action = fallback_action_default(&state).expect("fallback returns an action");
        assert_eq!(
            action,
            GameAction::ChooseTarget {
                target: Some(target),
            }
        );
        assert!(engine::game::engine::apply_as_current(&mut state, action).is_ok());
    }

    #[test]
    fn fallback_spell_target_selection_skips_optional_empty_current_slot() {
        let mut state =
            spell_target_selection_state(Vec::new(), vec![TargetRef::Player(PlayerId(1))], true);

        let action = fallback_action_default(&state).expect("fallback returns an action");
        assert_eq!(action, GameAction::ChooseTarget { target: None });
        assert!(engine::game::engine::apply_as_current(&mut state, action).is_ok());
    }

    #[test]
    fn fallback_spell_target_selection_cancels_required_empty_current_slot() {
        let mut state =
            spell_target_selection_state(Vec::new(), vec![TargetRef::Player(PlayerId(1))], false);

        let action = fallback_action_default(&state).expect("fallback returns an action");
        assert_eq!(action, GameAction::CancelCast);
        assert!(engine::game::engine::apply_as_current(&mut state, action).is_ok());
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
            blocker_constraints: Default::default(),
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
            valid_attack_targets_by_attacker: None,
            attacker_constraints: Default::default(),
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
            valid_attack_targets_by_attacker: None,
            attacker_constraints: Default::default(),
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
            library_owner: None,
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
        let action = fallback_action_default(&state).expect("fallback returns an action");
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
        let action = fallback_action_default(&state).expect("fallback returns an action");
        assert!(
            matches!(action, GameAction::ChooseOption { ref choice } if choice == "foe"),
            "AI labeling opponent must pick foe, got {action:?}"
        );
    }

    #[test]
    fn ai_land_nonland_opponent_guess_uses_rng() {
        let mut state = make_state();
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Gollum, Scheming Guide".to_string(),
            Zone::Battlefield,
        );
        state.waiting_for = WaitingFor::NamedChoice {
            player: PlayerId(1),
            choice_type: ChoiceType::CardPredicateGuess {
                options: ChoiceType::land_or_nonland_card_predicate_options(),
            },
            options: ChoiceType::card_predicate_labels(
                &ChoiceType::land_or_nonland_card_predicate_options(),
            ),
            source: Some(resolution_choice_source(&state, source_id)),
            persist_player: None,
        };
        let config = create_config(AiDifficulty::Medium, Platform::Native);
        let mut saw_land = false;
        let mut saw_nonland = false;

        for seed in 0..64 {
            let mut rng = SmallRng::seed_from_u64(seed);
            match choose_action(&state, PlayerId(1), &config, &mut rng) {
                Some(GameAction::ChooseOption { choice }) if choice == "Land" => saw_land = true,
                Some(GameAction::ChooseOption { choice }) if choice == "Nonland" => {
                    saw_nonland = true;
                }
                other => panic!("expected Land/Nonland ChooseOption, got {other:?}"),
            }
        }

        assert!(
            saw_land && saw_nonland,
            "seeded AI guesses must exercise both Land and Nonland"
        );
    }

    #[test]
    fn opponent_guess_ai_choice_is_independent_of_private_answer_authority() {
        let mut state = make_state();
        let source_id = create_object(
            &mut state,
            CardId(0x0A11),
            PlayerId(1),
            "Private guess source".to_string(),
            Zone::Battlefield,
        );
        let context = engine::game::triggers::trigger_source_context_for_latch(
            &state,
            state.objects.get(&source_id).expect("source exists"),
        );
        state.waiting_for = WaitingFor::OpponentGuess {
            player: PlayerId(0),
            options: vec!["greater".to_string(), "not greater".to_string()],
            choice_type: ChoiceType::Labeled {
                options: vec!["greater".to_string(), "not greater".to_string()],
            },
            source: OpponentGuessSource {
                prompt: PromptSourceBinding::from_trigger_source(&context),
            },
            owner: Some(OpponentGuessOwner {
                context: context.clone(),
                committed_choice: Some(ChosenAttribute::Number(7)),
            }),
            proposition_truth: Some(true),
        };
        let config = create_config(AiDifficulty::Medium, Platform::Native);
        let mut first_rng = SmallRng::seed_from_u64(71);
        let first = choose_action(&state, PlayerId(0), &config, &mut first_rng)
            .expect("the guesser receives a legal option");

        let WaitingFor::OpponentGuess {
            owner,
            proposition_truth,
            ..
        } = &mut state.waiting_for
        else {
            unreachable!("fixture remains an opponent guess");
        };
        *owner = Some(OpponentGuessOwner {
            context,
            committed_choice: Some(ChosenAttribute::Number(1)),
        });
        *proposition_truth = Some(false);
        let mut second_rng = SmallRng::seed_from_u64(71);
        let second = choose_action(&state, PlayerId(0), &config, &mut second_rng)
            .expect("the guesser receives a legal option after private facts change");

        assert_eq!(
            first, second,
            "the seeded AI may use only public options, never private truth or committed choice"
        );
    }

    #[test]
    fn ai_regular_land_nonland_choice_does_not_use_guess_randomizer() {
        let mut state = make_state();
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Abundance".to_string(),
            Zone::Battlefield,
        );
        state.waiting_for = WaitingFor::NamedChoice {
            player: PlayerId(1),
            choice_type: ChoiceType::CardPredicate {
                options: ChoiceType::land_or_nonland_card_predicate_options(),
            },
            options: ChoiceType::card_predicate_labels(
                &ChoiceType::land_or_nonland_card_predicate_options(),
            ),
            source: Some(resolution_choice_source(&state, source_id)),
            persist_player: None,
        };
        let mut rng = SmallRng::seed_from_u64(1);

        assert!(
            random_card_predicate_guess(&state, PlayerId(1), &mut rng).is_none(),
            "ordinary land/nonland kind choices are strategic choices, not random guesses"
        );
    }

    /// Issue #6393: CardName NamedChoice keeps `options` empty and synthesizes
    /// candidates from `all_card_names`. Fallback must ask `legal_actions`, not
    /// `options.first()`, or restore softlocks after a successful rehydrate.
    #[test]
    fn named_choice_card_name_fallback_uses_legal_actions_when_options_empty() {
        let mut state = make_state();
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        state.all_card_names = vec!["Forest".to_string(), "Island".to_string()].into();
        state.waiting_for = WaitingFor::NamedChoice {
            player: PlayerId(0),
            choice_type: ChoiceType::CardName,
            options: Vec::new(),
            source: None,
            persist_player: None,
        };

        let action = fallback_action_default(&state).expect("fallback returns ChooseOption");
        assert!(
            matches!(action, GameAction::ChooseOption { ref choice } if choice == "Forest"),
            "expected Forest from legal_actions, got {action:?}"
        );
    }

    /// Issue #6393: when rehydrate never populated `all_card_names`, CardName
    /// prompts have zero legal actions — fallback must return None rather than
    /// inventing an option from the empty `options` list.
    #[test]
    fn named_choice_card_name_fallback_none_when_all_card_names_empty() {
        let mut state = make_state();
        state.all_card_names = Vec::new().into();
        state.waiting_for = WaitingFor::NamedChoice {
            player: PlayerId(0),
            choice_type: ChoiceType::CardName,
            options: Vec::new(),
            source: None,
            persist_player: None,
        };

        assert!(
            engine::ai_support::legal_actions(&state).is_empty(),
            "test premise: empty all_card_names must yield no legal ChooseOption"
        );
        assert_eq!(
            fallback_action_default(&state),
            None,
            "empty legal set must not fabricate a NamedChoice option"
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

        let action = fallback_action_default(&state).expect("fallback returns an action");
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

        let action = fallback_action_default(&state).expect("fallback returns an action");
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

        let action = fallback_action_default(&state).expect("fallback returns an action");
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
        let action = fallback_action_default(&state).expect("fallback returns an action");
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

        let action = fallback_action_default(&state).expect("fallback returns an action");
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
            origin: engine::types::ability::AdditionalCostOrigin::Kicker,
            gift_kind: None,
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

    /// Create a creature (high `intrinsic_value`) directly in `owner`'s hand.
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

        state.waiting_for = WaitingFor::MulliganDecision {
            pending: vec![
                engine::types::game_state::MulliganDecisionEntry {
                    player: PlayerId(0),
                    mulligan_count: 1,
                    phase: MulliganDecisionPhase::BottomCards {
                        count: 1,
                        then: PendingMulliganAction::Keep,
                    },
                },
                engine::types::game_state::MulliganDecisionEntry {
                    player: PlayerId(1),
                    mulligan_count: 3,
                    phase: MulliganDecisionPhase::BottomCards {
                        count: 3,
                        then: PendingMulliganAction::Keep,
                    },
                },
            ],
            free_first_mulligan: false,
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

    /// The AI must scope to its own owed count for the `OpeningHandBottomCards`
    /// path (TL:R 906.6 Tiny Leaders forced bottom), not just the folded
    /// `MulliganDecision` bottoming, when a second player is pending. Guards
    /// against a future refactor silently dropping one variant.
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
        let bottoms = plan_aware_bottom_cards(
            &state,
            PlayerId(1),
            2,
            &DeckFeatures::default(),
            &plan,
            None,
        );
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

        let bottoms = plan_aware_bottom_cards(
            &state,
            PlayerId(1),
            1,
            &features,
            &PlanSnapshot::default(),
            None,
        );

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
        fallback_action_default(&state).expect("AssignCombatDamage fallback must produce an action")
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
        scored.sort_by(|a, b| a.0.cmp_stable(&b.0));
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

    // ---- U2: PV threading + rung witnesses (drive `run_iterative_deepening`) ----

    /// Reach the production root beam seam so iterative-deepening tests observe
    /// the same validation, payment-successor retention, rank, and width path
    /// that public scoring uses.
    fn build_root_beam(state: &GameState, services: &PlannerServices<'_>) -> Vec<RankedCandidate> {
        let ctx = build_decision_context(state);
        let prepared = prepare_payment_candidates(state, ctx.candidates.clone());
        let candidates = services.validate_candidates(
            state,
            prepared
                .iter()
                .map(|candidate| candidate.candidate.clone())
                .collect(),
        );
        let gated = gate_candidates(
            state,
            &ctx,
            candidates,
            services.ai_player,
            services.config,
            &services.context,
        );
        let mut gated: Vec<_> = gated
            .into_iter()
            .filter(|candidate| {
                priority_action_is_allowed_by_loop_guards(
                    state,
                    services.ai_player,
                    &candidate.candidate.action,
                )
            })
            .collect();
        gated.sort_by(|left, right| left.candidate.action.cmp_stable(&right.candidate.action));
        rank_root_payment_candidates(
            state,
            &ctx,
            &prepared,
            &gated,
            services,
            services.config.search.max_branching as usize,
        )
    }

    fn score_of(scored: &[(GameAction, f64)], action: &GameAction) -> f64 {
        scored
            .iter()
            .find(|(a, _)| a == action)
            .map(|(_, s)| *s)
            .unwrap_or_else(|| panic!("action {action:?} absent from scored output"))
    }

    #[test]
    fn retained_root_payment_successor_bypasses_inapplicable_fallback() {
        let state = make_state();
        let retained = apply_candidate(
            &state,
            &CandidateAction {
                action: GameAction::PassPriority,
                metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Pass),
            },
        )
        .expect("reach-guard: a concrete root successor exists");
        let hostile = CandidateAction {
            action: GameAction::ActivateAbility {
                source_id: ObjectId(99999),
                ability_index: 0,
            },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Mana),
        };
        assert!(
            apply_candidate(&state, &hostile).is_none(),
            "reach-guard: the fallback action is inapplicable at the root"
        );
        let mut config = create_config(AiDifficulty::Hard, Platform::Native).into_measurement(43);
        config.search.planner_mode = PlannerMode::BeamOnly;
        let policies = PolicyRegistry::shared();
        let mut hostile_services = PlannerServices::new_default(PlayerId(0), &config, policies);
        let hostile_result = run_iterative_deepening(
            &state,
            vec![RankedCandidate::with_payment_successor(
                hostile,
                0.0,
                retained.clone(),
            )],
            0.1,
            &config,
            &mut hostile_services,
        );
        let mut control_services = PlannerServices::new_default(PlayerId(0), &config, policies);
        let control_result = run_iterative_deepening(
            &state,
            vec![RankedCandidate::with_payment_successor(
                CandidateAction {
                    action: GameAction::PassPriority,
                    metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Pass),
                },
                0.0,
                retained,
            )],
            0.1,
            &config,
            &mut control_services,
        );
        assert_eq!(hostile_result[0].1, control_result[0].1);
        assert!(
            hostile_result[0].1 > -900.0,
            "the retained successor prevents the failed-apply penalty"
        );
    }

    /// Fixture with several cheap castable creatures + an opponent threat, so the
    /// search tree has rich interior branching (subtrees far exceed a tiny node
    /// cap => genuine budget starvation) AND a value gradient (casting a creature
    /// beats passing, so the search argmax can differ from a pass-first beam).
    fn starvation_state() -> GameState {
        let mut state = make_state();
        state.lands_played_this_turn = 1;
        let _opp = add_creature(&mut state, PlayerId(1), 3, 3);
        for i in 0..4u64 {
            let id = create_object(
                &mut state,
                CardId(900 + i),
                PlayerId(0),
                format!("Bear{i}"),
                Zone::Hand,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.mana_cost = engine::types::mana::ManaCost::Cost {
                shards: Vec::new(),
                generic: 1,
            };
        }
        add_mana(&mut state, PlayerId(0), ManaType::Colorless, 6);
        state
    }

    /// Extract (PassPriority, first CastSpell) real candidates from `state`.
    fn pass_and_first_cast(state: &GameState) -> (CandidateAction, CandidateAction) {
        let ctx = build_decision_context(state);
        let pass = ctx
            .candidates
            .iter()
            .find(|c| matches!(c.action, GameAction::PassPriority))
            .cloned()
            .expect("a PassPriority candidate exists at priority");
        let cast = ctx
            .candidates
            .iter()
            .find(|c| matches!(c.action, GameAction::CastSpell { .. }))
            .cloned()
            .expect("a CastSpell candidate exists (creatures in hand + mana)");
        (pass, cast)
    }

    // V5: empty-state equivalence — a BeamOnly (ceiling 0) run enters `search_value`
    // zero times, so killers stay clean, both cutoff/ordering counters are 0, and
    // exactly one rung witness (rung 0) is recorded.
    #[test]
    fn beam_only_run_is_search_value_free() {
        let state = searchable_state();
        let policies = PolicyRegistry::shared();
        let mut config = create_config(AiDifficulty::Hard, Platform::Native).into_measurement(7);
        config.search.planner_mode = PlannerMode::BeamOnly; // ceiling 0
        let mut services = PlannerServices::new_default(PlayerId(0), &config, policies);
        let ranked = build_root_beam(&state, &services);
        let out = run_iterative_deepening(&state, ranked, 0.1, &config, &mut services);

        assert!(!out.is_empty(), "rung 0 produces the floor");
        // Reach-guard: rung 0 ran (non-vacuous).
        assert_eq!(services.rung_stats.len(), 1, "exactly rung 0 executed");
        assert!(services.rung_stats[0].completed);
        assert_eq!(services.rung_stats[0].depth, 0);
        assert_eq!(services.beta_cutoffs, 0, "no search_value => no cutoffs");
        assert_eq!(
            services.killer_orderings, 0,
            "no search_value => no killer ordering"
        );
        assert!(
            services
                .killers
                .iter()
                .all(|ply| ply.iter().all(Option::is_none)),
            "no cutoffs => killer table stays empty"
        );
    }

    // V6: the rung witness records completion + node usage for every executed rung.
    #[test]
    fn rung_stats_record_completion_and_node_usage() {
        let state = searchable_state();
        let policies = PolicyRegistry::shared();
        let config = create_config(AiDifficulty::Hard, Platform::Native).into_measurement(7);
        let mut services = PlannerServices::new_default(PlayerId(0), &config, policies);
        let ranked = build_root_beam(&state, &services);
        let _ = run_iterative_deepening(&state, ranked, 0.1, &config, &mut services);

        let ceiling = config.search.max_depth.saturating_sub(1);
        assert!(
            ceiling >= 1,
            "fixture precondition: ceiling deepens past rung 0"
        );
        assert_eq!(
            services.rung_stats.len() as u32,
            ceiling + 1,
            "one witness per rung 0..=ceiling"
        );
        assert!(
            services.rung_stats.iter().all(|r| r.completed),
            "roomy measurement budget: every rung completes"
        );
        for r in services.rung_stats.iter().filter(|r| r.depth >= 1) {
            assert!(
                r.nodes_used > 0,
                "searched rungs (depth >= 1) consume nodes"
            );
        }
    }

    // V6 hostile (saturation): a tiny node cap saturates the deepest searched rung
    // while it is still ACCEPTED (node-budget exhaustion does not discard). The
    // saturation predicate is `nodes_used >= max_nodes` (not `==`): `tick()`
    // increments unconditionally at `search_value` entry while `exhausted()` checks
    // `>=`, so the counter can overshoot the cap by one.
    #[test]
    fn rung_stats_saturated_rung_is_still_accepted() {
        let state = searchable_state();
        let policies = PolicyRegistry::shared();
        let mut config = create_config(AiDifficulty::Hard, Platform::Native).into_measurement(7);
        config.search.max_nodes = 4; // tiny -> deepest searched rung saturates
        let mut services = PlannerServices::new_default(PlayerId(0), &config, policies);
        let ranked = build_root_beam(&state, &services);
        let _ = run_iterative_deepening(&state, ranked, 0.1, &config, &mut services);

        let deepest = services.rung_stats.last().expect("at least rung 0 ran");
        assert!(
            deepest.completed,
            "node-budget exhaustion must NOT discard a rung"
        );
        assert!(
            services
                .rung_stats
                .iter()
                .any(|r| r.depth >= 1 && r.nodes_used >= r.max_nodes),
            "a searched rung must saturate the tiny node pool (nodes_used >= max_nodes)"
        );
    }

    // V6 hostile (pre-expired): an already-expired interactive deadline breaks at
    // the rung-entry guard before any candidate loop runs, so zero rungs execute
    // and the witness list is empty — the honest "no search happened" trace (and
    // the floor is still returned).
    #[test]
    fn pre_expired_deadline_records_no_rungs() {
        let state = searchable_state();
        let policies = PolicyRegistry::shared();
        let config = create_config(AiDifficulty::Hard, Platform::Native); // interactive
        let context = crate::context::AiContext::empty(&config.weights);
        let mut services = PlannerServices::with_deadline(
            PlayerId(0),
            &config,
            policies,
            context,
            Some(engine::util::Deadline::after(0)), // pre-expired
        );
        let ranked = build_root_beam(&state, &services);
        assert!(!ranked.is_empty(), "reach-guard: the beam is non-empty");
        let out = run_iterative_deepening(&state, ranked, 0.1, &config, &mut services);
        assert!(!out.is_empty(), "the tactical-only floor is still returned");
        assert!(
            services.rung_stats.is_empty(),
            "a pre-expired deadline executes zero rungs => no rung witness"
        );
    }

    // V3 tie row: `pv_argmax` resolves ties and non-finite scores through the
    // `cmp_stable` total order — deterministic across calls and panic-free on NaN
    // (never a bare `max_by(|a, b| a.partial_cmp(b).unwrap())`).
    #[test]
    fn pv_argmax_is_deterministic_and_nan_safe() {
        let tied = vec![
            (GameAction::PassPriority, 5.0),
            (GameAction::CancelCast, 5.0),
        ];
        let pick = pv_argmax(&tied).cloned();
        assert_eq!(
            pv_argmax(&tied).cloned(),
            pick,
            "tie resolution is byte-stable across repeated calls"
        );
        assert!(
            pick == Some(GameAction::PassPriority) || pick == Some(GameAction::CancelCast),
            "the winner is one of the tied actions"
        );
        // A NaN score must resolve via the Equal fallback, never panic.
        let with_nan = vec![
            (GameAction::PassPriority, f64::NAN),
            (GameAction::CancelCast, 1.0),
        ];
        let _ = pv_argmax(&with_nan);
        assert!(pv_argmax(&[]).is_none(), "empty input yields None");
    }

    // V3: the rung-1 PV rotate steers the shared per-rung budget to the PV
    // candidate. Budget-starvation fixture: a tight node cap means the first-
    // searched root subtree drains the pool. With the rotate, the PV candidate B
    // is searched FIRST at rung 2, so its rung-2 score equals its independent
    // full-depth continuation (computed on FRESH services). Reverting the rotate
    // makes A drain the pool first and B collapse toward quiesced eval.
    #[test]
    fn pv_rotate_gives_pv_candidate_full_depth_under_starvation() {
        let state = starvation_state();
        let policies = PolicyRegistry::shared();
        let mut config = create_config(AiDifficulty::Hard, Platform::Native).into_measurement(7);
        config.search.max_depth = 3; // ceiling 2 (rung 1 sets PV, rung 2 uses it)
        config.search.max_nodes = 6; // tight: one root subtree drains the pool
        let tw = 0.1;

        // Beam deliberately ordered PASS-FIRST so ranked[0] = A = pass while the
        // board-improving cast (B) is the search argmax — the case where the PV
        // rotate matters. Scores are 0.0 so the value function is pure continuation
        // (no tactical term interfering with the demonstration).
        let (pass, cast) = pass_and_first_cast(&state);
        let ranked = vec![
            RankedCandidate::new(pass.clone(), 0.0),
            RankedCandidate::new(cast.clone(), 0.0),
        ];
        let a = ranked[0].candidate.action.clone();

        // The PV rung 2 searches first == rung-1's argmax under this beam/budget.
        let b = {
            let mut cfg1 = config.clone();
            cfg1.search.max_depth = 2; // ceiling 1
            let mut s = PlannerServices::new_default(PlayerId(0), &cfg1, policies);
            let rung1 = run_iterative_deepening(&state, ranked.clone(), tw, &cfg1, &mut s);
            pv_argmax(&rung1).cloned().expect("rung 1 has an argmax")
        };
        assert_ne!(b, a, "reach-guard: the PV must differ from ranked[0]");

        let b_ranked = ranked
            .iter()
            .find(|r| r.candidate.action == b)
            .expect("B is in the beam");
        let b_tactical = b_ranked.score;
        let b_sim = apply_candidate(&state, &b_ranked.candidate).expect("B applies");

        // Independent full-depth control on FRESH services (empty TT) + fresh
        // budget. `eval_cache` is a pure-function memo (value-transparent), so only
        // the TT could contaminate the comparison — guarded below by tt_hits == 0.
        let control_cont = {
            let mut fresh = PlannerServices::new_default(PlayerId(0), &config, policies);
            let mut fresh_budget = SearchBudget::new(config.search.max_nodes);
            let planner = BeamContinuationPlanner {
                depth: 2,
                rollout_depth: config.search.rollout_depth,
            };
            planner.search_value(
                &b_sim,
                2,
                0,
                f64::NEG_INFINITY,
                f64::INFINITY,
                &mut fresh,
                &mut fresh_budget,
            )
        };
        let control_quiesced = {
            let mut q = PlannerServices::new_default(PlayerId(0), &config, policies);
            q.evaluate_state_quiesced(&b_sim)
        };
        // Precondition (b): B's searched value differs from its quiesced eval, else
        // reverting the rotate could not fail the score assertion.
        assert_ne!(
            control_cont, control_quiesced,
            "B's depth-2 searched value must differ from its quiesced eval"
        );

        // Measured run: ceiling 2, pass-first beam. Rung 1 sets PV = B; rung 2
        // rotates B to the front and searches it first with the fresh per-rung pool.
        let mut services = PlannerServices::new_default(PlayerId(0), &config, policies);
        let out = run_iterative_deepening(&state, ranked, tw, &config, &mut services);

        // TT-contamination reach-guard: the measured/control equality is TT-free.
        assert_eq!(
            services.tt_hits, 0,
            "no transposition hits => control equality is TT-provenance-free"
        );
        // Starvation regime reach-guard: a searched rung saturated the pool.
        assert!(
            services
                .rung_stats
                .iter()
                .any(|r| r.depth >= 1 && r.nodes_used >= r.max_nodes),
            "a searched rung saturated the node pool (the starvation regime)"
        );

        let out_b = score_of(&out, &b);
        assert!(
            (out_b - (control_cont + b_tactical * tw)).abs() < 1e-9,
            "PV-first gives B its full-depth continuation value \
             (got {out_b}, expected {})",
            control_cont + b_tactical * tw
        );
    }

    // V4: the rung-0 rotate is skipped (the `iter_depth >= 1` gate), so rung 1
    // provably sees today's ordering. Two ceiling-1 runs on fresh services: one on
    // the natural beam, one on a beam pre-rotated to put rung-0's argmax first.
    // With the gate present, run 1's rung-0 does NOT rotate, so its rung-1 order
    // differs from the pre-rotated run under starvation => outputs differ. Removing
    // the gate makes run 1 also rotate rung-0's argmax to the front, collapsing the
    // two outputs to equality — so `assert_ne!` is revert-failing for the gate.
    #[test]
    fn rung_zero_rotate_is_gated_off() {
        let state = starvation_state();
        let policies = PolicyRegistry::shared();
        let mut config = create_config(AiDifficulty::Hard, Platform::Native).into_measurement(7);
        config.search.max_depth = 2; // ceiling 1
                                     // Depth-1 rung subtrees are shallow, so the cap must be very tight to
                                     // starve at rung 1 (make its output order-sensitive). 3 nodes lets the
                                     // first candidate search while the second collapses to quiesced eval.
        config.search.max_nodes = 3;
        let tw = 0.1;

        // Pass-first beam so rung-0's argmax (the board-improving cast) differs
        // from ranked[0] = pass — making a rung-0 rotate observable.
        let (pass, cast) = pass_and_first_cast(&state);
        let ranked = vec![
            RankedCandidate::new(pass.clone(), 0.0),
            RankedCandidate::new(cast.clone(), 0.0),
        ];
        let a = ranked[0].candidate.action.clone();

        // rung-0 argmax (quiesced eval per candidate) via a ceiling-0 run.
        let b0 = {
            let mut cfg0 = config.clone();
            cfg0.search.planner_mode = PlannerMode::BeamOnly; // ceiling 0
            let mut s = PlannerServices::new_default(PlayerId(0), &cfg0, policies);
            let rung0 = run_iterative_deepening(&state, ranked.clone(), tw, &cfg0, &mut s);
            pv_argmax(&rung0).cloned().expect("rung 0 has an argmax")
        };
        // Reach-guard: rung-0 argmax must differ from ranked[0], else pre-rotating
        // is a no-op and the test is vacuous.
        assert_ne!(
            b0, a,
            "reach-guard: rung-0 argmax differs from ranked[0] (rotate is observable)"
        );

        // Run 1: natural beam (with the gate, rung 1 keeps this order).
        let out_natural = {
            let mut s = PlannerServices::new_default(PlayerId(0), &config, policies);
            run_iterative_deepening(&state, ranked.clone(), tw, &config, &mut s)
        };
        // Run 2: beam pre-rotated so B0 is first (mimics an un-gated rung-0 rotate).
        let out_prerotated = {
            let mut pre = ranked.clone();
            rotate_pv_to_front(&mut pre, &b0);
            let mut s = PlannerServices::new_default(PlayerId(0), &config, policies);
            run_iterative_deepening(&state, pre, tw, &config, &mut s)
        };

        assert_ne!(
            out_natural, out_prerotated,
            "with the rung-0 gate, rung 1 keeps today's order; the pre-rotated \
             (un-gated) order diverges under starvation. Removing the gate makes \
             these equal."
        );
    }

    // V7b: ensemble determinism on the public surface. K >= 2 measurement runs must
    // be byte-identical — the new killer/rung state is arrays with no HashMap
    // iteration order, so #4878-style ordering stability holds end-to-end.
    #[test]
    fn ensemble_is_deterministic_with_move_ordering() {
        let state = searchable_state();
        let mut config = create_config(AiDifficulty::Hard, Platform::Native).into_measurement(7);
        config.search.determinization_samples = 2;
        let session = AiSession::arc_from_game(&state);

        let first = score_candidates_with_session(&state, PlayerId(0), &config, &session);
        let second = score_candidates_with_session(&state, PlayerId(0), &config, &session);

        assert!(
            has_cast(&first),
            "reach-guard: the search-enabled ID loop is reached"
        );
        assert_eq!(
            first, second,
            "K >= 2 ensemble output must be byte-identical across runs"
        );
    }

    // ---------------------------------------------------------------------
    // CR 514.1 cleanup discard — keep-tier fixtures.
    //
    // TEST FOOT-GUN: `deterministic_choice(.., None)` yields `plan == None`,
    // every card `Ordinary`, and therefore `main` behaviour. A tiering test
    // that forgets `Some(&ctx)` observes `main` VACUOUSLY and proves nothing.
    // Exactly two tests below pass `None` on purpose — `discard_..._no_plan_entry`
    // and `quiescence_context_none_keeps_main_discard_ordering` — and both
    // assert an exact object id, so neither can pass by accident.
    //
    // The plan key is the DISCARDING player (the `WaitingFor`'s `player`), not
    // `ai_player`. In most fixtures they coincide; in
    // `discard_to_hand_size_keys_plan_and_lands_on_the_discarding_player` they
    // do not, and that is the point of that fixture.
    // ---------------------------------------------------------------------

    /// A 4-player Commander state at turn 5 — the regime three of the four
    /// user reports come from. `scripts/ai-gate.sh` is structurally two-player,
    /// so it cannot reach this regime; these fixtures are the primary evidence.
    fn commander_discard_state() -> GameState {
        let mut state = GameState::new(engine::types::format::FormatConfig::commander(), 4, 0);
        state.turn_number = 5;
        state.phase = Phase::PreCombatMain;
        state
    }

    /// A land on `player`'s battlefield — the subtrahend in `lands_behind`.
    fn land_on_battlefield(state: &mut GameState, player: PlayerId) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            player,
            "Swamp".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
        id
    }

    fn set_cost(state: &mut GameState, id: ObjectId, shards: Vec<ManaCostShard>, generic: u32) {
        state.objects.get_mut(&id).unwrap().mana_cost =
            engine::types::mana::ManaCost::Cost { shards, generic };
    }

    /// An untargeted activated `Effect::Mana` ability — the structural mark of
    /// a mana source under CR 605.1a / `is_mana_ability`.
    fn push_mana_ability(state: &mut GameState, id: ObjectId) {
        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: engine::types::ability::ManaProduction::Fixed {
                    colors: vec![engine::types::mana::ManaColor::Black],
                    contribution: engine::types::ability::ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        );
        ability.cost = Some(engine::types::ability::AbilityCost::Tap);
        let obj = state.objects.get_mut(&id).unwrap();
        Arc::make_mut(&mut obj.abilities).push(ability);
    }

    /// MV-3 artifact with a mana ability (Commander's Sphere shape).
    /// `intrinsic_value` = (0 shards + 3 generic) * 0.5 = **1.5**.
    fn mana_rock_in_hand(state: &mut GameState, owner: PlayerId) -> ObjectId {
        let id = named_vanilla_in_hand(state, owner, "Mana Rock");
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);
        set_cost(state, id, Vec::new(), 3);
        push_mana_ability(state, id);
        id
    }

    /// 5/5 for `{B}{5}`. `intrinsic_value` = 5*1.5 + 5 + (1+5)*0.5 = **15.5**.
    fn fatty_in_hand(state: &mut GameState, owner: PlayerId) -> ObjectId {
        let id = named_vanilla_in_hand(state, owner, "Fatty");
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(5);
            obj.toughness = Some(5);
        }
        set_cost(state, id, vec![ManaCostShard::Black], 5);
        id
    }

    /// MV-1 noncreature spell. `intrinsic_value` = (0 + 1) * 0.5 = **0.5**.
    fn junk_instant_in_hand(state: &mut GameState, owner: PlayerId) -> ObjectId {
        let id = named_vanilla_in_hand(state, owner, "Junk");
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);
        set_cost(state, id, Vec::new(), 1);
        id
    }

    fn discard_waiting_for(state: &GameState, player: PlayerId, count: usize) -> WaitingFor {
        WaitingFor::DiscardToHandSize {
            player,
            count,
            cards: state.players[player.0 as usize]
                .hand
                .iter()
                .copied()
                .collect(),
        }
    }

    fn selected_card(action: Option<GameAction>) -> ObjectId {
        match action {
            Some(GameAction::SelectCards { cards }) => {
                assert_eq!(
                    cards.len(),
                    1,
                    "reach-guard: the discard arm must select exactly one card, \
                     not an empty or multi selection"
                );
                cards[0]
            }
            other => panic!("expected SelectCards from the discard arm, got {other:?}"),
        }
    }

    /// MAIN TEST. CR 514.1 + CR 701.9a: while the discarding player is behind
    /// their own land schedule, cleanup discard surrenders a creature rather
    /// than a mana rock or a land.
    ///
    /// FAILS ON BASE: the pre-change arm sorts on the raw scalar, whose minimum
    /// is the MV-3 rock at 1.5 (vs. Swamp 3.0, 5/5 creature 15.5).
    #[test]
    fn discard_to_hand_size_keeps_mana_sources_while_behind_on_lands() {
        let mut state = commander_discard_state();
        let ai = PlayerId(0);
        land_on_battlefield(&mut state, ai);

        let rock = mana_rock_in_hand(&mut state, ai);
        let swamps = [land_in_hand(&mut state, ai), land_in_hand(&mut state, ai)];
        let fatties: Vec<_> = (0..4).map(|_| fatty_in_hand(&mut state, ai)).collect();
        state.waiting_for = discard_waiting_for(&state, ai, 1);

        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        // Derived land target 6 against 1 land on board => lands_behind = +5.
        let ctx = context_with_plans(&state, ai, &config, &[(ai, default_deck_plan())]);

        let chosen = selected_card(deterministic_choice(&state, ai, &config, &[], Some(&ctx)));

        assert_ne!(
            chosen, rock,
            "the mana rock must not be pitched while behind"
        );
        assert!(
            !swamps.contains(&chosen),
            "a land must not be pitched while behind"
        );
        assert!(
            fatties.contains(&chosen),
            "positive reach-guard: the discarded card must be one of the four creatures"
        );
    }

    /// F1 — exactly on curve. `lands_behind == 0` puts every card in
    /// `Ordinary`, so the tuple comparator degenerates to the scalar and the
    /// selection is identical to `main`: the junk instant (0.5).
    ///
    /// Discriminates against a naive "protect lands whenever a plan exists"
    /// design, which would tier the Swamp above the instant and pitch it.
    #[test]
    fn discard_to_hand_size_on_curve_matches_scalar_ordering() {
        let mut state = commander_discard_state();
        let ai = PlayerId(0);
        // Exactly the derived land target (6) — the `Ordinary` boundary.
        for _ in 0..6 {
            land_on_battlefield(&mut state, ai);
        }

        let swamp = land_in_hand(&mut state, ai);
        let junk = junk_instant_in_hand(&mut state, ai);
        let fatties: Vec<_> = (0..2).map(|_| fatty_in_hand(&mut state, ai)).collect();
        state.waiting_for = discard_waiting_for(&state, ai, 1);

        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let plan = default_deck_plan();
        assert_eq!(
            plan.land_target(),
            6,
            "fixture premise: 6 lands on board must be exactly on plan"
        );
        let ctx = context_with_plans(&state, ai, &config, &[(ai, plan)]);

        let chosen = selected_card(deterministic_choice(&state, ai, &config, &[], Some(&ctx)));

        assert_eq!(
            chosen, junk,
            "on curve, the lowest scalar (the junk instant) is discarded — \
             identical to pre-change behaviour"
        );
        assert_ne!(chosen, swamp);
        assert!(!fatties.contains(&chosen));
    }

    /// F2 — a live context whose session carries NO plan entry for the
    /// discarding player (the shape `AiSession`'s `deck.is_empty()` early
    /// return produces). `plan.get()` returns `None`, every card is `Ordinary`,
    /// and `main` ordering is reproduced.
    #[test]
    fn discard_to_hand_size_without_plan_entry_matches_scalar_ordering() {
        let mut state = commander_discard_state();
        let ai = PlayerId(0);
        land_on_battlefield(&mut state, ai);

        let swamp = land_in_hand(&mut state, ai);
        let junk = junk_instant_in_hand(&mut state, ai);
        for _ in 0..2 {
            fatty_in_hand(&mut state, ai);
        }
        state.waiting_for = discard_waiting_for(&state, ai, 1);

        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        // Context present, plan map EMPTY — the `session.plan.get()` None arm.
        let ctx = context_with_plans(&state, ai, &config, &[]);

        let chosen = selected_card(deterministic_choice(&state, ai, &config, &[], Some(&ctx)));

        assert_eq!(
            chosen, junk,
            "with no plan authority every card is Ordinary, so the scalar minimum wins"
        );
        assert_ne!(chosen, swamp);
    }

    /// F2b — Pins the root/rollout asymmetry as DELIBERATE, not accidental.
    /// `planner/mod.rs`'s quiescence loop calls `deterministic_choice` with
    /// `context: None` on every rollout step, so the keep-tier is inert there and
    /// the rollout still models `main`'s "pitch the mana rock" behaviour. Threading
    /// an `AiContext` into quiescence is a declared follow-up — building a
    /// `DeckProfile` + `SynergyGraph` per quiescence step is the expense the
    /// root-only design deliberately refuses. If you make quiescence
    /// plan-aware, THIS TEST MUST CHANGE, and that change should be deliberate.
    ///
    /// The hand, board, turn and `waiting_for` are the main test's exactly; only
    /// the `context` argument differs.
    #[test]
    fn quiescence_context_none_keeps_main_discard_ordering() {
        let mut state = commander_discard_state();
        let ai = PlayerId(0);
        land_on_battlefield(&mut state, ai);

        let rock = mana_rock_in_hand(&mut state, ai);
        for _ in 0..2 {
            land_in_hand(&mut state, ai);
        }
        for _ in 0..4 {
            fatty_in_hand(&mut state, ai);
        }
        state.waiting_for = discard_waiting_for(&state, ai, 1);

        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let chosen = selected_card(deterministic_choice(&state, ai, &config, &[], None));

        assert_eq!(
            chosen, rock,
            "with no context the tier is inert and the rollout reproduces main's \
             scalar minimum — the mana rock"
        );
    }

    /// F3 — flooded (`lands_behind < 0`). The SAME Swamp that the main test
    /// protects is surrendered here, proving the valuation is contextual and
    /// not equivalent to bumping the land constant.
    ///
    /// FAILS ON BASE: `main`'s minimum is the junk instant (0.5).
    #[test]
    fn discard_to_hand_size_pitches_surplus_land_while_flooded() {
        let mut state = commander_discard_state();
        let ai = PlayerId(0);
        for _ in 0..10 {
            land_on_battlefield(&mut state, ai);
        }

        let swamps = [land_in_hand(&mut state, ai), land_in_hand(&mut state, ai)];
        let junk = junk_instant_in_hand(&mut state, ai);
        for _ in 0..2 {
            fatty_in_hand(&mut state, ai);
        }
        state.waiting_for = discard_waiting_for(&state, ai, 1);

        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        // Derived land target 6 against 10 lands => lands_behind = -4.
        let ctx = context_with_plans(&state, ai, &config, &[(ai, default_deck_plan())]);

        let chosen = selected_card(deterministic_choice(&state, ai, &config, &[], Some(&ctx)));

        assert!(
            swamps.contains(&chosen),
            "while flooded a surplus land is Surplus-tiered and pitched ahead of \
             the junk instant"
        );
        assert_ne!(chosen, junk);
    }

    /// A mana rock on `player`'s battlefield — an artifact carrying a
    /// renewable `{T}: Add {B}` ability, so `zone_eval::is_intrinsic_mana_source`
    /// counts it toward `mana_behind` while `plan::controlled_lands` ignores it.
    fn rock_on_battlefield(state: &mut GameState, player: PlayerId) -> ObjectId {
        let id = artifact_on_battlefield(state, player, 3);
        push_mana_ability(state, id);
        id
    }

    /// F13 — the two development axes, at the production discard seam. THE
    /// reported 4-player-Commander failure, end to end.
    ///
    /// Board: 2 lands + 4 mana rocks. The land schedule reads **+4 behind**
    /// (rocks are not lands, CR 305.1); the mana schedule reads **0 — exactly on
    /// plan** (6 sources against a mature target of 6). So a spare rock in hand
    /// is `Ordinary` and, at 1.5, the cheapest card to surrender; a Swamp in
    /// hand is still `NeededManaSource` and must survive.
    ///
    /// FAILS ON THE SINGLE-AXIS RULE: reading `lands_behind` for both roles
    /// promotes the spare rock to `NeededManaSource` off a deficit that playing
    /// the rock could never close, and the arm surrenders a 15.5 fatty instead.
    #[test]
    fn discard_to_hand_size_does_not_promote_a_rock_on_a_land_only_deficit() {
        let mut state = commander_discard_state();
        let ai = PlayerId(0);
        for _ in 0..2 {
            land_on_battlefield(&mut state, ai);
        }
        for _ in 0..4 {
            rock_on_battlefield(&mut state, ai);
        }

        let spare_rock = mana_rock_in_hand(&mut state, ai);
        let swamp = land_in_hand(&mut state, ai);
        let fatties: Vec<_> = (0..2).map(|_| fatty_in_hand(&mut state, ai)).collect();
        state.waiting_for = discard_waiting_for(&state, ai, 1);

        let plan = default_deck_plan();
        let realized = crate::plan::PlanState::realize(&state, ai, &plan);
        assert_eq!(
            (realized.lands_behind, realized.mana_behind),
            (4, 0),
            "fixture premise: the two axes must DISAGREE here, or this test \
             cannot discriminate between them"
        );

        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let ctx = context_with_plans(&state, ai, &config, &[(ai, plan)]);

        let chosen = selected_card(deterministic_choice(&state, ai, &config, &[], Some(&ctx)));

        assert_eq!(
            chosen, spare_rock,
            "with the manabase complete the spare rock is Ordinary and is the \
             cheapest card in hand; only a LAND-keyed deficit would protect it"
        );
        assert_ne!(
            chosen, swamp,
            "positive reach-guard: the land axis is still live — the Swamp is \
             NeededManaSource off lands_behind = +4, so this is not a \
             plan-blind pass"
        );
        assert!(!fatties.contains(&chosen));
    }

    /// The sibling direction: while the manabase itself is short, an accelerant
    /// IS promoted. Without this, F13 alone would be satisfied by deleting the
    /// accelerant branch entirely.
    #[test]
    fn discard_to_hand_size_keeps_an_accelerant_while_behind_on_mana() {
        let mut state = commander_discard_state();
        let ai = PlayerId(0);
        for _ in 0..2 {
            land_on_battlefield(&mut state, ai);
        }

        let spare_rock = mana_rock_in_hand(&mut state, ai);
        let fatties: Vec<_> = (0..3).map(|_| fatty_in_hand(&mut state, ai)).collect();
        state.waiting_for = discard_waiting_for(&state, ai, 1);

        let plan = default_deck_plan();
        let realized = crate::plan::PlanState::realize(&state, ai, &plan);
        assert_eq!(
            (realized.lands_behind, realized.mana_behind),
            (4, 4),
            "fixture premise: both axes are short here"
        );

        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let ctx = context_with_plans(&state, ai, &config, &[(ai, plan)]);

        let chosen = selected_card(deterministic_choice(&state, ai, &config, &[], Some(&ctx)));

        assert_ne!(
            chosen, spare_rock,
            "behind on the MANA schedule, the rock is NeededManaSource"
        );
        assert!(
            fatties.contains(&chosen),
            "positive reach-guard: a creature is surrendered instead"
        );
    }

    /// A 4-player Commander cleanup step in which `controller` controls
    /// `controlled` under CR 723.1 (the Mindslaver shape), and `controlled` is
    /// the active player discarding down to hand size.
    ///
    /// REACHABILITY: this is the ONLY production shape in which the AI is asked
    /// to submit a `DiscardToHandSize` for a seat that is not its own. At the
    /// root the engine only prompts the authorized submitter, and the rollout
    /// quiescence loop (`planner/mod.rs`) passes the *acting* player as the
    /// optimizing seat, so `waiting_for.player != ai_player` there never
    /// happens either. A fixture without the control latch would be testing a
    /// state the engine cannot produce.
    ///
    /// Returns `(state, swamp, fatty)` — the controlled player's whole hand.
    fn mindslaver_discard_state(
        controller: PlayerId,
        controlled: PlayerId,
        controller_lands: usize,
        controlled_lands: usize,
    ) -> (GameState, ObjectId, ObjectId) {
        let mut state = commander_discard_state();
        // CR 514.1: the cleanup discard belongs to the active player, and
        // CR 723.1 control applies for the whole of that player's turn.
        state.active_player = controlled;
        state.turn_decision_controller = Some(controller);

        for _ in 0..controller_lands {
            land_on_battlefield(&mut state, controller);
        }
        for _ in 0..controlled_lands {
            land_on_battlefield(&mut state, controlled);
        }

        let swamp = land_in_hand(&mut state, controlled);
        let fatty = fatty_in_hand(&mut state, controlled);
        state.waiting_for = discard_waiting_for(&state, controlled, 1);

        assert_eq!(
            engine::game::turn_control::authorized_submitter_for_player(&state, controlled),
            controller,
            "fixture premise: the controller must be the authorized submitter, \
             or the arm never reaches the CR 723.5 branch"
        );
        (state, swamp, fatty)
    }

    /// F4 — the authority key AND the CR 723.5 direction, over the whole design
    /// space, at a reachable turn-control state.
    ///
    /// `ai_player` is `PlayerId(0)` and controls `PlayerId(1)`, who is
    /// discarding. Seat 0 runs a plain deck (land target 6) with 10 lands; seat
    /// 1 runs a ramp deck (land target 7) with 6 lands. Those are the only two
    /// reachable land targets, and the divergence makes all four key
    /// combinations compute different tiers for the controlled player's Swamp:
    ///
    /// | plan key | lands key | lands_behind | Swamp tier | selected |
    /// |---|---|---|---|---|
    /// | waiting_for.player | waiting_for.player | 7-6 = +1 | NeededManaSource | **the Swamp** (correct) |
    /// | ai_player | ai_player | 6-10 = -4 | Surplus | the fatty |
    /// | waiting_for.player | ai_player | 7-10 = -3 | Surplus | the fatty |
    /// | ai_player | waiting_for.player | 6-6 = 0 | Ordinary | the fatty |
    ///
    /// Under CR 723.5 the AI decides *against* the player it controls, so the
    /// comparator is reversed and the top tier is surrendered first — which is
    /// why row 1 pitches the mana source seat 1 still needs. That also makes
    /// this the discriminating test for the direction itself: the protective
    /// (unreversed) comparator selects the fatty in row 1 too.
    ///
    /// DO NOT "simplify" the two decks to a common plan — rows 1 and 4 would
    /// then compute the same tier and a wrong plan key would pass.
    #[test]
    fn discard_to_hand_size_keys_plan_and_lands_on_the_discarding_player() {
        let ai = PlayerId(0);
        let discarder = PlayerId(1);
        let (state, swamp, fatty) = mindslaver_discard_state(ai, discarder, 10, 6);

        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let ctx = context_with_plans(
            &state,
            ai,
            &config,
            &[(ai, default_deck_plan()), (discarder, ramp_deck_plan())],
        );

        let chosen = selected_card(deterministic_choice(&state, ai, &config, &[], Some(&ctx)));

        assert_eq!(
            chosen, swamp,
            "the tier must read the DISCARDING player's schedule against the \
             DISCARDING player's board, and CR 723.5 must surrender the mana \
             source that player still needs; every other key combination, and \
             the unreversed comparator, select the fatty"
        );
        assert_ne!(chosen, fatty);
    }

    /// F4b — the CR 723.5 reversal is gated on turn control, not on a bare
    /// seat comparison. Same board, same hand, same plans; the only change is
    /// that seat 1 is deciding for itself (the shape the rollout quiescence
    /// loop produces, which passes the acting player as the optimizing seat).
    /// The protective order returns, so the needed Swamp is kept and the fatty
    /// goes.
    #[test]
    fn discard_to_hand_size_protects_a_self_deciding_seat() {
        let ai = PlayerId(0);
        let discarder = PlayerId(1);
        let (mut state, swamp, fatty) = mindslaver_discard_state(ai, discarder, 10, 6);
        // Drop the control latch: seat 1 decides for itself again.
        state.turn_decision_controller = None;
        assert_eq!(
            engine::game::turn_control::authorized_submitter_for_player(&state, discarder),
            discarder,
            "reach-guard: without the latch the discarder is its own submitter"
        );

        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let ctx = context_with_plans(
            &state,
            discarder,
            &config,
            &[(ai, default_deck_plan()), (discarder, ramp_deck_plan())],
        );

        let chosen = selected_card(deterministic_choice(
            &state,
            discarder,
            &config,
            &[],
            Some(&ctx),
        ));

        assert_eq!(
            chosen, fatty,
            "deciding for itself, seat 1 keeps the mana source it is behind on"
        );
        assert_ne!(chosen, swamp);
    }

    /// F4c — the GATE SHAPE, isolated. F4b varies two inputs at once (it drops
    /// the latch *and* moves `ai_player` from `0` to the discarder), so under a
    /// bare `*player != ai_player` gate F4b would take the protective branch
    /// too: it discriminates the CR 723.5 *reversal*, not the gate's *shape*.
    /// Here only the latch is removed — `ai_player` stays `PlayerId(0)` while
    /// `waiting_for.player` stays `PlayerId(1)` — so the two gates disagree and
    /// the assertion pins the authority gate specifically.
    ///
    /// **THIS IS NOT PRODUCTION-PATH COVERAGE, and must not be counted as
    /// such.** The state it asserts at is unreachable as a *game* state: with no
    /// turn control, the engine prompts only seat 1 for seat 1's cleanup
    /// discard, and the rollout quiescence loop passes the acting player as the
    /// optimizing seat, so no production caller can present
    /// `ai_player = 0, waiting_for.player = 1, no latch`. It is legitimate only
    /// as a **caller-contract** test: it fixes what this arm does if some future
    /// caller ever hands it that pair, and it is the only fixture that
    /// distinguishes the two candidate gates. A successor reading this must not
    /// promote it to evidence that production reaches this branch.
    #[test]
    fn discard_to_hand_size_gate_is_the_submitter_authority_not_a_seat_compare() {
        let ai = PlayerId(0);
        let discarder = PlayerId(1);
        let (mut state, swamp, fatty) = mindslaver_discard_state(ai, discarder, 10, 6);
        // Drop ONLY the latch. `ai_player` below is still seat 0.
        state.turn_decision_controller = None;
        assert_eq!(
            engine::game::turn_control::authorized_submitter_for_player(&state, discarder),
            discarder,
            "reach-guard: without the latch seat 1 is its own submitter, so the \
             authority gate is false while `*player != ai_player` is TRUE — \
             this is exactly where the two candidate gates disagree"
        );

        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let ctx = context_with_plans(
            &state,
            ai,
            &config,
            &[(ai, default_deck_plan()), (discarder, ramp_deck_plan())],
        );

        let chosen = selected_card(deterministic_choice(&state, ai, &config, &[], Some(&ctx)));

        assert_eq!(
            chosen, fatty,
            "with no control latch the arm must serve the discarder even though \
             the seats differ; a bare `*player != ai_player` gate would reverse \
             here and pitch the Swamp seat 1 still needs"
        );
        assert_ne!(chosen, swamp);
    }

    /// F5 — the accelerant axis, isolated from the land axis. A `{G}` 1/1 mana
    /// dork and a `{G}` 1/1 vanilla both score exactly 3.0, so `main`'s pick is
    /// decided by the stable sort retaining insertion order.
    ///
    /// CONSTRUCTION REQUIREMENT: the dork is inserted FIRST, so `main` selects
    /// the dork and this test fails on base. Inserting the vanilla first would
    /// make it pass on `main` by accident.
    #[test]
    fn discard_to_hand_size_prefers_a_vanilla_sibling_over_a_mana_dork() {
        let mut state = commander_discard_state();
        let ai = PlayerId(0);
        land_on_battlefield(&mut state, ai);

        // Dork first — see the construction requirement above.
        let dork = named_vanilla_in_hand(&mut state, ai, "Dork");
        {
            let obj = state.objects.get_mut(&dork).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(1);
            obj.toughness = Some(1);
        }
        set_cost(&mut state, dork, vec![ManaCostShard::Green], 0);
        push_mana_ability(&mut state, dork);

        let vanilla = named_vanilla_in_hand(&mut state, ai, "Bear Cub");
        {
            let obj = state.objects.get_mut(&vanilla).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(1);
            obj.toughness = Some(1);
        }
        set_cost(&mut state, vanilla, vec![ManaCostShard::Green], 0);

        fatty_in_hand(&mut state, ai);
        state.waiting_for = discard_waiting_for(&state, ai, 1);

        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        // Derived land target 6 against 1 land on board => behind on lands.
        let ctx = context_with_plans(&state, ai, &config, &[(ai, default_deck_plan())]);

        assert_eq!(
            crate::card_value::intrinsic_value(&state, dork),
            crate::card_value::intrinsic_value(&state, vanilla),
            "fixture premise: the two 1/1s must score identically, so only the \
             tier can separate them"
        );

        let chosen = selected_card(deterministic_choice(&state, ai, &config, &[], Some(&ctx)));

        assert_eq!(
            chosen, vanilla,
            "the mana dork is an Accelerant and outranks its statistically \
             identical vanilla sibling while behind on lands"
        );
        assert_ne!(chosen, dork);
    }

    /// An MV-`mv` noncreature artifact on `owner`'s battlefield.
    /// `sacrifice_cost` prices it at `min(mv, NONCREATURE_SACRIFICE_CAP)`.
    fn artifact_on_battlefield(state: &mut GameState, owner: PlayerId, mv: u32) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            "Gilded Lotus".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);
        set_cost(state, id, Vec::new(), mv);
        id
    }

    /// Park a mandatory 1-of-N `EffectZoneChoice { Sacrifice }` over `cards`.
    ///
    /// The order of `cards` is load-bearing: `pick_lowest_value_sacrifices`
    /// sorts *stably*, so at equal scores the first entry is the one given up.
    /// Every tie-boundary fixture below therefore lists the permanent it must
    /// NOT lose first.
    fn park_forced_sacrifice(state: &mut GameState, cards: Vec<ObjectId>) {
        let ai = PlayerId(0);
        let source_card = CardId(state.next_object_id);
        let source = create_object(
            state,
            source_card,
            ai,
            "Edict Source".to_string(),
            Zone::Battlefield,
        );
        state.waiting_for = WaitingFor::EffectZoneChoice {
            player: ai,
            cards,
            count: 1,
            min_count: 1,
            up_to: false,
            source_id: source,
            effect_kind: EffectKind::Sacrifice,
            zone: Zone::Battlefield,
            destination: None,
            enter_tapped: engine::types::zones::EtbTapState::Unspecified,
            enter_transformed: false,
            enters_under_player: None,
            enters_attacking: false,
            owner_library: false,
            track_exiled_by_source: false,
            face_down_profile: None,
            enter_with_counters: Vec::new(),
            conditional_enter_with_counters: Vec::new(),
            count_param: 0,
            library_position: None,
            is_cost_payment: false,
            enters_modified_if: None,
            duration: None,
        };
    }

    /// Build a mandatory `EffectZoneChoice { Sacrifice }` over an artifact land
    /// and a 1/1 MV-2 creature. Returns `(state, land, creature)`.
    fn forced_sacrifice_state() -> (GameState, ObjectId, ObjectId) {
        let mut state = commander_discard_state();
        let ai = PlayerId(0);

        let land_card = CardId(state.next_object_id);
        let land = create_object(
            &mut state,
            land_card,
            ai,
            "Artifact Land".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&land).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.mana_cost = engine::types::mana::ManaCost::NoCost;
        }

        let creature = add_creature(&mut state, ai, 1, 1);
        set_cost(&mut state, creature, Vec::new(), 2);

        park_forced_sacrifice(&mut state, vec![land, creature]);
        (state, land, creature)
    }

    /// F11 — the tie boundary. `sacrifice_land_penalty` must be strictly above
    /// `NONCREATURE_SACRIFICE_CAP`, or a land merely TIES every permanent of
    /// mana value 4 or more and the stable sort gives up whichever is listed
    /// first — which, for a `[Swamp, Gilded Lotus]` battlefield, is the Swamp.
    ///
    /// FAILS ON BASE (and on the first round of this unit): at 4.0 vs 4.0 the
    /// land is selected.
    #[test]
    fn deterministic_sacrifice_prefers_an_expensive_artifact_over_a_land() {
        let mut state = commander_discard_state();
        let ai = PlayerId(0);
        // Land FIRST — see `park_forced_sacrifice`'s ordering note.
        let land = land_on_battlefield(&mut state, ai);
        let lotus = artifact_on_battlefield(&mut state, ai, 5);
        park_forced_sacrifice(&mut state, vec![land, lotus]);

        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let action = deterministic_choice(&state, ai, &config, &[], None);
        assert_eq!(
            action,
            Some(GameAction::SelectCards { cards: vec![lotus] }),
            "an MV-5 artifact caps at {} and must be given up before a land \
             worth {} ({land:?})",
            crate::policies::strategy_helpers::NONCREATURE_SACRIFICE_CAP,
            config.policy_penalties.sacrifice_land_penalty
        );
    }

    /// F11b — the ordering survives a TRAINED scalar that inverts the cap.
    ///
    /// `sacrifice_land_penalty` is in `config::ACTIVE_POLICY_PENALTY_FIELDS`,
    /// so CMA-ES can legitimately train it below `NONCREATURE_SACRIFICE_CAP`
    /// and the 4.5-vs-4.0 default gap says nothing about what a trained profile
    /// ships. Here the penalty is driven to **1.0** — far under the cap, so the
    /// bare scalar ranks the land cheapest and would sacrifice it — and the
    /// land must still be given up last, because
    /// `strategy_helpers::SacrificeTier` carries that axis structurally.
    ///
    /// FAILS ON A SCALAR-ONLY ORDERING, including this unit's own round-2 form:
    /// ranking on `sacrifice_cost` alone selects the land at 1.0 over the
    /// artifact at 4.0. Note the deliberate choice NOT to make an out-of-bounds
    /// trained config a hard error at load: a bad-but-legal trained profile
    /// should cost strength, not crash the AI.
    #[test]
    fn sacrifice_ordering_survives_a_trained_land_penalty_under_the_cap() {
        let mut state = commander_discard_state();
        let ai = PlayerId(0);
        // Land FIRST, so a stable sort on tied scores would also betray it.
        let land = land_on_battlefield(&mut state, ai);
        let lotus = artifact_on_battlefield(&mut state, ai, 5);
        park_forced_sacrifice(&mut state, vec![land, lotus]);

        let mut config = create_config(AiDifficulty::VeryHard, Platform::Native);
        config.policy_penalties.sacrifice_land_penalty = 1.0;
        assert!(
            config.policy_penalties.sacrifice_land_penalty
                < crate::policies::strategy_helpers::NONCREATURE_SACRIFICE_CAP,
            "fixture premise: the trained penalty must be UNDER the cap, or the \
             scalar and the tier agree and this test cannot discriminate"
        );

        let action = deterministic_choice(&state, ai, &config, &[], None);
        assert_eq!(
            action,
            Some(GameAction::SelectCards { cards: vec![lotus] }),
            "the land ({land:?}) must be surrendered last on the tier even when \
             the trained scalar prices it at 1.0 against the artifact's 4.0"
        );
    }

    /// The tier is the ordering authority; this pins the *scalar* invariant the
    /// shipped defaults still hold, so a config edit fails here with a
    /// diagnosis rather than as a within-tier surprise.
    ///
    /// NOTE: `sacrifice_land_penalty` is a CMA-ES-tuned field
    /// (`ACTIVE_POLICY_PENALTY_FIELDS`), so a *trained* config can still land
    /// below the cap. That no longer inverts the land-vs-nonland order — see
    /// `sacrifice_ordering_survives_a_trained_land_penalty_under_the_cap` — it
    /// only changes weights *within* a tier. Deliberately NOT enforced at
    /// config load: turning a bad-but-legal trained config into a hard error is
    /// the wrong trade.
    #[test]
    fn land_penalty_strictly_exceeds_the_noncreature_cap() {
        let cap = crate::policies::strategy_helpers::NONCREATURE_SACRIFICE_CAP;
        assert!(
            crate::config::PolicyPenalties::default().sacrifice_land_penalty > cap,
            "a land that only ties an MV-4+ permanent is sacrificed by list order"
        );
        for difficulty in [
            AiDifficulty::VeryEasy,
            AiDifficulty::Easy,
            AiDifficulty::Medium,
            AiDifficulty::Hard,
            AiDifficulty::VeryHard,
            AiDifficulty::CEDH,
        ] {
            let config = create_config(difficulty, Platform::Native);
            assert!(
                config.policy_penalties.sacrifice_land_penalty > cap,
                "{difficulty:?} config ties the land penalty with the cap"
            );
        }
    }

    /// F12 — the non-land ordering flip this unit introduced, measured rather
    /// than assumed. Routing `pick_lowest_value_sacrifices` through
    /// `sacrifice_cost` replaced the old card scalar (creature `p*1.5 + t +
    /// mv*0.5` = 3.5, artifact `mv*0.5` = 2.0, so the ARTIFACT was given up)
    /// with the battlefield authority (`evaluate_creature` = 2.5, artifact
    /// capped at 4.0, so the CREATURE is given up). The new ordering is the
    /// intended one — it matches `SacrificeValuePolicy` — and this test exists
    /// so the flip cannot regress silently in either direction.
    #[test]
    fn deterministic_sacrifice_gives_up_a_small_creature_before_a_costly_artifact() {
        let mut state = commander_discard_state();
        let ai = PlayerId(0);
        let creature = add_creature(&mut state, ai, 1, 1);
        set_cost(&mut state, creature, Vec::new(), 2);
        let artifact = artifact_on_battlefield(&mut state, ai, 4);
        park_forced_sacrifice(&mut state, vec![creature, artifact]);

        let cap = crate::policies::strategy_helpers::NONCREATURE_SACRIFICE_CAP;
        assert!(
            crate::eval::evaluate_creature(&state, creature) < cap,
            "fixture premise broken: `eval::evaluate_creature` now prices a 1/1 \
             at or above the noncreature cap, so this fixture no longer \
             discriminates"
        );

        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        assert_eq!(
            deterministic_choice(&state, ai, &config, &[], None),
            Some(GameAction::SelectCards {
                cards: vec![creature]
            }),
            "the 1/1 is the cheapest permanent under the battlefield authority; \
             the pre-unit card scalar gave up the artifact ({artifact:?})"
        );
    }

    /// The mandatory-sacrifice entry point must use the commander-aware key,
    /// rather than relying on the stable input order that used to break the
    /// equal-priced pair. Both input orders are intentionally exercised.
    #[test]
    fn pick_lowest_value_sacrifices_spares_an_owned_commander_in_both_input_orders() {
        let mut state = commander_discard_state();
        let ai = PlayerId(0);
        let commander = add_creature(&mut state, ai, 4, 4);
        let bear = add_creature(&mut state, ai, 4, 4);
        {
            let obj = state.objects.get_mut(&commander).unwrap();
            obj.is_commander = true;
            obj.mana_cost = engine::types::mana::ManaCost::generic(4);
            obj.base_mana_cost = engine::types::mana::ManaCost::generic(4);
        }
        state.commander_cast_count.insert(commander, 1);
        let penalties = crate::config::PolicyPenalties::default();

        assert_eq!(
            sacrifice_key(&state, bear, &penalties).1,
            10.0,
            "reach guard: the ordinary 4/4 must retain its board price"
        );
        assert_eq!(
            sacrifice_key(&state, commander, &penalties).1,
            16.0,
            "reach guard: the owned commander must carry its 6.0 repurchase premium"
        );
        assert_eq!(
            pick_lowest_value_sacrifices(&state, &[bear, commander], 1, &penalties),
            vec![bear],
            "the bear is selected when it is already first"
        );
        assert_eq!(
            pick_lowest_value_sacrifices(&state, &[commander, bear], 1, &penalties),
            vec![bear],
            "the bear is still selected when the commander arrives first"
        );
    }

    /// F8 — CR 701.21a: `pick_lowest_value_sacrifices` now routes through
    /// `strategy_helpers::sacrifice_cost`, the same battlefield authority
    /// `SacrificeValuePolicy` uses, instead of the land-blind card scalar.
    ///
    /// FAILS ON BASE: under `evaluate_card_value` the artifact land scores 3.0
    /// and the 1/1 MV-2 creature 3.5, so `main` sacrifices the land — the
    /// reported bug in miniature.
    #[test]
    fn deterministic_sacrifice_prefers_creature_over_land() {
        let (state, land, creature) = forced_sacrifice_state();
        let config = create_config(AiDifficulty::VeryHard, Platform::Native);

        // Anti-vacuity guard: `evaluate_creature` lives in `eval.rs`. If it ever
        // exceeds the land penalty, this fixture stops discriminating — fail
        // loudly with a diagnosis instead of passing for the wrong reason.
        assert!(
            crate::eval::evaluate_creature(&state, creature)
                < config.policy_penalties.sacrifice_land_penalty,
            "fixture premise broken: creature valuation now exceeds the land penalty, \
             so this test no longer discriminates"
        );

        let action = deterministic_choice(&state, PlayerId(0), &config, &[], None);
        assert_eq!(
            action,
            Some(GameAction::SelectCards {
                cards: vec![creature]
            }),
            "the forced sacrifice must give up the creature, not the land ({land:?})"
        );
    }

    /// F8, fallback leg. `fallback_action` reaches the same
    /// `pick_lowest_value_sacrifices` authority and must not be land-blind
    /// there either — that is why `config` is threaded through the signature.
    /// Substituting `PolicyPenalties::default()` at this seam would silently
    /// diverge from a configured penalty and reintroduce the bypass.
    #[test]
    fn fallback_sacrifice_prefers_creature_over_land() {
        let (state, _land, creature) = forced_sacrifice_state();
        let config = create_config(AiDifficulty::VeryHard, Platform::Native);

        assert_eq!(
            fallback_action(&state, &config),
            Some(GameAction::SelectCards {
                cards: vec![creature]
            }),
            "the fallback sacrifice escape must use the land-aware authority"
        );
    }

    /// F9 — the `DigChoice` `up_to` path tests the raw scalar against a literal
    /// `0.1`. That is the only numeric coupling across the twelve former
    /// `evaluate_card_value` sites, so it guards the relocation: any change to
    /// `intrinsic_value`'s arithmetic breaks it.
    #[test]
    fn dig_choice_up_to_still_takes_nothing_below_the_scalar_threshold() {
        let mut state = commander_discard_state();
        let ai = PlayerId(0);
        // Vanilla cards: no creature type, no land, zero mana cost => 0.0 < 0.1.
        let pool: Vec<_> = (0..3).map(|_| vanilla_in_hand(&mut state, ai)).collect();
        for &id in &pool {
            assert_eq!(
                crate::card_value::intrinsic_value(&state, id),
                0.0,
                "fixture premise: the pool must score below the 0.1 threshold"
            );
        }
        state.waiting_for = WaitingFor::DigChoice {
            player: ai,
            library_owner: ai,
            cards: pool.clone(),
            keep_count: 1,
            up_to: true,
            selectable_cards: pool,
            kept_destination: None,
            rest_destination: None,
            source_id: None,
            enter_tapped: false,
        };

        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        match deterministic_choice(&state, ai, &config, &[], None) {
            Some(GameAction::SelectCards { cards }) => assert!(
                cards.is_empty(),
                "up_to Dig over a worthless pool must take nothing, got {cards:?}"
            ),
            other => panic!("expected SelectCards from the DigChoice arm, got {other:?}"),
        }
    }
}
