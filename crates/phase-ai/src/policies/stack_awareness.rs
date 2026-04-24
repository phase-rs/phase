use engine::types::ability::{Effect, QuantityExpr, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, StackEntry, StackEntryKind};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::eval::evaluate_creature;
use crate::features::DeckFeatures;

use super::activation::turn_only;
use super::context::{collect_ability_effects, PolicyContext};
use super::effect_classify::{effect_polarity, is_spell_beneficial, EffectPolarity};
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};

pub struct StackAwarenessPolicy;

impl StackAwarenessPolicy {
    pub fn score(&self, ctx: &PolicyContext<'_>) -> f64 {
        match &ctx.candidate.action {
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(id)),
            } => score_target(ctx, *id),
            GameAction::SelectTargets { targets } => targets
                .iter()
                .map(|t| match t {
                    TargetRef::Object(id) => score_target(ctx, *id),
                    _ => 0.0,
                })
                .sum(),
            _ => 0.0,
        }
    }
}

impl TacticalPolicy for StackAwarenessPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::StackAwareness
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
        PolicyVerdict::Score {
            delta: self.score(ctx),
            reason: PolicyReason::new("stack_awareness_score"),
        }
    }
}

fn score_target(ctx: &PolicyContext<'_>, target_id: ObjectId) -> f64 {
    score_target_redundancy(ctx, target_id)
        + score_counter_target_value(ctx, target_id)
        + score_pump_response(ctx, target_id)
}

fn score_target_redundancy(ctx: &PolicyContext<'_>, target_id: ObjectId) -> f64 {
    if is_spell_beneficial(ctx) {
        return 0.0;
    }

    if !has_pending_removal(ctx.state, target_id) {
        return 0.0;
    }

    if will_target_die_from_stack(ctx.state, target_id) {
        ctx.penalties().redundant_removal_penalty
    } else {
        // Pending removal that might not kill — still penalize but less
        ctx.penalties().redundant_damage_penalty * 0.5
    }
}

/// When the AI is casting a counter spell, score the target stack entry by its
/// impact. Higher-value spells (by mana value, creature stats, effects) should be
/// preferred counter targets. Returns 0.0 if the pending spell is not a counter.
fn score_counter_target_value(ctx: &PolicyContext<'_>, target_id: ObjectId) -> f64 {
    // Only applies when the AI's pending spell has a Counter effect
    let is_counter = ctx
        .effects()
        .iter()
        .any(|e| matches!(e, Effect::Counter { .. }));
    if !is_counter {
        return 0.0;
    }

    // Find the stack entry being targeted
    let Some(entry) = ctx.state.stack.iter().find(|e| e.id == target_id) else {
        return 0.0;
    };

    // Only counter opponent spells — countering your own spell is almost always wrong
    if entry.controller == ctx.ai_player {
        return -10.0;
    }

    let mut score = assess_spell_impact(ctx.state, entry);

    // Last-counter reservation: if this is the AI's only counterspell, penalize
    // spending it on low-impact targets. Save it for something that matters.
    let impact_threshold = 3.0;
    if score < impact_threshold {
        let counters_in_hand =
            super::strategy_helpers::count_counterspells_in_hand(ctx.state, ctx.ai_player);
        if counters_in_hand == 1 {
            score += ctx.penalties().counter_last_reservation_penalty;
        }
    }

    // Low-MV creature penalty: scale by counter density in hand.
    // With many counters, save them for high-impact threats. With few counters,
    // the current threat IS the thing to counter — don't hold out for something better.
    if let Some(obj) = ctx.state.objects.get(&entry.source_id) {
        let is_cheap_creature = obj.mana_cost.mana_value() <= 2
            && obj
                .card_types
                .core_types
                .contains(&engine::types::card_type::CoreType::Creature);
        if is_cheap_creature {
            let intent = crate::eval::strategic_intent(ctx.state, ctx.ai_player);
            if !matches!(intent, crate::eval::StrategicIntent::Stabilize) {
                let counters_in_hand =
                    super::strategy_helpers::count_counterspells_in_hand(ctx.state, ctx.ai_player);
                // 1 counter = no penalty, 2 = -0.3, 3+ = -0.6
                let penalty = -0.3 * (counters_in_hand as f64 - 1.0).clamp(0.0, 2.0);
                score += penalty;
            }
        }
    }

    score
}

/// When the AI's pending spell is harmful, boost targeting a creature that an
/// opponent is currently pumping on the stack — removing it wastes both the
/// creature and the pump spell (2-for-1).
fn score_pump_response(ctx: &PolicyContext<'_>, target_id: ObjectId) -> f64 {
    if is_spell_beneficial(ctx) {
        return 0.0;
    }

    // Skip if target is already dying — redundancy penalty handles that case
    if will_target_die_from_stack(ctx.state, target_id) {
        return 0.0;
    }

    let has_opponent_pump = ctx.state.stack.iter().any(|entry| {
        entry.controller != ctx.ai_player && {
            let Some(ability) = entry.ability() else {
                return false;
            };
            let targets_this = ability
                .targets
                .iter()
                .any(|t| matches!(t, TargetRef::Object(id) if *id == target_id));
            targets_this
                && collect_ability_effects(ability)
                    .iter()
                    .any(|e| matches!(e, Effect::Pump { .. } | Effect::DoublePT { .. }))
        }
    });

    if has_opponent_pump {
        ctx.penalties().pump_response_bonus
    } else {
        0.0
    }
}

/// Estimate the game impact of a stack entry based on its effects.
/// Used for counter-target valuation and protect-my-spell incentives.
pub(crate) fn assess_spell_impact(state: &GameState, entry: &StackEntry) -> f64 {
    match &entry.kind {
        StackEntryKind::Spell { .. } => {
            let mv = state
                .objects
                .get(&entry.source_id)
                .map(|o| o.mana_cost.mana_value())
                .unwrap_or(0) as f64;

            let mut score = mv * 0.3;

            let effects = entry
                .ability()
                .map(|a| collect_ability_effects(a))
                .unwrap_or_default();
            for effect in effects {
                score += match effect {
                    Effect::ExtraTurn { .. } => 5.0,
                    Effect::DestroyAll { .. }
                    | Effect::DamageAll { .. }
                    | Effect::ChangeZoneAll { .. } => 4.0,
                    Effect::GainControl { .. } => 2.5,
                    Effect::Destroy { .. } | Effect::Fight { .. } => 1.5,
                    Effect::Counter { .. } => 1.5,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value },
                        ..
                    } => *value as f64 * 1.5,
                    Effect::DealDamage { .. } => 1.0,
                    Effect::SearchLibrary { .. } => 1.0,
                    Effect::Token { .. } => 0.5,
                    _ => 0.0,
                };
            }

            // Creature spells: factor in the creature's board value
            let creature_value = evaluate_creature(state, entry.source_id);
            if creature_value > 0.0 {
                score += creature_value * 0.3;
            }

            score.min(8.0)
        }
        // Activated/triggered abilities: moderate value — they're free to re-trigger.
        // KeywordAction (Crew/Equip/Saddle/Station) is similarly low-value to counter:
        // the cost was paid at announcement and the activation can be repeated.
        StackEntryKind::ActivatedAbility { .. }
        | StackEntryKind::TriggeredAbility { .. }
        | StackEntryKind::KeywordAction { .. } => 0.5,
    }
}

/// Check if any stack entry targets this object with a harmful effect.
pub(crate) fn has_pending_removal(state: &GameState, target_id: ObjectId) -> bool {
    state.stack.iter().any(|entry| {
        let Some(ability) = entry.ability() else {
            return false;
        };
        let targets_this = ability
            .targets
            .iter()
            .any(|t| matches!(t, TargetRef::Object(id) if *id == target_id));
        if !targets_this {
            return false;
        }
        // Check if any effect in the chain is harmful
        collect_ability_effects(ability)
            .iter()
            .any(|e| matches!(effect_polarity(e), EffectPolarity::Harmful))
    })
}

/// Estimate whether pending stack effects will remove this object (creature or spell).
pub(crate) fn will_target_die_from_stack(state: &GameState, target_id: ObjectId) -> bool {
    let Some(object) = state.objects.get(&target_id) else {
        return false;
    };

    let mut pending_damage: i32 = 0;

    for entry in state.stack.iter() {
        let Some(ability) = entry.ability() else {
            continue;
        };
        let targets_this = ability
            .targets
            .iter()
            .any(|t| matches!(t, TargetRef::Object(id) if *id == target_id));
        if !targets_this {
            continue;
        }

        for effect in collect_ability_effects(ability) {
            match effect {
                // Destroy is lethal unless target is indestructible
                Effect::Destroy { .. } if !object.has_keyword(&Keyword::Indestructible) => {
                    return true;
                }
                // Counter removes the spell from the stack
                Effect::Counter { .. } => return true,
                // Bounce removes from battlefield
                Effect::Bounce { .. } => return true,
                // ChangeZone to non-battlefield removes from battlefield
                Effect::ChangeZone {
                    destination: Zone::Exile | Zone::Graveyard | Zone::Hand | Zone::Library,
                    ..
                } => {
                    return true;
                }
                // Accumulate pending damage
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value },
                    ..
                } => {
                    pending_damage += value;
                }
                _ => {}
            }
        }
    }

    // Check if accumulated pending damage is lethal
    if let Some(toughness) = object.toughness {
        let remaining = toughness - object.damage_marked as i32;
        pending_damage >= remaining
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiConfig;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{ResolvedAbility, TargetFilter};
    use engine::types::card_type::CoreType;
    use engine::types::game_state::{
        GameState, PendingCast, StackEntry, StackEntryKind, TargetSelectionSlot, WaitingFor,
    };
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::mana::ManaCost;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;

    fn make_state() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state
    }

    fn add_creature(
        state: &mut GameState,
        owner: PlayerId,
        power: i32,
        toughness: i32,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            "Creature".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(power);
        obj.toughness = Some(toughness);
        id
    }

    fn push_stack_entry(state: &mut GameState, effect: Effect, targets: Vec<TargetRef>) {
        let ability = ResolvedAbility::new(effect, targets, ObjectId(999), PlayerId(1));
        state.stack.push_back(StackEntry {
            id: ObjectId(state.next_object_id),
            source_id: ObjectId(999),
            controller: PlayerId(1),
            kind: StackEntryKind::Spell {
                ability: Some(ability),
                card_id: CardId(999),
                casting_variant: Default::default(),
                actual_mana_spent: 0,
            },
        });
        state.next_object_id += 1;
    }

    fn make_target_ctx(
        _state: &GameState,
        target_id: ObjectId,
        source_effect: Effect,
    ) -> (AiDecisionContext, CandidateAction) {
        let ability = ResolvedAbility::new(source_effect, Vec::new(), ObjectId(888), PlayerId(1));
        let pending_cast = PendingCast::new(ObjectId(888), CardId(888), ability, ManaCost::zero());
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TargetSelection {
                player: PlayerId(1),
                pending_cast: Box::new(pending_cast),
                target_slots: vec![TargetSelectionSlot {
                    legal_targets: vec![TargetRef::Object(target_id)],
                    optional: false,
                }],
                selection: Default::default(),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target_id)),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(1)),
                tactical_class: TacticalClass::Target,
            },
        };
        (decision, candidate)
    }

    fn score_policy(
        state: &GameState,
        decision: &AiDecisionContext,
        candidate: &CandidateAction,
    ) -> f64 {
        let config = AiConfig::default();
        let ctx = PolicyContext {
            state,
            decision,
            candidate,
            ai_player: PlayerId(1),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };
        StackAwarenessPolicy.score(&ctx)
    }

    // --- Helper tests ---

    #[test]
    fn has_pending_removal_finds_destroy() {
        let mut state = make_state();
        let creature = add_creature(&mut state, PlayerId(0), 3, 3);
        push_stack_entry(
            &mut state,
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            vec![TargetRef::Object(creature)],
        );
        assert!(has_pending_removal(&state, creature));
    }

    #[test]
    fn has_pending_removal_ignores_different_target() {
        let mut state = make_state();
        let creature_a = add_creature(&mut state, PlayerId(0), 3, 3);
        let creature_b = add_creature(&mut state, PlayerId(0), 2, 2);
        push_stack_entry(
            &mut state,
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            vec![TargetRef::Object(creature_a)],
        );
        assert!(!has_pending_removal(&state, creature_b));
    }

    #[test]
    fn will_target_die_destroy() {
        let mut state = make_state();
        let creature = add_creature(&mut state, PlayerId(0), 3, 3);
        push_stack_entry(
            &mut state,
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            vec![TargetRef::Object(creature)],
        );
        assert!(will_target_die_from_stack(&state, creature));
    }

    #[test]
    fn will_target_die_indestructible_survives_destroy() {
        let mut state = make_state();
        let creature = add_creature(&mut state, PlayerId(0), 3, 3);
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .keywords
            .push(Keyword::Indestructible);
        push_stack_entry(
            &mut state,
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            vec![TargetRef::Object(creature)],
        );
        assert!(!will_target_die_from_stack(&state, creature));
    }

    #[test]
    fn will_target_die_lethal_damage() {
        let mut state = make_state();
        let creature = add_creature(&mut state, PlayerId(0), 2, 3);
        push_stack_entry(
            &mut state,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
            },
            vec![TargetRef::Object(creature)],
        );
        assert!(will_target_die_from_stack(&state, creature));
    }

    #[test]
    fn will_target_die_insufficient_damage() {
        let mut state = make_state();
        let creature = add_creature(&mut state, PlayerId(0), 2, 4);
        push_stack_entry(
            &mut state,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Any,
                damage_source: None,
            },
            vec![TargetRef::Object(creature)],
        );
        assert!(!will_target_die_from_stack(&state, creature));
    }

    #[test]
    fn will_target_die_bounce() {
        let mut state = make_state();
        let creature = add_creature(&mut state, PlayerId(0), 3, 3);
        push_stack_entry(
            &mut state,
            Effect::Bounce {
                target: TargetFilter::Any,
                destination: None,
            },
            vec![TargetRef::Object(creature)],
        );
        assert!(will_target_die_from_stack(&state, creature));
    }

    // --- Policy-level tests ---

    #[test]
    fn redundant_destroy_heavily_penalized() {
        let mut state = make_state();
        let creature = add_creature(&mut state, PlayerId(0), 3, 3);
        push_stack_entry(
            &mut state,
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            vec![TargetRef::Object(creature)],
        );

        let (decision, candidate) = make_target_ctx(
            &state,
            creature,
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
        );
        let score = score_policy(&state, &decision, &candidate);
        assert!(
            score < -5.0,
            "Should heavily penalize redundant destroy, got {score}"
        );
    }

    #[test]
    fn no_penalty_when_different_targets() {
        let mut state = make_state();
        let creature_a = add_creature(&mut state, PlayerId(0), 3, 3);
        let creature_b = add_creature(&mut state, PlayerId(0), 2, 2);
        push_stack_entry(
            &mut state,
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            vec![TargetRef::Object(creature_a)],
        );

        let (decision, candidate) = make_target_ctx(
            &state,
            creature_b,
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
        );
        let score = score_policy(&state, &decision, &candidate);
        assert!(
            score.abs() < 0.01,
            "No penalty when targeting different creature, got {score}"
        );
    }

    #[test]
    fn empty_stack_no_penalty() {
        let mut state = make_state();
        let creature = add_creature(&mut state, PlayerId(0), 3, 3);

        let (decision, candidate) = make_target_ctx(
            &state,
            creature,
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
        );
        let score = score_policy(&state, &decision, &candidate);
        assert!(
            score.abs() < 0.01,
            "No penalty with empty stack, got {score}"
        );
    }

    #[test]
    fn indestructible_not_penalized_second_removal() {
        let mut state = make_state();
        let creature = add_creature(&mut state, PlayerId(0), 3, 3);
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .keywords
            .push(Keyword::Indestructible);
        // First Destroy won't kill it (indestructible)
        push_stack_entry(
            &mut state,
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            vec![TargetRef::Object(creature)],
        );

        // Second removal should still get partial penalty (there IS pending removal,
        // just not lethal)
        let (decision, candidate) = make_target_ctx(
            &state,
            creature,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 5 },
                target: TargetFilter::Any,
                damage_source: None,
            },
        );
        let score = score_policy(&state, &decision, &candidate);
        // Should get partial penalty (redundant_damage * 0.5), not full redundant_removal
        assert!(
            score < 0.0 && score > -5.0,
            "Should get partial penalty for indestructible, got {score}"
        );
    }
}
