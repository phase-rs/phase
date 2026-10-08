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
use super::removal_lethality::{
    divided_damage_remaining_budget, divided_damage_verdict, DividedDamage,
};
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
        // #6582: threat value alone rewards the biggest body, so a damage spell
        // gets pointed at a creature it can't kill. Fold in whether the pending
        // damage is actually lethal (CR 704.5g) so a clean kill outranks a whiff.
        let lethality_bonus = super::removal_lethality::lethality_bonus(ctx, *target_id, target);

        target_quality_bonus + evasion_bonus + velocity_bonus + lethality_bonus
    }
}

/// CR 601.2d: is this candidate an opposing CREATURE that the divided-damage
/// pool remaining after the already-declared targets cannot kill?
///
/// Deliberately narrow. Player and planeswalker candidates are untouched — chip
/// damage to the face is a legitimate use of a leftover point (CR 120.3a /
/// CR 120.3c) — and so are non-creature and AI-controlled objects, the latter
/// being `anti_self_harm`'s business. Card-local: one map lookup plus the
/// divided-damage term's bounded scans, no board walk.
fn divided_damage_target_is_wasted(ctx: &PolicyContext<'_>) -> bool {
    let GameAction::ChooseTarget {
        target: Some(target_ref),
    } = &ctx.candidate.action
    else {
        return false;
    };
    // CR 601.2d: with the pool fully reserved there is no free point left. Any
    // further target — player and planeswalker included — must still be given
    // its mandatory one, and that one comes out of a reserve that was killing
    // something. Chip damage to the face is only legitimate while a point is
    // actually spare, so the non-creature exemption below applies to
    // `remaining >= 1` alone.
    if divided_damage_remaining_budget(ctx) == Some(0) {
        return true;
    }
    let TargetRef::Object(target_id) = target_ref else {
        return false;
    };
    let Some(target) = ctx.state.objects.get(target_id) else {
        return false;
    };
    // An AI-controlled body is `anti_self_harm`'s business, and a non-creature
    // recipient has no lethality to model (CR 120.3c).
    if !target.card_types.core_types.contains(&CoreType::Creature)
        || target.controller == ctx.ai_player
    {
        return false;
    }
    matches!(
        divided_damage_verdict(ctx, *target_id, target),
        Some(DividedDamage::CannotKill)
    )
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
///
/// Measurement mode (`cargo ai-gate`, duel suite) passes a non-expiring
/// projection deadline (`projection::projection_deadline`), so the simulation is
/// bounded by `STEP_CAP` rather than by host speed — a wall-clock bail here
/// scores `0.0` against a completed projection's up-to-`+3.0`, and this term
/// selects the removal target.
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
                let Ok(fresh) = session.get_or_project(
                    ctx.state,
                    ctx.ai_player,
                    target.controller,
                    horizon,
                    crate::projection::projection_deadline(ctx.config.execution_mode),
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
        // CR 601.2d: a divided-damage target the remaining pool cannot kill is
        // categorically wrong, not merely low-value — every chosen target must
        // receive at least one point, so adding it strictly shrinks what the
        // targets that CAN die are assigned. Computed before `score`, whose
        // early returns (impact threshold, threat value) would otherwise leave
        // the candidate at a plain 0.0 that another policy's positive term
        // outvotes. The registry maps `Reject` to -inf, so this is a bound.
        if divided_damage_target_is_wasted(ctx) {
            return PolicyVerdict::reject(PolicyReason::new("divided_damage_cannot_kill"));
        }
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
        AbilityDefinition, AbilityKind, Effect, EffectKind, PtValue, QuantityExpr, ResolvedAbility,
        TargetFilter, TargetRef, TypedFilter,
    };
    use engine::types::format::FormatConfig;
    use engine::types::game_state::{
        CastPaymentMode, CopyTargetSlot, GameState, PendingCast, TargetEffectDetail,
        TargetSelectionProgress, TargetSelectionSlot, WaitingFor,
    };
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::keywords::Keyword;
    use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
    use engine::types::phase::Phase;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;
    use rand::rngs::SmallRng;
    use rand::SeedableRng;
    use web_time::{Duration, Instant};

    const BEAST_WITHIN_ORACLE: &str =
        "Destroy target permanent. Its controller creates a 3/3 green Beast creature token.";
    const LIGHTNING_BOLT_ORACLE: &str = "Lightning Bolt deals 3 damage to any target.";

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

    /// The `PolicyContext` the scoring helpers below run under. Exposed
    /// separately so a test can also interrogate the context's own gates (e.g.
    /// `can_afford_projection`) on the exact shape production would see, rather
    /// than re-deriving the answer from config fields.
    fn policy_ctx<'a>(
        state: &'a GameState,
        decision: &'a AiDecisionContext,
        candidate: &'a CandidateAction,
        config: &'a AiConfig,
        context: &'a crate::context::AiContext,
    ) -> PolicyContext<'a> {
        PolicyContext {
            state,
            decision,
            candidate,
            ai_player: P0,
            config,
            context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        }
    }

    /// Score with a CALLER-OWNED `AiContext`, so the test retains a handle on
    /// `context.session` (to inspect `projection_cache`) and on
    /// `context.deadline` (to inject a pre-expired budget). Mirrors
    /// `policies::context::deadline_test_ctx`. `policy_score` is the
    /// don't-care-about-the-context wrapper.
    fn policy_score_in_context(
        state: &GameState,
        decision: &AiDecisionContext,
        target: ObjectId,
        config: &AiConfig,
        context: &crate::context::AiContext,
    ) -> f64 {
        let candidate = candidate_for(target);
        EvasionRemovalPriorityPolicy
            .score(&policy_ctx(state, decision, &candidate, config, context))
    }

    fn policy_score(
        state: &GameState,
        decision: &AiDecisionContext,
        target: ObjectId,
        config: &AiConfig,
    ) -> f64 {
        let ai_context = crate::context::AiContext::empty(&config.weights);
        policy_score_in_context(state, decision, target, config, &ai_context)
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

    /// #6582 end-to-end: a real 3-damage burn spell must be aimed at the body it
    /// can actually kill, not at the biggest threat on the board. This drives the
    /// PRODUCTION path — a cast that reaches `TargetSelection`, the registered
    /// `EvasionRemovalPriorityPolicy` verdict, and the full `score_candidates`
    /// ranking — so the lethality term is proven to survive registry wiring, not
    /// just to compute the right number in isolation.
    #[test]
    fn burn_prefers_the_killable_body_over_the_bigger_unkillable_threat() {
        let mut scenario = GameScenario::new_n_player(2, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let bolt = scenario
            .add_spell_to_hand_from_oracle(P0, "Lightning Bolt", true, LIGHTNING_BOLT_ORACLE)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            })
            .id();
        // The killable body is deliberately the LOWER threat, so threat value
        // alone (the pre-#6582 ranking) would pick the 7/7 the Bolt can't kill.
        let killable = scenario
            .add_creature(PlayerId(1), "Scrappy Skirmisher", 2, 2)
            .id();
        let unkillable = scenario
            .add_creature(PlayerId(1), "Looming Colossus", 7, 7)
            .id();
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])],
        );

        let mut runner = scenario.build();
        let card_id = runner.state().objects[&bolt].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: bolt,
                card_id,
                targets: Vec::new(),
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("the real Lightning Bolt fixture should reach target selection");

        let (pending_cast, target_slots) = match &runner.state().waiting_for {
            WaitingFor::TargetSelection {
                pending_cast,
                target_slots,
                ..
            } => (pending_cast, target_slots),
            other => panic!("expected Lightning Bolt target selection, got {other:?}"),
        };
        let effects = crate::policies::context::collect_ability_effects(&pending_cast.ability);
        assert!(
            effects.iter().any(|effect| matches!(
                effect,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 3 },
                    damage_source: None,
                    ..
                }
            )),
            "reach guard: Lightning Bolt must parse as 3 default-sourced damage"
        );
        assert!(
            effects
                .iter()
                .all(|effect| !matches!(effect, Effect::Unimplemented { .. })),
            "the regression fixture must not silently drop an unsupported clause"
        );
        assert!(target_slots[0]
            .legal_targets
            .contains(&TargetRef::Object(killable)));
        assert!(target_slots[0]
            .legal_targets
            .contains(&TargetRef::Object(unkillable)));

        let state = runner.state();
        let decision = build_decision_context(state);
        let config = create_config(AiDifficulty::VeryHard, Platform::Native).into_measurement(42);

        let killable_delta = registry_delta(state, &decision, killable, &config);
        let unkillable_delta = registry_delta(state, &decision, unkillable, &config);
        assert!(
            killable_delta > unkillable_delta,
            "the registered removal policy must prefer the body the Bolt kills: \
             killable 2/2={killable_delta}, unkillable 7/7={unkillable_delta}"
        );

        let scores = crate::search::score_candidates(state, P0, &config);
        assert!(
            full_score_for_target(&scores, killable) > full_score_for_target(&scores, unkillable),
            "the complete Very Hard scorer must carry the lethality preference through to the \
             final target ranking"
        );

        // The #6582 misplay itself: whatever else Very Hard does with a Bolt
        // ("any target" keeps the opponent's face on the table), it must not
        // burn it on a 7/7 that survives.
        let mut rng = SmallRng::seed_from_u64(42);
        assert_ne!(
            crate::choose_action(state, P0, &config, &mut rng),
            Some(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(unkillable)),
            }),
            "Very Hard must not spend a 3-damage burn spell on a 7/7 it cannot kill"
        );
    }

    /// T7 — the evasion production wiring is live under a measurement config:
    /// `velocity_score` reaches `get_or_project` and the projection is taken.
    ///
    /// Revert-failing: replacing the `get_or_project` call with a
    /// `cached_projection`-only lookup, or deleting the fresh-projection arm,
    /// leaves `projection_cache` empty and turns the positive arm red.
    ///
    /// The fixture is deliberately ALREADY AT `OpponentBeginCombat`, the horizon
    /// `velocity_score` hardcodes. That horizon is not reachable by priority
    /// passing at all: `auto_advance_once`'s `Phase::BeginCombat` arm opens a
    /// priority window only when a begin-combat trigger fires, and otherwise
    /// either advances to `DeclareAttackers` or, per CR 508.8, enters
    /// `PostCombatMain`. A "natural" fixture here would fail its reach guard
    /// essentially always.
    #[test]
    fn velocity_score_takes_projection_under_measurement_config() {
        let mut scenario = GameScenario::new_n_player(2, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let bolt = scenario
            .add_spell_to_hand_from_oracle(P0, "Lightning Bolt", true, LIGHTNING_BOLT_ORACLE)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            })
            .id();
        let killable = scenario
            .add_creature(PlayerId(1), "Scrappy Skirmisher", 2, 2)
            .id();
        scenario
            .add_creature(PlayerId(1), "Looming Colossus", 7, 7)
            .id();
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])],
        );

        let mut runner = scenario.build();
        let card_id = runner.state().objects[&bolt].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: bolt,
                card_id,
                targets: Vec::new(),
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("the real Lightning Bolt fixture should reach target selection");

        // `decision` is captured from the PRE-mutation state and carries the
        // TargetSelection pending cast, which is what
        // `EvasionRemovalPriorityPolicy::score`'s first gate and
        // `effect_classify::effect_source_id` both read. Capturing it AFTER the
        // mutation below can never work: the mutation sets `waiting_for` to
        // `Priority`, which IS the OpponentBeginCombat horizon predicate, and
        // `build_decision_context` copies `state.waiting_for` verbatim.
        let decision = build_decision_context(runner.state());
        assert!(
            matches!(&decision.waiting_for, WaitingFor::TargetSelection { .. }),
            "T7 fixture: `decision` must be captured BEFORE the horizon mutation"
        );

        // Mutate into the OpponentBeginCombat already-at-horizon shape: the
        // predicate is active_player == target_opponent && phase == BeginCombat
        // && stack empty && waiting_for == Priority { target_opponent }.
        let mut state = runner.state().clone();
        state.active_player = PlayerId(1);
        state.phase = Phase::BeginCombat;
        state.stack.clear();
        state.priority_player = PlayerId(1);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(1),
        };
        // The bolt leaves the STACK but must remain a live object, because
        // `effect_source_id` resolves the impact chain's source through it.
        assert!(state.objects.contains_key(&bolt));

        let config = create_config(AiDifficulty::Medium, Platform::Native).into_measurement(7);

        // Rung 1 — the policy's non-velocity gates still pass on the mutated
        // state, AND this is the control arm. `Deadline::after(0)` is expired, so
        // `can_afford_projection` returns false and `velocity_score` returns 0.0
        // before reaching `get_or_project`. A non-zero total score therefore
        // proves the other gates pass, while the empty cache proves the
        // projection did not run.
        let mut expired_ctx = crate::context::AiContext::empty(&config.weights);
        expired_ctx.deadline = engine::util::Deadline::after(0);
        let control = policy_score_in_context(&state, &decision, killable, &config, &expired_ctx);
        assert_ne!(
            control, 0.0,
            "T7 rung 1: EvasionRemovalPriorityPolicy::score must reach velocity_score on this \
             fixture — a 0.0 here means one of its gates rejected the mutated state, so the \
             POSITIVE arm below would be red for a fixture reason, not a wiring reason. Stop \
             and report; do not weaken the positive assertion."
        );
        assert!(
            expired_ctx
                .session
                .projection_cache
                .read()
                .unwrap()
                .is_empty(),
            "T7 control arm: with can_afford_projection() false, velocity_score must not project"
        );

        // Rung 2 — the projection itself succeeds on this state.
        crate::projection::projection_fixtures::assert_already_at_horizon(
            &state,
            P0,
            PlayerId(1),
            crate::projection::ProjectionHorizon::OpponentBeginCombat,
        );

        // Positive arm. `AiContext::empty` initializes `deadline` to
        // `Deadline::none()` — exactly what `PlannerServices::with_deadline`
        // installs in measurement mode — so this reproduces the production
        // deadline state rather than approximating it. Medium's
        // `projection_min_budget_ms = 2000` would block the projection in
        // interactive mode, so this arm also exercises the `is_none_or` bypass.
        let ai_ctx = crate::context::AiContext::empty(&config.weights);
        let _ = policy_score_in_context(&state, &decision, killable, &config, &ai_ctx);
        assert_eq!(
            ai_ctx.session.projection_cache.read().unwrap().len(),
            1,
            "velocity_score must take the fresh projection through get_or_project under a \
             measurement config (revert-failing: cached_projection-only leaves this empty)"
        );
    }

    /// T7b — the evasion production wiring passes an EXECUTION-MODE-DERIVED
    /// deadline to `get_or_project`, on a fixture where that argument's value
    /// decides the outcome.
    ///
    /// T7 above cannot see that argument at all, and adding an assertion there
    /// would not help: its fixture is deliberately already at
    /// `OpponentBeginCombat`, so `project_to` returns from the
    /// `Confidence::Exact` short-circuit BEFORE the loop's only
    /// `deadline.expired()` read. The deadline is structurally inert there, so
    /// every possible value of that argument leaves T7 green. This test is the
    /// traversing sibling; keep both, since T7 still guards the
    /// already-at-horizon path and the cache interaction that this fixture does
    /// not exercise.
    ///
    /// Both arms share one fixture and one target, and each is revert-failing
    /// for a different wrong argument:
    ///   * measurement arm — `projection_deadline` yields `Deadline::none()`,
    ///     the traversal completes and the projection is cached. Red for
    ///     `Deadline::after(0)` (bails at the loop head) and for the pre-change
    ///     hardcoded `Deadline::after(TIME_CAP_MS)` (this traversal costs
    ///     several times the 15 ms cap), both of which leave the cache empty.
    ///   * interactive arm — the SAME fixture under `ExecutionMode::Interactive`
    ///     must NOT complete: the 15 ms cap bails it and the cache stays empty.
    ///     Red for passing `ctx.context.deadline` instead, which is
    ///     `Deadline::none()` here — and in production is the planner's
    ///     whole-turn budget — so the projection would complete and cache one
    ///     entry. That substitution is invisible under measurement alone,
    ///     because measurement mode installs `Deadline::none()` on the context
    ///     too; only the interactive side can discriminate it.
    ///
    /// The interactive arm is therefore the one assertion here that compares a
    /// real elapsed traversal against a wall-clock cap. It is written as a
    /// negative (`is_empty`) so a drifted-faster traversal fails LOUDLY with a
    /// populated cache rather than passing vacuously, and the fixture's measured
    /// traversal is several times the cap in a debug build, which is the only
    /// build `cargo test` produces.
    #[test]
    fn velocity_score_projection_deadline_is_live_on_a_traversing_fixture() {
        let mut scenario = GameScenario::new_n_player(2, 42);
        scenario.at_phase(Phase::PreCombatMain);
        // The grower is both the reachability mechanism for the
        // `OpponentBeginCombat` horizon and the removal target under test: a
        // creature that grows before the opponent's combat is precisely what
        // `velocity_score` exists to prioritize.
        let grower = crate::projection::projection_fixtures::seed_opponent_begin_combat_horizon(
            &mut scenario,
            P0,
            PlayerId(1),
        );
        let bolt = scenario
            .add_spell_to_hand_from_oracle(P0, "Lightning Bolt", true, LIGHTNING_BOLT_ORACLE)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            })
            .id();
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])],
        );

        let mut runner = scenario.build();
        let card_id = runner.state().objects[&bolt].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: bolt,
                card_id,
                targets: Vec::new(),
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("the real Lightning Bolt fixture should reach target selection");

        // No horizon mutation here: unlike T7, this state is used exactly as the
        // engine produced it, so `decision` and `state` cannot disagree.
        let state = runner.state().clone();
        let decision = build_decision_context(&state);
        assert!(
            matches!(&decision.waiting_for, WaitingFor::TargetSelection { .. }),
            "T7b fixture: the policy's first gate needs a live TargetSelection"
        );

        // Guard 1 — the begin-combat trigger still parses. Without it the
        // horizon is unreachable from any state, and guard 2 would report a
        // confusing GameOverDuringProjection instead.
        crate::projection::projection_fixtures::assert_begin_combat_trigger_parsed(&state, grower);

        // Guard 2 — and the state genuinely TRAVERSES `project_to`'s loop to
        // the horizon `velocity_score` hardcodes. A `Confidence::Exact` result
        // here would mean the fixture had drifted into the short-circuit class,
        // which is what makes T7 blind to the deadline; both arms below would
        // then be vacuous.
        crate::projection::projection_fixtures::assert_traverses_to(
            &state,
            P0,
            PlayerId(1),
            crate::projection::ProjectionHorizon::OpponentBeginCombat,
        );

        let measurement = create_config(AiDifficulty::Medium, Platform::Native).into_measurement(7);

        // Guard 3 — fixture-drift guard for the interactive arm. That arm discriminates
        // by wall clock: it asserts the 15 ms cap BAILS this traversal, which is a claim
        // about deadline wiring only while the traversal costs materially more than the
        // cap. Should the fixture get cheaper (fewer filler cards in
        // `seed_opponent_begin_combat_horizon`, a cheaper `auto_advance_once` or
        // `project_to`) or the host get fast enough that it fits inside 15 ms, the arm
        // silently stops testing anything and its `is_empty` assertion fails as though
        // the wiring had broken. Fail here instead, naming the real cause.
        //
        // Timed on `get_or_project` DIRECTLY, with the same coordinates Guard 2 pinned
        // and `velocity_score` passes, under an unbounded deadline. Timing
        // `policy_score_in_context` instead would fold in `candidate_for` and the
        // policy's other gates, so a slow wrapper could satisfy this guard while the
        // projection — the only thing that actually reads the deadline — finished well
        // inside the cap. The probe uses its own session so it cannot warm either arm's
        // cache.
        //
        // MEASURED, and narrower than this test used to claim: the uncapped projection
        // costs ~29 ms on an M-series debug build, i.e. it clears the 15 ms cap by about
        // 1.9x — NOT the "several times" the measurement arm's message asserted before
        // this guard existed. That is the whole reason to time the projection rather
        // than the wrapper: the wrapper cleared 45 ms comfortably and hid how thin the
        // real margin is. The threshold sits just above the cap because the arm's actual
        // precondition is `uncapped > 15 ms` — anything less and it stops discriminating
        // entirely. A host ~2x faster than this one will trip this guard, which is the
        // intended outcome: it names the fixture, instead of the arm reporting a wiring
        // regression that never happened.
        let probe = crate::context::AiContext::empty(&measurement.weights);
        let uncapped_start = Instant::now();
        let uncapped = probe.session.get_or_project(
            &state,
            P0,
            PlayerId(1),
            crate::projection::ProjectionHorizon::OpponentBeginCombat,
            engine::util::Deadline::none(),
        );
        let uncapped_cost = uncapped_start.elapsed();
        assert!(
            uncapped.is_ok(),
            "reach-guard: the uncapped projection must complete, else this measures a bail \
             rather than the traversal cost"
        );
        assert!(
            uncapped_cost >= Duration::from_millis(20),
            "T7b interactive arm can no longer discriminate: the uncapped projection costs \
             {uncapped_cost:?}, which no longer clears the 15 ms interactive cap with any \
             margin (it measured ~29 ms when this guard was written). Re-seed the fixture so \
             the traversal is expensive again, or rewrite the arm to observe the deadline \
             `velocity_score` hands `get_or_project` directly. Do NOT lower this threshold \
             below the cap — under it the arm asserts nothing."
        );

        // Control — the policy's non-velocity gates pass on THIS fixture.
        // `Deadline::after(0)` is expired, so `can_afford_projection` is false
        // and `velocity_score` returns before `get_or_project`. A non-zero total
        // therefore proves the other gates accept the fixture, so a red positive
        // arm below is a wiring failure and not a fixture failure.
        let mut expired_ctx = crate::context::AiContext::empty(&measurement.weights);
        expired_ctx.deadline = engine::util::Deadline::after(0);
        let control =
            policy_score_in_context(&state, &decision, grower, &measurement, &expired_ctx);
        assert_ne!(
            control, 0.0,
            "T7b control: EvasionRemovalPriorityPolicy::score must reach velocity_score on this \
             fixture — a 0.0 means one of its gates rejected it. Stop and report; do not weaken \
             the arms below."
        );
        assert!(
            expired_ctx
                .session
                .projection_cache
                .read()
                .unwrap()
                .is_empty(),
            "T7b control: with can_afford_projection() false, velocity_score must not project"
        );

        // Measurement arm — kills `Deadline::after(0)` and `Deadline::after(15)`.
        let ai_ctx = crate::context::AiContext::empty(&measurement.weights);
        let _ = policy_score_in_context(&state, &decision, grower, &measurement, &ai_ctx);
        assert_eq!(
            ai_ctx.session.projection_cache.read().unwrap().len(),
            1,
            "under a measurement config the projection deadline must not expire, so this \
             traversal completes and caches (revert-failing: any finite budget passed here bails \
             a traversal Guard 3 measured at ~29 ms against the 15 ms interactive cap)"
        );

        // Interactive arm — kills `ctx.context.deadline`.
        let interactive = create_config(AiDifficulty::Medium, Platform::Native);
        assert!(
            !interactive.execution_mode.is_measurement(),
            "T7b interactive arm: create_config must default to ExecutionMode::Interactive"
        );
        let interactive_ctx = crate::context::AiContext::empty(&interactive.weights);
        // Non-vacuity guard: the arm is only meaningful if the policy actually
        // REACHES `get_or_project` in this context. `AiContext::empty` carries
        // `Deadline::none()`, so `can_afford_projection`'s `is_none_or` floor
        // bypass applies despite Medium's 2000 ms `projection_min_budget_ms`.
        // Were that false, the cache would be empty because nothing projected,
        // and the assertion below would pass no matter what deadline the
        // production line passes.
        let candidate = candidate_for(grower);
        assert!(
            policy_ctx(
                &state,
                &decision,
                &candidate,
                &interactive,
                &interactive_ctx
            )
            .can_afford_projection(),
            "T7b interactive arm would be vacuous: velocity_score never reaches get_or_project \
             because can_afford_projection() is false"
        );
        let _ = policy_score_in_context(&state, &decision, grower, &interactive, &interactive_ctx);
        assert!(
            interactive_ctx
                .session
                .projection_cache
                .read()
                .unwrap()
                .is_empty(),
            "under an interactive config the 15 ms projection cap must bail this traversal, so \
             nothing is cached (revert-failing: passing ctx.context.deadline here hands the \
             projection the caller's whole-turn budget and it completes)"
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
        state.pending_trigger = Some(Box::new(engine::game::triggers::PendingTrigger {
            source_id: trigger_source,
            controller: P0,
            condition: None,
            ability: Box::new(ResolvedAbility::new(
                Effect::Destroy {
                    target: TargetFilter::Any,
                    cant_regenerate: false,
                },
                Vec::new(),
                trigger_source,
                P0,
            )),
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
            provenance: None,
        }));
        let config = AiConfig::default();
        let slot = TargetSelectionSlot {
            legal_targets: vec![TargetRef::Object(low), TargetRef::Object(high)],
            optional: false,
            chooser: None,
            effect_kind: EffectKind::NoOp,
            effect_detail: TargetEffectDetail::None,
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
                    effect_kind: EffectKind::NoOp,
                    effect_detail: TargetEffectDetail::None,
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
                    effect_kind: EffectKind::NoOp,
                    effect_detail: TargetEffectDetail::None,
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
                    effect_kind: EffectKind::NoOp,
                    effect_detail: TargetEffectDetail::None,
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
