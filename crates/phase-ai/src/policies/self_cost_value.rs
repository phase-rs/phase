//! Net-value gate policy for self-cost ability activations.
//!
//! Thin adapter over `self_cost.rs`: fetches the activated ability, confirms its
//! cost spends a self-resource (sacrifice / pay-life / discard / self-exile),
//! stands down when off-ability deck synergy justifies the cost, then prices the
//! cost against the ability's immediate payoff. A real cost with a trivial
//! payoff is rejected (scoring `-inf`, so Pass wins); a cheap cost is merely
//! deprioritized; a payoff that can be confidently and completely priced is
//! compared against the cost — and a payoff **certified smaller than the cost is
//! rejected**, not discounted — and a real payoff that cannot be soundly priced
//! (unpriceable effect, or an unmodeled rider in the chain) is left alone.
//!
//! This policy is the **single authority** for the cost-vs-benefit question on
//! every self-cost activation, mana-costed sacrifice outlets included.
//!
//! # Why the underwater arm is categorical and not graduated
//!
//! An earlier revision scored the underwater case in proportion to the
//! shortfall, on the argument that search should stay able to override it. That
//! was **falsified by measurement** (see `policies::tests::sac_outlet_drain_repro`
//! for the executed record). The final decision is a softmax *sample*
//! (`search::softmax_select_pairs`) at Medium `temperature = 1.0`, repeated over
//! ~100 priority windows per game: repricing a repeatable outlet from `+0.85` to
//! `-0.65` cut the per-window activation probability 63.9% → 28.3%, a real 2.3×
//! improvement, and the board drained **identically**, because P(at least one
//! selection over that many windows) ≈ 1.0 either way.
//!
//! The general law, worth carrying to any policy verdict on a *repeatable*
//! candidate: **a graduated penalty is a rate, a `Reject` is a bound, and a rate
//! cannot enforce a bound over unbounded trials.** Only `-inf` is categorical —
//! its softmax weight is `exp(-inf) = 0`. So a graduated "discouragement" on a
//! trade this policy has certified as a loss means "do it eventually", which is
//! the opposite of what the certification says.

use engine::types::ability::AbilityTag;
use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use super::self_cost::{
    appraise_benefit, real_self_cost, self_cost_in_scope, self_counter_cost_preview,
    synergy_justifies_self_cost, BenefitAppraisal, SelfCounterCostPreview,
};
use crate::features::DeckFeatures;

/// At or above this priced self-cost, a trivial-benefit activation is a real
/// loss and is rejected. Below it, a trivial-benefit activation is only
/// deprioritized (never hard-rejected) — but it is still never treated as a
/// benefit-present play.
const REAL_COST_FLOOR: f64 = 1.0;

pub struct SelfCostValuePolicy;

impl TacticalPolicy for SelfCostValuePolicy {
    fn id(&self) -> PolicyId {
        PolicyId::SelfCostValue
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::ActivateAbility]
    }

    fn activation(
        &self,
        _features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        // activation-constant: cost-axis backstop for every activated-ability candidate; scope gating happens in `verdict`.
        Some(1.0)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let GameAction::ActivateAbility {
            source_id,
            ability_index: _,
        } = &ctx.candidate.action
        else {
            return PolicyVerdict::neutral(PolicyReason::new("self_cost_value_na"));
        };

        let Some(ability) = ctx.effective_activated_ability() else {
            return PolicyVerdict::neutral(PolicyReason::new("self_cost_value_na"));
        };

        if let Some(verdict) = counter_replenishment_verdict(
            self_counter_cost_preview(ctx.state, ctx.ai_player, *source_id, &ability),
            ctx.penalties(),
        ) {
            return verdict;
        }

        if ability.ability_tag == Some(AbilityTag::Cycling) {
            return PolicyVerdict::neutral(PolicyReason::new("self_cost_cycling_deferred"));
        }

        let Some(cost) = ability.cost.as_ref() else {
            return PolicyVerdict::neutral(PolicyReason::new("self_cost_value_na"));
        };

        if !self_cost_in_scope(cost) {
            return PolicyVerdict::neutral(PolicyReason::new("self_cost_value_na"));
        }

        let features = ctx
            .context
            .session
            .features
            .get(&ctx.ai_player)
            .cloned()
            .unwrap_or_default();

        if synergy_justifies_self_cost(&features, ctx.state, ctx.ai_player, &ability) {
            return PolicyVerdict::neutral(PolicyReason::new("self_cost_synergy_justified"));
        }

        let cost_value =
            real_self_cost(ctx.state, ctx.ai_player, *source_id, cost, ctx.penalties());

        let cost_milli = (cost_value * 1000.0) as i64;

        match appraise_benefit(
            ctx.state,
            ctx.ai_player,
            *source_id,
            &ability,
            ctx.penalties(),
        ) {
            BenefitAppraisal::Trivial => {
                if cost_value >= REAL_COST_FLOOR {
                    return PolicyVerdict::reject(
                        PolicyReason::new("self_cost_trivial_benefit")
                            .with_fact("cost_milli", cost_milli)
                            .with_fact("benefit", 0),
                    );
                }
                // Trivial payoff, but the priced self-cost is below the
                // real-loss floor: deprioritize with an auto-banded negative
                // delta. No trivial self-cost play may resolve to
                // `self_cost_benefit_present`, and the `self_cost_marginal`
                // reason deliberately does NOT claim a benefit.
                PolicyVerdict::score(
                    -cost_value,
                    PolicyReason::new("self_cost_marginal").with_fact("cost_milli", cost_milli),
                )
            }
            BenefitAppraisal::Priced { value } => {
                let net = value - cost_value;
                let benefit_milli = (value * 1000.0) as i64;
                if net >= 0.0 {
                    // Inclusive boundary: net == 0 covers. A 0/1 creature token
                    // prices at `max(creature_combat_value(0,1) = 1.0, 0.5) =
                    // 1.0` against draw(1) = 1.0 — exactly 0. Cracking it is
                    // intended; the comparison means "not a loss", and an
                    // exact-cover crack is allowed. Neutral rather than
                    // positive: `CardAdvantagePolicy`/`DrawPayoffPolicy` already
                    // reward the draw on this same candidate, so a positive
                    // delta here would double-count.
                    //
                    // NOTE: tap state is deliberately NOT part of this boundary.
                    // `sacrifice_cost` prices the permanent intrinsically, so a
                    // tapped 1/1 token still costs 2.5 and stays underwater. The
                    // earlier tapped-discounted reading put it at exactly 0 here
                    // and made this arm the escape hatch a whole board drained
                    // through. Fixed at the give-up authority, not at this
                    // boundary.
                    PolicyVerdict::neutral(
                        PolicyReason::new("self_cost_benefit_covers_cost")
                            .with_fact("cost_milli", cost_milli)
                            .with_fact("benefit_milli", benefit_milli),
                    )
                } else {
                    // net < 0: a CERTIFIED losing trade — the chain was fully
                    // priced, the quantity read, the cost bound
                    // filter-faithfully, and every modeled justification
                    // (synergy payoff on board, life pressure, unpriceable or
                    // unmodeled value, counter replenishment, cycling, cEDH
                    // bracket) already declined to stand the comparison down.
                    //
                    // Categorical, not graduated: under repeated softmax
                    // sampling any finite negative is eventually selected while
                    // fodder remains (measured — see the module docs and
                    // `policies::tests::sac_outlet_drain_repro`), so a graduated
                    // "discouragement" here means "do it eventually", the
                    // opposite of what this verdict certifies.
                    //
                    // There is no threshold constant in the restraint: the
                    // boundary is the sign of the net, inclusive at zero. The
                    // magnitudes that set WHERE that sign flips
                    // (`creature_combat_value`'s 1.5*P + T, `SINGLE_CARD_VALUE`,
                    // `sacrifice_token_cost`, the per-life coefficient) live in
                    // eval, which is where calibration belongs.
                    PolicyVerdict::reject(
                        PolicyReason::new("self_cost_benefit_underwater")
                            .with_fact("cost_milli", cost_milli)
                            .with_fact("benefit_milli", benefit_milli),
                    )
                }
            }
            BenefitAppraisal::Unpriced => {
                PolicyVerdict::neutral(PolicyReason::new("self_cost_benefit_present"))
            }
        }
    }
}

/// Conservatively prices the exact self-counter-replenishment preview.
///
/// CR 614.1: A replacement can prevent the counter event or redirect it to an
/// unsupported event class. Either outcome means this policy cannot assume the
/// activation repays its counter cost, so both receive the bounded penalty.
/// Applied, choice-required, and transformed outcomes remain neutral because
/// they do not establish that replenishment has failed.
fn counter_replenishment_verdict(
    preview: Option<SelfCounterCostPreview>,
    penalties: &crate::config::PolicyPenalties,
) -> Option<PolicyVerdict> {
    let reason = match preview {
        Some(SelfCounterCostPreview::Prevented) => "self_cost_counter_replacement_prevented",
        Some(SelfCounterCostPreview::Unsupported) => "self_cost_counter_replacement_unsupported",
        Some(
            SelfCounterCostPreview::Applied
            | SelfCounterCostPreview::ChoiceRequired
            | SelfCounterCostPreview::Transformed,
        )
        | None => return None,
    };
    Some(PolicyVerdict::strong(
        -penalties.self_cost_counter_replacement_prevented_penalty,
        PolicyReason::new(reason),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiConfig;
    use crate::context::AiContext;
    use crate::features::aristocrats::AristocratsFeature;
    use crate::features::landfall::LandfallFeature;
    use crate::features::lifegain::LifegainFeature;
    use crate::features::reanimator::ReanimatorFeature;
    use crate::features::DeckFeatures;
    use crate::session::AiSession;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::bracket_estimate::CommanderBracketTier;
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, ContinuousModification, ControllerRef, Effect,
        ManaContribution, ManaProduction, ObjectScope, QuantityExpr, QuantityModification,
        QuantityRef, ReplacementDefinition, SacrificeCost, StaticDefinition, TargetFilter,
        TypeFilter, TypedFilter,
    };
    use engine::types::card_type::CoreType;
    use engine::types::counter::{CounterMatch, CounterType};
    use engine::types::game_state::{GameState, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::keywords::{Keyword, KeywordKind};
    use engine::types::player::PlayerId;
    use engine::types::replacements::ReplacementEvent;
    use engine::types::zones::Zone;
    use std::sync::Arc;

    const AI: PlayerId = PlayerId(0);
    const OPP: PlayerId = PlayerId(1);

    // --- fixture builders -------------------------------------------------

    fn activated(effect: Effect, cost: AbilityCost) -> AbilityDefinition {
        let mut ability = AbilityDefinition::new(AbilityKind::Activated, effect);
        ability.cost = Some(cost);
        ability
    }

    fn sac_creature_cost() -> AbilityCost {
        AbilityCost::Sacrifice(SacrificeCost::count(
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            1,
        ))
    }

    fn sac_land_cost() -> AbilityCost {
        AbilityCost::Sacrifice(SacrificeCost::count(
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Land)),
            1,
        ))
    }

    fn gain_life(amount: i32) -> Effect {
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: amount },
            player: TargetFilter::Controller,
        }
    }

    fn draw(count: i32) -> Effect {
        Effect::Draw {
            count: QuantityExpr::Fixed { value: count },
            target: TargetFilter::Controller,
        }
    }

    fn deal_fixed(value: i32) -> Effect {
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        }
    }

    fn deal_dynamic() -> Effect {
        // Fling shape: damage equal to a creature's power (non-Fixed quantity).
        Effect::DealDamage {
            amount: QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::Source,
                },
            },
            target: TargetFilter::Player,
            damage_source: None,
            excess: None,
        }
    }

    fn add_two_colorless() -> Effect {
        Effect::Mana {
            produced: ManaProduction::Fixed {
                colors: Vec::new(),
                contribution: ManaContribution::Base,
            },
            restrictions: Vec::new(),
            grants: Vec::new(),
            expiry: None,
            target: None,
        }
    }

    fn search_for_land() -> Effect {
        Effect::SearchLibrary {
            source_zones: vec![Zone::Library],
            filter: TargetFilter::Typed(TypedFilter::new(TypeFilter::Land)),
            count: QuantityExpr::Fixed { value: 1 },
            reveal: false,
            target_player: None,
            selection_constraint: engine::types::ability::SearchSelectionConstraint::None,
            split: None,
        }
    }

    fn shroud_self_grant() -> Effect {
        Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .modifications(vec![ContinuousModification::AddKeyword {
                    keyword: Keyword::Shroud,
                }])],
            target: Some(TargetFilter::SelfRef),
            duration: None,
            end_cost: None,
        }
    }

    fn put_counter(counter: CounterType, target: TargetFilter) -> Effect {
        Effect::PutCounter {
            counter_type: counter,
            count: QuantityExpr::Fixed { value: 1 },
            target,
        }
    }

    fn self_counter_replenisher() -> AbilityDefinition {
        activated(
            put_counter(CounterType::Plus1Plus1, TargetFilter::SelfRef),
            AbilityCost::RemoveCounter {
                count: 1,
                counter_type: CounterMatch::OfType(CounterType::Plus1Plus1),
                target: None,
                selection: Default::default(),
            },
        )
    }

    fn install_counter_replacement(state: &mut GameState, modification: QuantityModification) {
        let replacement = create_object(
            state,
            CardId(next_id()),
            AI,
            "Counter replacement".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&replacement)
            .expect("replacement exists")
            .replacement_definitions
            .push(
                ReplacementDefinition::new(ReplacementEvent::AddCounter)
                    .quantity_modification(modification),
            );
    }

    // --- state / context helpers -----------------------------------------

    fn creature(
        state: &mut GameState,
        controller: PlayerId,
        name: &str,
        p: i32,
        t: i32,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(next_id()),
            controller,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(p);
        obj.toughness = Some(t);
        id
    }

    fn token_creature(state: &mut GameState, name: &str, p: i32, t: i32) -> ObjectId {
        let id = creature(state, AI, name, p, t);
        state.objects.get_mut(&id).unwrap().is_token = true;
        id
    }

    fn artifact_token(state: &mut GameState, name: &str) -> ObjectId {
        let id = create_object(
            state,
            CardId(next_id()),
            AI,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.is_token = true;
        id
    }

    fn sac_artifact_cost() -> AbilityCost {
        AbilityCost::Sacrifice(SacrificeCost::count(
            TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Artifact).controller(ControllerRef::You),
            ),
            1,
        ))
    }

    fn put_counter_all(counter: CounterType, target: TargetFilter) -> Effect {
        Effect::PutCounterAll {
            counter_type: counter,
            count: QuantityExpr::Fixed { value: 1 },
            target,
        }
    }

    fn land(state: &mut GameState, name: &str) -> ObjectId {
        let id = create_object(
            state,
            CardId(next_id()),
            AI,
            name.to_string(),
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

    fn source_with(
        state: &mut GameState,
        name: &str,
        core: &[CoreType],
        ability: AbilityDefinition,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(next_id()),
            AI,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        for &ct in core {
            obj.card_types.core_types.push(ct);
        }
        Arc::make_mut(&mut obj.abilities).push(ability);
        id
    }

    fn next_id() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(1000);
        COUNTER.fetch_add(1, Ordering::Relaxed)
    }

    fn features_with(
        landfall: f32,
        lifegain: f32,
        reanimator: f32,
        death_triggers: Vec<String>,
        bracket: CommanderBracketTier,
    ) -> DeckFeatures {
        DeckFeatures {
            landfall: LandfallFeature {
                commitment: landfall,
                ..Default::default()
            },
            lifegain: LifegainFeature {
                commitment: lifegain,
                ..Default::default()
            },
            reanimator: ReanimatorFeature {
                commitment: reanimator,
                ..Default::default()
            },
            aristocrats: AristocratsFeature {
                death_trigger_count: death_triggers.len() as u32,
                death_trigger_names: death_triggers,
                ..Default::default()
            },
            bracket_tier: bracket,
            ..DeckFeatures::default()
        }
    }

    fn plain_features() -> DeckFeatures {
        features_with(0.0, 0.0, 0.0, Vec::new(), CommanderBracketTier::Core)
    }

    fn verdict_for(
        state: &GameState,
        source_id: ObjectId,
        features: DeckFeatures,
    ) -> PolicyVerdict {
        let candidate = CandidateAction {
            action: GameAction::ActivateAbility {
                source_id,
                ability_index: 0,
            },
            metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Ability),
        };
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority { player: AI },
            candidates: Vec::new(),
        };
        let config = AiConfig::default();
        let mut session = AiSession::empty();
        session.features.insert(AI, features);
        let mut context = AiContext::empty(&config.weights);
        context.session = Arc::new(session);
        context.player = AI;
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        SelfCostValuePolicy.verdict(&ctx)
    }

    fn assert_reject(verdict: &PolicyVerdict, kind: &str) {
        match verdict {
            PolicyVerdict::Reject { reason } => assert_eq!(reason.kind, kind, "reject kind"),
            PolicyVerdict::Score { delta, reason } => {
                panic!(
                    "expected reject {kind}, got Score {{ delta: {delta}, kind: {} }}",
                    reason.kind
                )
            }
        }
    }

    /// Pins the arithmetic of a verdict that carries no delta. A `Reject`
    /// propagates to `-inf`, so the comparison it certifies is only observable
    /// through `PolicyReason::facts` — these are `(value * 1000.0) as i64`, so
    /// the expectations are exact, not epsilon'd.
    fn assert_facts(verdict: &PolicyVerdict, cost_milli: i64, benefit_milli: i64) {
        let reason = match verdict {
            PolicyVerdict::Reject { reason } | PolicyVerdict::Score { reason, .. } => reason,
        };
        assert_eq!(
            reason.facts,
            vec![("cost_milli", cost_milli), ("benefit_milli", benefit_milli)],
            "verdict facts"
        );
    }

    fn assert_neutral(verdict: &PolicyVerdict, kind: &str) {
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, kind, "neutral kind");
                assert_eq!(*delta, 0.0, "neutral delta");
            }
            PolicyVerdict::Reject { reason } => {
                panic!("expected neutral {kind}, got Reject {}", reason.kind)
            }
        }
    }

    fn assert_not_reject(verdict: &PolicyVerdict) {
        assert!(
            matches!(verdict, PolicyVerdict::Score { .. }),
            "expected a Score (not a hard veto)"
        );
    }

    // --- Row 1: sac-creature trivial lifegain rejected --------------------

    #[test]
    fn sac_creature_for_small_lifegain_rejected() {
        let mut state = GameState::new_two_player(42);
        creature(&mut state, AI, "Bear", 2, 2);
        let source = source_with(
            &mut state,
            "High Market",
            &[CoreType::Land],
            activated(gain_life(1), sac_creature_cost()),
        );
        assert_reject(
            &verdict_for(&state, source, plain_features()),
            "self_cost_trivial_benefit",
        );
    }

    #[test]
    fn sac_creature_for_draw_is_vetoed_underwater() {
        // Row 1's positive reach-guard, now pinning the VETO contract:
        // identical cost to `sac_creature_for_small_lifegain_rejected`, real
        // payoff (a card), so the input passed `self_cost_in_scope` and the
        // chain walk classified the draw NON-trivial — reaching `underwater` at
        // all proves both, which is the reach-guard this test carries.
        //
        // The comparison: the Bear prices at `evaluate_creature_intrinsic(2,2)`
        // = 1.5*2+2 = 5.0 against `draw(1)` = 1.0 → net -4.0, certified losing.
        //
        // HISTORY: before the veto this arm emitted a graduated `Score` with
        // delta -4.0. The graduated shape was falsified by measurement (module
        // docs) — a rate cannot bound a repeatable candidate. Reverting to a
        // graduated score flips this to `Score` and the test goes red on shape;
        // reverting the pricing entirely flips the kind to
        // `self_cost_benefit_present` and it goes red on kind.
        let mut state = GameState::new_two_player(42);
        creature(&mut state, AI, "Bear", 2, 2);
        let source = source_with(
            &mut state,
            "High Market",
            &[CoreType::Land],
            activated(draw(1), sac_creature_cost()),
        );
        let verdict = verdict_for(&state, source, plain_features());
        assert_reject(&verdict, "self_cost_benefit_underwater");
        assert_facts(&verdict, 5000, 1000);
    }

    // --- Row 2: Fling-class dynamic damage NOT rejected -------------------

    #[test]
    fn dynamic_power_damage_not_rejected() {
        let mut state = GameState::new_two_player(42);
        state.players[OPP.0 as usize].life = 12;
        creature(&mut state, AI, "Bear", 2, 2);
        let source = source_with(
            &mut state,
            "Fling-like",
            &[CoreType::Artifact],
            activated(deal_dynamic(), sac_creature_cost()),
        );
        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_present",
        );
    }

    #[test]
    fn fixed_one_face_ping_rejected() {
        // Hostile boundary for row 2: same sac cost, a fixed 1 to face with no
        // kill is trivial → reject.
        let mut state = GameState::new_two_player(42);
        state.players[OPP.0 as usize].life = 12;
        creature(&mut state, AI, "Bear", 2, 2);
        let source = source_with(
            &mut state,
            "Pinger",
            &[CoreType::Artifact],
            activated(deal_fixed(1), sac_creature_cost()),
        );
        assert_reject(
            &verdict_for(&state, source, plain_features()),
            "self_cost_trivial_benefit",
        );
    }

    // --- Row 3: burn above the ceiling NOT rejected, boundary at 2 --------

    #[test]
    fn fixed_three_face_damage_not_rejected() {
        let mut state = GameState::new_two_player(42);
        state.players[OPP.0 as usize].life = 20;
        creature(&mut state, AI, "Bear", 2, 2);
        let source = source_with(
            &mut state,
            "Burn",
            &[CoreType::Artifact],
            activated(deal_fixed(3), sac_creature_cost()),
        );
        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_present",
        );
    }

    #[test]
    fn fixed_two_face_damage_no_kill_rejected() {
        let mut state = GameState::new_two_player(42);
        state.players[OPP.0 as usize].life = 20;
        creature(&mut state, AI, "Bear", 2, 2);
        let source = source_with(
            &mut state,
            "Weak Burn",
            &[CoreType::Artifact],
            activated(deal_fixed(2), sac_creature_cost()),
        );
        assert_reject(
            &verdict_for(&state, source, plain_features()),
            "self_cost_trivial_benefit",
        );
    }

    // --- Row 4 / 4b: Zuran Orb rejected, land-search allowed --------------

    #[test]
    fn zuran_orb_land_sac_lifegain_rejected() {
        let mut state = GameState::new_two_player(42);
        land(&mut state, "Forest");
        let source = source_with(
            &mut state,
            "Zuran Orb",
            &[CoreType::Artifact],
            activated(gain_life(2), sac_land_cost()),
        );
        assert_reject(
            &verdict_for(&state, source, plain_features()),
            "self_cost_trivial_benefit",
        );
    }

    #[test]
    fn zuran_orb_still_rejected_in_landfall_deck() {
        // NEW-1 regression guard: landfall commitment above the synergy floor
        // must NOT stand Zuran Orb down — landfall triggers on a land entering,
        // never on one being sacrificed.
        let mut state = GameState::new_two_player(42);
        land(&mut state, "Forest");
        let source = source_with(
            &mut state,
            "Zuran Orb",
            &[CoreType::Artifact],
            activated(gain_life(2), sac_land_cost()),
        );
        let features = features_with(0.9, 0.0, 0.0, Vec::new(), CommanderBracketTier::Core);
        assert_reject(
            &verdict_for(&state, source, features),
            "self_cost_trivial_benefit",
        );
    }

    #[test]
    fn land_sac_search_for_land_allowed_even_in_landfall_deck() {
        // Reach-guard for 4b: a real "sacrifice a land: search a land" ramp line
        // reaches scoring (in-scope land sacrifice) and is allowed via the
        // SearchLibrary-for-land arm, NOT a synergy stand-down.
        let mut state = GameState::new_two_player(42);
        land(&mut state, "Forest");
        let source = source_with(
            &mut state,
            "Ramp Land",
            &[CoreType::Land],
            activated(search_for_land(), sac_land_cost()),
        );
        let features = features_with(0.9, 0.0, 0.0, Vec::new(), CommanderBracketTier::Core);
        assert_neutral(
            &verdict_for(&state, source, features),
            "self_cost_benefit_present",
        );
    }

    // --- Row 5: Ashnod's Altar (mana) allowed, cEDH stand-down ------------

    #[test]
    fn sac_for_mana_not_rejected() {
        let mut state = GameState::new_two_player(42);
        creature(&mut state, AI, "Bear", 2, 2);
        let source = source_with(
            &mut state,
            "Ashnod's Altar",
            &[CoreType::Artifact],
            activated(add_two_colorless(), sac_creature_cost()),
        );
        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_present",
        );
    }

    #[test]
    fn cedh_bracket_stands_down_self_cost() {
        // Same trivial sac-for-lifegain that rejects in a Core deck is stood
        // down at the Cedh bracket.
        let mut state = GameState::new_two_player(42);
        creature(&mut state, AI, "Bear", 2, 2);
        let source = source_with(
            &mut state,
            "High Market",
            &[CoreType::Land],
            activated(gain_life(1), sac_creature_cost()),
        );
        let features = features_with(0.0, 0.0, 0.0, Vec::new(), CommanderBracketTier::Cedh);
        assert_neutral(
            &verdict_for(&state, source, features),
            "self_cost_synergy_justified",
        );
    }

    // --- Row 6: discard-to-grant no-threat rejected -----------------------

    #[test]
    fn discard_for_self_protection_no_threat_rejected() {
        let mut state = GameState::new_two_player(42);
        state.active_player = AI;
        // Give the AI a spare card so the discard cost is meaningful context.
        create_object(
            &mut state,
            CardId(next_id()),
            AI,
            "Filler".to_string(),
            Zone::Hand,
        );
        let cost = AbilityCost::Discard {
            count: QuantityExpr::Fixed { value: 1 },
            filter: None,
            selection: Default::default(),
            self_scope: Default::default(),
        };
        let source = source_with(
            &mut state,
            "Loopy Creature",
            &[CoreType::Creature],
            activated(shroud_self_grant(), cost),
        );
        assert_reject(
            &verdict_for(&state, source, plain_features()),
            "self_cost_trivial_benefit",
        );
    }

    #[test]
    fn discard_stands_down_in_reanimator_deck() {
        let mut state = GameState::new_two_player(42);
        state.active_player = AI;
        let cost = AbilityCost::Discard {
            count: QuantityExpr::Fixed { value: 1 },
            filter: None,
            selection: Default::default(),
            self_scope: Default::default(),
        };
        let source = source_with(
            &mut state,
            "Loopy Creature",
            &[CoreType::Creature],
            activated(shroud_self_grant(), cost),
        );
        let features = features_with(0.0, 0.0, 0.9, Vec::new(), CommanderBracketTier::Core);
        assert_neutral(
            &verdict_for(&state, source, features),
            "self_cost_synergy_justified",
        );
    }

    // --- Row 7: self-exile-graveyard priced cheap (marginal, not reject) --

    #[test]
    fn self_exile_graveyard_single_card_is_marginal_not_rejected() {
        // DEVIATION from matrix row 7 ("reject"): the plan prices graveyard
        // exile at 0.15/card, well below the 0.5 marginal floor, so a single
        // self-exile is deprioritized, never hard-vetoed. Multi-card exiles
        // (>=7 cards) would clear the reject floor.
        let mut state = GameState::new_two_player(42);
        let cost = AbilityCost::Exile {
            count: 1,
            zone: Some(Zone::Graveyard),
            filter: Some(TargetFilter::SelfRef),
        };
        let source = source_with(
            &mut state,
            "Psychic Frog",
            &[CoreType::Creature],
            activated(shroud_self_grant(), cost),
        );
        let verdict = verdict_for(&state, source, plain_features());
        assert_not_reject(&verdict);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "self_cost_marginal");
                assert!(delta < 0.0, "expected a deprioritizing nudge, got {delta}");
            }
            PolicyVerdict::Reject { .. } => unreachable!(),
        }
    }

    #[test]
    fn self_exile_hand_is_in_scope_and_priced_as_discard() {
        // Exile{Hand} is priced as a discard (1.0/card), so a trivial-benefit
        // hand-exile clears the reject floor — proves Exile{Hand} reaches scoring.
        let mut state = GameState::new_two_player(42);
        let cost = AbilityCost::Exile {
            count: 1,
            zone: Some(Zone::Hand),
            filter: None,
        };
        let source = source_with(
            &mut state,
            "Hand Exiler",
            &[CoreType::Creature],
            activated(shroud_self_grant(), cost),
        );
        assert_reject(
            &verdict_for(&state, source, plain_features()),
            "self_cost_trivial_benefit",
        );
    }

    // --- Row 8: ExilesCards siblings never fire the gate ------------------

    #[test]
    fn exile_cost_siblings_out_of_scope() {
        // CollectEvidence / ExileWithAggregate / Behold are structurally
        // distinct from a self-resource exile — the gate must not fire.
        assert!(!self_cost_in_scope(&AbilityCost::CollectEvidence {
            amount: 3
        }));
        assert!(!self_cost_in_scope(&AbilityCost::Exile {
            count: 1,
            zone: Some(Zone::Library),
            filter: None,
        }));
        assert!(!self_cost_in_scope(&AbilityCost::Exile {
            count: 1,
            zone: None,
            filter: None,
        }));
        // A Composite of only out-of-scope costs stays out of scope.
        assert!(!self_cost_in_scope(&AbilityCost::Composite {
            costs: vec![AbilityCost::Tap, AbilityCost::CollectEvidence { amount: 2 },],
        }));
        // Selective, not blanket: a graveyard/hand exile IS in scope.
        assert!(self_cost_in_scope(&AbilityCost::Exile {
            count: 1,
            zone: Some(Zone::Graveyard),
            filter: None,
        }));
    }

    #[test]
    fn collect_evidence_cost_yields_na() {
        let mut state = GameState::new_two_player(42);
        let source = source_with(
            &mut state,
            "Evidence Card",
            &[CoreType::Creature],
            activated(gain_life(1), AbilityCost::CollectEvidence { amount: 3 }),
        );
        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_value_na",
        );
    }

    // --- Row 9: Tyrite-Sanctum-class beneficial counter allowed (M2) ------

    #[test]
    fn beneficial_indestructible_counter_not_rejected() {
        // M2: real card Tyrite Sanctum parses this as PutCounter{Keyword(
        // Indestructible)} on a target God — a beneficial counter, non-trivial.
        let mut state = GameState::new_two_player(42);
        let effect = put_counter(
            CounterType::Keyword(KeywordKind::Indestructible),
            TargetFilter::Typed(TypedFilter::default()),
        );
        let cost = AbilityCost::Composite {
            costs: vec![AbilityCost::Tap, sac_land_cost()],
        };
        land(&mut state, "Forest");
        let source = source_with(
            &mut state,
            "Tyrite Sanctum",
            &[CoreType::Land],
            activated(effect, cost),
        );
        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_present",
        );
    }

    // --- Row 10: Carrion Feeder fizzle rejected, multi-authority guards ---

    #[test]
    fn self_counter_fizzles_when_source_is_only_sac_target() {
        // Sacrifice a creature: +1/+1 on itself. With only the source creature
        // on board, paying the cost removes the counter's only recipient →
        // trivial → reject.
        let mut state = GameState::new_two_player(42);
        let effect = put_counter(CounterType::Plus1Plus1, TargetFilter::SelfRef);
        let source = source_with(
            &mut state,
            "Carrion Feeder",
            &[CoreType::Creature],
            activated(effect, sac_creature_cost()),
        );
        // Make the source itself a creature that matches the sac filter.
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.power = Some(1);
            obj.toughness = Some(1);
        }
        assert_reject(
            &verdict_for(&state, source, plain_features()),
            "self_cost_trivial_benefit",
        );
    }

    #[test]
    fn self_counter_does_not_fizzle_with_other_fodder() {
        // Multi-authority: a separate token can be sacrificed instead, so the
        // +1/+1 counter lands → non-trivial → not rejected.
        let mut state = GameState::new_two_player(42);
        let effect = put_counter(CounterType::Plus1Plus1, TargetFilter::SelfRef);
        let source = source_with(
            &mut state,
            "Carrion Feeder",
            &[CoreType::Creature],
            activated(effect, sac_creature_cost()),
        );
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.power = Some(1);
            obj.toughness = Some(1);
        }
        token_creature(&mut state, "Zombie Token", 1, 1);
        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_present",
        );
    }

    #[test]
    fn counter_on_other_creature_does_not_fizzle() {
        // recipient != source: even with the source as the only sac target, a
        // counter aimed at a different creature filter is not a fizzle.
        let mut state = GameState::new_two_player(42);
        let effect = put_counter(
            CounterType::Plus1Plus1,
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
        );
        let source = source_with(
            &mut state,
            "Counter Sac",
            &[CoreType::Creature],
            activated(effect, sac_creature_cost()),
        );
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.power = Some(1);
            obj.toughness = Some(1);
        }
        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_present",
        );
    }

    // --- Row 11: non-self-cost / OneOf-min untouched ----------------------

    #[test]
    fn tap_only_ability_yields_na() {
        let mut state = GameState::new_two_player(42);
        let source = source_with(
            &mut state,
            "Tapper",
            &[CoreType::Artifact],
            activated(gain_life(1), AbilityCost::Tap),
        );
        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_value_na",
        );
    }

    #[test]
    fn one_of_min_picks_free_alternative_never_rejects() {
        // OneOf{ pay 3 life | {2} } — the cheapest branch is the mana cost (0),
        // so the priced self-cost is 0 and the gate never rejects.
        let mut state = GameState::new_two_player(42);
        let cost = AbilityCost::OneOf {
            costs: vec![
                AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: 3 },
                },
                AbilityCost::Mana {
                    cost: engine::types::mana::ManaCost::generic(2),
                },
            ],
        };
        let source = source_with(
            &mut state,
            "Flexible",
            &[CoreType::Artifact],
            activated(gain_life(1), cost),
        );
        assert_not_reject(&verdict_for(&state, source, plain_features()));
    }

    // --- Marginal branch: cheap pay-life deprioritized, never vetoed ------

    #[test]
    fn cheap_pay_life_trivial_is_marginal() {
        let mut state = GameState::new_two_player(42);
        let cost = AbilityCost::PayLife {
            amount: QuantityExpr::Fixed { value: 1 },
        };
        let source = source_with(
            &mut state,
            "Life Sink",
            &[CoreType::Artifact],
            activated(gain_life(1), cost),
        );
        let verdict = verdict_for(&state, source, plain_features());
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "self_cost_marginal");
                assert!(
                    delta < 0.0 && delta > -0.5,
                    "expected small nudge, got {delta}"
                );
            }
            PolicyVerdict::Reject { .. } => panic!("cheap pay-life must never be vetoed"),
        }
    }

    // --- MED-1: trivial self-costs in [0.5, 1.0) deprioritize, never neutral --

    #[test]
    fn pay_five_life_trivial_deprioritizes_not_neutral() {
        // MED-1: pay 5 life (0.75 priced, in the [0.5, 1.0) sub-veto range) for a
        // trivial 1 lifegain used to fall through to `self_cost_benefit_present`
        // (a losing play mislabeled as a benefit). It must now deprioritize.
        // Reverting the widening flips this back to `self_cost_benefit_present`.
        let mut state = GameState::new_two_player(42);
        let cost = AbilityCost::PayLife {
            amount: QuantityExpr::Fixed { value: 5 },
        };
        let source = source_with(
            &mut state,
            "Life Sink",
            &[CoreType::Artifact],
            activated(gain_life(1), cost),
        );
        let verdict = verdict_for(&state, source, plain_features());
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(
                    reason.kind, "self_cost_marginal",
                    "must not be benefit_present"
                );
                assert!(delta < 0.0, "expected a deprioritizing nudge, got {delta}");
            }
            PolicyVerdict::Reject { .. } => panic!("0.75 priced cost must not hard-veto"),
        }
    }

    #[test]
    fn non_creature_token_sac_trivial_deprioritizes_not_neutral() {
        // MED-1: sacrifice a non-creature token (0.5 priced, the lower edge of the
        // [0.5, 1.0) range) for a trivial 1 lifegain must deprioritize, not resolve
        // to `self_cost_benefit_present`.
        let mut state = GameState::new_two_player(42);
        artifact_token(&mut state, "Treasure");
        // The source is an enchantment (not an artifact) so the sole artifact the
        // "sacrifice an artifact" cost can consume is the 0.5-priced token.
        let source = source_with(
            &mut state,
            "Token Sink",
            &[CoreType::Enchantment],
            activated(gain_life(1), sac_artifact_cost()),
        );
        let verdict = verdict_for(&state, source, plain_features());
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(
                    reason.kind, "self_cost_marginal",
                    "must not be benefit_present"
                );
                assert!(delta < 0.0, "expected a deprioritizing nudge, got {delta}");
            }
            PolicyVerdict::Reject { .. } => panic!("0.5 priced cost must not hard-veto"),
        }
    }

    // --- MED-2: harmful mass counter with a worthwhile target is non-trivial --

    #[test]
    fn mass_harmful_counter_hitting_opponent_creature_not_rejected() {
        // MED-2: "Sacrifice a creature: put a -1/-1 counter on each creature" with a
        // worthwhile opponent creature present is real board interaction — it must
        // NOT be auto-classified trivial and hard-vetoed. Reverting the fix (the old
        // `counter_is_harmful(counter_type)` arm returns true → trivial) turns this
        // into a `self_cost_trivial_benefit` reject.
        let mut state = GameState::new_two_player(42);
        creature(&mut state, AI, "Bear", 2, 2);
        creature(&mut state, OPP, "Ogre", 4, 4);
        let effect = put_counter_all(
            CounterType::Minus1Minus1,
            TargetFilter::Typed(TypedFilter::creature()),
        );
        let source = source_with(
            &mut state,
            "Mass Wither",
            &[CoreType::Artifact],
            activated(effect, sac_creature_cost()),
        );
        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_present",
        );
    }

    #[test]
    fn mass_harmful_counter_no_worthwhile_target_rejected() {
        // Hostile boundary for MED-2: the same mass -1/-1 with no opponent creature
        // on board has no worthwhile board impact → trivial → reject. This pairs
        // with the positive row above so neither is a vacuous assertion.
        let mut state = GameState::new_two_player(42);
        creature(&mut state, AI, "Bear", 2, 2);
        let effect = put_counter_all(
            CounterType::Minus1Minus1,
            TargetFilter::Typed(TypedFilter::creature()),
        );
        let source = source_with(
            &mut state,
            "Mass Wither",
            &[CoreType::Artifact],
            activated(effect, sac_creature_cost()),
        );
        assert_reject(
            &verdict_for(&state, source, plain_features()),
            "self_cost_trivial_benefit",
        );
    }

    // --- Row 6 threat waiver: self-protection under threat allowed --------

    #[test]
    fn discard_for_self_protection_allowed_under_threat() {
        use engine::types::ability::{ResolvedAbility, TargetRef};
        use engine::types::game_state::{StackEntry, StackEntryKind};

        let mut state = GameState::new_two_player(42);
        state.active_player = OPP;
        let cost = AbilityCost::Discard {
            count: QuantityExpr::Fixed { value: 1 },
            filter: None,
            selection: Default::default(),
            self_scope: Default::default(),
        };
        let source = source_with(
            &mut state,
            "Loopy Creature",
            &[CoreType::Creature],
            activated(shroud_self_grant(), cost),
        );
        // Opponent removal on the stack targeting the creature that receives
        // shroud makes the protection grant a live payoff.
        let spell_id = create_object(
            &mut state,
            CardId(next_id()),
            OPP,
            "Doom Blade".to_string(),
            Zone::Stack,
        );
        let ability = ResolvedAbility::new(
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            vec![TargetRef::Object(source)],
            spell_id,
            OPP,
        );
        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: OPP,
            kind: StackEntryKind::Spell {
                card_id: CardId(99),
                ability: Some(Box::new(ability)),
                casting_variant: Default::default(),
                actual_mana_spent: 0,
            },
        });
        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_present",
        );
    }

    // --- Parsed-Oracle reach-guards (production parser AST) ----------------

    #[test]
    fn parsed_zuran_orb_rejected() {
        use engine::parser::oracle::parse_oracle_text;

        let mut state = GameState::new_two_player(42);
        land(&mut state, "Forest");
        let parsed = parse_oracle_text(
            "Sacrifice a land: You gain 2 life.",
            "Zuran Orb",
            &[],
            &["Artifact".to_string()],
            &[],
        );
        let ability = parsed
            .abilities
            .into_iter()
            .next()
            .expect("one activated ability");
        let source = create_object(
            &mut state,
            CardId(next_id()),
            AI,
            "Zuran Orb".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            *Arc::make_mut(&mut obj.abilities) = vec![ability];
        }
        assert_reject(
            &verdict_for(&state, source, plain_features()),
            "self_cost_trivial_benefit",
        );
    }

    #[test]
    fn parsed_ashnods_altar_not_rejected() {
        use engine::parser::oracle::parse_oracle_text;

        let mut state = GameState::new_two_player(42);
        creature(&mut state, AI, "Bear", 2, 2);
        let parsed = parse_oracle_text(
            "Sacrifice a creature: Add {C}{C}.",
            "Ashnod's Altar",
            &[],
            &["Artifact".to_string()],
            &[],
        );
        let ability = parsed
            .abilities
            .into_iter()
            .next()
            .expect("one activated ability");
        let source = create_object(
            &mut state,
            CardId(next_id()),
            AI,
            "Ashnod's Altar".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            *Arc::make_mut(&mut obj.abilities) = vec![ability];
        }
        assert_not_reject(&verdict_for(&state, source, plain_features()));
    }

    #[test]
    fn parsed_tyrite_sanctum_indestructible_counter_not_rejected() {
        // LOW: production-parser reach guard for the M2 beneficial-counter path.
        // Tyrite Sanctum's third ability parses as a Composite{Mana, Tap,
        // Sacrifice(SelfRef)} cost with a PutCounter{indestructible} payoff on a
        // target God — a beneficial counter, so the self-cost activation must NOT
        // be vetoed even though the sacrificed land prices at 4.0. Guards the M2
        // classification against future parser AST changes.
        use engine::parser::oracle::parse_oracle_text;

        let mut state = GameState::new_two_player(42);
        let parsed = parse_oracle_text(
            "{T}: Add {C}.\n{2}, {T}: Target legendary creature becomes a God in addition to its other types. Put a +1/+1 counter on it.\n{4}, {T}, Sacrifice this land: Put an indestructible counter on target God.",
            "Tyrite Sanctum",
            &[],
            &["Land".to_string()],
            &[],
        );
        let ability = parsed
            .abilities
            .into_iter()
            .find(|a| a.cost.as_ref().is_some_and(self_cost_in_scope))
            .expect("the sacrifice-this-land activation");
        let source = create_object(
            &mut state,
            CardId(next_id()),
            AI,
            "Tyrite Sanctum".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            *Arc::make_mut(&mut obj.abilities) = vec![ability];
        }
        assert_not_reject(&verdict_for(&state, source, plain_features()));
    }

    #[test]
    fn self_counter_replenishment_preview_outcomes_only_penalize_prevention() {
        let applied = |_state: &mut GameState| {};
        let transformed = |state: &mut GameState| {
            install_counter_replacement(state, QuantityModification::DOUBLE);
        };
        let prevented = |state: &mut GameState| {
            install_counter_replacement(state, QuantityModification::Prevent);
        };
        let choice_required = |state: &mut GameState| {
            install_counter_replacement(state, QuantityModification::DOUBLE);
            install_counter_replacement(state, QuantityModification::Plus { value: 1 });
        };

        for (install, expected_preview, expected_reason, penalized) in [
            (
                applied as fn(&mut GameState),
                SelfCounterCostPreview::Applied,
                "self_cost_value_na",
                false,
            ),
            (
                transformed as fn(&mut GameState),
                SelfCounterCostPreview::Transformed,
                "self_cost_value_na",
                false,
            ),
            (
                prevented as fn(&mut GameState),
                SelfCounterCostPreview::Prevented,
                "self_cost_counter_replacement_prevented",
                true,
            ),
            (
                choice_required as fn(&mut GameState),
                SelfCounterCostPreview::ChoiceRequired,
                "self_cost_value_na",
                false,
            ),
        ] {
            let mut state = GameState::new_two_player(42);
            install(&mut state);
            let source = source_with(
                &mut state,
                "Counter Replenisher",
                &[CoreType::Creature],
                self_counter_replenisher(),
            );
            state
                .objects
                .get_mut(&source)
                .expect("source exists")
                .counters
                .insert(CounterType::Plus1Plus1, 1);

            let ability = state.objects[&source]
                .abilities
                .first()
                .expect("counter replenisher ability");
            assert_eq!(
                self_counter_cost_preview(&state, AI, source, ability),
                Some(expected_preview),
                "replacement preview must reach the expected outcome"
            );

            let result = verdict_for(&state, source, plain_features());
            if penalized {
                assert!(matches!(
                    result,
                    PolicyVerdict::Score { delta, reason }
                        if delta < 0.0 && reason.kind == expected_reason
                ));
            } else {
                assert_neutral(&result, expected_reason);
            }
        }
    }

    #[test]
    fn self_counter_replenishment_preview_accepts_single_cost_composite() {
        let mut state = GameState::new_two_player(42);
        let mut ability = self_counter_replenisher();
        let remove_counter = ability.cost.take().expect("counter payment");
        ability.cost = Some(AbilityCost::Composite {
            costs: vec![remove_counter],
        });
        let source = source_with(
            &mut state,
            "Composite Counter Replenisher",
            &[CoreType::Creature],
            ability,
        );
        state
            .objects
            .get_mut(&source)
            .expect("source exists")
            .counters
            .insert(CounterType::Plus1Plus1, 1);

        let ability = state.objects[&source]
            .abilities
            .first()
            .expect("counter replenisher ability");
        assert_eq!(
            self_counter_cost_preview(&state, AI, source, ability),
            Some(SelfCounterCostPreview::Applied)
        );
    }

    #[test]
    fn self_counter_replenishment_preview_rejects_multi_cost_composite() {
        let mut state = GameState::new_two_player(42);
        let mut ability = self_counter_replenisher();
        let remove_counter = ability.cost.take().expect("counter payment");
        ability.cost = Some(AbilityCost::Composite {
            costs: vec![AbilityCost::Tap, remove_counter],
        });
        let source = source_with(
            &mut state,
            "Composite Counter Replenisher",
            &[CoreType::Creature],
            ability,
        );
        state
            .objects
            .get_mut(&source)
            .expect("source exists")
            .counters
            .insert(CounterType::Plus1Plus1, 1);

        let ability = state.objects[&source]
            .abilities
            .first()
            .expect("counter replenisher ability");
        assert_eq!(self_counter_cost_preview(&state, AI, source, ability), None);
    }

    #[test]
    fn self_counter_rewritten_preview_is_conservatively_deprioritized() {
        let config = AiConfig::default();

        assert!(matches!(
            counter_replenishment_verdict(
                Some(SelfCounterCostPreview::Unsupported),
                &config.policy_penalties,
            ),
            Some(PolicyVerdict::Score { delta, reason })
                if delta < 0.0 && reason.kind == "self_cost_counter_replacement_unsupported"
        ));
    }

    // --- The priced comparison: cost vs benefit ---------------------------
    //
    // Every test below reaches `BenefitAppraisal` through the real
    // `SelfCostValuePolicy::verdict` entry point via `verdict_for`, past the
    // scope gate and the synergy stand-down.

    /// Chain an extra effect onto an ability as its `sub_ability`, so
    /// `collect_chain_effects` yields both in order.
    fn with_rider(mut ability: AbilityDefinition, rider: Effect) -> AbilityDefinition {
        ability.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Activated,
            rider,
        )));
        ability
    }

    /// A token-creation rider: no `effect_triviality` arm models `Effect::Token`,
    /// so it classifies `Unmodeled`. Benefit-signed.
    fn create_token_rider() -> Effect {
        Effect::Token {
            name: "Servo".to_string(),
            power: engine::types::ability::PtValue::Fixed(1),
            toughness: engine::types::ability::PtValue::Fixed(1),
            types: vec!["Artifact".to_string(), "Creature".to_string()],
            colors: Vec::new(),
            keywords: Vec::new(),
            tapped: false,
            count: QuantityExpr::Fixed { value: 1 },
            owner: TargetFilter::Controller,
            attach_to: None,
            enters_attacking: false,
            supertypes: Vec::new(),
            static_abilities: Vec::new(),
            enter_with_counters: Vec::new(),
        }
    }

    /// A life-loss rider: also `Unmodeled`, but drawback-signed. The pair proves
    /// the stand-down is direction-independent.
    fn lose_life_rider() -> Effect {
        Effect::LoseLife {
            amount: QuantityExpr::Fixed { value: 2 },
            target: None,
        }
    }

    #[test]
    fn noncreature_token_sac_for_draw_covers_cost() {
        // A Clue/Food/Treasure-class crack: the artifact token prices at
        // `sacrifice_token_cost` = 0.5 against draw(1) = 1.0 → net +0.5, so the
        // comparison must NOT deprioritize it. Source is an enchantment so it
        // cannot itself join the artifact cheapest-match pool.
        let mut state = GameState::new_two_player(42);
        artifact_token(&mut state, "Clue");
        let source = source_with(
            &mut state,
            "Token Cracker",
            &[CoreType::Enchantment],
            activated(draw(1), sac_artifact_cost()),
        );
        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_covers_cost",
        );
    }

    #[test]
    fn draw_quantity_scales_the_comparison() {
        // Same 1/1 fodder (2.5) as the underwater token case, but drawing THREE
        // cards (3.0) clears it. The quantity must be read, not assumed to be 1
        // — an implementation hardcoding SINGLE_CARD_VALUE reports `underwater`.
        let mut state = GameState::new_two_player(42);
        creature(&mut state, AI, "Squire", 1, 1);
        let source = source_with(
            &mut state,
            "High Market",
            &[CoreType::Land],
            activated(draw(3), sac_creature_cost()),
        );
        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_covers_cost",
        );
    }

    #[test]
    fn pricing_uses_cheapest_matching_fodder() {
        // MULTI-AUTHORITY hostile fixture for the identity contract: two legal
        // sacrifices are on board, a 1/1 token (2.5) and a Bear (5.0). draw(3)
        // = 3.0 covers the CHEAPEST but not the dearest, so a binding that
        // priced anything other than the cheapest live match reports
        // `underwater` here.
        let mut state = GameState::new_two_player(42);
        token_creature(&mut state, "Goblin Token", 1, 1);
        creature(&mut state, AI, "Bear", 2, 2);
        let source = source_with(
            &mut state,
            "High Market",
            &[CoreType::Land],
            activated(draw(3), sac_creature_cost()),
        );
        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_covers_cost",
        );
    }

    #[test]
    fn large_lifegain_is_vetoed_underwater() {
        // The second pricing arm. gain_life(10) exceeds TRIVIAL_LIFEGAIN_CEILING
        // so it classifies NON-trivial, then prices at 10 *
        // self_cost_pay_life_per_point (0.15) = 1.5 against the Bear's 5.0 →
        // net -3.5, certified losing. Pricing lifegain on the same per-point
        // axis the cost side already uses is what makes this comparable at all.
        //
        // HISTORY: graduated delta -3.5 before the veto.
        let mut state = GameState::new_two_player(42);
        creature(&mut state, AI, "Bear", 2, 2);
        let source = source_with(
            &mut state,
            "High Market",
            &[CoreType::Land],
            activated(gain_life(10), sac_creature_cost()),
        );
        let verdict = verdict_for(&state, source, plain_features());
        assert_reject(&verdict, "self_cost_benefit_underwater");
        assert_facts(&verdict, 5000, 1500);
    }

    #[test]
    fn large_lifegain_unpriced_under_life_pressure() {
        // NEGATIVE SIBLING of the row above: identical fixture, AI life dropped
        // to 4 so `ai_life_critical` holds. Life is then genuinely worth more
        // than the per-point axis can bound, so the pricing arm declines and the
        // comparison stands down to the pre-existing neutral — today's exact
        // behaviour, preserved. An implementation that priced lifegain
        // unconditionally reports `underwater` here.
        let mut state = GameState::new_two_player(42);
        state.players[AI.0 as usize].life = 4;
        creature(&mut state, AI, "Bear", 2, 2);
        let source = source_with(
            &mut state,
            "High Market",
            &[CoreType::Land],
            activated(gain_life(10), sac_creature_cost()),
        );
        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_present",
        );
    }

    #[test]
    fn mixed_chain_with_unpriceable_effect_stands_down() {
        // Aggregation guard: `Effect::Mana` is classifier-NON-trivial but has no
        // confident price, so one unpriceable member must suppress the whole
        // comparison rather than let a partial sum (draw 1.0 vs Bear 5.0) go
        // underwater. Reaches the `None => Unpriced` early return.
        let mut state = GameState::new_two_player(42);
        creature(&mut state, AI, "Bear", 2, 2);
        let source = source_with(
            &mut state,
            "High Market",
            &[CoreType::Land],
            with_rider(activated(draw(1), sac_creature_cost()), add_two_colorless()),
        );
        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_present",
        );
    }

    #[test]
    fn unmodeled_benefit_rider_stands_down_the_comparison() {
        // UNMODELED rider, BENEFIT direction. A rider-blind implementation
        // prices this chain at draw(1) = 1.0 against the Bear's 5.0 and reports
        // `underwater` with the token silently valued at 0 — understating the
        // payoff. The sum is not a lower bound, so no conclusion is drawn.
        let mut state = GameState::new_two_player(42);
        creature(&mut state, AI, "Bear", 2, 2);
        let source = source_with(
            &mut state,
            "High Market",
            &[CoreType::Land],
            with_rider(
                activated(draw(1), sac_creature_cost()),
                create_token_rider(),
            ),
        );
        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_present",
        );
    }

    #[test]
    fn unmodeled_drawback_rider_blocks_covers_conclusion() {
        // UNMODELED rider, DRAWBACK direction — the paired sibling. Cheap
        // artifact-token fodder (0.5) vs draw(1) = 1.0, so the partial sum
        // "covers"; but the chain also loses 2 life, which the sum omits. An
        // implementation that helpfully concluded `covers_cost` from a partial
        // sum fails on the reason kind here. Together these two rows pin that
        // the stand-down never consults the net's sign.
        let mut state = GameState::new_two_player(42);
        artifact_token(&mut state, "Clue");
        let source = source_with(
            &mut state,
            "Token Cracker",
            &[CoreType::Enchantment],
            with_rider(activated(draw(1), sac_artifact_cost()), lose_life_rider()),
        );
        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_present",
        );
    }

    #[test]
    fn underwater_veto_is_categorical_at_any_depth() {
        // DEPTH-INVARIANCE discriminator (replaces
        // `underwater_delta_routes_through_the_band_rescale`, whose whole
        // discrimination — WHERE in the critical band a shortfall lands — died
        // with the graduated arm). Sole fodder is an 8/8 non-token, untapped,
        // keywordless: cost = creature_combat_value(8,8) = 1.5*8 + 8 = 20.0;
        // benefit = 1.0; net = -19.0.
        //
        // The source MUST be a non-creature (the High Market land pattern):
        // `sac_creature_cost()` is Typed(creature, You) with no `Another`
        // property, so a creature-typed source would join the cheapest-match
        // pool and silently break the 20.0 arithmetic.
        //
        // What this now pins: a -19.0 shortfall and the -4.0 shortfall of
        // `sac_creature_for_draw_is_vetoed_underwater` share ONE categorical
        // fate. Any implementation that re-introduces depth sensitivity — a band
        // rescale, a clamp, a "only veto past N" threshold — produces a `Score`
        // here and goes red on shape. The facts still carry the depth, so the
        // magnitude remains observable without being actionable.
        let mut state = GameState::new_two_player(42);
        creature(&mut state, AI, "Colossus", 8, 8);
        let source = source_with(
            &mut state,
            "High Market",
            &[CoreType::Land],
            activated(draw(1), sac_creature_cost()),
        );
        let verdict = verdict_for(&state, source, plain_features());
        assert_reject(&verdict, "self_cost_benefit_underwater");
        assert_facts(&verdict, 20000, 1000);
    }

    #[test]
    fn tapped_fodder_still_prices_at_full_body_value() {
        // THE TAPPED-INHERITANCE DISCRIMINATOR. Sole fodder is a 1/1 creature
        // token that is TAPPED; source is a non-creature so it cannot join the
        // cheapest-match pool.
        //
        // Correct pricing: `max(evaluate_creature_intrinsic(1,1) = 2.5,
        // sacrifice_token_cost = 0.5) = 2.5` against draw(1) = 1.0 → net -1.5 →
        // vetoed.
        //
        // Revert image (this is the exact defect the unit exists to close):
        // routing `sacrifice_cost` back through `evaluate_creature` prices the
        // tapped token at `max(2.5 - 1.5, 0.5) = 1.0` against 1.0 → net exactly
        // 0 → `self_cost_benefit_covers_cost`. The test then goes red on BOTH
        // the verdict shape and the reason kind. That `covers_cost` boundary is
        // the escape hatch a five-body board measurably drained through the
        // moment its tokens attacked and tapped.
        let mut state = GameState::new_two_player(42);
        let fodder = token_creature(&mut state, "Goblin Token", 1, 1);
        state.objects.get_mut(&fodder).unwrap().tapped = true;
        let source = source_with(
            &mut state,
            "High Market",
            &[CoreType::Land],
            activated(draw(1), sac_creature_cost()),
        );
        // Reach guard: the fixture is genuinely tapped, or it proves nothing
        // about tap inheritance.
        assert!(state.objects[&fodder].tapped);

        let verdict = verdict_for(&state, source, plain_features());
        assert_reject(&verdict, "self_cost_benefit_underwater");
        assert_facts(&verdict, 2500, 1000);
    }

    #[test]
    fn zero_power_token_crack_covers_at_the_boundary() {
        // The INCLUSIVE boundary's positive pin, replacing the tapped-1/1
        // example the boundary comment used to carry (that example is now false
        // — a tapped 1/1 is underwater). A 0/1 creature token prices at
        // `max(creature_combat_value(0,1) = 0*1.5 + 1 = 1.0, 0.5) = 1.0` against
        // draw(1) = 1.0 → net exactly 0.
        //
        // Revert image: an EXCLUSIVE boundary (`net > 0.0`) vetoes this
        // exact-cover crack and the test goes red on shape — the veto-overreach
        // direction, paired against `tapped_fodder_still_prices_at_full_body_value`
        // one arm over.
        let mut state = GameState::new_two_player(42);
        token_creature(&mut state, "Wall Token", 0, 1);
        let source = source_with(
            &mut state,
            "High Market",
            &[CoreType::Land],
            activated(draw(1), sac_creature_cost()),
        );
        let verdict = verdict_for(&state, source, plain_features());
        assert_neutral(&verdict, "self_cost_benefit_covers_cost");
        assert_facts(&verdict, 1000, 1000);
    }

    #[test]
    fn one_of_free_branch_still_covers() {
        // `OneOf` takes the payer's cheapest branch: the mana leg is out of
        // scope and prices 0, so the priced self-cost is 0 and draw(1) covers.
        // The comparison must not resurrect a cost the payer would never choose.
        let mut state = GameState::new_two_player(42);
        let cost = AbilityCost::OneOf {
            costs: vec![
                AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: 3 },
                },
                AbilityCost::Mana {
                    cost: engine::types::mana::ManaCost::generic(2),
                },
            ],
        };
        let source = source_with(
            &mut state,
            "Flexible",
            &[CoreType::Artifact],
            activated(draw(1), cost),
        );
        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_covers_cost",
        );
    }

    #[test]
    fn token_cost_default_stays_below_single_card_value() {
        // The invariant that keeps Clue/Food/Treasure cracking profitable under
        // the comparison. STATED LIMITATION: this pins the shipped DEFAULT only.
        // `sacrifice_token_cost` is a CMA-ES-tuned parameter; a retrain that
        // pushed it to or above 1.0 would flip non-creature token cracking to
        // `underwater` without failing any test but this one.
        //
        // That consequence is now STRONGER, not weaker: since the underwater arm
        // became a categorical veto, crossing this constant does not merely
        // deprioritize Clue cracking — it forbids it outright, at every
        // difficulty, on every deck. This pin matters more after the veto than
        // it did before it.
        assert!(
            crate::config::PolicyPenalties::default().sacrifice_token_cost
                < crate::policies::strategy_helpers::SINGLE_CARD_VALUE,
            "a token must stay cheaper than the card a crack draws"
        );
    }
}
