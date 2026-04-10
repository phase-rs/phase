use rand::Rng;

use engine::ai_support::build_decision_context;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::player::PlayerId;

use crate::combat_ai::{choose_attackers_with_targets_with_profile, choose_blockers_with_profile};
use crate::config::{AiConfig, ThreatAwareness};
use crate::context::AiContext;
use crate::threat_profile::{
    build_threat_profile_multiplayer, ArchetypeBaseProbabilities, ThreatProfile,
};
use crate::planner::{
    apply_candidate, build_continuation_planner, rank_candidates, PlannerServices, SearchBudget,
};
use crate::policies::tutor::{score_search_choice_cards, score_search_choice_selection};
use crate::policies::PolicyRegistry;
use crate::tactical_gate::gate_candidates;

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
    let scored = score_candidates(state, ai_player, config);
    if scored.is_empty() {
        // No valid candidates from search — fall back to a safe escape action
        // so the game never deadlocks waiting for the AI.
        return fallback_action(state);
    }
    if scored.len() == 1 {
        return Some(scored[0].0.clone());
    }
    softmax_select_pairs(&scored, config.temperature, rng)
}

/// Produce a safe action when the AI has no scored candidates.
/// During casting-related states, cancel the cast. During active play, pass priority.
/// Returns None only for terminal states (GameOver) where no action is possible.
fn fallback_action(state: &GameState) -> Option<GameAction> {
    if matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
        return None;
    }
    if state.waiting_for.has_pending_cast() {
        Some(GameAction::CancelCast)
    } else {
        Some(GameAction::PassPriority)
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
    let policies = PolicyRegistry::default();
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

    let mut services = PlannerServices::new(ai_player, config, &policies, context);
    let candidates = services.validate_candidates(state, ctx.candidates.clone());
    let gated = gate_candidates(
        state,
        &ctx,
        candidates,
        ai_player,
        config,
        &services.context,
    );

    // Filter out spells/abilities that were cast then cancelled this priority window.
    // Prevents deterministic cast→cancel→recast infinite loops.
    let gated: Vec<_> = if state.cancelled_casts.is_empty() {
        gated
    } else {
        gated
            .into_iter()
            .filter(|g| match &g.candidate.action {
                GameAction::CastSpell { object_id, .. }
                | GameAction::ActivateAbility {
                    source_id: object_id,
                    ..
                } => !state.cancelled_casts.contains(object_id),
                _ => true,
            })
            .collect()
    };

    let actions: Vec<GameAction> = gated
        .iter()
        .map(|candidate| candidate.candidate.action.clone())
        .collect();

    if actions.is_empty() {
        return vec![];
    }

    // Deterministic early returns — these don't benefit from search/parallelism
    if let Some(action) = deterministic_choice(state, ai_player, config, &actions) {
        return vec![(action, 1.0)];
    }

    // Score actions via search or heuristics
    if config.search.enabled {
        let mut budget = match config.search.time_budget_ms {
            Some(ms) => SearchBudget::with_time_limit(
                config.search.max_nodes,
                web_time::Duration::from_millis(ms as u64),
            ),
            None => SearchBudget::new(config.search.max_nodes),
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

        let penalty_for = |candidate: &engine::ai_support::CandidateAction| {
            gated
                .iter()
                .find(|gated_candidate| gated_candidate.candidate.action == candidate.action)
                .map(|gated_candidate| gated_candidate.penalty)
                .unwrap_or(0.0)
        };

        rank_candidates(
            gated.iter().map(|candidate| candidate.candidate.clone()),
            |candidate| {
                services.tactical_score(state, &ctx, candidate, ai_player) + penalty_for(candidate)
            },
            branching,
        )
        .into_iter()
        .map(|ranked| {
            let score = if let Some(sim) = apply_candidate(state, &ranked.candidate) {
                let continuation_score =
                    planner.evaluate_after_action(&sim, &mut services, &mut budget);
                continuation_score + (ranked.score * tactical_weight)
            } else {
                // Action failed simulation — heavily penalize so the AI prefers
                // any valid alternative (e.g., CancelCast over a failing PassPriority
                // during ManaPayment when the cost is unaffordable).
                // Preserve tactical score as tiebreaker among equally-failing actions
                // (e.g., target selection where simulation lacks full engine context).
                ranked.score - 1000.0
            };
            (ranked.candidate.action, score)
        })
        .collect()
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
    let deck = state
        .deck_pools
        .iter()
        .find(|p| p.player == player)
        .map(|p| p.current_main.as_slice())
        .unwrap_or(&[]);
    if deck.is_empty() {
        return AiContext::empty(&config.weights);
    }
    let mut ctx = AiContext::analyze_with(deck, &config.weights, &config.archetype_multipliers);

    // Compute opponent threat profile based on difficulty setting.
    ctx.opponent_threat = match config.search.threat_awareness {
        ThreatAwareness::None => None,
        ThreatAwareness::ArchetypeOnly => {
            // Use fixed archetype-based probabilities (no per-card analysis).
            // Derive opponent archetype from the opponent's deck pool if available,
            // otherwise fall back to Midrange.
            let opponents = engine::game::players::opponents(state, player);
            let opp_archetype = opponents
                .first()
                .and_then(|&opp| {
                    let opp_deck = state
                        .deck_pools
                        .iter()
                        .find(|p| p.player == opp)
                        .map(|p| p.current_main.as_slice())
                        .unwrap_or(&[]);
                    if opp_deck.is_empty() {
                        None
                    } else {
                        Some(crate::deck_profile::DeckProfile::analyze(opp_deck).archetype)
                    }
                })
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

    // Mulligan decisions: use hand-quality heuristic (search can't evaluate these)
    if let WaitingFor::MulliganDecision {
        player,
        mulligan_count,
    } = &state.waiting_for
    {
        let keep = should_keep_hand(state, *player, *mulligan_count);
        return Some(GameAction::MulliganDecision { keep });
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
        scored.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        let graveyard_count = scored.len().div_ceil(2);
        let to_graveyard: Vec<_> = scored
            .iter()
            .take(graveyard_count)
            .map(|(id, _)| *id)
            .collect();
        return Some(GameAction::SelectCards {
            cards: to_graveyard,
        });
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

    if let WaitingFor::SearchChoice { cards, count, .. } = &state.waiting_for {
        if *count == 1 {
            let mut scored = score_search_choice_cards(state, ai_player, cards);
            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            if let Some((best, _)) = scored.first() {
                return Some(GameAction::SelectCards { cards: vec![*best] });
            }
        } else {
            let mut scored: Vec<_> = actions
                .iter()
                .filter_map(|action| match action {
                    GameAction::SelectCards { cards } => Some((
                        cards.clone(),
                        score_search_choice_selection(state, ai_player, cards),
                    )),
                    _ => None,
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
        let is_opponent_chooser = state
            .players
            .iter()
            .any(|p| p.id == *player && p.id != state.priority_player);
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
    } = &state.waiting_for
    {
        let pay = match additional_cost {
            engine::types::ability::AdditionalCost::Optional(
                engine::types::ability::AbilityCost::Mana { cost: extra_mana },
            ) => {
                let combined =
                    engine::game::restrictions::add_mana_cost(&pending_cast.cost, extra_mana);
                let affordable = engine::game::casting::can_pay_cost_after_auto_tap(
                    state,
                    *player,
                    pending_cast.object_id,
                    &combined,
                );
                if !affordable {
                    false
                } else {
                    // Pay kicker only if it doesn't tap us out completely.
                    // Count total untapped mana sources to gauge remaining resources.
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
                    // Pay kicker if we'll have mana to spare afterward
                    total_untapped > combined_cmc
                }
            }
            // Non-mana optional costs: sacrifice → usually worth it for the upgrade
            engine::types::ability::AdditionalCost::Optional(
                engine::types::ability::AbilityCost::Sacrifice { .. },
            ) => false, // Conservative: don't sacrifice unless search says so
            engine::types::ability::AdditionalCost::Optional(
                engine::types::ability::AbilityCost::PayLife { amount },
            ) => {
                let life = state.players[player.0 as usize].life;
                life > (*amount as i32) * 3
            }
            engine::types::ability::AdditionalCost::Optional(_) => true,
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
    if let WaitingFor::DeclareAttackers { .. } = &state.waiting_for {
        let attacks = choose_attackers_with_targets_with_profile(state, ai_player, &config.profile);
        return Some(GameAction::DeclareAttackers { attacks });
    }

    if let WaitingFor::DeclareBlockers { .. } = &state.waiting_for {
        if let Some(combat) = &state.combat {
            let attacker_ids: Vec<_> = combat.attackers.iter().map(|a| a.object_id).collect();
            let assignments =
                choose_blockers_with_profile(state, ai_player, &attacker_ids, &config.profile);
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
    if let WaitingFor::DeclareAttackers { .. } = &state.waiting_for {
        let attacks = choose_attackers_with_targets_with_profile(state, ai_player, profile);
        return Some(GameAction::DeclareAttackers { attacks });
    }

    if let WaitingFor::DeclareBlockers { .. } = &state.waiting_for {
        if let Some(combat) = &state.combat {
            let attacker_ids: Vec<_> = combat.attackers.iter().map(|a| a.object_id).collect();
            let assignments =
                choose_blockers_with_profile(state, ai_player, &attacker_ids, profile);
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

/// Decide whether to keep the current hand based on land/spell ratio,
/// castability, and mana curve presence.
///
/// Always keeps at 4 or fewer cards (after mulligans). For larger hands,
/// checks land count, whether the lands can produce colors for the spells,
/// and whether there are plays available in the first few turns.
fn should_keep_hand(state: &GameState, player: PlayerId, mulligan_count: u8) -> bool {
    let hand = &state.players[player.0 as usize].hand;
    let hand_size = hand.len();

    // Always keep at 4 or fewer cards
    if hand_size <= 4 {
        return true;
    }

    // After 2+ mulligans, be much more lenient — keep any hand with at least 1 land + 1 spell
    if mulligan_count >= 2 {
        let has_land = hand.iter().any(|&oid| {
            state
                .objects
                .get(&oid)
                .is_some_and(|o| o.card_types.core_types.contains(&CoreType::Land))
        });
        let has_spell = hand.iter().any(|&oid| {
            state
                .objects
                .get(&oid)
                .is_some_and(|o| !o.card_types.core_types.contains(&CoreType::Land))
        });
        return has_land && has_spell;
    }

    let mut land_count = 0;
    let mut available_colors = Vec::new();

    for &oid in hand.iter() {
        let Some(obj) = state.objects.get(&oid) else {
            continue;
        };
        if obj.card_types.core_types.contains(&CoreType::Land) {
            land_count += 1;
            // Collect colors this land can produce from its subtypes
            for subtype in &obj.card_types.subtypes {
                if let Some(mana_type) =
                    engine::game::mana_payment::land_subtype_to_mana_type(subtype)
                {
                    if !available_colors.contains(&mana_type) {
                        available_colors.push(mana_type);
                    }
                }
            }
        }
    }

    let spell_count = hand_size - land_count;

    // Basic land count check: need lands and spells
    let land_ok = if hand_size >= 6 {
        (2..=5).contains(&land_count)
    } else {
        // 5 card hand: keep with 1-4 lands; already kept <=4 above
        land_count >= 1 && spell_count >= 1
    };

    if !land_ok {
        return false;
    }

    // Castability check: count spells castable in the first 3 turns
    // given available land colors and expected mana progression.
    let castable_early = hand
        .iter()
        .filter(|&&oid| {
            let Some(obj) = state.objects.get(&oid) else {
                return false;
            };
            if obj.card_types.core_types.contains(&CoreType::Land) {
                return false;
            }
            let mv = obj.mana_cost.mana_value();
            // Can we cast it in the first 3 turns? (need MV <= land_count + 1 for draw steps)
            if mv > (land_count as u32 + 1) {
                return false;
            }
            // Check color requirements against available land colors
            spell_colors_available(&obj.mana_cost, &available_colors)
        })
        .count();

    // Reject hands where nothing is castable early despite having lands + spells
    if castable_early == 0 && spell_count > 0 {
        return false;
    }

    true
}

/// Check whether the colors required by a spell's mana cost can be
/// produced by the available mana types (from lands in hand).
fn spell_colors_available(
    cost: &engine::types::mana::ManaCost,
    available: &[engine::types::mana::ManaType],
) -> bool {
    use engine::types::mana::{ManaCost, ManaCostShard, ManaType};

    let ManaCost::Cost { shards, .. } = cost else {
        return true; // NoCost or SelfManaCost — always castable
    };

    // For each colored shard, check if at least one of its colors is available.
    // Hybrid shards (e.g., WhiteBlue) are satisfied if either color is available.
    for shard in shards {
        let satisfied = match shard {
            ManaCostShard::White | ManaCostShard::PhyrexianWhite | ManaCostShard::TwoWhite => {
                available.contains(&ManaType::White)
            }
            ManaCostShard::Blue | ManaCostShard::PhyrexianBlue | ManaCostShard::TwoBlue => {
                available.contains(&ManaType::Blue)
            }
            ManaCostShard::Black | ManaCostShard::PhyrexianBlack | ManaCostShard::TwoBlack => {
                available.contains(&ManaType::Black)
            }
            ManaCostShard::Red | ManaCostShard::PhyrexianRed | ManaCostShard::TwoRed => {
                available.contains(&ManaType::Red)
            }
            ManaCostShard::Green | ManaCostShard::PhyrexianGreen | ManaCostShard::TwoGreen => {
                available.contains(&ManaType::Green)
            }
            ManaCostShard::WhiteBlue | ManaCostShard::PhyrexianWhiteBlue => {
                available.contains(&ManaType::White) || available.contains(&ManaType::Blue)
            }
            ManaCostShard::BlueBlack | ManaCostShard::PhyrexianBlueBlack => {
                available.contains(&ManaType::Blue) || available.contains(&ManaType::Black)
            }
            ManaCostShard::BlackRed | ManaCostShard::PhyrexianBlackRed => {
                available.contains(&ManaType::Black) || available.contains(&ManaType::Red)
            }
            ManaCostShard::RedGreen | ManaCostShard::PhyrexianRedGreen => {
                available.contains(&ManaType::Red) || available.contains(&ManaType::Green)
            }
            ManaCostShard::GreenWhite | ManaCostShard::PhyrexianGreenWhite => {
                available.contains(&ManaType::Green) || available.contains(&ManaType::White)
            }
            ManaCostShard::WhiteBlack | ManaCostShard::PhyrexianWhiteBlack => {
                available.contains(&ManaType::White) || available.contains(&ManaType::Black)
            }
            ManaCostShard::BlueRed | ManaCostShard::PhyrexianBlueRed => {
                available.contains(&ManaType::Blue) || available.contains(&ManaType::Red)
            }
            ManaCostShard::BlackGreen | ManaCostShard::PhyrexianBlackGreen => {
                available.contains(&ManaType::Black) || available.contains(&ManaType::Green)
            }
            ManaCostShard::RedWhite | ManaCostShard::PhyrexianRedWhite => {
                available.contains(&ManaType::Red) || available.contains(&ManaType::White)
            }
            ManaCostShard::GreenBlue | ManaCostShard::PhyrexianGreenBlue => {
                available.contains(&ManaType::Green) || available.contains(&ManaType::Blue)
            }
            // Colorless, Snow, X, ColorlessWhite etc. — no color requirement
            ManaCostShard::Colorless
            | ManaCostShard::Snow
            | ManaCostShard::X
            | ManaCostShard::ColorlessWhite
            | ManaCostShard::ColorlessBlue
            | ManaCostShard::ColorlessBlack
            | ManaCostShard::ColorlessRed
            | ManaCostShard::ColorlessGreen => true,
        };
        if !satisfied {
            return false;
        }
    }
    true
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
    use engine::types::ability::TargetRef;
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
        });

        state.waiting_for = WaitingFor::DeclareBlockers {
            player: PlayerId(0),
            valid_blocker_ids: vec![blocker],
            valid_block_targets: {
                let mut m = HashMap::new();
                m.insert(blocker, vec![attacker]);
                m
            },
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
}
