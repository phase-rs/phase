//! Aggro pressure tactical policy.
//!
//! Scores `DeclareAttackers` and `CastSpell` candidates to bias aggro decks
//! toward maximising attack pressure, committing cheap threats to the board
//! early, and leveraging burn finishers when the opponent is low.
//!
//! CR 508.1: declaring attackers. CR 120.3: damage to players.
//! CR 202.3: mana value. CR 702.10: haste.

use engine::game::players;
use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::features::aggro_pressure::{
    is_burn_spell_parts, is_evasion_creature_parts, is_hasty_creature_parts,
    is_low_curve_creature_parts, AGGRO_COMMITMENT_FLOOR,
};
use crate::features::DeckFeatures;

/// Opponent life total threshold for burn-finisher bonus. CR 120.3.
const BURN_FINISHER_LIFE_THRESHOLD: i32 = 6;

pub struct AggroPressurePolicy;

impl TacticalPolicy for AggroPressurePolicy {
    fn id(&self) -> PolicyId {
        PolicyId::AggroPressure
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::DeclareAttackers, DecisionKind::CastSpell]
    }

    /// Opt out below `AGGRO_COMMITMENT_FLOOR`; otherwise return `Some(commitment)`
    /// so the registry scales verdict deltas by the feature strength.
    fn activation(
        &self,
        features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        let commitment = features.aggro_pressure.commitment;
        if commitment < AGGRO_COMMITMENT_FLOOR {
            None
        } else {
            Some(commitment)
        }
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        match &ctx.candidate.action {
            GameAction::DeclareAttackers { attacks } => score_declare_attackers(ctx, attacks),
            GameAction::CastSpell { object_id, .. } => score_cast_spell(ctx, *object_id),
            _ => PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("aggro_cast_na"),
            },
        }
    }
}

/// Score a `DeclareAttackers` candidate.
///
/// CR 508.1: each declared attacker must be able to attack. A non-empty attack
/// set earns a bonus proportional to attacker count and total power. An empty
/// attack set when eligible attackers exist earns a penalty — in an aggro deck,
/// failing to apply pressure is a tempo loss.
fn score_declare_attackers(
    ctx: &PolicyContext<'_>,
    attacks: &[(
        engine::types::identifiers::ObjectId,
        engine::game::combat::AttackTarget,
    )],
) -> PolicyVerdict {
    let attack_count = attacks.len() as f64;

    if attack_count > 0.0 {
        // Sum the power of all attacking creatures. CR 508.1 + CR 302.4a.
        let total_power: f64 = attacks
            .iter()
            .filter_map(|(id, _)| ctx.state.objects.get(id))
            .map(|obj| obj.power.unwrap_or(0).max(0) as f64)
            .sum();

        // Bonus: 0.5 per attacker + 0.3 per total power, capped at 5.0.
        let delta = (0.5 * attack_count + 0.3 * total_power).min(5.0);
        return PolicyVerdict::Score {
            delta,
            reason: PolicyReason::new("aggro_attack_pressure")
                .with_fact("attack_count", attack_count as i64)
                .with_fact("total_power", total_power as i64),
        };
    }

    // Empty attack set — check if there were eligible attackers.
    let eligible_attackers = count_eligible_attackers(ctx);
    if eligible_attackers > 0 {
        return PolicyVerdict::Score {
            delta: -1.5,
            reason: PolicyReason::new("aggro_skipped_attack")
                .with_fact("eligible_attackers", eligible_attackers as i64),
        };
    }

    PolicyVerdict::Score {
        delta: 0.0,
        reason: PolicyReason::new("aggro_attack_pressure"),
    }
}

/// Score a `CastSpell` candidate.
///
/// Three bonus cases:
/// 1. Low-curve creature → +0.6 (deploy early threats). CR 302 + CR 202.3.
/// 2. Hasty creature cast post-combat (Main 2) → +0.4. CR 702.10b.
/// 3. Burn spell targeting player when opponent life ≤ 6 → +1.5. CR 120.3.
fn score_cast_spell(
    ctx: &PolicyContext<'_>,
    object_id: engine::types::identifiers::ObjectId,
) -> PolicyVerdict {
    let Some(obj) = ctx.state.objects.get(&object_id) else {
        return PolicyVerdict::Score {
            delta: 0.0,
            reason: PolicyReason::new("aggro_cast_na"),
        };
    };

    let core_types = &obj.card_types.core_types;
    let keywords = &obj.keywords;
    let static_defs = obj.static_definitions.as_slice();
    let abilities = &obj.abilities;
    let mana_cost = &obj.mana_cost;

    // Case 1: low-curve creature priority. CR 302 + CR 202.3.
    if is_low_curve_creature_parts(core_types, mana_cost) {
        return PolicyVerdict::Score {
            delta: 0.6,
            reason: PolicyReason::new("aggro_cheap_creature_priority")
                .with_fact("mana_value", mana_cost.mana_value() as i64),
        };
    }

    // Case 2: hasty creature cast post-combat. CR 702.10b.
    if is_hasty_creature_parts(core_types, keywords, static_defs)
        && ctx.state.phase == Phase::PostCombatMain
    {
        return PolicyVerdict::Score {
            delta: 0.4,
            reason: PolicyReason::new("aggro_hasty_postcombat"),
        };
    }

    // Case 3: burn finisher when opponent is low. CR 120.3.
    if is_burn_spell_parts(core_types, abilities) {
        let min_opp_life = min_opponent_life(ctx.state, ctx.ai_player);
        if let Some(life) = min_opp_life {
            if life <= BURN_FINISHER_LIFE_THRESHOLD {
                return PolicyVerdict::Score {
                    delta: 1.5,
                    reason: PolicyReason::new("aggro_burn_finisher")
                        .with_fact("opponent_life", life as i64),
                };
            }
        }
    }

    // Mild bonus to evasion creatures as threats.
    if is_evasion_creature_parts(core_types, keywords, static_defs) {
        return PolicyVerdict::Score {
            delta: 0.3,
            reason: PolicyReason::new("aggro_evasion_threat"),
        };
    }

    PolicyVerdict::Score {
        delta: 0.0,
        reason: PolicyReason::new("aggro_cast_na"),
    }
}

/// Count eligible attackers from the `WaitingFor::DeclareAttackers` context.
/// Uses the pre-computed `valid_attacker_ids` list — the engine already
/// enforces CR 508.1a (summoning sickness, tap, Defender, etc.). CR 508.1.
fn count_eligible_attackers(ctx: &PolicyContext<'_>) -> usize {
    match &ctx.decision.waiting_for {
        engine::types::game_state::WaitingFor::DeclareAttackers {
            valid_attacker_ids, ..
        } => valid_attacker_ids.len(),
        _ => 0,
    }
}

/// Minimum life total among the AI's opponents. Returns `None` if there are
/// no opponents (shouldn't happen in a real game).
fn min_opponent_life(state: &GameState, ai_player: PlayerId) -> Option<i32> {
    players::opponents(state, ai_player)
        .into_iter()
        .map(|pid| state.players[pid.0 as usize].life)
        .min()
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiConfig;
    use crate::context::AiContext;
    use crate::features::aggro_pressure::AggroPressureFeature;
    use crate::features::DeckFeatures;
    use crate::session::AiSession;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter,
    };
    use engine::types::card_type::{CardType, CoreType};
    use engine::types::game_state::{GameState, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::keywords::Keyword;
    use engine::types::mana::ManaCost;
    use engine::types::phase::Phase;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;
    use std::sync::Arc;

    const AI: PlayerId = PlayerId(0);
    const OPP: PlayerId = PlayerId(1);

    fn features_with_commitment(commitment: f32) -> DeckFeatures {
        DeckFeatures {
            aggro_pressure: AggroPressureFeature {
                commitment,
                low_curve_creature_count: 12,
                hasty_creature_count: 4,
                evasion_creature_count: 4,
                burn_spell_count: 8,
                combat_pump_count: 4,
                total_nonland: 36,
                low_curve_density: 0.33,
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

    fn declare_attackers_decision() -> AiDecisionContext {
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

    fn add_creature(state: &mut GameState, id: u64, mv: u32, kw: Option<Keyword>) -> ObjectId {
        let card_id = CardId(id);
        let oid = create_object(state, card_id, AI, format!("Creature {id}"), Zone::Hand);
        let obj = state.objects.get_mut(&oid).unwrap();
        obj.card_types = CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Creature],
            subtypes: Vec::new(),
        };
        obj.mana_cost = ManaCost::generic(mv);
        obj.power = Some(mv as i32);
        obj.toughness = Some(mv as i32);
        if let Some(k) = kw {
            obj.keywords.push(k);
        }
        oid
    }

    fn add_battlefield_creature(state: &mut GameState, id: u64, power: i32) -> ObjectId {
        let card_id = CardId(id);
        let oid = create_object(
            state,
            card_id,
            AI,
            format!("Attacker {id}"),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&oid).unwrap();
        obj.card_types = CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Creature],
            subtypes: Vec::new(),
        };
        obj.power = Some(power);
        obj.toughness = Some(power);
        state.battlefield.push_back(oid);
        oid
    }

    fn add_burn_spell(state: &mut GameState, id: u64) -> ObjectId {
        let card_id = CardId(id);
        let oid = create_object(state, card_id, AI, format!("Bolt {id}"), Zone::Hand);
        let mut ability = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
            },
        );
        ability.kind = AbilityKind::Spell;
        let obj = state.objects.get_mut(&oid).unwrap();
        obj.card_types = CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Instant],
            subtypes: Vec::new(),
        };
        obj.mana_cost = ManaCost::generic(1);
        obj.abilities.push(ability);
        oid
    }

    // ── Activation tests ──────────────────────────────────────────────────

    #[test]
    fn activation_opts_out_below_floor() {
        let features = features_with_commitment(0.3); // below 0.45
        let state = GameState::new_two_player(42);
        assert!(AggroPressurePolicy
            .activation(&features, &state, AI)
            .is_none());
    }

    #[test]
    fn non_aggro_deck_skips_via_activation() {
        let features = DeckFeatures::default(); // commitment = 0.0
        let state = GameState::new_two_player(42);
        assert!(AggroPressurePolicy
            .activation(&features, &state, AI)
            .is_none());
    }

    // ── CastSpell tests ───────────────────────────────────────────────────

    #[test]
    fn cast_low_curve_creature_scores_positive() {
        let mut state = GameState::new_two_player(42);
        let oid = add_creature(&mut state, 1, 1, None); // MV 1 creature
        let card_id = CardId(1);

        let candidate = cast_candidate(oid, card_id);
        let (context, config) = context_with_features(features_with_commitment(0.8));
        let ctx = PolicyContext {
            state: &state,
            decision: &decision(),
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };

        let verdict = AggroPressurePolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert!(
                    delta > 0.0,
                    "expected positive delta for low-curve creature"
                );
                assert_eq!(reason.kind, "aggro_cheap_creature_priority");
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected Reject"),
        }
    }

    #[test]
    fn cast_burn_with_low_opponent_life_scores_high() {
        let mut state = GameState::new_two_player(42);
        state.players[OPP.0 as usize].life = 4; // ≤ 6

        let oid = add_burn_spell(&mut state, 10);
        let card_id = CardId(10);

        let candidate = cast_candidate(oid, card_id);
        let (context, config) = context_with_features(features_with_commitment(0.8));
        let ctx = PolicyContext {
            state: &state,
            decision: &decision(),
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };

        let verdict = AggroPressurePolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert!(
                    delta >= 1.5,
                    "expected high delta for burn finisher, got {delta}"
                );
                assert_eq!(reason.kind, "aggro_burn_finisher");
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected Reject"),
        }
    }

    // ── DeclareAttackers tests ────────────────────────────────────────────

    #[test]
    fn declare_attackers_with_attackers_scores_positive() {
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::DeclareAttackers;

        let oid = add_battlefield_creature(&mut state, 20, 3);
        let attacks = vec![(oid, engine::game::combat::AttackTarget::Player(OPP))];

        let candidate = attack_candidate(attacks);
        let (context, config) = context_with_features(features_with_commitment(0.8));
        let ctx = PolicyContext {
            state: &state,
            decision: &declare_attackers_decision(),
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };

        let verdict = AggroPressurePolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert!(
                    delta > 0.0,
                    "expected positive delta for attack, got {delta}"
                );
                assert_eq!(reason.kind, "aggro_attack_pressure");
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected Reject"),
        }
    }

    #[test]
    fn declare_attackers_empty_set_penalized_when_eligible_creatures_exist() {
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::DeclareAttackers;

        // Add a non-sick, untapped creature on the battlefield.
        let oid = add_battlefield_creature(&mut state, 21, 2);

        // Populate valid_attacker_ids with the creature so count_eligible_attackers fires.
        let attacker_decision = AiDecisionContext {
            waiting_for: WaitingFor::DeclareAttackers {
                player: AI,
                valid_attacker_ids: vec![oid],
                valid_attack_targets: vec![],
            },
            candidates: Vec::new(),
        };

        let candidate = attack_candidate(vec![]); // empty attacks
        let (context, config) = context_with_features(features_with_commitment(0.8));
        let ctx = PolicyContext {
            state: &state,
            decision: &attacker_decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };

        let verdict = AggroPressurePolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert!(
                    delta < 0.0,
                    "expected penalty for skipped attack, got {delta}"
                );
                assert_eq!(reason.kind, "aggro_skipped_attack");
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected Reject"),
        }
    }
}
