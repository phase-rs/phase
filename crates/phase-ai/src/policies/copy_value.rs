use engine::ai_support::{
    copy_effect_adds_flying, copy_target_filter, copy_target_mana_value_ceiling,
    project_copy_mana_spent_for_x,
};
use engine::game::filter::{matches_target_filter, FilterContext};
use engine::types::ability::{AbilityDefinition, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::Supertype;
use engine::types::game_state::{GameState, PendingCast, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use crate::eval::{evaluate_creature, strategic_intent, StrategicIntent};
use crate::features::DeckFeatures;

use super::activation::turn_only;
use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};

pub struct CopyValuePolicy;

impl CopyValuePolicy {
    pub fn score(&self, ctx: &PolicyContext<'_>) -> f64 {
        match (&ctx.decision.waiting_for, &ctx.candidate.action) {
            (
                WaitingFor::ChooseXValue {
                    pending_cast, max, ..
                },
                GameAction::ChooseX { value },
            ) => score_choose_x(ctx, pending_cast, *max, *value),
            (
                WaitingFor::CopyTargetChoice {
                    source_id,
                    valid_targets,
                    ..
                },
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(target_id)),
                },
            ) if valid_targets.contains(target_id) => {
                score_target_choice(ctx.state, ctx.ai_player, *source_id, *target_id)
            }
            _ => 0.0,
        }
    }
}

impl TacticalPolicy for CopyValuePolicy {
    fn id(&self) -> PolicyId {
        PolicyId::CopyValue
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::ChooseX, DecisionKind::SelectTarget]
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
            reason: PolicyReason::new("copy_value_score"),
        }
    }
}

fn score_choose_x(
    ctx: &PolicyContext<'_>,
    pending_cast: &PendingCast,
    max_x: u32,
    candidate_x: u32,
) -> f64 {
    let Some(source) = ctx.state.objects.get(&pending_cast.object_id) else {
        return 0.0;
    };
    let Some(effect_def) = copy_effect_for_object(source) else {
        return 0.0;
    };

    let scores: Vec<_> = (0..=max_x)
        .map(|x_value| {
            let projected_spent = project_copy_mana_spent_for_x(pending_cast, x_value);
            let ceiling = copy_target_mana_value_ceiling(projected_spent, effect_def);
            let best_target =
                legal_copy_targets(ctx.state, source.id, source.controller, effect_def, ceiling)
                    .into_iter()
                    .map(|target_id| {
                        score_target_choice(ctx.state, ctx.ai_player, source.id, target_id)
                    })
                    .max_by(|left, right| left.total_cmp(right))
                    .unwrap_or(0.10);
            let raw = best_target - (0.03 * x_value as f64);
            (x_value, raw)
        })
        .collect();

    let preferred_x = preferred_x_value(&scores);
    let raw_score = scores
        .iter()
        .find(|(x_value, _)| *x_value == candidate_x)
        .map(|(_, score)| *score)
        .unwrap_or(0.0);

    if candidate_x == preferred_x {
        100.0 + raw_score
    } else {
        raw_score
    }
}

fn preferred_x_value(scores: &[(u32, f64)]) -> u32 {
    let mut best = None;

    for &(x_value, score) in scores {
        best = match best {
            None => Some((x_value, score)),
            Some((best_x, best_score)) => {
                if score > best_score + 0.05
                    || ((score - best_score).abs() <= 0.05 && x_value < best_x)
                {
                    Some((x_value, score))
                } else {
                    Some((best_x, best_score))
                }
            }
        };
    }

    best.map(|(x_value, _)| x_value).unwrap_or(0)
}

fn score_target_choice(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    target_id: ObjectId,
) -> f64 {
    let Some(source) = state.objects.get(&source_id) else {
        return 0.0;
    };
    let Some(target) = state.objects.get(&target_id) else {
        return 0.0;
    };
    let Some(effect_def) = copy_effect_for_object(source) else {
        return 0.0;
    };

    let base_creature_value = evaluate_creature(state, target_id);
    let mut copy_bonus = 0.0;
    let mut copy_penalty = 0.0;

    if target_has_etb_value(target) {
        copy_bonus += 0.12;
    }

    if copy_effect_adds_flying(effect_def)
        && !target.has_keyword(&Keyword::Flying)
        && strategic_intent(state, ai_player) != StrategicIntent::Stabilize
        && target.power.unwrap_or(0) > 0
    {
        copy_bonus += 0.08;
    }

    if target.controller == ai_player && strengthens_supported_plan(state, ai_player, target) {
        copy_bonus += 0.06;
    }

    if target.controller == source.controller
        && target.card_types.supertypes.contains(&Supertype::Legendary)
    {
        copy_penalty += 0.20;
    }

    if base_creature_value < 3.0 {
        copy_penalty += 0.08;
    }

    base_creature_value + copy_bonus - copy_penalty
}

fn copy_effect_for_object(
    object: &engine::game::game_object::GameObject,
) -> Option<&AbilityDefinition> {
    object
        .replacement_definitions
        .iter_unchecked()
        .filter_map(|replacement| replacement.execute.as_deref())
        .find(|effect_def| copy_target_filter(effect_def).is_some())
}

fn legal_copy_targets(
    state: &GameState,
    source_id: ObjectId,
    controller: PlayerId,
    effect_def: &AbilityDefinition,
    max_mana_value: Option<u32>,
) -> Vec<ObjectId> {
    let Some(filter) = copy_target_filter(effect_def) else {
        return Vec::new();
    };

    state
        .battlefield
        .iter()
        .copied()
        .filter(|target_id| *target_id != source_id)
        .filter(|target_id| {
            state.objects.get(target_id).is_some_and(|object| {
                max_mana_value.is_none_or(|max| object.mana_cost.mana_value() <= max)
                    && matches_target_filter(
                        state,
                        *target_id,
                        filter,
                        &FilterContext::from_source_with_controller(source_id, controller),
                    )
            })
        })
        .collect()
}

fn target_has_etb_value(object: &engine::game::game_object::GameObject) -> bool {
    object.trigger_definitions.iter_unchecked().any(|trigger| {
        trigger.mode == TriggerMode::ChangesZone && trigger.destination == Some(Zone::Battlefield)
    })
}

fn strengthens_supported_plan(
    state: &GameState,
    ai_player: PlayerId,
    object: &engine::game::game_object::GameObject,
) -> bool {
    match strategic_intent(state, ai_player) {
        StrategicIntent::PushLethal
        | StrategicIntent::PreserveAdvantage
        | StrategicIntent::Develop => {
            object.power.unwrap_or(0) >= 3
                || object.has_keyword(&Keyword::Flying)
                || object.has_keyword(&Keyword::Trample)
                || object.has_keyword(&Keyword::Menace)
                || !object.abilities.is_empty()
                || !object.trigger_definitions.is_empty()
        }
        StrategicIntent::Stabilize => {
            object.toughness.unwrap_or(0) >= 4
                || object.has_keyword(&Keyword::Deathtouch)
                || object.has_keyword(&Keyword::Lifelink)
                || object.has_keyword(&Keyword::Vigilance)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityKind, ContinuousModification, CopyManaValueLimit, Effect, QuantityExpr,
        ReplacementDefinition, TargetFilter,
    };
    use engine::types::card_type::CoreType;
    use engine::types::game_state::PendingCast;
    use engine::types::identifiers::CardId;
    use engine::types::mana::{ManaCost, ManaCostShard};
    use engine::types::replacements::ReplacementEvent;

    fn make_state() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state
    }

    fn add_mockingbird_like_card(state: &mut GameState, zone: Zone) -> ObjectId {
        let object_id = create_object(
            state,
            CardId(100),
            PlayerId(0),
            "Mockingbird".to_string(),
            zone,
        );
        let object = state.objects.get_mut(&object_id).unwrap();
        object.card_types.core_types.push(CoreType::Creature);
        object.power = Some(1);
        object.toughness = Some(1);
        object.base_power = Some(1);
        object.base_toughness = Some(1);
        object.base_keywords.push(Keyword::Flying);
        object.keywords.push(Keyword::Flying);
        object.replacement_definitions.push(
            ReplacementDefinition::new(ReplacementEvent::Moved).execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::BecomeCopy {
                    target: TargetFilter::Any,
                    duration: None,
                    mana_value_limit: Some(CopyManaValueLimit::AmountSpentToCastSource),
                    additional_modifications: vec![
                        ContinuousModification::AddSubtype {
                            subtype: "Bird".to_string(),
                        },
                        ContinuousModification::AddKeyword {
                            keyword: Keyword::Flying,
                        },
                    ],
                },
            )),
        );
        object_id
    }

    fn add_creature(
        state: &mut GameState,
        card_id: u64,
        owner: PlayerId,
        name: &str,
        power: i32,
        toughness: i32,
        mana_value: u32,
    ) -> ObjectId {
        let object_id = create_object(
            state,
            CardId(card_id),
            owner,
            name.to_string(),
            Zone::Battlefield,
        );
        let object = state.objects.get_mut(&object_id).unwrap();
        object.card_types.core_types.push(CoreType::Creature);
        object.power = Some(power);
        object.toughness = Some(toughness);
        object.base_power = Some(power);
        object.base_toughness = Some(toughness);
        object.mana_cost = ManaCost::generic(mana_value);
        object.card_types.supertypes.retain(|_| false);
        object_id
    }

    #[test]
    fn choose_x_prefers_smallest_value_when_no_copy_targets_exist() {
        let mut state = make_state();
        let mockingbird_id = add_mockingbird_like_card(&mut state, Zone::Hand);
        let pending_cast = PendingCast::new(
            mockingbird_id,
            CardId(100),
            engine::types::ability::ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: engine::types::ability::TargetFilter::Controller,
                },
                Vec::new(),
                mockingbird_id,
                PlayerId(0),
            ),
            ManaCost::Cost {
                shards: vec![ManaCostShard::X, ManaCostShard::Blue],
                generic: 0,
            },
        );
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::ChooseXValue {
                player: PlayerId(0),
                max: 3,
                pending_cast: Box::new(pending_cast),
                convoke_mode: None,
            },
            candidates: Vec::new(),
        };

        let score_zero = CopyValuePolicy.score(&PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &CandidateAction {
                action: GameAction::ChooseX { value: 0 },
                metadata: ActionMetadata {
                    actor: Some(PlayerId(0)),
                    tactical_class: TacticalClass::Selection,
                },
            },
            ai_player: PlayerId(0),
            config: &crate::config::AiConfig::default(),
            context: &crate::context::AiContext::empty(&crate::eval::EvalWeightSet::default()),
            cast_facts: None,
        });
        let score_two = CopyValuePolicy.score(&PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &CandidateAction {
                action: GameAction::ChooseX { value: 2 },
                metadata: ActionMetadata {
                    actor: Some(PlayerId(0)),
                    tactical_class: TacticalClass::Selection,
                },
            },
            ai_player: PlayerId(0),
            config: &crate::config::AiConfig::default(),
            context: &crate::context::AiContext::empty(&crate::eval::EvalWeightSet::default()),
            cast_facts: None,
        });

        assert!(score_zero > score_two);
    }

    #[test]
    fn choose_x_unlocks_higher_mana_value_target_when_materially_better() {
        let mut state = make_state();
        let mockingbird_id = add_mockingbird_like_card(&mut state, Zone::Hand);
        add_creature(&mut state, 1, PlayerId(0), "Otter", 1, 1, 1);
        add_creature(&mut state, 2, PlayerId(1), "Dragon", 4, 4, 4);
        let pending_cast = PendingCast::new(
            mockingbird_id,
            CardId(100),
            engine::types::ability::ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: engine::types::ability::TargetFilter::Controller,
                },
                Vec::new(),
                mockingbird_id,
                PlayerId(0),
            ),
            ManaCost::Cost {
                shards: vec![ManaCostShard::X, ManaCostShard::Blue],
                generic: 0,
            },
        );
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::ChooseXValue {
                player: PlayerId(0),
                max: 3,
                pending_cast: Box::new(pending_cast),
                convoke_mode: None,
            },
            candidates: Vec::new(),
        };

        let score_zero = CopyValuePolicy.score(&PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &CandidateAction {
                action: GameAction::ChooseX { value: 0 },
                metadata: ActionMetadata {
                    actor: Some(PlayerId(0)),
                    tactical_class: TacticalClass::Selection,
                },
            },
            ai_player: PlayerId(0),
            config: &crate::config::AiConfig::default(),
            context: &crate::context::AiContext::empty(&crate::eval::EvalWeightSet::default()),
            cast_facts: None,
        });
        let score_three = CopyValuePolicy.score(&PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &CandidateAction {
                action: GameAction::ChooseX { value: 3 },
                metadata: ActionMetadata {
                    actor: Some(PlayerId(0)),
                    tactical_class: TacticalClass::Selection,
                },
            },
            ai_player: PlayerId(0),
            config: &crate::config::AiConfig::default(),
            context: &crate::context::AiContext::empty(&crate::eval::EvalWeightSet::default()),
            cast_facts: None,
        });

        assert!(score_three > score_zero);
    }

    #[test]
    fn copy_target_choice_prefers_higher_value_target() {
        let mut state = make_state();
        let mockingbird_id = add_mockingbird_like_card(&mut state, Zone::Battlefield);
        let small = add_creature(&mut state, 1, PlayerId(1), "Mouse", 1, 1, 1);
        let large = add_creature(&mut state, 2, PlayerId(1), "Dragon", 4, 4, 4);
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::CopyTargetChoice {
                player: PlayerId(0),
                source_id: mockingbird_id,
                valid_targets: vec![small, large],
                max_mana_value: Some(4),
            },
            candidates: Vec::new(),
        };

        let score_small = CopyValuePolicy.score(&PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &CandidateAction {
                action: GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(small)),
                },
                metadata: ActionMetadata {
                    actor: Some(PlayerId(0)),
                    tactical_class: TacticalClass::Selection,
                },
            },
            ai_player: PlayerId(0),
            config: &crate::config::AiConfig::default(),
            context: &crate::context::AiContext::empty(&crate::eval::EvalWeightSet::default()),
            cast_facts: None,
        });
        let score_large = CopyValuePolicy.score(&PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &CandidateAction {
                action: GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(large)),
                },
                metadata: ActionMetadata {
                    actor: Some(PlayerId(0)),
                    tactical_class: TacticalClass::Selection,
                },
            },
            ai_player: PlayerId(0),
            config: &crate::config::AiConfig::default(),
            context: &crate::context::AiContext::empty(&crate::eval::EvalWeightSet::default()),
            cast_facts: None,
        });

        assert!(score_large > score_small);
    }
}
