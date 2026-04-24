use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;

use crate::features::DeckFeatures;
use crate::projection::{ProjectionHorizon, VelocitySample};

use super::activation::turn_only;
use super::context::PolicyContext;
use super::effect_classify::is_spell_beneficial;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use super::strategy_helpers::ai_can_block;

pub struct EvasionRemovalPriorityPolicy;

/// Scaling factor applied to projected growth when ranking removal targets.
/// Empirically calibrated so a creature that grows by +3/+3 between now and
/// opponent's next combat gets ~1.0 of extra removal score — comparable to
/// the evasion bonus for a mid-sized flyer.
const VELOCITY_BONUS_MULT: f64 = 0.3;
/// Cap on the velocity contribution so a single runaway Ouroboroid doesn't
/// completely drown out other signals.
const VELOCITY_BONUS_MAX: f64 = 3.0;

impl EvasionRemovalPriorityPolicy {
    pub fn score(&self, ctx: &PolicyContext<'_>) -> f64 {
        let GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target_id)),
        } = &ctx.candidate.action
        else {
            return 0.0;
        };

        // Only for harmful effects (removal)
        if is_spell_beneficial(ctx) {
            return 0.0;
        }

        let Some(target) = ctx.state.objects.get(target_id) else {
            return 0.0;
        };

        // Only relevant for creatures
        if !target.card_types.core_types.contains(&CoreType::Creature) {
            return 0.0;
        }

        let evasion_bonus = evasion_score(ctx, target, *target_id);
        let velocity_bonus = velocity_score(ctx, target, *target_id);

        evasion_bonus + velocity_bonus
    }
}

/// Score contribution from evasion keywords (original behavior).
fn evasion_score(
    ctx: &PolicyContext<'_>,
    target: &engine::game::game_object::GameObject,
    target_id: engine::types::identifiers::ObjectId,
) -> f64 {
    let power = target.power.unwrap_or(0) as f64;
    let mult = ctx.penalties().evasion_removal_bonus_mult;

    let has_flying = target.has_keyword(&Keyword::Flying);
    let has_shadow = target.has_keyword(&Keyword::Shadow);
    let has_menace = target.has_keyword(&Keyword::Menace);

    if !has_flying && !has_shadow && !has_menace {
        return 0.0;
    }

    let can_block = ai_can_block(ctx.state, ctx.ai_player, target_id);

    if !can_block {
        (power * mult).min(3.0)
    } else if has_menace {
        let legal_blocker_count = ctx
            .state
            .battlefield
            .iter()
            .filter(|&&id| {
                ctx.state.objects.get(&id).is_some_and(|obj| {
                    obj.controller == ctx.ai_player
                        && !obj.tapped
                        && obj.card_types.core_types.contains(&CoreType::Creature)
                        && engine::game::combat::can_block_pair(ctx.state, id, target_id)
                })
            })
            .count();
        if legal_blocker_count < 2 {
            (power * mult * 0.5).min(3.0)
        } else {
            0.0
        }
    } else {
        0.0
    }
}

/// Score contribution from projected-turn growth. Creatures that scale
/// significantly before their controller's next combat (Ouroboroid, sagas,
/// Predator Ooze, tokens-spawning engines) become high-priority removal
/// targets automatically — no per-card AI code. Failure to project or
/// non-opponent target → 0.
///
/// **Deadline-gated**: the underlying `project_to` simulates the opponent's
/// next turn. On large multi-player states this costs ~1.5s per uncached
/// opponent. When the wall-clock deadline has expired or the remaining
/// budget is too tight to absorb another uncached projection, fall back
/// to cache-only lookups and return 0 on miss — preserves the evasion
/// signal and doesn't blow the user-visible turn-time budget for a
/// nice-to-have bonus. The threshold comes from
/// `SearchConfig::projection_min_budget_ms` so it's tunable per difficulty.
fn velocity_score(
    ctx: &PolicyContext<'_>,
    target: &engine::game::game_object::GameObject,
    target_id: engine::types::identifiers::ObjectId,
) -> f64 {
    if target.controller == ctx.ai_player {
        return 0.0;
    }

    // Prefer a cached projection; only fall through to the live simulator
    // when the budget clearly affords it. The hot path in multi-opponent
    // target selection is several uncached (ai_player, target_opponent)
    // pairs back-to-back — without this gate they each pay the ~1.5s
    // simulation cost serially.
    let session = &ctx.context.session;
    let horizon = ProjectionHorizon::OpponentBeginCombat;
    let projection = match session.cached_projection(
        ctx.state,
        ctx.ai_player,
        target.controller,
        horizon,
    ) {
        Some(cached) => cached,
        None => {
            if !ctx.can_afford_projection() {
                return 0.0;
            }
            let Ok(fresh) = session.get_or_project(
                ctx.state,
                ctx.ai_player,
                target.controller,
                horizon,
            ) else {
                return 0.0;
            };
            fresh
        }
    };

    let samples = crate::projection::threat_velocity(ctx.state, &projection, target.controller);

    match samples.get(&target_id) {
        Some(VelocitySample::Changed { delta }) if *delta > 0 => {
            (*delta as f64 * VELOCITY_BONUS_MULT).min(VELOCITY_BONUS_MAX)
        }
        _ => 0.0,
    }
}

impl TacticalPolicy for EvasionRemovalPriorityPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::EvasionRemovalPriority
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::SelectTarget]
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
            reason: PolicyReason::new("evasion_removal_priority_score"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiConfig;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{Effect, ResolvedAbility, TargetFilter, TargetRef};
    use engine::types::game_state::{GameState, PendingCast, TargetSelectionSlot, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::keywords::Keyword;
    use engine::types::mana::ManaCost;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;

    #[test]
    fn bonus_for_unblockable_flyer() {
        let mut state = GameState::new_two_player(42);

        // Opponent's flyer
        let flyer = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Dragon".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&flyer).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(4);
        obj.toughness = Some(4);
        obj.keywords.push(Keyword::Flying);

        // AI has a ground creature (can't block flyer)
        let ground = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&ground).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(2);
        obj.toughness = Some(2);

        let config = AiConfig::default();
        let ability = ResolvedAbility::new(
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            Vec::new(),
            ObjectId(100),
            PlayerId(0),
        );
        let pending_cast = PendingCast::new(ObjectId(100), CardId(100), ability, ManaCost::zero());
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TargetSelection {
                player: PlayerId(0),
                pending_cast: Box::new(pending_cast),
                target_slots: vec![TargetSelectionSlot {
                    legal_targets: vec![TargetRef::Object(flyer)],
                    optional: false,
                }],
                selection: Default::default(),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::ChooseTarget {
                target: Some(TargetRef::Object(flyer)),
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
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };

        let score = EvasionRemovalPriorityPolicy.score(&ctx);
        assert!(
            score > 1.0,
            "Should give significant bonus for unblockable flyer, got {score}"
        );
    }

    #[test]
    fn no_bonus_for_ground_creature() {
        let mut state = GameState::new_two_player(42);

        // Opponent's ground creature
        let ground_opp = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Elephant".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&ground_opp).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(4);
        obj.toughness = Some(4);

        let config = AiConfig::default();
        let ability = ResolvedAbility::new(
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            Vec::new(),
            ObjectId(100),
            PlayerId(0),
        );
        let pending_cast = PendingCast::new(ObjectId(100), CardId(100), ability, ManaCost::zero());
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TargetSelection {
                player: PlayerId(0),
                pending_cast: Box::new(pending_cast),
                target_slots: vec![TargetSelectionSlot {
                    legal_targets: vec![TargetRef::Object(ground_opp)],
                    optional: false,
                }],
                selection: Default::default(),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::ChooseTarget {
                target: Some(TargetRef::Object(ground_opp)),
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
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };

        let score = EvasionRemovalPriorityPolicy.score(&ctx);
        assert!(
            score.abs() < 0.01,
            "No bonus for ground creature, got {score}"
        );
    }
}
