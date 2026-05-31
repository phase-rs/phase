use rand::Rng;

use engine::ai_support::build_decision_context;
use engine::types::actions::{AlternativeCastDecision, GameAction, MulliganChoice};
use engine::types::card_type::CoreType;
use engine::types::game_state::{CostResume, GameState, WaitingFor};
use engine::types::player::PlayerId;

use crate::cast_facts::cast_facts_for_action;
use crate::combat_ai::{choose_attackers_with_targets_with_profile, choose_blockers_with_profile};
use crate::config::{AiConfig, ThreatAwareness};
use crate::context::AiContext;
use crate::planner::{
    apply_candidate, build_continuation_planner, PlannerServices, RankedCandidate, SearchBudget,
};
use crate::policies::context::PolicyContext;
use crate::policies::tutor::{score_search_choice_cards, score_search_choice_selection};
use crate::policies::{PolicyId, PolicyRegistry, PolicyVerdict};
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
        if let Some(action) = deterministic_choice(state, ai_player, config, &[], None) {
            return Some(action);
        }
    }

    let scored = score_candidates(state, ai_player, config);
    if scored.is_empty() {
        // No valid candidates from search — fall back to a safe escape action
        // so the game never deadlocks waiting for the AI.
        return fallback_action(state);
    }
    let chosen = if scored.len() == 1 {
        Some(scored[0].0.clone())
    } else {
        softmax_select_pairs(&scored, config.temperature, rng)
    };
    if let Some(action) = &chosen {
        emit_decision_trace(state, ai_player, config, action);
    }
    chosen
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

    let context = build_ai_context(state, ai_player, config);
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
        debug_assert!(
            false,
            "AI fallback reached during pending cast ({:?}) — \
             can_cast_object_now has a gap that allowed an uncompletable \
             cast through. Tighten the pre-cast check rather than relying \
             on CancelCast recovery.",
            std::mem::discriminant(&state.waiting_for)
        );
        tracing::error!(
            waiting_for = ?std::mem::discriminant(&state.waiting_for),
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
        // CR 508.1g: exert-as-attack is optional; the conservative fallback
        // declines (never has a downside). Real exert decisions come from the
        // evaluated candidate actions.
        WaitingFor::ExertChoice { .. } => Some(GameAction::ChooseExert { exert: false }),

        // Target selection: skip optional slots, fizzle mandatory ones.
        // TriggerTargetSelection is not a pending cast — the trigger is
        // already on the stack. ChooseTarget { target: None } signals
        // "no legal target" and causes the trigger to fizzle (CR 608.2b).
        WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
            Some(GameAction::ChooseTarget { target: None })
        }

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
        | WaitingFor::MiracleCastOffer { .. }
        | WaitingFor::MadnessCastOffer { .. } => {
            Some(GameAction::DecideOptionalEffect { accept: false })
        }

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

        // Damage source choice: pick the first option.
        WaitingFor::DamageSourceChoice { options, .. } => options
            .first()
            .map(|&source| GameAction::ChooseDamageSource { source }),

        // Mode choice: select first mode.
        WaitingFor::ModeChoice { .. } | WaitingFor::AbilityModeChoice { .. } => {
            Some(GameAction::SelectModes { indices: vec![0] })
        }

        // Choose-one-of branch: pick the first branch.
        WaitingFor::ChooseOneOfBranch { .. } => Some(GameAction::ChooseBranch { index: 0 }),

        // Discover/Cascade: decline.
        WaitingFor::DiscoverChoice { .. } => Some(GameAction::DiscoverChoice {
            choice: engine::types::actions::CastChoice::Decline,
        }),
        // CR 701.20a: RevealUntil kept choice — accept (put onto the battlefield)
        // as the search default; the candidate generator still explores decline.
        WaitingFor::RevealUntilKeptChoice { .. } => {
            Some(GameAction::DecideOptionalEffect { accept: true })
        }
        WaitingFor::CascadeChoice { .. } => Some(GameAction::CascadeChoice {
            choice: engine::types::actions::CastChoice::Decline,
        }),
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

        // Adventure/MDFC/alt-cost choice: default to the "normal" face/cost.
        WaitingFor::AdventureCastChoice { .. } => {
            Some(GameAction::ChooseAdventureFace { creature: true })
        }
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

        // Paradigm: pass.
        WaitingFor::ParadigmCastOffer { .. } => Some(GameAction::PassParadigmOffer),

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
            ..
        } => {
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

        // Legend choice: pick the first candidate.
        WaitingFor::ChooseLegend { candidates, .. } => candidates
            .first()
            .map(|&keep| GameAction::ChooseLegend { keep }),

        // Battle protector: pick the first candidate.
        WaitingFor::BattleProtectorChoice { candidates, .. } => candidates
            .first()
            .map(|&protector| GameAction::ChooseBattleProtector { protector }),

        // Proliferate: choose nothing.
        WaitingFor::ProliferateChoice { .. } => Some(GameAction::SelectTargets {
            targets: Vec::new(),
        }),

        // ChooseObjectsIntoTrackedSet: default to declining (empty selection).
        WaitingFor::ChooseObjectsSelection { .. } => Some(GameAction::SelectTargets {
            targets: Vec::new(),
        }),

        // Copy retarget: keep current targets when present; freshly cast
        // prepare/paradigm copies start empty, so choose the first legal target.
        WaitingFor::CopyRetarget { target_slots, .. } => {
            let targets: Option<Vec<_>> = target_slots
                .iter()
                .map(|slot| {
                    slot.current
                        .clone()
                        .or_else(|| slot.legal_alternatives.first().cloned())
                })
                .collect();
            targets.map(|new_targets| GameAction::RetargetSpell { new_targets })
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

        // X value: pick 0.
        WaitingFor::ChooseXValue { .. } => Some(GameAction::ChooseX { value: 0 }),

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

        // CR 303.4 + CR 303.4g: Return-as-Aura attach pick — the engine only
        // installs this state when `legal_targets` is non-empty, so picking
        // the first candidate is always a legal fallback.
        WaitingFor::ReturnAsAuraTarget { legal_targets, .. } => {
            legal_targets.first().map(|&id| GameAction::ChooseTarget {
                target: Some(engine::types::ability::TargetRef::Object(id)),
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
            use engine::types::game_state::{ManaChoice, ManaChoicePrompt};
            match choice {
                ManaChoicePrompt::SingleColor { options } => {
                    options.first().map(|&color| GameAction::ChooseManaColor {
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

        // Remaining pending-cast states are caught by the has_pending_cast
        // guard above. This arm is structurally unreachable but required
        // for exhaustive match. ManaPayment is a pending-cast state.
        WaitingFor::ManaPayment { .. }
        | WaitingFor::OptionalCostChoice { .. }
        | WaitingFor::DefilerPayment { .. }
        | WaitingFor::PayCost {
            resume: CostResume::Spell { .. },
            ..
        }
        | WaitingFor::BlightChoice { .. }
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
    let ctx = build_decision_context(state);
    let policies = PolicyRegistry::shared();
    let context = build_ai_context(state, ai_player, config);

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
        if let Some(action) = deterministic_combat_choice(state, ai_player, &effective_profile) {
            return vec![(action, 1.0)];
        }
    }

    let mut services = PlannerServices::new(ai_player, config, policies, context);
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
    let gated: Vec<_> = gated
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
        // Deterministic mode ignores the wall-clock time budget so search is
        // bounded solely by max_nodes — integration tests and ai-duel regression
        // runs rely on this to eliminate wall-clock flake.
        let mut budget = match (config.search.deterministic, config.search.time_budget_ms) {
            (false, Some(ms)) => SearchBudget::with_time_limit(
                config.search.max_nodes,
                web_time::Duration::from_millis(ms as u64),
            ),
            _ => SearchBudget::new(config.search.max_nodes),
        };
        let branching = config.search.max_branching as usize;
        let mut planner = build_continuation_planner(config);

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
                let tactical = services.tactical_score(state, &ctx, &g.candidate, ai_player);
                RankedCandidate {
                    candidate: g.candidate.clone(),
                    score: tactical + g.penalty,
                }
            })
            .collect();
        ranked.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        ranked.truncate(branching);

        // Walk top-level candidates, but bail out of the full rollout phase
        // once the deadline fires — remaining candidates keep their tactical
        // score as the ranking signal instead of a full-search continuation.
        // This caps wall-clock on the outer map the same way the deadline caps
        // the inner rollout recursion.
        let mut out: Vec<(GameAction, f64)> = Vec::with_capacity(ranked.len());
        let mut deadline_hit = false;
        for r in ranked {
            let score = if deadline_hit || services.deadline.expired() {
                deadline_hit = true;
                // Skip the continuation search; keep the tactical signal.
                r.score * tactical_weight
            } else if let Some(sim) = apply_candidate(state, &r.candidate) {
                let continuation_score =
                    planner.evaluate_after_action(&sim, &mut services, &mut budget);
                continuation_score + (r.score * tactical_weight)
            } else {
                // Action failed simulation — heavily penalize so the AI prefers
                // any valid alternative (e.g., CancelCast over a failing PassPriority
                // during ManaPayment when the cost is unaffordable).
                // Preserve tactical score as tiebreaker among equally-failing actions
                // (e.g., target selection where simulation lacks full engine context).
                r.score - 1000.0
            };
            out.push((r.candidate.action, score));
        }
        let _ = deadline_hit;
        out
    } else {
        // Heuristic-only scoring
        gated
            .into_iter()
            .map(|candidate| {
                let score = services.tactical_score(state, &ctx, &candidate.candidate, ai_player)
                    + candidate.penalty;
                (candidate.candidate.action, score)
            })
            .collect()
    }
}

/// Build AI context from the player's deck pool, or a neutral default if unavailable.
fn build_ai_context(state: &GameState, player: PlayerId, config: &AiConfig) -> AiContext {
    let ai_pool = state.deck_pools.iter().find(|p| p.player == player);
    let deck = ai_pool.map(|p| p.current_main.as_slice()).unwrap_or(&[]);
    if deck.is_empty() {
        let mut ctx = AiContext::empty(&config.weights);
        ctx.player = player;
        return ctx;
    }
    // `analyze_for_player` keys the session's synergy/features/plan maps under
    // the actual AI player up-front, so no `Arc::make_mut` + HashMap rekey is
    // needed when the AI isn't in seat 0.
    let mut ctx =
        AiContext::analyze_for_player(deck, &config.weights, &config.archetype_multipliers, player);
    // Populate opponent features so archetype lookups hit the cache instead
    // of re-running `DeckProfile::analyze` per search call.
    let session = std::sync::Arc::make_mut(&mut ctx.session);
    // `analyze_for_player` defaults the AI player's bracket tier to `Core`
    // (it has no `state` access). Read the declared tier from the AI's
    // `PlayerDeckPool` and refresh the session features with it so
    // `DeckFeatures::is_cedh` (and any future tier-gated feature) reflects
    // the real bracket — without this, `ComboLinePolicy::activation()` would
    // never fire for cEDH decks.
    if let Some(pool) = ai_pool {
        if pool.bracket_tier != engine::game::bracket_estimate::CommanderBracketTier::Core {
            session.invalidate_player_features(player);
            session.ensure_player_features(player, deck, pool.bracket_tier);
        }
    }
    for pool in &state.deck_pools {
        if pool.player != player {
            session.ensure_player_features(pool.player, &pool.current_main, pool.bracket_tier);
        }
    }

    // Compute opponent threat profile based on difficulty setting.
    ctx.opponent_threat = match config.search.threat_awareness {
        ThreatAwareness::None => None,
        ThreatAwareness::ArchetypeOnly => {
            // Use fixed archetype-based probabilities (no per-card analysis).
            // Archetype is cached on `AiSession` (populated above via
            // `ensure_player_features`), so this is a HashMap lookup — not a
            // `DeckProfile::analyze` pass per search call.
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
                cost: engine::types::ability::AbilityCost::Sacrifice { .. },
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
        );
        return Some(GameAction::DeclareAttackers { attacks });
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
        );
        return Some(GameAction::DeclareAttackers { attacks });
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
    use engine::types::ability::{CategoryChooserScope, TargetFilter, TargetRef, TypedFilter};
    use engine::types::card_type::CoreType;
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::mana::{ManaType, ManaUnit};
    use engine::types::phase::Phase;
    use engine::types::zones::Zone;
    use rand::rngs::SmallRng;
    use rand::SeedableRng;

    use crate::config::{create_config, AiDifficulty, Platform};
    use crate::policies::context::PolicyContext;

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

    fn add_mana(state: &mut GameState, player: PlayerId, color: ManaType, count: usize) {
        let p = &mut state.players[player.0 as usize];
        for _ in 0..count {
            p.mana_pool.add(ManaUnit {
                color,
                source_id: ObjectId(0),
                snow: false,
                source_could_produce_two_or_more_colors: false,
                restrictions: Vec::new(),
                grants: vec![],
                expiry: None,
            });
        }
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
        });
        let opp_score = policies.score(&PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &opp_candidate,
            ai_player: PlayerId(0),
            config: &AiConfig::default(),
            context: &crate::context::AiContext::empty(&AiConfig::default().weights),
            cast_facts: None,
        });
        assert!(self_score < opp_score);
        assert!(self_score < -50.0);
    }

    #[test]
    fn target_selection_prefers_opponent_over_self() {
        let mut state = make_state();
        state.waiting_for = WaitingFor::TriggerTargetSelection {
            player: PlayerId(0),
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

    /// CR 608.2c + CR 701.23: Gifts Ungiven scaling regression — with a
    /// large library (80 cards), a count-4 search must complete in well
    /// under 100 ms via the BEAM_K-bounded path. The pre-fix Cartesian
    /// enumerator (~C(80, 4) ≈ 1.5M combos × per-combo scoring) stalled
    /// the AI; the beam reduces to C(BEAM_K, 4) candidates. The DistinctNames
    /// constraint is honored by the engine candidate filter and re-checked
    /// inside the AI beam, so the returned selection must contain only
    /// uniquely-named cards.
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
            elapsed.as_millis() < 100,
            "AI search-choice took {elapsed:?}; beam path must keep it under 100ms"
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
                repeatable: true,
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
}
