//! Spellslinger casting tactical policy.
//!
//! Scores `CastSpell` candidates for decks built around instants/sorceries,
//! prowess triggers, and spell-copy payoffs. Biases the AI toward cheap spells
//! with cantrip value, copy spells when the stack is non-empty, burn with board
//! support, and cast-payoff creature deployment.
//!
//! CR 702.108a: Prowess — noncreature spell triggers +1/+1 until end of turn.
//! CR 601.2i: A spell becomes cast when placed on the stack.
//! CR 707.10: Copying a spell puts a copy onto the stack.
//! CR 120.3: Damage to players from burn spells.
//! CR 202.3b: Mana value.

use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::features::spellslinger_prowess::{
    has_prowess_parts, is_burn_to_player_parts, is_cast_payoff_parts, is_copy_effect_parts,
    is_low_curve_spell_parts, is_nth_spell_payoff_parts, COMMITMENT_FLOOR,
};
use crate::features::DeckFeatures;

pub struct SpellslingerCastingPolicy;

impl TacticalPolicy for SpellslingerCastingPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::SpellslingerCasting
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::CastSpell]
    }

    /// Opt out below `COMMITMENT_FLOOR`. CR 702.108a: prowess requires consistent
    /// non-creature spell casting to be relevant.
    fn activation(
        &self,
        features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        let commitment = features.spellslinger_prowess.commitment;
        if commitment < COMMITMENT_FLOOR {
            None
        } else {
            Some(commitment)
        }
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let GameAction::CastSpell { object_id, .. } = &ctx.candidate.action else {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("spellslinger_casting_na"),
            };
        };

        let Some(obj) = ctx.state.objects.get(object_id) else {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("spellslinger_casting_na"),
            };
        };

        let core_types = &obj.card_types.core_types;
        let mana_cost = &obj.mana_cost;
        let abilities = &obj.abilities;
        let keywords = &obj.keywords;
        let triggers = obj.trigger_definitions.as_slice();
        let features = &ctx
            .context
            .session
            .features
            .get(&ctx.ai_player)
            .cloned()
            .unwrap_or_default();

        // Non-spell objects (lands, some activated abilities) return neutral.
        let is_spell = core_types.iter().any(|t| {
            matches!(
                t,
                CoreType::Instant
                    | CoreType::Sorcery
                    | CoreType::Creature
                    | CoreType::Enchantment
                    | CoreType::Artifact
                    | CoreType::Planeswalker
            )
        });
        if !is_spell {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("spellslinger_casting_na"),
            };
        }

        let mut delta = 0.0_f64;
        let mut reason_kind = "spellslinger_defer";
        let mut reason_facts: Vec<(&'static str, i64)> = Vec::new();

        let mv = mana_cost.mana_value();

        // Cheap spell bonus — the core of the spellslinger game plan.
        // CR 202.3b + CR 702.108a: cheap spells trigger prowess early and often.
        if is_low_curve_spell_parts(core_types, mana_cost) {
            delta += 0.6;
            reason_kind = "spellslinger_cheap_spell";
            reason_facts.push(("mana_value", mv as i64));

            // Additional cantrip chain bonus (stacks with cheap spell). CR 121.1.
            // Only applies when the card is not itself a cast-payoff (deploying a
            // payoff creature is scored separately and should not double-stack).
            let spell_is_cantrip = {
                // We can't call is_cantrip(face) here — we only have a GameObject.
                // Re-check: the spell must be an instant/sorcery AND draw cards.
                (core_types.contains(&CoreType::Instant) || core_types.contains(&CoreType::Sorcery))
                    && crate::features::control::is_card_draw_parts(abilities)
            };
            let spell_is_payoff = is_cast_payoff_parts(triggers);
            if spell_is_cantrip && !spell_is_payoff {
                delta += 0.4;
                reason_kind = "spellslinger_cantrip_chain";
            }
        }

        // Copy-spell bonus: much stronger when there is a legal target on the stack.
        // CR 707.10: copying a spell is only useful if there is a spell to copy.
        if is_copy_effect_parts(abilities) {
            let stack_non_empty = !ctx.state.stack.is_empty();
            if stack_non_empty {
                delta += 1.5;
                reason_kind = "spellslinger_copy_with_target";
            } else {
                delta += 0.2;
                if reason_kind == "spellslinger_defer" {
                    reason_kind = "spellslinger_copy_no_target";
                }
            }
        }

        // Burn-to-player bonus: scales with prowess creatures already on board.
        // CR 120.3: damage to players. CR 702.108a: prowess pumps in response.
        if is_burn_to_player_parts(abilities) {
            let prowess_on_board = count_prowess_creatures(ctx.state, ctx.ai_player);
            // +0.3 base + 0.5 per prowess creature on board, capped at +2.3 total.
            let burn_bonus = (0.3 + prowess_on_board.min(4) as f64 * 0.5).min(2.3);
            delta += burn_bonus;
            reason_kind = "spellslinger_burn_with_prowess";
            reason_facts.push(("prowess_creatures_on_board", prowess_on_board as i64));
        }

        // Cast-payoff creature: deploying the engine is the highest-priority play.
        // CR 603.1: triggered abilities fire after a spell is cast.
        if (is_cast_payoff_parts(triggers) || has_prowess_parts(keywords))
            && features
                .spellslinger_prowess
                .payoff_names
                .iter()
                .any(|n| n == &obj.name)
        {
            delta += 1.5;
            reason_kind = "spellslinger_payoff_deploy";
        }

        // Off-strategy penalty: expensive non-payoff spells slow the deck down.
        // A spell is off-strategy if: MV > 4 AND not an IS AND not a payoff.
        let is_is =
            core_types.contains(&CoreType::Instant) || core_types.contains(&CoreType::Sorcery);
        // Include nth-spell payoffs explicitly. In practice every nth-spell
        // trigger also satisfies `is_cast_payoff_parts` (same SpellCast mode,
        // same scope), but documenting the disjunction guards against future
        // tightening of `is_cast_payoff_parts` that would silently demote
        // nth-spell cards into the off-strategy bucket.
        let is_payoff_card = is_cast_payoff_parts(triggers)
            || is_nth_spell_payoff_parts(triggers)
            || has_prowess_parts(keywords);
        if mv > 4 && !is_is && !is_payoff_card && delta == 0.0 {
            delta -= 0.4;
            reason_kind = "spellslinger_off_strategy";
            reason_facts.push(("mana_value", mv as i64));
        }

        let mut reason = PolicyReason::new(reason_kind);
        for (k, v) in reason_facts {
            reason = reason.with_fact(k, v);
        }

        PolicyVerdict::Score { delta, reason }
    }
}

/// Count AI-controlled creatures with Prowess on the battlefield.
/// CR 702.108a: each prowess creature triggers on noncreature spells.
fn count_prowess_creatures(state: &GameState, player: PlayerId) -> usize {
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|obj| {
            obj.controller == player
                && obj.zone == Zone::Battlefield
                && obj.card_types.core_types.contains(&CoreType::Creature)
                && has_prowess_parts(&obj.keywords)
        })
        .count()
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiConfig;
    use crate::context::AiContext;
    use crate::features::spellslinger_prowess::SpellslingerProwessFeature;
    use crate::features::DeckFeatures;
    use crate::session::AiSession;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, QuantityExpr, ResolvedAbility, TargetFilter,
    };
    use engine::types::card_type::{CardType, CoreType};
    use engine::types::game_state::{GameState, StackEntry, StackEntryKind, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::keywords::Keyword;
    use engine::types::mana::ManaCost;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;
    use std::sync::Arc;

    const AI: PlayerId = PlayerId(0);

    fn make_context(commitment: f32) -> (AiContext, AiConfig) {
        let config = AiConfig::default();
        let mut session = AiSession::empty();
        let features = DeckFeatures {
            spellslinger_prowess: SpellslingerProwessFeature {
                commitment,
                prowess_count: 8,
                low_curve_spell_count: 20,
                instant_sorcery_count: 24,
                payoff_names: vec!["Prowess Monk".to_string(), "Archmage".to_string()],
                ..Default::default()
            },
            ..DeckFeatures::default()
        };
        session.features.insert(AI, features);
        let mut context = AiContext::empty(&config.weights);
        context.session = Arc::new(session);
        context.player = AI;
        (context, config)
    }

    fn cast_spell_action(object_id: ObjectId) -> CandidateAction {
        CandidateAction {
            action: GameAction::CastSpell {
                object_id,
                card_id: CardId(object_id.0),
                targets: Vec::new(),
            },
            metadata: ActionMetadata {
                actor: Some(AI),
                tactical_class: TacticalClass::Spell,
            },
        }
    }

    fn decision() -> AiDecisionContext {
        AiDecisionContext {
            waiting_for: WaitingFor::Priority { player: AI },
            candidates: Vec::new(),
        }
    }

    fn add_instant(state: &mut GameState, card_idx: u64, mv: u32) -> ObjectId {
        let card_id = CardId(card_idx);
        let oid = create_object(
            state,
            card_id,
            AI,
            format!("Instant {card_idx}"),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&oid).unwrap();
        obj.card_types = CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Instant],
            subtypes: Vec::new(),
        };
        obj.mana_cost = ManaCost::generic(mv);
        oid
    }

    fn add_instant_with_ability(
        state: &mut GameState,
        card_idx: u64,
        mv: u32,
        effect: Effect,
    ) -> ObjectId {
        let oid = add_instant(state, card_idx, mv);
        let obj = state.objects.get_mut(&oid).unwrap();
        Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(AbilityKind::Spell, effect));
        oid
    }

    fn add_creature_with_prowess(state: &mut GameState, card_idx: u64, zone: Zone) -> ObjectId {
        let card_id = CardId(card_idx);
        let oid = create_object(
            state,
            card_id,
            AI,
            format!("Prowess Creature {card_idx}"),
            zone,
        );
        let obj = state.objects.get_mut(&oid).unwrap();
        obj.card_types = CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Creature],
            subtypes: Vec::new(),
        };
        obj.mana_cost = ManaCost::generic(1);
        obj.keywords.push(Keyword::Prowess);
        obj.controller = AI;
        oid
    }

    // ── Tests ──────────────────────────────────────────────────────────────────

    #[test]
    fn activation_opts_out_below_floor() {
        let features = DeckFeatures {
            spellslinger_prowess: SpellslingerProwessFeature {
                commitment: 0.1, // below COMMITMENT_FLOOR (0.30)
                ..Default::default()
            },
            ..DeckFeatures::default()
        };
        let state = GameState::new_two_player(42);
        let result = SpellslingerCastingPolicy.activation(&features, &state, AI);
        assert!(result.is_none(), "should opt out below COMMITMENT_FLOOR");
    }

    #[test]
    fn cheap_spell_scores_positive() {
        let mut state = GameState::new_two_player(42);
        let oid = add_instant(&mut state, 1, 1);
        let (context, config) = make_context(0.8);
        let decision = decision();
        let candidate = cast_spell_action(oid);
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };
        let verdict = SpellslingerCastingPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert!(
                    delta > 0.0,
                    "cheap spell should score positive, got {delta}"
                );
                assert!(
                    reason.kind == "spellslinger_cheap_spell"
                        || reason.kind == "spellslinger_cantrip_chain",
                    "unexpected reason: {}",
                    reason.kind
                );
            }
            PolicyVerdict::Reject { .. } => panic!("should not reject"),
        }
    }

    #[test]
    fn cantrip_chain_bonus_stacks_with_cheap() {
        // A MV-1 instant that also draws a card should score >= 1.0 (0.6 + 0.4).
        let mut state = GameState::new_two_player(42);
        let oid = add_instant_with_ability(
            &mut state,
            1,
            1,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: engine::types::ability::TargetFilter::Controller,
            },
        );
        let (context, config) = make_context(0.8);
        let decision = decision();
        let candidate = cast_spell_action(oid);
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };
        let verdict = SpellslingerCastingPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, .. } => {
                assert!(
                    delta >= 1.0,
                    "cantrip+cheap should score >= 1.0, got {delta}"
                );
            }
            _ => panic!("expected Score"),
        }
    }

    #[test]
    fn copy_spell_with_target_on_stack_scores_high() {
        // CopySpell effect + non-empty stack → +1.5. CR 707.10.
        let mut state = GameState::new_two_player(42);
        // Add something to the stack so it's non-empty.
        let dummy_ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: engine::types::ability::TargetFilter::Controller,
            },
            Vec::new(),
            ObjectId(999),
            AI,
        );
        state.stack.push_back(StackEntry {
            id: ObjectId(9000),
            source_id: ObjectId(999),
            controller: AI,
            kind: StackEntryKind::Spell {
                ability: Some(dummy_ability),
                card_id: CardId(999),
                casting_variant: Default::default(),
                actual_mana_spent: 0,
            },
        });
        let oid = add_instant_with_ability(
            &mut state,
            2,
            2,
            Effect::CopySpell {
                target: TargetFilter::Any,
            },
        );
        let (context, config) = make_context(0.8);
        let decision = decision();
        let candidate = cast_spell_action(oid);
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };
        let verdict = SpellslingerCastingPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert!(
                    delta >= 1.5,
                    "copy with stack target should score >= 1.5, got {delta}"
                );
                assert_eq!(reason.kind, "spellslinger_copy_with_target");
            }
            _ => panic!("expected Score"),
        }
    }

    #[test]
    fn copy_spell_no_target_scores_low() {
        // CopySpell with empty stack → +0.2 (lower than copy-with-target +1.5).
        // A MV-5 (expensive) non-instant copy spell gets no cheap-spell bonus — only +0.2.
        // CR 707.10: copy is only useful when there is a spell to copy.
        let mut state = GameState::new_two_player(42);
        // Use a MV 5 sorcery to avoid triggering the cheap-spell bonus.
        let card_id = CardId(50);
        let oid = create_object(
            &mut state,
            card_id,
            AI,
            "Expensive Copy".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&oid).unwrap();
        obj.card_types = CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Sorcery],
            subtypes: Vec::new(),
        };
        obj.mana_cost = ManaCost::generic(5); // not low-curve
        Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::CopySpell {
                target: TargetFilter::Any,
            },
        ));
        let (context, config) = make_context(0.8);
        let decision = decision();
        let candidate = cast_spell_action(oid);
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };
        let verdict = SpellslingerCastingPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                // +0.2 only (no cheap bonus, no stack), and significantly less than
                // the copy-with-target score (+1.5).
                assert!(
                    delta <= 0.5,
                    "copy with no stack target should score ≤ 0.5, got {delta}"
                );
                assert_eq!(reason.kind, "spellslinger_copy_no_target");
            }
            _ => panic!("expected Score"),
        }
    }

    #[test]
    fn burn_with_prowess_on_board_stacks() {
        // Burn spell + prowess creatures on board → elevated score. CR 120.3 + CR 702.108a.
        let mut state = GameState::new_two_player(42);
        add_creature_with_prowess(&mut state, 200, Zone::Battlefield);
        add_creature_with_prowess(&mut state, 201, Zone::Battlefield);
        let oid = add_instant_with_ability(
            &mut state,
            1,
            1,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
            },
        );
        let (context, config) = make_context(0.8);
        let decision = decision();
        let candidate = cast_spell_action(oid);
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };
        let verdict = SpellslingerCastingPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                // 0.6 (cheap) + 0.3 (burn base) + 2 * 0.5 (prowess) = 1.9
                assert!(delta > 1.0, "burn+prowess should score > 1.0, got {delta}");
                assert_eq!(reason.kind, "spellslinger_burn_with_prowess");
            }
            _ => panic!("expected Score"),
        }
    }

    #[test]
    fn expensive_offstrat_spell_scores_negative() {
        // Expensive non-IS non-payoff card → -0.4. CR 202.3b.
        let mut state = GameState::new_two_player(42);
        let card_id = CardId(999);
        let oid = create_object(
            &mut state,
            card_id,
            AI,
            "Giant Dragon".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&oid).unwrap();
        obj.card_types = CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Creature],
            subtypes: Vec::new(),
        };
        obj.mana_cost = ManaCost::generic(6); // MV 6 > 4
        let (context, config) = make_context(0.8);
        let decision = decision();
        let candidate = cast_spell_action(oid);
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };
        let verdict = SpellslingerCastingPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert!(
                    delta < 0.0,
                    "off-strategy expensive spell should score negative, got {delta}"
                );
                assert_eq!(reason.kind, "spellslinger_off_strategy");
            }
            _ => panic!("expected Score"),
        }
    }

    #[test]
    fn non_castspell_action_returns_zero() {
        // A non-CastSpell action should return delta=0. CR 601.2i.
        let state = GameState::new_two_player(42);
        let (context, config) = make_context(0.8);
        let decision = decision();
        let candidate = CandidateAction {
            action: GameAction::PassPriority,
            metadata: ActionMetadata {
                actor: Some(AI),
                tactical_class: TacticalClass::Pass,
            },
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };
        let verdict = SpellslingerCastingPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(delta, 0.0);
                assert_eq!(reason.kind, "spellslinger_casting_na");
            }
            _ => panic!("expected Score"),
        }
    }
}
