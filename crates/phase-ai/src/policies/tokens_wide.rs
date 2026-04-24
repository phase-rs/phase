//! Tokens-wide tactical policy.
//!
//! Scores `CastSpell` and `DeclareAttackers` candidates to bias tokens-wide
//! decks toward deploying token generators and swinging wide with a full board.
//!
//! CR 111.1: token creation by spells/abilities.
//! CR 508.1: declaring attackers.
//! CR 613.4c: P/T modification effects (mass pump).

use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::features::tokens_wide::{
    is_mass_pump_parts, is_token_generator_parts, COMMITMENT_FLOOR, WIDE_ATTACK_FLOOR,
};
use crate::features::DeckFeatures;

pub struct TokensWidePolicy;

impl TacticalPolicy for TokensWidePolicy {
    fn id(&self) -> PolicyId {
        PolicyId::TokensWide
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::CastSpell, DecisionKind::DeclareAttackers]
    }

    /// Opt out below `COMMITMENT_FLOOR`; otherwise return `Some(commitment)`
    /// so the registry scales verdict deltas by feature strength.
    fn activation(
        &self,
        features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        let commitment = features.tokens_wide.commitment;
        if commitment < COMMITMENT_FLOOR {
            None
        } else {
            Some(commitment)
        }
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        match &ctx.candidate.action {
            GameAction::DeclareAttackers { attacks } => score_declare_attackers(attacks),
            GameAction::CastSpell { object_id, .. } => score_cast_spell(ctx, *object_id),
            _ => PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("tokens_wide_na"),
            },
        }
    }
}

/// Score a `CastSpell` candidate for tokens-wide decks.
///
/// Token generators earn +1.0 — deploying the factory is the core game plan.
/// Mass pump spells (`PumpAll` scoped to your creatures) earn +0.8 — they turn
/// the existing token army into a lethal attack. Anthems are handled separately
/// by `AnthemPriorityPolicy` — return neutral here to avoid double-counting.
/// CR 111.1 (token generator). CR 613.4c (mass pump).
fn score_cast_spell(
    ctx: &PolicyContext<'_>,
    object_id: engine::types::identifiers::ObjectId,
) -> PolicyVerdict {
    let Some(obj) = ctx.state.objects.get(&object_id) else {
        return PolicyVerdict::Score {
            delta: 0.0,
            reason: PolicyReason::new("tokens_wide_cast_na"),
        };
    };

    // Token generator: casting the factory is the highest-priority play. CR 111.1.
    //
    // KNOWN GAP: `GameObject` does not carry a `triggers` field — triggered
    // abilities are registered elsewhere in the trigger-watcher system and not
    // queryable here. So this branch can only see token-generation in the
    // direct ability chain. Cards like Bitterblossom (whose token creation
    // lives in an upkeep `TriggerDefinition.execute`) won't get the per-cast
    // bonus from this policy. They DO contribute to `commitment` (which is
    // computed at deck-build time over `face.triggers` in the feature's
    // `detect()`), so tempo class, mulligan, and board-wipe amplification all
    // still react correctly. Only the per-cast +1.0 is suppressed.
    if is_token_generator_parts(&obj.abilities, &[]) {
        return PolicyVerdict::Score {
            delta: 1.0,
            reason: PolicyReason::new("tokens_wide_generator_cast"),
        };
    }

    // Mass pump: turning a token army into a lethal swing. CR 613.4c.
    if is_mass_pump_parts(&obj.abilities) {
        return PolicyVerdict::Score {
            delta: 0.8,
            reason: PolicyReason::new("tokens_wide_mass_pump_cast"),
        };
    }

    PolicyVerdict::Score {
        delta: 0.0,
        reason: PolicyReason::new("tokens_wide_cast_na"),
    }
}

/// Score a `DeclareAttackers` candidate for tokens-wide decks.
///
/// Swinging with ≥ `WIDE_ATTACK_FLOOR` attackers earns +1.5 — a tokens deck
/// wins by flooding the board and attacking with everything. Fewer attackers
/// return 0.0 (no bonus, but no penalty — the aggro pressure policy handles
/// punishment for skipped attacks). CR 508.1 + CR 508.3d.
fn score_declare_attackers(
    attacks: &[(
        engine::types::identifiers::ObjectId,
        engine::game::combat::AttackTarget,
    )],
) -> PolicyVerdict {
    let attack_count = attacks.len() as u32;
    if attack_count >= WIDE_ATTACK_FLOOR {
        PolicyVerdict::Score {
            delta: 1.5,
            reason: PolicyReason::new("tokens_wide_swing_wide")
                .with_fact("attacker_count", attack_count as i64),
        }
    } else {
        PolicyVerdict::Score {
            delta: 0.0,
            reason: PolicyReason::new("tokens_wide_attack_below_floor")
                .with_fact("attacker_count", attack_count as i64),
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiConfig;
    use crate::context::AiContext;
    use crate::features::tokens_wide::TokensWideFeature;
    use crate::features::DeckFeatures;
    use crate::session::AiSession;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, PtValue, QuantityExpr, TargetFilter,
    };
    use engine::types::card_type::{CardType, CoreType};
    use engine::types::game_state::{GameState, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;
    use std::sync::Arc;

    const AI: PlayerId = PlayerId(0);
    const OPP: PlayerId = PlayerId(1);

    fn features_with_commitment(commitment: f32) -> DeckFeatures {
        DeckFeatures {
            tokens_wide: TokensWideFeature {
                commitment,
                token_generator_count: 8,
                mass_token_generator_count: 4,
                anthem_count: 3,
                mass_pump_count: 2,
                wide_payoff_count: 4,
                payoff_names: Vec::new(),
                anthem_names: Vec::new(),
            },
            ..DeckFeatures::default()
        }
    }

    fn decision() -> AiDecisionContext {
        AiDecisionContext {
            waiting_for: WaitingFor::Priority { player: AI },
            candidates: Vec::new(),
        }
    }

    fn attackers_decision() -> AiDecisionContext {
        AiDecisionContext {
            waiting_for: WaitingFor::DeclareAttackers {
                player: AI,
                valid_attacker_ids: vec![],
                valid_attack_targets: vec![],
            },
            candidates: Vec::new(),
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

    fn attack_candidate(
        attacks: Vec<(ObjectId, engine::game::combat::AttackTarget)>,
    ) -> CandidateAction {
        CandidateAction {
            action: GameAction::DeclareAttackers { attacks },
            metadata: ActionMetadata {
                actor: Some(AI),
                tactical_class: TacticalClass::Attack,
            },
        }
    }

    fn add_token_generator(state: &mut GameState, id: u64) -> ObjectId {
        let card_id = CardId(id);
        let oid = create_object(state, card_id, AI, format!("Token Gen {id}"), Zone::Hand);
        let token_effect = Effect::Token {
            name: "Saproling".to_string(),
            power: PtValue::Fixed(1),
            toughness: PtValue::Fixed(1),
            types: vec!["Creature".to_string()],
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
        };
        let mut ability = AbilityDefinition::new(AbilityKind::Spell, token_effect);
        ability.kind = AbilityKind::Spell;
        let obj = state.objects.get_mut(&oid).unwrap();
        obj.card_types = CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Sorcery],
            subtypes: Vec::new(),
        };
        obj.abilities.push(ability);
        oid
    }

    fn add_pump_all_spell(state: &mut GameState, id: u64) -> ObjectId {
        use engine::types::ability::{ControllerRef, TypeFilter, TypedFilter};
        let card_id = CardId(id);
        let oid = create_object(state, card_id, AI, format!("Overrun {id}"), Zone::Hand);
        let creature_you_control = TargetFilter::Typed(TypedFilter {
            controller: Some(ControllerRef::You),
            type_filters: vec![TypeFilter::Creature],
            ..TypedFilter::default()
        });
        let pump_effect = Effect::PumpAll {
            power: PtValue::Fixed(3),
            toughness: PtValue::Fixed(3),
            target: creature_you_control,
        };
        let mut ability = AbilityDefinition::new(AbilityKind::Spell, pump_effect);
        ability.kind = AbilityKind::Spell;
        let obj = state.objects.get_mut(&oid).unwrap();
        obj.card_types = CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Sorcery],
            subtypes: Vec::new(),
        };
        obj.abilities.push(ability);
        oid
    }

    fn add_battlefield_creature(state: &mut GameState, id: u64) -> ObjectId {
        let card_id = CardId(id);
        let oid = create_object(state, card_id, AI, format!("Token {id}"), Zone::Battlefield);
        let obj = state.objects.get_mut(&oid).unwrap();
        obj.card_types = CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Creature],
            subtypes: Vec::new(),
        };
        obj.power = Some(1);
        obj.toughness = Some(1);
        state.battlefield.push_back(oid);
        oid
    }

    // ── Activation tests ──────────────────────────────────────────────────

    #[test]
    fn activation_opts_out_below_floor() {
        // COMMITMENT_FLOOR = 0.30; use 0.25 to be below.
        let features = features_with_commitment(0.25);
        let state = GameState::new_two_player(42);
        assert!(TokensWidePolicy.activation(&features, &state, AI).is_none());
    }

    #[test]
    fn activation_fires_at_or_above_floor() {
        let features = features_with_commitment(0.5);
        let state = GameState::new_two_player(42);
        assert!(TokensWidePolicy.activation(&features, &state, AI).is_some());
    }

    // ── CastSpell tests ───────────────────────────────────────────────────

    #[test]
    fn scores_token_generator_cast_positive() {
        let mut state = GameState::new_two_player(42);
        let oid = add_token_generator(&mut state, 1);
        let card_id = CardId(1);
        let candidate = cast_candidate(oid, card_id);
        let (context, config) = context_with_features(features_with_commitment(0.6));
        let ctx = PolicyContext {
            state: &state,
            decision: &decision(),
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };
        let verdict = TokensWidePolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert!(delta > 0.0, "token generator cast should score positive");
                assert_eq!(reason.kind, "tokens_wide_generator_cast");
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected Reject"),
        }
    }

    #[test]
    fn scores_mass_pump_cast_positive() {
        let mut state = GameState::new_two_player(42);
        let oid = add_pump_all_spell(&mut state, 2);
        let card_id = CardId(2);
        let candidate = cast_candidate(oid, card_id);
        let (context, config) = context_with_features(features_with_commitment(0.6));
        let ctx = PolicyContext {
            state: &state,
            decision: &decision(),
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };
        let verdict = TokensWidePolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert!(delta > 0.0, "mass pump cast should score positive");
                assert_eq!(reason.kind, "tokens_wide_mass_pump_cast");
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected Reject"),
        }
    }

    #[test]
    fn neutral_for_unrelated_spell() {
        let mut state = GameState::new_two_player(42);
        let card_id = CardId(3);
        let oid = create_object(
            &mut state,
            card_id,
            AI,
            "Grizzly Bears".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&oid).unwrap();
            obj.card_types = CardType {
                supertypes: Vec::new(),
                core_types: vec![CoreType::Creature],
                subtypes: Vec::new(),
            };
        }
        let candidate = cast_candidate(oid, card_id);
        let (context, config) = context_with_features(features_with_commitment(0.6));
        let ctx = PolicyContext {
            state: &state,
            decision: &decision(),
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };
        let verdict = TokensWidePolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(delta, 0.0, "unrelated spell should be neutral");
                assert_eq!(reason.kind, "tokens_wide_cast_na");
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected Reject"),
        }
    }

    // ── DeclareAttackers tests ────────────────────────────────────────────

    #[test]
    fn scores_swing_wide_at_three_attackers() {
        let mut state = GameState::new_two_player(42);
        let a1 = add_battlefield_creature(&mut state, 10);
        let a2 = add_battlefield_creature(&mut state, 11);
        let a3 = add_battlefield_creature(&mut state, 12);
        let attacks = vec![
            (a1, engine::game::combat::AttackTarget::Player(OPP)),
            (a2, engine::game::combat::AttackTarget::Player(OPP)),
            (a3, engine::game::combat::AttackTarget::Player(OPP)),
        ];
        let candidate = attack_candidate(attacks);
        let (context, config) = context_with_features(features_with_commitment(0.6));
        let ctx = PolicyContext {
            state: &state,
            decision: &attackers_decision(),
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };
        let verdict = TokensWidePolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert!(
                    delta >= 1.5,
                    "swing wide at 3+ attackers should score ≥ 1.5"
                );
                assert_eq!(reason.kind, "tokens_wide_swing_wide");
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected Reject"),
        }
    }

    #[test]
    fn below_floor_attack_count_is_neutral() {
        let mut state = GameState::new_two_player(42);
        let a1 = add_battlefield_creature(&mut state, 20);
        let attacks = vec![(a1, engine::game::combat::AttackTarget::Player(OPP))];
        let candidate = attack_candidate(attacks);
        let (context, config) = context_with_features(features_with_commitment(0.6));
        let ctx = PolicyContext {
            state: &state,
            decision: &attackers_decision(),
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };
        let verdict = TokensWidePolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, .. } => {
                assert_eq!(delta, 0.0, "1 attacker is below WIDE_ATTACK_FLOOR (3)");
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected Reject"),
        }
    }
}
