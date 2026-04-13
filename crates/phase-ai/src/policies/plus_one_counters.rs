//! +1/+1 counters tactical policy.
//!
//! Scores counter-related spell casts and ability activations for decks
//! committed to the +1/+1 counters axis. Opts out below `COMMITMENT_FLOOR`.
//!
//! CR 122.1a: +1/+1 counters add to power and toughness.
//! CR 122.6: counter-placement events trigger counter-payoff abilities.
//! CR 701.34: proliferate adds one counter of each kind to chosen permanents.
//! CR 614.1a: doubling replacements modify counter quantities.

use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::features::plus_one_counters::{
    ability_places_plus_one_counter, ability_proliferates, COMMITMENT_FLOOR, COMMITTED_VALUE_FLOOR,
};
use crate::features::DeckFeatures;

pub struct PlusOneCountersPolicy;

impl TacticalPolicy for PlusOneCountersPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::PlusOneCountersTactical
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
        if features.plus_one_counters.commitment < COMMITMENT_FLOOR {
            None
        } else {
            Some(features.plus_one_counters.commitment)
        }
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        // Determine source object and ability index from the action.
        // CastSpell uses object_id with spell ability at index 0.
        // ActivateAbility uses source_id with the given ability_index.
        let (source_id, ability_index) = match &ctx.candidate.action {
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            } => (*source_id, *ability_index),
            GameAction::CastSpell { object_id, .. } => (*object_id, 0),
            _ => {
                return PolicyVerdict::Score {
                    delta: 0.0,
                    reason: PolicyReason::new("plus_one_counters_na"),
                };
            }
        };

        let Some(object) = ctx.state.objects.get(&source_id) else {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("plus_one_counters_na"),
            };
        };

        let Some(ability) = object.abilities.get(ability_index) else {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("plus_one_counters_na"),
            };
        };

        let features = ctx
            .context
            .session
            .features
            .get(&ctx.ai_player)
            .cloned()
            .unwrap_or_default();

        // Branch 1: Proliferate ability. CR 701.34.
        if ability_proliferates(ability) {
            let things_with_counters = count_permanents_with_counters(ctx.state, ctx.ai_player);
            if things_with_counters > 0 {
                return PolicyVerdict::Score {
                    delta: 2.0,
                    reason: PolicyReason::new("proliferate_with_targets")
                        .with_fact("things_with_counters", things_with_counters as i64),
                };
            } else {
                return PolicyVerdict::Score {
                    delta: -1.5,
                    reason: PolicyReason::new("proliferate_no_targets"),
                };
            }
        }

        // Branch 2: Counter generator. CR 122.1a + CR 122.6.
        if ability_places_plus_one_counter(ability) {
            let creatures_on_board = count_creatures_on_board(ctx.state, ctx.ai_player);
            if creatures_on_board > 0 {
                return PolicyVerdict::Score {
                    delta: 1.5,
                    reason: PolicyReason::new("counter_generator_with_targets")
                        .with_fact("creatures_on_board", creatures_on_board as i64),
                };
            } else {
                return PolicyVerdict::Score {
                    delta: -0.8,
                    reason: PolicyReason::new("counter_generator_no_targets"),
                };
            }
        }

        // Branch 3: Doubler synergy — source has doubler AND a pending counter
        // effect is on the stack. CR 614.1a.
        let source_has_doubler = object.replacement_definitions.iter().any(|r| {
            r.event == engine::types::replacements::ReplacementEvent::AddCounter
                && r.quantity_modification.is_some()
        });
        if source_has_doubler && stack_has_pending_counter_effect(ctx.state) {
            return PolicyVerdict::Score {
                delta: 1.2,
                reason: PolicyReason::new("doubler_active_with_pending_counter"),
            };
        }

        // Branch 4: Payoff cast with active +1/+1 counters on board. CR 122.6 + CR 613.1f.
        let is_payoff = features
            .plus_one_counters
            .payoff_names
            .iter()
            .any(|n| n == &object.name);
        if is_payoff && features.plus_one_counters.commitment >= COMMITTED_VALUE_FLOOR {
            let has_counters = any_creature_with_p1p1_on_board(ctx.state, ctx.ai_player);
            if has_counters {
                return PolicyVerdict::Score {
                    delta: 1.0,
                    reason: PolicyReason::new("payoff_with_active_counters"),
                };
            }
        }

        PolicyVerdict::Score {
            delta: 0.0,
            reason: PolicyReason::new("plus_one_counters_na"),
        }
    }
}

/// Count AI-controlled creatures on the battlefield. CR 122.1a: +1/+1 counter
/// generators need valid targets to be useful.
fn count_creatures_on_board(state: &GameState, player: PlayerId) -> usize {
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|obj| {
            obj.controller == player
                && obj.zone == Zone::Battlefield
                && obj
                    .card_types
                    .core_types
                    .contains(&engine::types::card_type::CoreType::Creature)
        })
        .count()
}

/// Count permanents with at least one counter on the battlefield (any controller).
/// CR 701.34: proliferate operates on any permanent with a counter.
fn count_permanents_with_counters(state: &GameState, player: PlayerId) -> usize {
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|obj| {
            obj.zone == Zone::Battlefield
                && (obj.controller == player
                    || state
                        .players
                        .iter()
                        .any(|p| p.id != player && p.poison_counters > 0))
                && !obj.counters.is_empty()
        })
        .count()
}

/// True if the current stack contains a pending AddCounter / PutCounter event.
/// Used by the doubler branch to detect "I can double this counter effect".
/// CR 614.1a: replacement applies at the moment the counter is added.
fn stack_has_pending_counter_effect(state: &GameState) -> bool {
    state.stack.iter().any(|entry| {
        let Some(resolved) = entry.ability() else {
            return false;
        };
        effect_has_p1p1_counter(&resolved.effect)
    })
}

/// Checks whether an effect places a P1P1 counter.
/// CR 614.1a: replacement applies to any counter-placement event.
fn effect_has_p1p1_counter(effect: &engine::types::ability::Effect) -> bool {
    use engine::types::ability::Effect;
    matches!(
        effect,
        Effect::AddCounter { counter_type, .. } | Effect::PutCounter { counter_type, .. }
            if counter_type == "P1P1"
    )
}

/// True if any AI-controlled creature on the battlefield has at least one
/// +1/+1 counter. CR 122.1a: counter presence required for payoff activation.
fn any_creature_with_p1p1_on_board(state: &GameState, player: PlayerId) -> bool {
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|obj| {
            obj.controller == player
                && obj.zone == Zone::Battlefield
                && obj
                    .card_types
                    .core_types
                    .contains(&engine::types::card_type::CoreType::Creature)
        })
        .any(|obj| {
            obj.counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0)
                > 0
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiConfig;
    use crate::context::AiContext;
    use crate::features::plus_one_counters::PlusOneCountersFeature;
    use crate::features::DeckFeatures;
    use crate::session::AiSession;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter,
    };
    use engine::types::card_type::{CardType, CoreType};
    use engine::types::counter::CounterType;
    use engine::types::game_state::{GameState, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;
    use std::sync::Arc;

    const AI: PlayerId = PlayerId(0);

    fn make_generator_ability() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::AddCounter {
                counter_type: "P1P1".to_string(),
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Any,
            },
        )
    }

    fn make_proliferate_ability() -> AbilityDefinition {
        AbilityDefinition::new(AbilityKind::Activated, Effect::Proliferate)
    }

    fn context_with_commitment(
        commitment: f32,
        payoff_names: Vec<String>,
    ) -> (AiContext, AiConfig) {
        let config = AiConfig::default();
        let mut session = AiSession::empty();
        let features = DeckFeatures {
            plus_one_counters: PlusOneCountersFeature {
                generator_count: 4,
                proliferate_count: 2,
                doubler_count: 0,
                payoff_count: payoff_names.len() as u32,
                etb_with_counters_count: 2,
                commitment,
                payoff_names,
            },
            ..DeckFeatures::default()
        };
        session.features.insert(AI, features);
        let mut context = AiContext::empty(&config.weights);
        context.session = Arc::new(session);
        context.player = AI;
        (context, config)
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

    fn add_creature(state: &mut GameState, card_idx: u64, zone: Zone) -> ObjectId {
        let oid = create_object(
            state,
            CardId(card_idx),
            AI,
            format!("Creature {card_idx}"),
            zone,
        );
        state.objects.get_mut(&oid).unwrap().card_types = CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Creature],
            subtypes: Vec::new(),
        };
        oid
    }

    // ─── activation() tests ───────────────────────────────────────────────────

    #[test]
    fn opts_out_below_commitment_floor() {
        let features = DeckFeatures::default(); // commitment = 0.0
        let state = GameState::new_two_player(42);
        assert!(PlusOneCountersPolicy
            .activation(&features, &state, AI)
            .is_none());
    }

    #[test]
    fn opts_in_above_floor() {
        let features = DeckFeatures {
            plus_one_counters: PlusOneCountersFeature {
                commitment: 0.5,
                ..Default::default()
            },
            ..DeckFeatures::default()
        };
        let state = GameState::new_two_player(42);
        assert!(PlusOneCountersPolicy
            .activation(&features, &state, AI)
            .is_some());
    }

    // ─── verdict() — generator tests ─────────────────────────────────────────

    #[test]
    fn generator_with_creatures_on_board_scored_positively() {
        let mut state = GameState::new_two_player(42);
        let gen_id = create_object(
            &mut state,
            CardId(1),
            AI,
            "Hardened Gen".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&gen_id)
            .unwrap()
            .abilities
            .push(make_generator_ability());
        // Add a creature target on board.
        let _creature = add_creature(&mut state, 2, Zone::Battlefield);

        let candidate = activate_candidate(gen_id, 0);
        let decision = decision();
        let (context, config) = context_with_commitment(0.9, vec![]);
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };

        let verdict = PlusOneCountersPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "counter_generator_with_targets");
                assert!(delta > 0.0, "expected positive delta, got {delta}");
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected Reject"),
        }
    }

    #[test]
    fn generator_with_no_creatures_penalized() {
        let mut state = GameState::new_two_player(42);
        let gen_id = create_object(
            &mut state,
            CardId(1),
            AI,
            "Lonely Gen".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&gen_id)
            .unwrap()
            .abilities
            .push(make_generator_ability());
        // No creatures on board.

        let candidate = activate_candidate(gen_id, 0);
        let decision = decision();
        let (context, config) = context_with_commitment(0.9, vec![]);
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };

        let verdict = PlusOneCountersPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "counter_generator_no_targets");
                assert!(delta < 0.0, "expected negative delta, got {delta}");
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected Reject"),
        }
    }

    // ─── verdict() — proliferate tests ───────────────────────────────────────

    #[test]
    fn proliferate_with_counters_present_strongly_positive() {
        let mut state = GameState::new_two_player(42);
        let gen_id = create_object(
            &mut state,
            CardId(1),
            AI,
            "Proliferator".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&gen_id)
            .unwrap()
            .abilities
            .push(make_proliferate_ability());
        // Add a creature with a counter.
        let creature_id = add_creature(&mut state, 2, Zone::Battlefield);
        state
            .objects
            .get_mut(&creature_id)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 1);

        let candidate = activate_candidate(gen_id, 0);
        let decision = decision();
        let (context, config) = context_with_commitment(0.9, vec![]);
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };

        let verdict = PlusOneCountersPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "proliferate_with_targets");
                assert!(delta > 1.5, "expected delta > 1.5, got {delta}");
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected Reject"),
        }
    }

    #[test]
    fn proliferate_no_counters_penalized() {
        let mut state = GameState::new_two_player(42);
        let gen_id = create_object(
            &mut state,
            CardId(1),
            AI,
            "Proliferator".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&gen_id)
            .unwrap()
            .abilities
            .push(make_proliferate_ability());
        // No permanents with counters.

        let candidate = activate_candidate(gen_id, 0);
        let decision = decision();
        let (context, config) = context_with_commitment(0.9, vec![]);
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };

        let verdict = PlusOneCountersPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "proliferate_no_targets");
                assert!(delta < 0.0, "expected negative delta, got {delta}");
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected Reject"),
        }
    }

    // ─── verdict() — non-counter spell ───────────────────────────────────────

    #[test]
    fn non_counter_spell_yields_na() {
        let mut state = GameState::new_two_player(42);
        let draw_id = create_object(
            &mut state,
            CardId(1),
            AI,
            "Brainstorm".to_string(),
            Zone::Battlefield,
        );
        let draw_ability = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
            },
        );
        state
            .objects
            .get_mut(&draw_id)
            .unwrap()
            .abilities
            .push(draw_ability);

        let candidate = activate_candidate(draw_id, 0);
        let decision = decision();
        let (context, config) = context_with_commitment(0.9, vec![]);
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };

        let verdict = PlusOneCountersPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "plus_one_counters_na");
                assert_eq!(delta, 0.0);
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected Reject"),
        }
    }
}
