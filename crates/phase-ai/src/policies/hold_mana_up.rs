//! `HoldManaUpForInteractionPolicy` — bias the AI toward holding mana available
//! for instant-speed interaction on the opponent's turn.
//!
//! This policy activates only when the deck has meaningful instant-speed
//! interaction density (`reactive_tempo`). A sorcery-heavy control deck (many
//! sweepers, few instants) scores high `commitment` but near-zero
//! `reactive_tempo` and therefore does NOT receive this bias.
//!
//! **Relationship to `InteractionReservationPolicy`**: that policy scores
//! `PassPriority` (early-returns on any other candidate); this policy scores
//! `CastSpell` and `ActivateAbility`. They fire on **disjoint candidate sets**
//! for the same game state — `InteractionReservationPolicy` rewards passing
//! to keep mana open, `HoldManaUpForInteractionPolicy` penalizes a cast that
//! would tap out below the cheapest instant in hand. Together they cover both
//! sides of the same decision without double-scoring any single candidate.
//!
//! CR 117.1a + CR 117.3a: a player may cast instants and activate abilities any
//! time they have priority — leaving mana open preserves these options on the
//! opponent's turn.
//!
//! CR 117.1a: instant spells can be cast any time a player has priority.
//! CR 304.1: casting an instant uses the stack.

use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::features::control::REACTIVE_TEMPO_FLOOR;
use crate::features::mana_ramp::is_mana_dork_parts;
use crate::features::DeckFeatures;

/// Penalty applied when the AI would tap out below the cost of the cheapest
/// instant in hand — it can no longer use its interaction this turn cycle.
const TAP_OUT_PENALTY: f64 = -0.4;

pub struct HoldManaUpForInteractionPolicy;

impl TacticalPolicy for HoldManaUpForInteractionPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::HoldManaUpForInteraction
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::CastSpell, DecisionKind::ActivateAbility]
    }

    fn activation(
        &self,
        features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        // This policy uses reactive_tempo, NOT commitment — a sorcery-heavy
        // control deck should not be biased toward holding mana up.
        if features.control.reactive_tempo < REACTIVE_TEMPO_FLOOR {
            return None;
        }
        Some(features.control.reactive_tempo)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        // Only applies on the AI's own main phase (pre- or post-combat).
        if !is_own_main_phase_cast(ctx) {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("hold_mana_up_na"),
            };
        }

        let ai_player = ctx.ai_player;
        let mana_before = count_untapped_mana_sources(ctx.state, ai_player);
        let spell_cost = spell_mana_value(ctx);

        // If the spell costs all or more of our available mana we'd tap out.
        // Only penalize if this leaves us unable to cast any instant we hold.
        let mana_after = mana_before.saturating_sub(spell_cost);
        let min_instant_cmc = min_instant_cmc_in_hand(ctx.state, ai_player);

        if let Some(min_cmc) = min_instant_cmc {
            if mana_after < min_cmc {
                return PolicyVerdict::Score {
                    delta: TAP_OUT_PENALTY,
                    reason: PolicyReason::new("hold_mana_up_tap_out")
                        .with_fact("min_instant_cmc", min_cmc as i64)
                        .with_fact("mana_after_cast", mana_after as i64),
                };
            }
        }

        PolicyVerdict::Score {
            delta: 0.0,
            reason: PolicyReason::new("hold_mana_up_ok"),
        }
    }
}

/// True when the candidate is a spell cast on the AI's own main phase with an
/// empty stack — the window where holding mana up is meaningful.
fn is_own_main_phase_cast(ctx: &PolicyContext<'_>) -> bool {
    if !matches!(
        ctx.state.phase,
        engine::types::phase::Phase::PreCombatMain | engine::types::phase::Phase::PostCombatMain
    ) {
        return false;
    }
    if !ctx.state.stack.is_empty() {
        return false;
    }
    let is_active = engine::game::turn_control::turn_decision_maker(ctx.state) == ctx.ai_player;
    if !is_active {
        return false;
    }
    matches!(
        ctx.candidate.action,
        GameAction::CastSpell { .. } | GameAction::ActivateAbility { .. }
    )
}

/// Count the number of untapped mana sources (lands + mana dorks/rocks) the
/// AI controls. Used as a proxy for available mana this turn.
///
/// CR 305.1: basic lands tap to produce mana. Mana rocks/dorks are detected
/// structurally via `features::mana_ramp::is_mana_dork_parts`, which already
/// gates on creature/artifact + tap-cost + chain-has-mana — Sol Ring, Mind
/// Stone, Llanowar Elves, Birds of Paradise all qualify.
fn count_untapped_mana_sources(state: &GameState, player: PlayerId) -> u32 {
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|obj| obj.controller == player && !obj.tapped)
        .filter(|obj| {
            obj.card_types.core_types.contains(&CoreType::Land)
                || is_mana_dork_parts(&obj.card_types.core_types, &obj.abilities)
        })
        .count() as u32
}

/// Extract the mana value of the candidate spell or ability's cost, as a
/// proxy for how much mana it will consume. Returns 0 for non-spell actions.
fn spell_mana_value(ctx: &PolicyContext<'_>) -> u32 {
    match &ctx.candidate.action {
        GameAction::CastSpell { object_id, .. } => ctx
            .state
            .objects
            .get(object_id)
            .map(|obj| obj.mana_cost.mana_value())
            .unwrap_or(0),
        _ => 0,
    }
}

/// Find the minimum mana value among instants in the AI player's hand.
/// Returns `None` if the hand has no instants.
fn min_instant_cmc_in_hand(state: &GameState, player: PlayerId) -> Option<u32> {
    let p = state.players.get(player.0 as usize)?;
    p.hand
        .iter()
        .filter_map(|&oid| state.objects.get(&oid))
        .filter(|obj| obj.card_types.core_types.contains(&CoreType::Instant))
        .map(|obj| obj.mana_cost.mana_value())
        .min()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiConfig;
    use crate::context::AiContext;
    use crate::features::control::ControlFeature;
    use crate::features::DeckFeatures;
    use crate::session::AiSession;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{AbilityDefinition, AbilityKind, Effect, QuantityExpr};
    use engine::types::card_type::{CardType, CoreType};
    use engine::types::game_state::{GameState, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::mana::ManaCost;
    use engine::types::phase::Phase;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;
    use std::sync::Arc;

    const AI: PlayerId = PlayerId(0);

    fn control_features(reactive_tempo: f32) -> DeckFeatures {
        DeckFeatures {
            control: ControlFeature {
                counterspell_count: 4,
                reactive_tempo,
                commitment: 0.8,
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

    fn main_phase_state() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::PreCombatMain;
        state
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

    fn priority_decision() -> AiDecisionContext {
        AiDecisionContext {
            waiting_for: WaitingFor::Priority { player: AI },
            candidates: Vec::new(),
        }
    }

    fn make_land(state: &mut GameState, idx: u64) -> ObjectId {
        let oid = create_object(
            state,
            CardId(3000 + idx),
            AI,
            format!("Forest {idx}"),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&oid).unwrap();
        obj.card_types = CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Land],
            subtypes: Vec::new(),
        };
        obj.tapped = false;
        oid
    }

    fn make_instant_in_hand(state: &mut GameState, idx: u64, mana_value: u32) -> ObjectId {
        let oid = create_object(
            state,
            CardId(4000 + idx),
            AI,
            format!("Instant {idx}"),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&oid).unwrap();
        obj.card_types = CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Instant],
            subtypes: Vec::new(),
        };
        obj.mana_cost = ManaCost::generic(mana_value);
        Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Counter {
                target: engine::types::ability::TargetFilter::Any,
                source_static: None,
                unless_payment: None,
            },
        ));
        oid
    }

    fn make_sorcery_in_hand(state: &mut GameState, idx: u64, mana_value: u32) -> ObjectId {
        let oid = create_object(
            state,
            CardId(5000 + idx),
            AI,
            format!("Sorcery {idx}"),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&oid).unwrap();
        obj.card_types = CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Sorcery],
            subtypes: Vec::new(),
        };
        obj.mana_cost = ManaCost::generic(mana_value);
        Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 2 },
                target: engine::types::ability::TargetFilter::Controller,
                up_to: false,
            },
        ));
        oid
    }

    #[test]
    fn activation_opts_out_below_reactive_tempo_floor() {
        let features = control_features(0.1);
        let state = GameState::new_two_player(42);
        assert!(HoldManaUpForInteractionPolicy
            .activation(&features, &state, AI)
            .is_none());
    }

    #[test]
    fn activation_active_above_floor() {
        let features = control_features(0.5);
        let state = GameState::new_two_player(42);
        assert!(HoldManaUpForInteractionPolicy
            .activation(&features, &state, AI)
            .is_some());
    }

    #[test]
    fn tap_out_penalty_fires_when_instant_left_unplayable() {
        // Setup: 2 untapped lands, counterspell (cmc 2) in hand.
        // Candidate: cast a 2-cmc sorcery that taps out. 0 mana left < 2 (min instant cmc).
        let mut state = main_phase_state();
        make_land(&mut state, 0);
        make_land(&mut state, 1);

        let counter_id = make_instant_in_hand(&mut state, 0, 2);
        state.players[0].hand.push_back(counter_id);

        let sorcery_id = make_sorcery_in_hand(&mut state, 1, 2);
        state.players[0].hand.push_back(sorcery_id);

        let candidate = cast_candidate(sorcery_id, CardId(5001));
        let decision = priority_decision();
        let (context, config) = context_with_features(control_features(0.6));
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };

        let verdict = HoldManaUpForInteractionPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "hold_mana_up_tap_out");
                assert!(delta < 0.0, "expected penalty, got {delta}");
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected Reject"),
        }
    }

    #[test]
    fn no_penalty_when_enough_mana_remains() {
        // 4 untapped lands, counterspell (cmc 2), casting a 1-cmc spell.
        // After cast: 3 mana left ≥ 2 (min instant cmc) → no penalty.
        let mut state = main_phase_state();
        for i in 0..4 {
            make_land(&mut state, i);
        }
        let counter_id = make_instant_in_hand(&mut state, 0, 2);
        state.players[0].hand.push_back(counter_id);

        let cheap_spell_id = make_sorcery_in_hand(&mut state, 1, 1);
        state.players[0].hand.push_back(cheap_spell_id);

        let candidate = cast_candidate(cheap_spell_id, CardId(5001));
        let decision = priority_decision();
        let (context, config) = context_with_features(control_features(0.6));
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };

        let verdict = HoldManaUpForInteractionPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "hold_mana_up_ok");
                assert_eq!(delta, 0.0);
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected Reject"),
        }
    }

    #[test]
    fn no_penalty_when_no_instants_in_hand() {
        // No instants in hand → min_instant_cmc is None → no penalty.
        let mut state = main_phase_state();
        make_land(&mut state, 0);
        make_land(&mut state, 1);

        let spell_id = make_sorcery_in_hand(&mut state, 0, 2);
        state.players[0].hand.push_back(spell_id);

        let candidate = cast_candidate(spell_id, CardId(5000));
        let decision = priority_decision();
        let (context, config) = context_with_features(control_features(0.6));
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };

        let verdict = HoldManaUpForInteractionPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "hold_mana_up_ok");
                assert_eq!(delta, 0.0);
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected Reject"),
        }
    }
}
