use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;

use crate::eval::opponent_battlefield_creature_threat_value;
use crate::features::DeckFeatures;
use crate::projection::{ProjectionHorizon, VelocitySample};

use super::activation::turn_only;
use super::context::PolicyContext;
use super::effect_classify::targeted_object_impact;
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
        if !matches!(
            ctx.decision.waiting_for,
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. }
        ) {
            return 0.0;
        }

        let GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target_id)),
        } = &ctx.candidate.action
        else {
            return 0.0;
        };

        if !targeted_object_impact(ctx, *target_id).is_some_and(|impact| impact < -0.25) {
            return 0.0;
        }

        let Some(target_value) =
            opponent_battlefield_creature_threat_value(ctx.state, ctx.ai_player, *target_id)
        else {
            return 0.0;
        };
        let Some(target) = ctx.state.objects.get(target_id) else {
            return 0.0;
        };

        let target_quality_bonus = removal_target_quality_score(target_value);
        let evasion_bonus = evasion_score(ctx, target, *target_id);
        let velocity_bonus = velocity_score(ctx, target, *target_id);

        target_quality_bonus + evasion_bonus + velocity_bonus
    }
}

fn removal_target_quality_score(value: f64) -> f64 {
    if value < 2.0 {
        -0.8
    } else {
        (value / 4.0).min(2.0)
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

    // Hoist block-legality statics once for this scoring pass.
    let slices = crate::combat_ai::BlockLegalitySlices::collect(ctx.state);

    let can_block = ai_can_block(ctx.state, ctx.ai_player, target_id, &slices);

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
                        && slices.can_block_pair(ctx.state, id, target_id)
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
    let projection =
        match session.cached_projection(ctx.state, ctx.ai_player, target.controller, horizon) {
            Some(cached) => cached,
            None => {
                if !ctx.can_afford_projection() {
                    return 0.0;
                }
                let Ok(fresh) =
                    session.get_or_project(ctx.state, ctx.ai_player, target.controller, horizon)
                else {
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
        PolicyVerdict::score(
            self.score(ctx),
            PolicyReason::new("evasion_removal_priority_score"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{create_config, AiConfig, AiDifficulty, Platform};
    use engine::ai_support::{
        build_decision_context, ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass,
    };
    use engine::game::scenario::{GameScenario, P0};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, EffectKind, PtValue, ResolvedAbility, TargetFilter,
        TargetRef, TypedFilter,
    };
    use engine::types::format::FormatConfig;
    use engine::types::game_state::{
        CastPaymentMode, CopyTargetSlot, GameState, PendingCast, TargetSelectionProgress,
        TargetSelectionSlot, WaitingFor,
    };
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::keywords::Keyword;
    use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
    use engine::types::phase::Phase;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;
    use rand::rngs::SmallRng;
    use rand::SeedableRng;

    const BEAST_WITHIN_ORACLE: &str =
        "Destroy target permanent. Its controller creates a 3/3 green Beast creature token.";

    fn add_creature(
        state: &mut GameState,
        controller: PlayerId,
        name: &str,
        power: i32,
        toughness: i32,
    ) -> ObjectId {
        let card_id = CardId(state.next_object_id);
        let id = create_object(
            state,
            card_id,
            controller,
            name.to_string(),
            Zone::Battlefield,
        );
        let object = state.objects.get_mut(&id).unwrap();
        object.card_types.core_types.push(CoreType::Creature);
        object.power = Some(power);
        object.toughness = Some(toughness);
        id
    }

    fn candidate_for(target: ObjectId) -> CandidateAction {
        CandidateAction {
            action: GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target)),
            },
            metadata: ActionMetadata::for_actor(Some(P0), TacticalClass::Target),
        }
    }

    fn policy_score(
        state: &GameState,
        decision: &AiDecisionContext,
        target: ObjectId,
        config: &AiConfig,
    ) -> f64 {
        let candidate = candidate_for(target);
        let ai_context = crate::context::AiContext::empty(&config.weights);
        let ctx = PolicyContext {
            state,
            decision,
            candidate: &candidate,
            ai_player: P0,
            config,
            context: &ai_context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        EvasionRemovalPriorityPolicy.score(&ctx)
    }

    fn registry_delta(
        state: &GameState,
        decision: &AiDecisionContext,
        target: ObjectId,
        config: &AiConfig,
    ) -> f64 {
        let candidate = candidate_for(target);
        let ai_context = crate::context::AiContext::empty(&config.weights);
        let ctx = PolicyContext {
            state,
            decision,
            candidate: &candidate,
            ai_player: P0,
            config,
            context: &ai_context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };

        crate::policies::registry::PolicyRegistry::shared()
            .verdicts(&ctx)
            .into_iter()
            .find_map(|(id, verdict)| {
                (id == PolicyId::EvasionRemovalPriority).then(|| match verdict {
                    PolicyVerdict::Score { delta, .. } => delta,
                    PolicyVerdict::Reject { .. } => {
                        panic!("removal target priority must not reject legal targets")
                    }
                })
            })
            .expect("EvasionRemovalPriorityPolicy must be active in the production registry")
    }

    fn full_score_for_target(scores: &[(GameAction, f64)], target: ObjectId) -> f64 {
        scores
            .iter()
            .find_map(|(action, score)| match action {
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(id)),
                } if *id == target => Some(*score),
                _ => None,
            })
            .expect("target must be a scored legal candidate")
    }

    fn activated_target_state(effect: Effect) -> (GameState, ObjectId, ObjectId) {
        let mut scenario = GameScenario::new_n_player(3, 42);
        let source = scenario
            .add_creature(P0, "Targeting Engine", 1, 1)
            .with_ability_definition(AbilityDefinition::new(AbilityKind::Activated, effect))
            .id();
        let low = scenario.add_creature(PlayerId(1), "Frog", 3, 3).id();
        let high = scenario.add_creature(PlayerId(2), "Krenko", 3, 3).id();
        for index in 0..10 {
            scenario.add_creature(PlayerId(2), &format!("Goblin {index}"), 1, 1);
        }
        let mut runner = scenario.build();
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
            })
            .expect("activation should reach target selection");
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::TargetSelection { .. }
            ),
            "activated targeted ability must use ordinary TargetSelection"
        );
        (runner.state().clone(), low, high)
    }

    #[test]
    fn beast_within_targets_equal_body_controlled_by_board_threat() {
        let mut scenario = GameScenario::new_n_player(3, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let beast_within = scenario
            .add_spell_to_hand_from_oracle(P0, "Beast Within", true, BEAST_WITHIN_ORACLE)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 2,
            })
            .id();
        let frog = scenario.add_creature(PlayerId(1), "Frog Lizard", 3, 3).id();
        let krenko = scenario
            .add_creature(PlayerId(2), "Krenko, Mob Boss", 3, 3)
            .id();
        for index in 0..10 {
            scenario.add_creature(PlayerId(2), &format!("Goblin {index}"), 1, 1);
        }
        scenario.with_mana_pool(
            P0,
            vec![
                ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
                ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
                ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ],
        );

        let mut runner = scenario.build();
        let card_id = runner.state().objects[&beast_within].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: beast_within,
                card_id,
                targets: Vec::new(),
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("the real Beast Within fixture should reach target selection");

        let (pending_cast, target_slots) = match &runner.state().waiting_for {
            WaitingFor::TargetSelection {
                pending_cast,
                target_slots,
                ..
            } => (pending_cast, target_slots),
            other => panic!("expected Beast Within target selection, got {other:?}"),
        };
        let effects = crate::policies::context::collect_ability_effects(&pending_cast.ability);
        assert!(
            effects
                .first()
                .is_some_and(|effect| matches!(effect, Effect::Destroy { .. })),
            "reach guard: Beast Within must parse its primary Destroy effect"
        );
        assert!(
            effects.iter().any(|effect| matches!(
                effect,
                Effect::Token {
                    power: PtValue::Fixed(3),
                    toughness: PtValue::Fixed(3),
                    colors,
                    owner: TargetFilter::ParentTargetController,
                    ..
                } if colors.contains(&ManaColor::Green)
            )),
            "reach guard: Beast Within must retain the controller-owned green 3/3 compensation"
        );
        assert!(
            effects
                .iter()
                .all(|effect| !matches!(effect, Effect::Unimplemented { .. })),
            "the regression fixture must not silently drop an unsupported clause"
        );
        assert_eq!(target_slots.len(), 1);
        assert!(target_slots[0]
            .legal_targets
            .contains(&TargetRef::Object(frog)));
        assert!(target_slots[0]
            .legal_targets
            .contains(&TargetRef::Object(krenko)));

        let state = runner.state();
        let decision = build_decision_context(state);
        let config = create_config(AiDifficulty::VeryHard, Platform::Native).into_measurement(42);
        let frog_delta = registry_delta(state, &decision, frog, &config);
        let krenko_delta = registry_delta(state, &decision, krenko, &config);
        assert!(
            krenko_delta > frog_delta,
            "registered removal policy must prefer the equal body controlled by the larger threat: Krenko={krenko_delta}, Frog={frog_delta}"
        );

        let scores = crate::search::score_candidates(state, P0, &config);
        assert!(
            full_score_for_target(&scores, krenko) > full_score_for_target(&scores, frog),
            "the complete Very Hard scorer must preserve the controller-threat preference"
        );

        let mut rng = SmallRng::seed_from_u64(42);
        assert_eq!(
            crate::choose_action(state, P0, &config, &mut rng),
            Some(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(krenko)),
            }),
            "Very Hard must spend Beast Within on Krenko rather than the Frog Lizard"
        );
    }

    #[test]
    fn activated_removal_weights_controller_threat_but_beneficial_activation_is_neutral() {
        let destroy = Effect::Destroy {
            target: TargetFilter::Typed(TypedFilter::creature()),
            cant_regenerate: false,
        };
        let (state, low, high) = activated_target_state(destroy);
        let decision = build_decision_context(&state);
        let config = AiConfig::default();
        assert!(
            policy_score(&state, &decision, high, &config)
                > policy_score(&state, &decision, low, &config)
        );

        let pump = Effect::Pump {
            power: PtValue::Fixed(2),
            toughness: PtValue::Fixed(2),
            target: TargetFilter::Typed(TypedFilter::creature()),
        };
        let (state, _, high) = activated_target_state(pump);
        let decision = build_decision_context(&state);
        assert_eq!(policy_score(&state, &decision, high, &config), 0.0);
    }

    #[test]
    fn harmful_trigger_uses_controller_threat_but_copy_retarget_is_neutral() {
        let mut state = GameState::new(FormatConfig::free_for_all(), 3, 42);
        let low = add_creature(&mut state, PlayerId(1), "Frog", 3, 3);
        let high = add_creature(&mut state, PlayerId(2), "Krenko", 3, 3);
        for index in 0..10 {
            add_creature(&mut state, PlayerId(2), &format!("Goblin {index}"), 1, 1);
        }
        let trigger_source = ObjectId(999);
        state.pending_trigger = Some(engine::game::triggers::PendingTrigger {
            source_id: trigger_source,
            controller: P0,
            condition: None,
            ability: ResolvedAbility::new(
                Effect::Destroy {
                    target: TargetFilter::Any,
                    cant_regenerate: false,
                },
                Vec::new(),
                trigger_source,
                P0,
            ),
            timestamp: 1,
            target_constraints: Vec::new(),
            distribute: None,
            trigger_event: None,
            modal: None,
            mode_abilities: Vec::new(),
            description: None,
            may_trigger_origin: None,
            subject_match_count: None,
            die_result: None,
        });
        let config = AiConfig::default();
        let slot = TargetSelectionSlot {
            legal_targets: vec![TargetRef::Object(low), TargetRef::Object(high)],
            optional: false,
            chooser: None,
        };
        let trigger = AiDecisionContext {
            waiting_for: WaitingFor::TriggerTargetSelection {
                player: P0,
                trigger_controller: Some(P0),
                trigger_event: None,
                trigger_events: Vec::new(),
                target_slots: vec![slot],
                mode_labels: Vec::new(),
                target_constraints: Vec::new(),
                selection: TargetSelectionProgress::default(),
                source_id: Some(trigger_source),
                description: None,
            },
            candidates: vec![candidate_for(low), candidate_for(high)],
        };
        assert!(
            policy_score(&state, &trigger, high, &config)
                > policy_score(&state, &trigger, low, &config),
            "harmful triggered removal must retain controller-threat targeting"
        );

        let copy = AiDecisionContext {
            waiting_for: WaitingFor::CopyRetarget {
                player: P0,
                copy_id: ObjectId(1000),
                target_slots: vec![CopyTargetSlot {
                    current: Some(TargetRef::Object(low)),
                    legal_alternatives: vec![TargetRef::Object(high)],
                }],
                effect_kind: EffectKind::Destroy,
                effect_source_id: None,
                current_slot: 0,
                paradigm_remaining_offers: None,
            },
            candidates: vec![candidate_for(high)],
        };
        assert_eq!(policy_score(&state, &copy, high, &config), 0.0);
    }

    #[test]
    fn teammate_and_eliminated_creatures_are_neutral() {
        let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
        let teammate = add_creature(&mut state, PlayerId(1), "Teammate", 4, 4);
        let opponent = add_creature(&mut state, PlayerId(2), "Opponent", 4, 4);
        let eliminated = add_creature(&mut state, PlayerId(3), "Eliminated", 4, 4);
        state.players[3].is_eliminated = true;
        let ability = ResolvedAbility::new(
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            Vec::new(),
            ObjectId(100),
            P0,
        );
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TargetSelection {
                player: P0,
                pending_cast: Box::new(PendingCast::new(
                    ObjectId(100),
                    CardId(100),
                    ability,
                    ManaCost::zero(),
                )),
                target_slots: vec![TargetSelectionSlot {
                    legal_targets: vec![
                        TargetRef::Object(teammate),
                        TargetRef::Object(opponent),
                        TargetRef::Object(eliminated),
                    ],
                    optional: false,
                    chooser: None,
                }],
                mode_labels: Vec::new(),
                selection: TargetSelectionProgress::default(),
            },
            candidates: vec![
                candidate_for(teammate),
                candidate_for(opponent),
                candidate_for(eliminated),
            ],
        };
        let config = AiConfig::default();
        assert_eq!(policy_score(&state, &decision, teammate, &config), 0.0);
        assert_eq!(policy_score(&state, &decision, eliminated, &config), 0.0);
        assert!(policy_score(&state, &decision, opponent, &config) > 0.0);
    }

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
                    chooser: None,
                }],
                mode_labels: Vec::new(),
                selection: Default::default(),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::ChooseTarget {
                target: Some(TargetRef::Object(flyer)),
            },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Target),
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
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
                    chooser: None,
                }],
                mode_labels: Vec::new(),
                selection: Default::default(),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::ChooseTarget {
                target: Some(TargetRef::Object(ground_opp)),
            },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Target),
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };

        let score = EvasionRemovalPriorityPolicy.score(&ctx);
        assert!(
            score > 0.0,
            "Ground creature should get baseline removal target-quality score, got {score}"
        );
    }
}
