//! Net-value gate policy for self-cost ability activations.
//!
//! Thin adapter over `self_cost.rs`: fetches the activated ability, confirms its
//! cost spends a self-resource (sacrifice / pay-life / discard / self-exile),
//! stands down when off-ability deck synergy justifies the cost, then prices the
//! cost against the ability's immediate payoff. A real cost with a trivial
//! payoff is rejected (scoring `-inf`, so Pass wins); a cheap cost is merely
//! deprioritized; anything with a genuine payoff is left alone.

use engine::types::ability::AbilityTag;
use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use super::self_cost::{
    benefit_is_trivial, real_self_cost, self_cost_in_scope, synergy_justifies_self_cost,
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
        let trivial = benefit_is_trivial(ctx.state, ctx.ai_player, *source_id, &ability);

        let cost_milli = (cost_value * 1000.0) as i64;

        if trivial {
            if cost_value >= REAL_COST_FLOOR {
                return PolicyVerdict::reject(
                    PolicyReason::new("self_cost_trivial_benefit")
                        .with_fact("cost_milli", cost_milli)
                        .with_fact("benefit", 0),
                );
            }
            // Trivial payoff, but the priced self-cost is below the real-loss
            // floor: deprioritize with an auto-banded negative delta. No trivial
            // self-cost play may resolve to `self_cost_benefit_present`, and the
            // `self_cost_marginal` reason deliberately does NOT claim a benefit.
            return PolicyVerdict::score(
                -cost_value,
                PolicyReason::new("self_cost_marginal").with_fact("cost_milli", cost_milli),
            );
        }

        PolicyVerdict::neutral(PolicyReason::new("self_cost_benefit_present"))
    }
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
        ManaContribution, ManaProduction, ObjectScope, QuantityExpr, QuantityRef, SacrificeCost,
        StaticDefinition, TargetFilter, TypeFilter, TypedFilter,
    };
    use engine::types::card_type::CoreType;
    use engine::types::counter::CounterType;
    use engine::types::game_state::{GameState, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::keywords::{Keyword, KeywordKind};
    use engine::types::player::PlayerId;
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
        }
    }

    fn put_counter(counter: CounterType, target: TargetFilter) -> Effect {
        Effect::PutCounter {
            counter_type: counter,
            count: QuantityExpr::Fixed { value: 1 },
            target,
        }
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
    fn sac_creature_for_draw_reaches_scoring_and_is_allowed() {
        // Positive reach-guard for row 1: identical cost, real payoff (a card) →
        // the input passed self_cost_in_scope and the benefit walk flips it to
        // a non-reject. If benefit detection were broken this would reject.
        let mut state = GameState::new_two_player(42);
        creature(&mut state, AI, "Bear", 2, 2);
        let source = source_with(
            &mut state,
            "High Market",
            &[CoreType::Land],
            activated(draw(1), sac_creature_cost()),
        );
        assert_neutral(
            &verdict_for(&state, source, plain_features()),
            "self_cost_benefit_present",
        );
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
                ability: Some(ability),
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
}
