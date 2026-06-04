use engine::game::filter::{
    matches_target_filter, player_matches_target_filter_in_state, FilterContext,
};
use engine::game::players;
use engine::types::ability::{Effect, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use crate::cast_facts::CastFacts;
use crate::features::DeckFeatures;

use super::activation::turn_only;
use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use super::strategy_helpers::best_proactive_cast_score;

pub struct HandDisruptionPolicy;

#[derive(Debug, Clone, Copy)]
pub(crate) struct DisruptionWindow {
    pub tactical_score: f64,
    pub hint_priority: f64,
}

impl HandDisruptionPolicy {
    pub fn score(&self, ctx: &PolicyContext<'_>) -> f64 {
        match &ctx.candidate.action {
            GameAction::CastSpell { .. } => {
                let Some(facts) = ctx.cast_facts() else {
                    return 0.0;
                };
                let Some(window) = disruption_window_score(ctx.state, ctx.ai_player, &facts) else {
                    return 0.0;
                };

                let mut score = window.tactical_score;
                if best_proactive_cast_score(ctx) >= 0.4 {
                    score -= 0.18;
                }

                score
            }
            GameAction::ChooseTarget {
                target: Some(TargetRef::Player(player)),
            } => score_reveal_hand_player_target(ctx, *player),
            GameAction::SelectTargets { targets } => targets
                .iter()
                .filter_map(|target| match target {
                    TargetRef::Player(player) => {
                        Some(score_reveal_hand_player_target(ctx, *player))
                    }
                    _ => None,
                })
                .sum(),
            _ => 0.0,
        }
    }
}

impl TacticalPolicy for HandDisruptionPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::HandDisruption
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::CastSpell, DecisionKind::SelectTarget]
    }

    fn activation(
        &self,
        features: &DeckFeatures,
        state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        turn_only(features, state)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        PolicyVerdict::Score {
            delta: self.score(ctx),
            reason: PolicyReason::new("hand_disruption_score"),
        }
    }
}

fn score_reveal_hand_player_target(ctx: &PolicyContext<'_>, target_player: PlayerId) -> f64 {
    let effects = ctx.effects();
    if !effects.iter().any(|effect| {
        reveal_hand_matches_chosen_player_target(ctx.state, effect, target_player, ctx.ai_player)
    }) {
        return 0.0;
    }

    if !players::opponents(ctx.state, ctx.ai_player).contains(&target_player) {
        return -6.0;
    }

    let Some(player_state) = ctx
        .state
        .players
        .iter()
        .find(|player| player.id == target_player)
    else {
        return 0.0;
    };

    let unrevealed = player_state
        .hand
        .iter()
        .filter(|card| !ctx.state.revealed_cards.contains(card))
        .count() as f64;
    let revealed = player_state.hand.len() as f64 - unrevealed;

    1.25 + (unrevealed * 0.20) + (revealed * 0.02)
}

fn reveal_hand_matches_chosen_player_target(
    state: &GameState,
    effect: &Effect,
    target_player: PlayerId,
    source_controller: PlayerId,
) -> bool {
    let Effect::RevealHand { target, .. } = effect else {
        return false;
    };
    player_matches_target_filter_in_state(state, target, target_player, Some(source_controller))
}

pub(crate) fn disruption_window_score(
    state: &GameState,
    ai_player: PlayerId,
    facts: &CastFacts<'_>,
) -> Option<DisruptionWindow> {
    if !facts.has_reveal_hand_or_discard() {
        return None;
    }

    let effects = facts.immediate_effects();
    let has_discard = effects
        .iter()
        .any(|effect| matches!(effect, Effect::DiscardCard { .. }));
    let has_reveal = effects
        .iter()
        .any(|effect| matches!(effect, Effect::RevealHand { .. }));
    let card_filter = effects.iter().find_map(|effect| match effect {
        Effect::RevealHand { card_filter, .. } => Some(card_filter),
        _ => None,
    });
    let broad_filter = card_filter.is_none_or(|filter| matches!(filter, TargetFilter::Any));

    let opponents = players::opponents(state, ai_player);
    let max_hand_size = opponents
        .iter()
        .map(|player| state.players[player.0 as usize].hand.len())
        .max()
        .unwrap_or(0);

    let mut visible_legal_hits = 0;
    let mut visible_hit_value: f64 = 0.0;
    for opponent in &opponents {
        for object_id in &state.players[opponent.0 as usize].hand {
            if !state.revealed_cards.contains(object_id) {
                continue;
            }
            let Some(object) = state.objects.get(object_id) else {
                continue;
            };
            let legal_hit = card_filter.is_none_or(|filter| {
                matches_target_filter(
                    state,
                    *object_id,
                    filter,
                    &FilterContext::from_source(state, facts.object.id),
                )
            });
            if legal_hit {
                visible_legal_hits += 1;
                visible_hit_value = visible_hit_value.max(visible_hand_card_value(object));
            }
        }
    }

    let mut tactical_score: f64 = if has_discard {
        match max_hand_size {
            0 => -0.52,
            1 if broad_filter => 0.02,
            1 => -0.22,
            2 if broad_filter => 0.08,
            2 => -0.08,
            _ if broad_filter => 0.14,
            _ => -0.02,
        }
    } else {
        match max_hand_size {
            0 => -0.24,
            1 => 0.02,
            2 => 0.06,
            _ => 0.1,
        }
    };

    if visible_legal_hits > 0 {
        tactical_score += visible_hit_value.min(0.28) + 0.06;
    } else if !broad_filter {
        tactical_score -= if has_discard { 0.22 } else { 0.12 };
    }

    if has_reveal && !has_discard {
        tactical_score = tactical_score.min(0.12);
    }

    let mut hint_priority = if visible_legal_hits > 0 {
        (0.42 + visible_hit_value.min(0.24)).min(0.72)
    } else if has_discard && broad_filter {
        match max_hand_size {
            0 => 0.16,
            1 => 0.28,
            2 => 0.4,
            _ => 0.5,
        }
    } else if has_reveal {
        0.22
    } else {
        0.18
    };

    if !broad_filter && visible_legal_hits == 0 {
        hint_priority = hint_priority.min(0.24);
    }

    Some(DisruptionWindow {
        tactical_score,
        hint_priority,
    })
}

fn visible_hand_card_value(object: &engine::game::game_object::GameObject) -> f64 {
    let mana_value = object.mana_cost.mana_value() as f64;
    let type_bonus = if object.card_types.core_types.contains(&CoreType::Creature) {
        ((object.power.unwrap_or(0) + object.toughness.unwrap_or(0)).max(0) as f64 / 12.0).min(0.18)
    } else {
        0.08
    };
    (mana_value / 10.0).min(0.14) + type_bonus
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::config::AiConfig;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ControllerRef, Effect, ResolvedAbility, TargetFilter,
        TargetRef, TypeFilter, TypedFilter,
    };
    use engine::types::format::FormatConfig;
    use engine::types::game_state::{GameState, PendingCast, TargetSelectionSlot, WaitingFor};
    use engine::types::identifiers::CardId;
    use engine::types::mana::ManaCost;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;

    #[test]
    fn penalizes_discard_into_empty_hand() {
        let mut state = GameState::new_two_player(42);
        state.phase = engine::types::phase::Phase::PreCombatMain;
        state.active_player = PlayerId(0);

        let discard = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Duress".to_string(),
            Zone::Hand,
        );
        Arc::make_mut(&mut state.objects.get_mut(&discard).unwrap().abilities).push(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::RevealHand {
                    target: TargetFilter::Any,
                    card_filter: TargetFilter::Any,
                    count: None,
                    random: false,
                    choice_optional: false,
                },
            ),
        );

        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: discard,
                card_id: CardId(10),
                targets: Vec::new(),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Spell,
            },
        };
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: vec![candidate.clone()],
        };
        let config = AiConfig::default();
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };

        assert!(HandDisruptionPolicy.score(&ctx) < 0.0);
    }

    #[test]
    fn discounts_narrow_discard_with_only_illegal_visible_hits() {
        let mut state = GameState::new_two_player(42);
        state.phase = engine::types::phase::Phase::PreCombatMain;
        state.active_player = PlayerId(0);

        let duress = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Duress".to_string(),
            Zone::Hand,
        );
        Arc::make_mut(&mut state.objects.get_mut(&duress).unwrap().abilities).push(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::RevealHand {
                    target: TargetFilter::Any,
                    card_filter: TargetFilter::Typed(TypedFilter {
                        type_filters: vec![
                            TypeFilter::Non(Box::new(TypeFilter::Creature)),
                            TypeFilter::Non(Box::new(TypeFilter::Land)),
                        ],
                        controller: None,
                        properties: vec![],
                    }),
                    count: None,
                    random: false,
                    choice_optional: false,
                },
            )
            .sub_ability(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::DiscardCard {
                    count: 1,
                    target: TargetFilter::Any,
                },
            )),
        );

        let creature = create_object(
            &mut state,
            CardId(20),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state.revealed_cards.insert(creature);

        let facts = crate::cast_facts::cast_facts_for_object(state.objects.get(&duress).unwrap());
        let score = disruption_window_score(&state, PlayerId(0), &facts)
            .expect("disruption window")
            .tactical_score;
        assert!(score < 0.0);
    }

    #[test]
    fn reveal_hand_target_selection_prefers_opponent_over_self() {
        let mut state = GameState::new_two_player(42);
        state.phase = engine::types::phase::Phase::PreCombatMain;
        state.active_player = PlayerId(0);

        let peek = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Peek".to_string(),
            Zone::Hand,
        );
        let _opponent_card = create_object(
            &mut state,
            CardId(20),
            PlayerId(1),
            "Opponent Card".to_string(),
            Zone::Hand,
        );
        let _own_card = create_object(
            &mut state,
            CardId(21),
            PlayerId(0),
            "Own Card".to_string(),
            Zone::Hand,
        );

        let ability = ResolvedAbility::new(
            Effect::RevealHand {
                target: TargetFilter::Player,
                card_filter: TargetFilter::Any,
                count: None,
                random: false,
                choice_optional: false,
            },
            Vec::new(),
            peek,
            PlayerId(0),
        );
        let pending_cast = PendingCast::new(peek, CardId(10), ability, ManaCost::zero());
        let legal_targets = vec![
            TargetRef::Player(PlayerId(0)),
            TargetRef::Player(PlayerId(1)),
        ];
        let waiting_for = WaitingFor::TargetSelection {
            player: PlayerId(0),
            pending_cast: Box::new(pending_cast),
            target_slots: vec![TargetSelectionSlot {
                legal_targets: legal_targets.clone(),
                optional: false,
            }],
            mode_labels: Vec::new(),
            selection: Default::default(),
        };
        state.waiting_for = waiting_for.clone();
        let decision = AiDecisionContext {
            waiting_for,
            candidates: Vec::new(),
        };
        let config = AiConfig::default();
        let context = crate::context::AiContext::empty(&config.weights);

        let target_score = |target| {
            let candidate = CandidateAction {
                action: GameAction::ChooseTarget {
                    target: Some(target),
                },
                metadata: ActionMetadata {
                    actor: Some(PlayerId(0)),
                    tactical_class: TacticalClass::Target,
                },
            };
            let ctx = PolicyContext {
                state: &state,
                decision: &decision,
                candidate: &candidate,
                ai_player: PlayerId(0),
                config: &config,
                context: &context,
                cast_facts: None,
            };
            HandDisruptionPolicy.score(&ctx)
        };

        assert!(
            target_score(TargetRef::Player(PlayerId(1)))
                > target_score(TargetRef::Player(PlayerId(0))),
            "Peek-style hand reveal should prefer an opponent's hand over the AI's own hand"
        );

        let scored = crate::search::score_candidates(&state, PlayerId(0), &config);
        let score_for_target = |target| {
            scored
                .iter()
                .find_map(|(action, score)| match action {
                    GameAction::ChooseTarget {
                        target: Some(chosen),
                    } if *chosen == target => Some(*score),
                    _ => None,
                })
                .expect("target candidate should be scored")
        };
        assert!(
            score_for_target(TargetRef::Player(PlayerId(1)))
                > score_for_target(TargetRef::Player(PlayerId(0))),
            "registered AI scoring should prefer the opponent target"
        );
    }

    #[test]
    fn reveal_hand_target_selection_prefers_unrevealed_opponent_cards() {
        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        state.phase = engine::types::phase::Phase::PreCombatMain;
        state.active_player = PlayerId(0);

        let peek = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Peek".to_string(),
            Zone::Hand,
        );
        let revealed_a = create_object(
            &mut state,
            CardId(20),
            PlayerId(1),
            "Revealed A".to_string(),
            Zone::Hand,
        );
        let revealed_b = create_object(
            &mut state,
            CardId(21),
            PlayerId(1),
            "Revealed B".to_string(),
            Zone::Hand,
        );
        let revealed_c = create_object(
            &mut state,
            CardId(22),
            PlayerId(1),
            "Revealed C".to_string(),
            Zone::Hand,
        );
        let _unrevealed = create_object(
            &mut state,
            CardId(23),
            PlayerId(2),
            "Unrevealed".to_string(),
            Zone::Hand,
        );
        state.revealed_cards.insert(revealed_a);
        state.revealed_cards.insert(revealed_b);
        state.revealed_cards.insert(revealed_c);

        let ability = ResolvedAbility::new(
            Effect::RevealHand {
                target: TargetFilter::Player,
                card_filter: TargetFilter::Any,
                count: None,
                random: false,
                choice_optional: false,
            },
            Vec::new(),
            peek,
            PlayerId(0),
        );
        let pending_cast = PendingCast::new(peek, CardId(10), ability, ManaCost::zero());
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TargetSelection {
                player: PlayerId(0),
                pending_cast: Box::new(pending_cast),
                target_slots: vec![TargetSelectionSlot {
                    legal_targets: vec![
                        TargetRef::Player(PlayerId(1)),
                        TargetRef::Player(PlayerId(2)),
                    ],
                    optional: false,
                }],
                mode_labels: Vec::new(),
                selection: Default::default(),
            },
            candidates: Vec::new(),
        };
        let config = AiConfig::default();
        let context = crate::context::AiContext::empty(&config.weights);
        let target_score = |target| {
            let candidate = CandidateAction {
                action: GameAction::ChooseTarget {
                    target: Some(target),
                },
                metadata: ActionMetadata {
                    actor: Some(PlayerId(0)),
                    tactical_class: TacticalClass::Target,
                },
            };
            let ctx = PolicyContext {
                state: &state,
                decision: &decision,
                candidate: &candidate,
                ai_player: PlayerId(0),
                config: &config,
                context: &context,
                cast_facts: None,
            };
            HandDisruptionPolicy.score(&ctx)
        };

        assert!(
            target_score(TargetRef::Player(PlayerId(2)))
                > target_score(TargetRef::Player(PlayerId(1))),
            "one unrevealed card should beat a larger already revealed hand"
        );
    }

    #[test]
    fn reveal_hand_target_selection_excludes_two_headed_giant_teammate() {
        let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
        state.phase = engine::types::phase::Phase::PreCombatMain;
        state.active_player = PlayerId(0);

        let peek = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Peek".to_string(),
            Zone::Hand,
        );
        for idx in 0..3 {
            create_object(
                &mut state,
                CardId(20 + idx),
                PlayerId(1),
                format!("Teammate Card {idx}"),
                Zone::Hand,
            );
        }
        create_object(
            &mut state,
            CardId(30),
            PlayerId(2),
            "Opponent Card".to_string(),
            Zone::Hand,
        );

        let ability = ResolvedAbility::new(
            Effect::RevealHand {
                target: TargetFilter::Player,
                card_filter: TargetFilter::Any,
                count: None,
                random: false,
                choice_optional: false,
            },
            Vec::new(),
            peek,
            PlayerId(0),
        );
        let pending_cast = PendingCast::new(peek, CardId(10), ability, ManaCost::zero());
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TargetSelection {
                player: PlayerId(0),
                pending_cast: Box::new(pending_cast),
                target_slots: vec![TargetSelectionSlot {
                    legal_targets: vec![
                        TargetRef::Player(PlayerId(1)),
                        TargetRef::Player(PlayerId(2)),
                    ],
                    optional: false,
                }],
                mode_labels: Vec::new(),
                selection: Default::default(),
            },
            candidates: Vec::new(),
        };
        let config = AiConfig::default();
        let context = crate::context::AiContext::empty(&config.weights);
        let target_score = |target| {
            let candidate = CandidateAction {
                action: GameAction::ChooseTarget {
                    target: Some(target),
                },
                metadata: ActionMetadata {
                    actor: Some(PlayerId(0)),
                    tactical_class: TacticalClass::Target,
                },
            };
            let ctx = PolicyContext {
                state: &state,
                decision: &decision,
                candidate: &candidate,
                ai_player: PlayerId(0),
                config: &config,
                context: &context,
                cast_facts: None,
            };
            HandDisruptionPolicy.score(&ctx)
        };

        assert!(
            target_score(TargetRef::Player(PlayerId(2)))
                > target_score(TargetRef::Player(PlayerId(1))),
            "opponent target should beat a larger teammate hand"
        );
    }

    #[test]
    fn reveal_hand_target_matching_uses_player_filter_semantics() {
        let state = GameState::new_two_player(42);
        let opponent_reveal = Effect::RevealHand {
            target: TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
            card_filter: TargetFilter::Any,
            count: None,
            random: false,
            choice_optional: false,
        };
        assert!(reveal_hand_matches_chosen_player_target(
            &state,
            &opponent_reveal,
            PlayerId(1),
            PlayerId(0)
        ));
        assert!(!reveal_hand_matches_chosen_player_target(
            &state,
            &opponent_reveal,
            PlayerId(0),
            PlayerId(0)
        ));

        let creature_reveal = Effect::RevealHand {
            target: TargetFilter::Typed(TypedFilter::creature()),
            card_filter: TargetFilter::Any,
            count: None,
            random: false,
            choice_optional: false,
        };
        assert!(!reveal_hand_matches_chosen_player_target(
            &state,
            &creature_reveal,
            PlayerId(1),
            PlayerId(0)
        ));
    }
}
