//! Ramp timing policy.
//!
//! Scores ramp spell casts and mana ability activations based on the current
//! turn and hand state. On-curve ramp (turns 1–3) is strongly preferred;
//! late ramp is contextually valued when it enables expensive threats; casting
//! non-ramp over an available ramp spell is penalized.
//!
//! CR 305.2: one land per turn normally; ramp spells effectively extend this
//! limit by accelerating mana availability. CR 605.1a: activated mana abilities
//! resolve immediately — they don't go on the stack. CR 601.2f: cost reductions
//! interact with the total cost after it's locked in.

use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::features::mana_ramp::{
    chain_has_mana_effect, is_land_fetch_spell_parts, is_ritual_parts,
};
use crate::features::DeckFeatures;

/// Minimum commitment required to activate this policy at all.
const COMMITMENT_FLOOR: f32 = 0.1;
/// Bonus for casting a ramp spell on curve (turn ≤ 3).
const DELTA_RAMP_ON_CURVE: f64 = 2.5;
/// Bonus for casting ramp when there are unreachable threats in hand.
const DELTA_RAMP_ENABLES_THREAT: f64 = 1.0;
/// Penalty for casting a non-ramp spell while an unplayed ramp spell sits in hand.
const DELTA_DEFER_TO_RAMP: f64 = -1.0;

pub struct RampTimingPolicy;

impl TacticalPolicy for RampTimingPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::RampTiming
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[
            DecisionKind::CastSpell,
            DecisionKind::ActivateManaAbility,
            DecisionKind::ActivateAbility,
        ]
    }

    fn activation(
        &self,
        features: &DeckFeatures,
        state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        // CR 305.2: ramp decks need land acceleration most in early turns.
        if features.mana_ramp.commitment < COMMITMENT_FLOOR {
            return None;
        }
        // After turn 4 the mana curve has mostly resolved — ramp guidance
        // becomes less useful and may interfere with threat-based decisions.
        if state.turn_number >= 5 {
            return None;
        }
        Some(features.mana_ramp.commitment)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let action_is_ramp = is_ramp_shaped_action(ctx);
        let turn = ctx.state.turn_number;

        // CR 605.1a + CR 305.2: ramp on turns ≤ 3 has the strongest payoff —
        // one extra mana source now multiplies over all remaining turns.
        if action_is_ramp && turn <= 3 {
            return PolicyVerdict::Score {
                delta: DELTA_RAMP_ON_CURVE,
                reason: PolicyReason::new("ramp_on_curve").with_fact("turn", turn as i64),
            };
        }

        // Late ramp (turn 4+) is still valuable if there are threats in hand
        // that cost more than current available lands + 1.
        if action_is_ramp && turn >= 4 {
            let land_count = count_ai_lands(ctx.state, ctx.ai_player);
            let has_expensive_threat = hand_has_expensive_spell(ctx, land_count + 1);
            if has_expensive_threat {
                return PolicyVerdict::Score {
                    delta: DELTA_RAMP_ENABLES_THREAT,
                    reason: PolicyReason::new("ramp_enables_threat")
                        .with_fact("turn", turn as i64)
                        .with_fact("land_count", land_count as i64),
                };
            }
        }

        // Casting a non-ramp spell while an unplayed ramp spell is in hand
        // suggests we should have ramped first.
        if !action_is_ramp && hand_has_ramp_spell(ctx) {
            return PolicyVerdict::Score {
                delta: DELTA_DEFER_TO_RAMP,
                reason: PolicyReason::new("defer_to_ramp"),
            };
        }

        PolicyVerdict::Score {
            delta: 0.0,
            reason: PolicyReason::new("ramp_timing_na"),
        }
    }
}

/// True when the current candidate action is ramp-shaped — a ramp mana
/// ability activation, land-fetch spell, or ritual. Delegates to the feature
/// module's parts-based classifiers so there is a single structural source of
/// truth across feature detection and runtime policy scoring.
///
/// Note: `GameAction::TapLandForMana` is deliberately NOT handled here. It
/// routes to `DecisionKind::ManaPayment` (see `decision_kind.rs`) and
/// `engine::ai_support::candidates` explicitly excludes it from priority
/// candidates, so this policy — which declares `CastSpell`,
/// `ActivateManaAbility`, `ActivateAbility` — never sees it.
fn is_ramp_shaped_action(ctx: &PolicyContext<'_>) -> bool {
    match &ctx.candidate.action {
        GameAction::ActivateAbility {
            source_id,
            ability_index,
        } => {
            let Some(obj) = ctx.state.objects.get(source_id) else {
                return false;
            };
            let Some(ability) = obj.abilities.get(*ability_index) else {
                return false;
            };
            // A dork/rock activation: the ability chain contains Effect::Mana.
            chain_has_mana_effect(ability)
        }
        GameAction::CastSpell { object_id, .. } => {
            let Some(obj) = ctx.state.objects.get(object_id) else {
                return false;
            };
            is_land_fetch_spell_parts(&obj.card_types.core_types, &obj.abilities)
                || is_ritual_parts(&obj.card_types.core_types, &obj.abilities)
        }
        _ => false,
    }
}

/// Count lands on the battlefield under the AI player's control. Used to
/// gauge whether expensive threats are reachable without more ramp.
fn count_ai_lands(state: &GameState, player: PlayerId) -> u32 {
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|obj| {
            obj.controller == player && obj.card_types.core_types.contains(&CoreType::Land)
        })
        .count() as u32
}

/// True if any card in the AI player's hand costs more than `threshold` mana.
/// CR 202.3: a spell's mana cost determines its mana value (CMC). We compare
/// the object's declared mana value against the available-land threshold to
/// approximate "unreachable this turn."
fn hand_has_expensive_spell(ctx: &PolicyContext<'_>, threshold: u32) -> bool {
    let Some(player) = ctx.state.players.get(ctx.ai_player.0 as usize) else {
        return false;
    };
    player.hand.iter().any(|&oid| {
        let Some(obj) = ctx.state.objects.get(&oid) else {
            return false;
        };
        // Skip lands — they have no mana cost to compare.
        if obj.card_types.core_types.contains(&CoreType::Land) {
            return false;
        }
        obj.mana_cost.mana_value() > threshold
    })
}

/// True if the AI player has a ramp spell (fetch or ritual) in hand that has
/// not been played yet. Used to penalize playing non-ramp over available ramp.
fn hand_has_ramp_spell(ctx: &PolicyContext<'_>) -> bool {
    let Some(player) = ctx.state.players.get(ctx.ai_player.0 as usize) else {
        return false;
    };
    player.hand.iter().any(|&oid| {
        let Some(obj) = ctx.state.objects.get(&oid) else {
            return false;
        };
        is_land_fetch_spell_parts(&obj.card_types.core_types, &obj.abilities)
            || is_ritual_parts(&obj.card_types.core_types, &obj.abilities)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiConfig;
    use crate::context::AiContext;
    use crate::features::{DeckFeatures, ManaRampFeature};
    use crate::session::AiSession;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, ManaContribution, ManaProduction, QuantityExpr,
        TargetFilter, TypedFilter,
    };
    use engine::types::card_type::{CardType, CoreType};
    use engine::types::game_state::{GameState, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;
    use std::sync::Arc;

    const AI: PlayerId = PlayerId(0);

    fn ramp_features(commitment: f32) -> DeckFeatures {
        DeckFeatures {
            mana_ramp: ManaRampFeature {
                dork_count: 4,
                land_fetch_count: 4,
                commitment,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn context_with_features(features: DeckFeatures) -> (AiContext, AiConfig) {
        let config = AiConfig::default();
        let mut session = AiSession::empty();
        session.features.insert(AI, features);
        let mut context = AiContext::empty(&config.weights);
        context.session = Arc::new(session);
        context.player = AI;
        (context, config)
    }

    fn cast_candidate(object_id: ObjectId, card_id: CardId) -> CandidateAction {
        CandidateAction {
            action: GameAction::CastSpell {
                object_id,
                card_id,
                targets: Vec::new(),
            },
            metadata: ActionMetadata {
                actor: Some(AI),
                tactical_class: TacticalClass::Spell,
            },
        }
    }

    fn activate_candidate(source_id: ObjectId, ability_index: usize) -> CandidateAction {
        CandidateAction {
            action: GameAction::ActivateAbility {
                source_id,
                ability_index,
            },
            metadata: ActionMetadata {
                actor: Some(AI),
                tactical_class: TacticalClass::Ability,
            },
        }
    }

    fn decision() -> AiDecisionContext {
        AiDecisionContext {
            waiting_for: WaitingFor::Priority { player: AI },
            candidates: Vec::new(),
        }
    }

    fn make_mana_ability() -> AbilityDefinition {
        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: Vec::new(),
                    contribution: ManaContribution::Base,
                },
                restrictions: Vec::new(),
                grants: Vec::new(),
                expiry: None,
                target: None,
            },
        );
        ability.cost = Some(engine::types::ability::AbilityCost::Tap);
        ability
    }

    fn make_fetch_spell_ability() -> AbilityDefinition {
        let mut ability = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(TypedFilter::land()),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
                selection_constraint: engine::types::ability::SearchSelectionConstraint::None,
            },
        );
        ability.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Battlefield,
                target: TargetFilter::Typed(TypedFilter::land()),
                owner_library: false,
                enter_transformed: false,
                under_your_control: true,
                enter_tapped: true,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
            },
        )));
        ability
    }

    #[test]
    fn activation_opts_out_below_floor() {
        let features = ramp_features(0.0);
        let state = GameState::new_two_player(42);
        assert!(RampTimingPolicy.activation(&features, &state, AI).is_none());
    }

    #[test]
    fn activation_opts_out_past_turn_four() {
        let features = ramp_features(0.8);
        let mut state = GameState::new_two_player(42);
        state.turn_number = 5;
        assert!(RampTimingPolicy.activation(&features, &state, AI).is_none());
    }

    #[test]
    fn ramp_on_curve_bonus_for_turn_two_fetch() {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;

        // Add a fetch-shaped spell to hand.
        let card_id = CardId(10);
        let spell_id = create_object(
            &mut state,
            card_id,
            AI,
            "Rampant Growth".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&spell_id).unwrap();
        obj.card_types = CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Sorcery],
            subtypes: Vec::new(),
        };
        Arc::make_mut(&mut obj.abilities).push(make_fetch_spell_ability());

        let candidate = cast_candidate(spell_id, card_id);
        let decision = decision();
        let (context, config) = context_with_features(ramp_features(0.5));
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };

        let verdict = RampTimingPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "ramp_on_curve");
                assert!(delta > 0.0, "expected positive delta for on-curve ramp");
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected Reject"),
        }
    }

    #[test]
    fn penalty_for_non_ramp_cast_with_ramp_in_hand() {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;

        // Add a fetch-shaped spell to hand (the unplayed ramp).
        let ramp_id = create_object(
            &mut state,
            CardId(20),
            AI,
            "Cultivate".to_string(),
            Zone::Hand,
        );
        let ramp_obj = state.objects.get_mut(&ramp_id).unwrap();
        ramp_obj.card_types = CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Sorcery],
            subtypes: Vec::new(),
        };
        Arc::make_mut(&mut ramp_obj.abilities).push(make_fetch_spell_ability());

        // The candidate action: cast a non-ramp creature spell.
        let creature_id = create_object(&mut state, CardId(21), AI, "Bear".to_string(), Zone::Hand);
        let creature_obj = state.objects.get_mut(&creature_id).unwrap();
        creature_obj.card_types = CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Creature],
            subtypes: Vec::new(),
        };

        let candidate = cast_candidate(creature_id, CardId(21));
        let decision = decision();
        let (context, config) = context_with_features(ramp_features(0.5));
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };

        let verdict = RampTimingPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "defer_to_ramp");
                assert!(delta < 0.0, "expected negative delta");
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected Reject"),
        }
    }

    #[test]
    fn no_penalty_for_irrelevant_spell_with_no_ramp_in_hand() {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;

        let creature_id =
            create_object(&mut state, CardId(30), AI, "Filler".to_string(), Zone::Hand);
        let creature_obj = state.objects.get_mut(&creature_id).unwrap();
        creature_obj.card_types = CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Creature],
            subtypes: Vec::new(),
        };

        let candidate = cast_candidate(creature_id, CardId(30));
        let decision = decision();
        let (context, config) = context_with_features(ramp_features(0.5));
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };

        let verdict = RampTimingPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "ramp_timing_na");
                assert_eq!(delta, 0.0);
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected Reject"),
        }
    }

    #[test]
    fn mana_ability_activation_yields_ramp_on_curve() {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;

        let source_id = create_object(
            &mut state,
            CardId(40),
            AI,
            "Llanowar Elves".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&source_id).unwrap();
        obj.card_types = CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Creature],
            subtypes: Vec::new(),
        };
        Arc::make_mut(&mut obj.abilities).push(make_mana_ability());

        let candidate = activate_candidate(source_id, 0);
        let decision = decision();
        let (context, config) = context_with_features(ramp_features(0.5));
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };

        let verdict = RampTimingPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "ramp_on_curve");
                assert!(delta > 0.0);
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected Reject"),
        }
    }
}
