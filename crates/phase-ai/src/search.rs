use rand::Rng;

use engine::ai_support::{build_decision_context, CandidateAction};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::player::PlayerId;

use crate::combat_ai::{choose_attackers_with_targets_with_profile, choose_blockers_with_profile};
use crate::config::AiConfig;
use crate::planner::{
    apply_candidate, build_continuation_planner, rank_candidates, PlannerServices, SearchBudget,
};
use crate::policies::PolicyRegistry;

struct ScoredCandidate {
    candidate: CandidateAction,
    score: f64,
}

/// Filter candidate actions by testing each against the engine.
/// Any candidate the engine rejects is dropped before scoring.
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
    let ctx = build_decision_context(state);
    let policies = PolicyRegistry::default();
    let mut services = PlannerServices::new(ai_player, config, &policies);
    let candidates = services.validate_candidates(state, ctx.candidates.clone());
    let actions: Vec<GameAction> = candidates
        .iter()
        .map(|candidate| candidate.action.clone())
        .collect();

    if actions.is_empty() {
        return None;
    }

    if matches!(
        state.waiting_for,
        WaitingFor::BetweenGamesChoosePlayDraw { .. }
    ) {
        return Some(GameAction::ChoosePlayDraw { play_first: true });
    }

    if matches!(state.waiting_for, WaitingFor::BetweenGamesSideboard { .. }) {
        return actions
            .into_iter()
            .find(|action| matches!(action, GameAction::SubmitSideboard { .. }));
    }

    if actions.len() == 1 {
        return Some(actions.into_iter().next().unwrap());
    }

    if let Some(action) = prefer_land_drop(state, ai_player, &actions) {
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
        // Put higher-value cards on top, lower-value on bottom
        let mut scored: Vec<_> = cards
            .iter()
            .map(|&id| (id, evaluate_card_value(state, id)))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let top_cards: Vec<_> = scored.iter().map(|(id, _)| *id).collect();
        return Some(GameAction::SelectCards { cards: top_cards });
    }

    if let WaitingFor::DigChoice {
        cards, keep_count, ..
    } = &state.waiting_for
    {
        // Keep the highest-value cards
        let mut scored: Vec<_> = cards
            .iter()
            .map(|&id| (id, evaluate_card_value(state, id)))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let kept: Vec<_> = scored.iter().take(*keep_count).map(|(id, _)| *id).collect();
        return Some(GameAction::SelectCards { cards: kept });
    }

    if let WaitingFor::SurveilChoice { cards, .. } = &state.waiting_for {
        // Send lowest-value cards to graveyard
        let mut scored: Vec<_> = cards
            .iter()
            .map(|&id| (id, evaluate_card_value(state, id)))
            .collect();
        scored.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        // Send bottom half to graveyard (heuristic: keep better cards on top)
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
        // Pick the highest-value card from opponent's hand to exile/discard
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
        // Pick the highest-value card(s) from search results
        let mut scored: Vec<_> = cards
            .iter()
            .map(|&id| (id, evaluate_card_value(state, id)))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let chosen: Vec<_> = scored.iter().take(*count).map(|(id, _)| *id).collect();
        if !chosen.is_empty() {
            return Some(GameAction::SelectCards { cards: chosen });
        }
    }

    if let WaitingFor::OptionalCostChoice { .. } = &state.waiting_for {
        // AI always pays optional costs when it can (simple heuristic for now)
        return Some(GameAction::DecideOptionalCost { pay: true });
    }

    if let WaitingFor::OptionalEffectChoice { .. } = &state.waiting_for {
        // AI always accepts optional effects (simple heuristic for now)
        return Some(GameAction::DecideOptionalEffect { accept: true });
    }

    if let WaitingFor::DiscardToHandSize { cards, count, .. } = &state.waiting_for {
        // Discard the lowest-value cards
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

    // Score actions
    let scored: Vec<ScoredCandidate> = if config.search.enabled {
        let mut budget = SearchBudget::new(config.search.max_nodes);
        let branching = config.search.max_branching as usize;
        let mut planner = build_continuation_planner(config);

        rank_candidates(
            candidates,
            |candidate| services.tactical_score(state, &ctx, candidate, ai_player),
            branching,
        )
        .into_iter()
        .map(|ranked| {
            let score = if let Some(sim) = apply_candidate(state, &ranked.candidate) {
                let continuation_score =
                    planner.evaluate_after_action(&sim, &mut services, &mut budget);
                continuation_score + (ranked.score * 0.1)
            } else {
                ranked.score
            };
            ScoredCandidate {
                candidate: ranked.candidate,
                score,
            }
        })
        .collect()
    } else {
        // Heuristic-only scoring
        candidates
            .into_iter()
            .map(|candidate| {
                let score = services.tactical_score(state, &ctx, &candidate, ai_player);
                ScoredCandidate { candidate, score }
            })
            .collect()
    };

    softmax_select(&scored, config.temperature, rng)
}

fn prefer_land_drop(
    state: &GameState,
    ai_player: PlayerId,
    actions: &[GameAction],
) -> Option<GameAction> {
    let WaitingFor::Priority { player } = &state.waiting_for else {
        return None;
    };

    if *player != ai_player
        || state.active_player != ai_player
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

/// Decide whether to keep the current hand based on land/spell ratio.
/// Always keeps at 4 or fewer cards. For larger hands, keeps if land count
/// is in the acceptable range (roughly 2-5 for 7 cards, scaled down).
fn should_keep_hand(state: &GameState, player: PlayerId, _mulligan_count: u8) -> bool {
    let hand_size = state.players[player.0 as usize].hand.len();

    // Always keep at 4 or fewer cards
    if hand_size <= 4 {
        return true;
    }

    let land_count = state.players[player.0 as usize]
        .hand
        .iter()
        .filter(|&&oid| {
            state
                .objects
                .get(&oid)
                .map(|o| o.card_types.core_types.contains(&CoreType::Land))
                .unwrap_or(false)
        })
        .count();

    let spell_count = hand_size - land_count;

    // Keep if we have 2-5 lands (for 7 cards) or at least 1 land + 1 spell (smaller hands)
    if hand_size >= 6 {
        (2..=5).contains(&land_count)
    } else {
        // 5 card hand: keep with 1-4 lands; already kept <=4 above
        land_count >= 1 && spell_count >= 1
    }
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

fn softmax_select(
    scored: &[ScoredCandidate],
    temperature: f64,
    rng: &mut impl Rng,
) -> Option<GameAction> {
    if scored.is_empty() {
        return None;
    }
    if scored.len() == 1 {
        return Some(scored[0].candidate.action.clone());
    }

    // Numerical stability: subtract max score
    let max_score = scored
        .iter()
        .map(|s| s.score)
        .fold(f64::NEG_INFINITY, f64::max);

    let weights: Vec<f64> = scored
        .iter()
        .map(|s| ((s.score - max_score) / temperature).exp())
        .collect();

    let total: f64 = weights.iter().sum();
    if total <= 0.0 || !total.is_finite() {
        // Fallback: pick the highest-scored action
        return scored
            .iter()
            .max_by(|a, b| {
                a.score
                    .partial_cmp(&b.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|s| s.candidate.action.clone());
    }

    let threshold: f64 = rng.random::<f64>() * total;
    let mut cumulative = 0.0;
    for (i, w) in weights.iter().enumerate() {
        cumulative += w;
        if cumulative >= threshold {
            return Some(scored[i].candidate.action.clone());
        }
    }

    // Fallback to last
    Some(scored.last().unwrap().candidate.action.clone())
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
            ScoredCandidate {
                candidate: CandidateAction {
                    action: GameAction::PassPriority,
                    metadata: engine::ai_support::ActionMetadata {
                        actor: Some(PlayerId(0)),
                        tactical_class: TacticalClass::Pass,
                    },
                },
                score: 1.0,
            },
            ScoredCandidate {
                candidate: CandidateAction {
                    action: GameAction::PlayLand { card_id: CardId(1) },
                    metadata: engine::ai_support::ActionMetadata {
                        actor: Some(PlayerId(0)),
                        tactical_class: TacticalClass::Land,
                    },
                },
                score: 10.0,
            },
        ];
        let mut rng = SmallRng::seed_from_u64(42);
        // Very low temperature = nearly deterministic
        let mut picked_land = 0;
        for _ in 0..20 {
            if let Some(GameAction::PlayLand { .. }) = softmax_select(&scored, 0.01, &mut rng) {
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
            ScoredCandidate {
                candidate: CandidateAction {
                    action: GameAction::PassPriority,
                    metadata: engine::ai_support::ActionMetadata {
                        actor: Some(PlayerId(0)),
                        tactical_class: TacticalClass::Pass,
                    },
                },
                score: 1.0,
            },
            ScoredCandidate {
                candidate: CandidateAction {
                    action: GameAction::PlayLand { card_id: CardId(1) },
                    metadata: engine::ai_support::ActionMetadata {
                        actor: Some(PlayerId(0)),
                        tactical_class: TacticalClass::Land,
                    },
                },
                score: 2.0,
            },
        ];
        let mut rng = SmallRng::seed_from_u64(42);
        let mut picked_pass = 0;
        for _ in 0..100 {
            if let Some(GameAction::PassPriority) = softmax_select(&scored, 4.0, &mut rng) {
                picked_pass += 1;
            }
        }
        // At high temp with close scores, should pick the lower option sometimes
        assert!(
            picked_pass > 10 && picked_pass < 90,
            "High temperature should produce mixed results, got pass={picked_pass}/100"
        );
    }

    #[test]
    fn budget_limits_stop_search() {
        let mut budget = SearchBudget {
            max_nodes: 3,
            nodes_evaluated: 0,
        };
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

        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let mut rng = SmallRng::seed_from_u64(42);
        let action = choose_action(&state, PlayerId(0), &config, &mut rng);

        assert!(
            matches!(action, Some(GameAction::CastSpell { .. })),
            "AI should cast creature, not tap lands. Got: {:?}",
            action
        );
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
        });
        let opp_score = policies.score(&PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &opp_candidate,
            ai_player: PlayerId(0),
            config: &AiConfig::default(),
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
        };

        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let mut rng = SmallRng::seed_from_u64(10);
        let action = choose_action(&state, PlayerId(0), &config, &mut rng);

        assert_eq!(action, Some(GameAction::ChooseTarget { target: None }));
    }
}
