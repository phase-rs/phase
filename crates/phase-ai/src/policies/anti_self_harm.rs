use engine::game::combat;
use engine::game::filter::{matches_target_filter, FilterContext};
use engine::game::keywords;
use engine::game::mana_abilities;
use engine::game::turn_control;
use engine::types::ability::{
    AbilityCost, Effect, QuantityExpr, ReplacementMode, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, WardCost};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::eval::{evaluate_creature, threat_level};

use super::activation::turn_only;
use super::context::PolicyContext;
use super::effect_classify::{
    aggregate_player_impact, aura_polarity, effect_polarity, extract_target_filter,
    is_spell_beneficial, targeted_player_impact, targets_creatures, targets_creatures_only,
    EffectPolarity,
};
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::features::DeckFeatures;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

pub struct AntiSelfHarmPolicy;

impl AntiSelfHarmPolicy {
    pub fn score(&self, ctx: &PolicyContext<'_>) -> f64 {
        match &ctx.candidate.action {
            GameAction::CastSpell { .. } | GameAction::ActivateAbility { .. } => {
                score_pre_cast(ctx)
            }
            GameAction::ChooseTarget { target } => target
                .as_ref()
                .map_or(-0.25, |target| score_target_ref(ctx, target)),
            GameAction::SelectTargets { targets } => targets
                .iter()
                .map(|target| score_target_ref(ctx, target))
                .sum(),
            // Penalise accepting an optional effect whose life cost would kill or nearly kill us.
            GameAction::DecideOptionalEffect { accept: true } => score_optional_effect_accept(ctx),
            _ => 0.0,
        }
    }
}

impl TacticalPolicy for AntiSelfHarmPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::AntiSelfHarm
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[
            DecisionKind::CastSpell,
            DecisionKind::ActivateAbility,
            DecisionKind::SelectTarget,
        ]
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
            reason: PolicyReason::new("anti_self_harm_score"),
        }
    }
}

/// Penalise casting a targeted spell when the only legal creature targets
/// would hurt the AI.  Two cases:
/// - Beneficial spell (pump/aura buff) but AI has no creatures → would buff opponents.
/// - Harmful spell (destroy) but opponents have no creatures → would kill own.
fn score_pre_cast(ctx: &PolicyContext<'_>) -> f64 {
    // CR 704.5j: Penalise casting a legendary permanent when we already control one
    // with the same name — the legend rule SBA will force us to sacrifice one.
    let legend_penalty = ctx
        .source_object()
        .filter(|source| source.card_types.supertypes.contains(&Supertype::Legendary))
        .and_then(|source| {
            ctx.state
                .battlefield
                .iter()
                .any(|&id| {
                    ctx.state.objects.get(&id).is_some_and(|o| {
                        o.controller == ctx.ai_player
                            && o.card_types.supertypes.contains(&Supertype::Legendary)
                            && o.name == source.name
                    })
                })
                .then_some(-8.0)
        })
        .unwrap_or(0.0);

    let effects = ctx.effects();

    let mut has_beneficial_creature_target = effects.iter().any(|effect| {
        matches!(effect_polarity(effect), EffectPolarity::Beneficial) && targets_creatures(effect)
    });
    // For harmful spells, only penalise when targeting is creature-exclusive.
    // Burn spells with TargetFilter::Any can still go face — don't block those.
    let mut has_harmful_creature_only_target = effects.iter().any(|effect| {
        !matches!(effect, Effect::Bounce { .. })
            && matches!(effect_polarity(effect), EffectPolarity::Harmful)
            && targets_creatures_only(effect)
    });
    let has_harmful_bounce = effects.iter().any(is_hostile_or_neutral_bounce);

    // Auras have no active effects — detect polarity via static definitions.
    if effects.is_empty() {
        if let Some(source) = ctx.source_object() {
            if source.card_types.subtypes.iter().any(|s| s == "Aura") {
                match aura_polarity(source) {
                    EffectPolarity::Beneficial => has_beneficial_creature_target = true,
                    EffectPolarity::Harmful => has_harmful_creature_only_target = true,
                    EffectPolarity::Contextual => {}
                }
            }
        }
    }

    if !has_beneficial_creature_target && !has_harmful_creature_only_target && !has_harmful_bounce {
        return legend_penalty;
    }

    let has_own_creature = ctx.state.battlefield.iter().any(|&id| {
        ctx.state.objects.get(&id).is_some_and(|o| {
            o.controller == ctx.ai_player && o.card_types.core_types.contains(&CoreType::Creature)
        })
    });
    // CR 702.11b: Hexproof prevents targeting by opponents' spells/abilities.
    // CR 702.18a: Shroud prevents targeting by any spell/ability.
    // TODO: HexproofFrom — requires source color check for accurate filtering
    let has_targetable_opponent_creature = ctx.state.battlefield.iter().any(|&id| {
        ctx.state.objects.get(&id).is_some_and(|o| {
            o.controller != ctx.ai_player
                && o.card_types.core_types.contains(&CoreType::Creature)
                && !o.has_keyword(&Keyword::Hexproof)
                && !o.has_keyword(&Keyword::Shroud)
        })
    });

    let mut penalty = 0.0;

    // Beneficial creature-targeting spell but no own creatures to buff.
    if has_beneficial_creature_target && !has_own_creature {
        penalty -= 8.0;
    }

    // Harmful creature-only spell (e.g. Murder) but no targetable opponent creatures.
    if has_harmful_creature_only_target && !has_targetable_opponent_creature {
        penalty -= 8.0;
    }

    // Harmful bounce with no opposing legal targets will force a self-bounce line.
    if has_harmful_bounce && !has_opponent_bounce_target(ctx, &effects) {
        penalty -= 8.0;
    }

    // ETB-only permanents (e.g. Seam Rip): the spell itself has no targets, but the
    // card's entire value comes from a targeted ETB trigger. If no valid target exists
    // for the ETB trigger, casting wastes the card.
    if let Some(facts) = ctx.cast_facts() {
        if facts.requires_targets_in_immediate_etb
            && !facts.requires_targets_in_spell_text
            && !etb_trigger_has_valid_targets(ctx, &facts)
        {
            penalty -= 8.0;
        }
    }

    penalty += legend_penalty;

    // Penalize pump spells during opponent's combat that would require tapping creature
    // mana sources. auto_tap prefers pure lands (tier 0) over non-land dorks (tier 1),
    // so creature sources are only tapped when lands can't cover the full mana cost.
    // Tapping a creature dork for mana removes it as a potential blocker — pumping a
    // creature that can't block afterwards is a wasted combat trick.
    let has_pump = effects.iter().any(|e| {
        matches!(e, Effect::Pump { .. } | Effect::DoublePT { .. })
            && matches!(effect_polarity(e), EffectPolarity::Beneficial)
    });
    if has_pump {
        let own_turn = turn_control::turn_decision_maker(ctx.state) == ctx.ai_player;
        if !own_turn
            && matches!(
                ctx.state.phase,
                Phase::BeginCombat | Phase::DeclareAttackers | Phase::DeclareBlockers
            )
        {
            penalty += pump_taps_blocker_penalty(ctx);
        }
    }

    penalty
}

/// Penalise accepting an optional effect when the life cost would be lethal or near-lethal.
/// Applies to ETB replacements like Multiversal Passage ("pay 2 life or enter tapped").
fn score_optional_effect_accept(ctx: &PolicyContext<'_>) -> f64 {
    let WaitingFor::OptionalEffectChoice {
        player, source_id, ..
    } = &ctx.state.waiting_for
    else {
        return 0.0;
    };
    let life = ctx.state.players[player.0 as usize].life;
    let Some(cost) = optional_effect_life_cost(ctx, *source_id) else {
        return 0.0;
    };
    if life <= cost {
        -100.0
    } else {
        0.0
    }
}

/// Walk a source object's optional replacement definitions to find a fixed LoseLife cost.
fn optional_effect_life_cost(ctx: &PolicyContext<'_>, source_id: ObjectId) -> Option<i32> {
    let obj = ctx.state.objects.get(&source_id)?;
    obj.replacement_definitions
        .iter_unchecked()
        .filter(|r| matches!(r.mode, ReplacementMode::Optional { .. }))
        .find_map(|r| {
            let mut node = r.execute.as_deref();
            while let Some(def) = node {
                if let Effect::LoseLife {
                    amount: QuantityExpr::Fixed { value },
                    ..
                } = &*def.effect
                {
                    return Some(*value);
                }
                node = def.sub_ability.as_deref();
            }
            None
        })
}

/// Check if any ETB trigger on the permanent has a valid target on the battlefield.
/// Uses the trigger's execute ability's target filter(s) and validates against live game state.
fn etb_trigger_has_valid_targets(
    ctx: &PolicyContext<'_>,
    facts: &crate::cast_facts::CastFacts<'_>,
) -> bool {
    let source_id = match &ctx.candidate.action {
        GameAction::CastSpell { object_id, .. } => *object_id,
        _ => return true, // Not a cast action — assume valid
    };

    for trigger in &facts.immediate_etb_triggers {
        let Some(execute) = &trigger.execute else {
            continue;
        };
        // Walk the trigger's effect chain looking for targeted effects
        let mut node = Some(execute.as_ref());
        while let Some(def) = node {
            if let Some(filter) = extract_target_filter(&def.effect) {
                // Check if any battlefield object matches this filter
                let filter_ctx = FilterContext::from_source(ctx.state, source_id);
                let has_match =
                    ctx.state.battlefield.iter().any(|&obj_id| {
                        matches_target_filter(ctx.state, obj_id, filter, &filter_ctx)
                    });
                if has_match {
                    return true;
                }
            }
            node = def.sub_ability.as_deref();
        }
    }

    false
}

fn has_opponent_bounce_target(ctx: &PolicyContext<'_>, effects: &[&Effect]) -> bool {
    let Some(source) = ctx.source_object() else {
        return false;
    };

    effects
        .iter()
        .filter(|effect| is_hostile_or_neutral_bounce(effect))
        .filter_map(|effect| match effect {
            Effect::Bounce { target, .. } => Some(target),
            _ => None,
        })
        .any(|target| {
            let filter_ctx = FilterContext::from_source(ctx.state, source.id);
            ctx.state.battlefield.iter().any(|&object_id| {
                ctx.state.objects.get(&object_id).is_some_and(|object| {
                    object.controller != ctx.ai_player
                        && matches_target_filter(ctx.state, object_id, target, &filter_ctx)
                })
            })
        })
}

fn is_hostile_or_neutral_bounce(effect: &&Effect) -> bool {
    let Effect::Bounce { .. } = effect else {
        return false;
    };
    !matches!(
        extract_target_filter(effect),
        Some(TargetFilter::Typed(typed))
            if matches!(typed.controller, Some(engine::types::ability::ControllerRef::You))
    )
}

fn score_target_ref(ctx: &PolicyContext<'_>, target: &TargetRef) -> f64 {
    let beneficial = is_spell_beneficial(ctx);
    match target {
        TargetRef::Player(player_id) => {
            let is_self = *player_id == ctx.ai_player;

            // Lethal burn check: if damage would kill opponent, overwhelm all other targeting
            if !is_self && !beneficial {
                if let Some(damage) = extract_damage_amount(&ctx.effects()) {
                    let opponent_life = ctx.state.players[player_id.0 as usize].life;
                    if damage >= opponent_life {
                        return ctx.penalties().lethal_burn_bonus;
                    }
                }
            }

            let player_impact = targeted_player_impact(ctx, *player_id)
                .unwrap_or_else(|| aggregate_player_impact(ctx));
            let prefers_self = if player_impact > 0.25 {
                true
            } else if player_impact < -0.25 {
                false
            } else {
                beneficial
            };
            // Beneficial spells → target self; harmful → target opponent
            if prefers_self == is_self {
                4.0 + threat_level(ctx.state, ctx.ai_player, *player_id) * 8.0
            } else {
                -100.0
            }
        }
        TargetRef::Object(object_id) => score_target_object(ctx, *object_id, beneficial),
    }
}

fn score_target_object(ctx: &PolicyContext<'_>, object_id: ObjectId, beneficial: bool) -> f64 {
    let Some(object) = ctx.state.objects.get(&object_id) else {
        return -10.0;
    };

    // Activated abilities with sacrifice-self cost: the source will be sacrificed when
    // costs are paid, so targeting it wastes the ability (target becomes illegal on
    // resolution). Applies to patterns like Mogg Fanatic ("Sacrifice ~: ~ deals 1 damage
    // to any target") where the AI must not target the source it's about to sacrifice.
    if target_is_sacrificed_source(ctx, object_id) {
        return -100.0;
    }

    let controller_delta = if object.controller == ctx.ai_player {
        if beneficial {
            1.0
        } else {
            -1.0
        }
    } else if beneficial {
        -1.0
    } else {
        1.0
    };
    let mut score = controller_delta * 2.0;

    if object.card_types.core_types.contains(&CoreType::Creature) {
        score += controller_delta * evaluate_creature(ctx.state, object_id);

        // Cache effects once — used by damage check, indestructible check, and bounce check
        let effects = ctx.effects();

        if !beneficial {
            if let Some(damage) = extract_damage_amount(&effects) {
                if let Some(toughness) = object.toughness {
                    let remaining = toughness - object.damage_marked as i32;
                    // Penalize targeting creatures that won't die to this damage.
                    // Graduated: almost-lethal burn (leaves 1 toughness) is less
                    // wasteful than burn that barely scratches a large creature.
                    if damage < remaining {
                        let survival_ratio = (remaining - damage) as f64 / remaining as f64;
                        // Full penalty (-8.0) when damage is negligible relative to toughness,
                        // reduced penalty (-4.0) when damage is almost lethal.
                        score -= 4.0 + 4.0 * survival_ratio;
                    }
                    // Penalize massive overkill (wasting damage capacity)
                    if remaining > 0 && damage >= remaining && damage > remaining * 2 {
                        let wasted = damage - remaining;
                        let waste_ratio = wasted as f64 / damage as f64;
                        score += ctx.penalties().overkill_base_penalty * waste_ratio.sqrt();
                    }
                }
            }

            // Penalize casting Destroy at indestructible creatures (does nothing)
            let is_destroy = effects.iter().any(|e| matches!(e, Effect::Destroy { .. }));
            if is_destroy && object.has_keyword(&Keyword::Indestructible) {
                score += ctx.penalties().indestructible_destroy_penalty;
            }

            // CR 702.16b + CR 702.16e: Protection prevents targeting and damage
            // from sources with the protected quality. Targeting a creature with
            // protection from the spell's qualities wastes the spell entirely.
            if let Some(source) = ctx.source_object() {
                if keywords::protection_prevents_from(object, source) {
                    score -= 100.0;
                }
            }

            // Penalize targeting creatures with ward (must pay additional cost)
            for keyword in &object.keywords {
                if let Keyword::Ward(ward_cost) = keyword {
                    let severity = match ward_cost {
                        WardCost::Mana(cost) => (cost.mana_value() as f64 / 2.0).min(2.0),
                        WardCost::PayLife(amount) => (*amount as f64 / 3.0).min(2.0),
                        WardCost::DiscardCard => 1.5,
                        WardCost::Sacrifice { count, .. } => *count as f64 * 2.0,
                        WardCost::Waterbend(cost) => (cost.mana_value() as f64 / 2.0).min(2.0),
                        // CR 702.21a: Compound costs sum severity of components.
                        WardCost::Compound(costs) => costs
                            .iter()
                            .map(|c| match c {
                                WardCost::Mana(cost) => (cost.mana_value() as f64 / 2.0).min(2.0),
                                WardCost::PayLife(amount) => (*amount as f64 / 3.0).min(2.0),
                                WardCost::DiscardCard => 1.5,
                                WardCost::Sacrifice { count, .. } => *count as f64 * 2.0,
                                WardCost::Waterbend(cost) => {
                                    (cost.mana_value() as f64 / 2.0).min(2.0)
                                }
                                WardCost::Compound(_) => 2.0,
                            })
                            .sum::<f64>()
                            .min(4.0),
                    };
                    score += ctx.penalties().ward_cost_penalty_base * severity;
                    break;
                }
            }

            // Removal quality mismatch: penalize premium removal on cheap targets
            if let Some(source) = ctx.source_object() {
                let spell_mv = source.mana_cost.mana_value();
                let target_value = evaluate_creature(ctx.state, object_id);
                if spell_mv >= 4 && target_value < 4.0 {
                    score += ctx.penalties().removal_quality_mismatch
                        * (1.0 - target_value / 4.0).max(0.0);
                }
            }

            // Penalize non-lethal removal on a tapped opponent creature pre-combat.
            // A tapped creature can't block — there's no combat lane to open, so
            // non-lethal removal has no urgency advantage over casting post-combat.
            // Lethal removal is exempt: killing a tapped creature still removes a
            // future threat (it untaps next turn and can attack/block).
            if object.tapped
                && object.controller != ctx.ai_player
                && matches!(ctx.state.phase, Phase::PreCombatMain)
            {
                let is_lethal_burn = extract_damage_amount(&effects)
                    .zip(object.toughness)
                    .is_some_and(|(dmg, t)| dmg >= t - object.damage_marked as i32);
                let is_destroy = effects.iter().any(|e| matches!(e, Effect::Destroy { .. }));
                if !is_lethal_burn && !is_destroy {
                    score -= 5.0;
                }
            }
        }

        // Penalize pumping own tapped creatures — they can't attack or block,
        // so the +N/+N expires at cleanup with no combat impact.
        // Exception: tapped creatures actively participating in combat (as attacker
        // or blocker) benefit from the pump during damage resolution.
        if beneficial && object.tapped && object.controller == ctx.ai_player {
            let has_pump = effects
                .iter()
                .any(|e| matches!(e, Effect::Pump { .. } | Effect::DoublePT { .. }));
            if has_pump {
                let in_combat_as_participant = ctx.state.combat.as_ref().is_some_and(|combat| {
                    combat.attackers.iter().any(|a| a.object_id == object_id)
                        || combat.blocker_to_attacker.contains_key(&object_id)
                });
                if !in_combat_as_participant {
                    score -= 6.0;
                }
            }
        }

        // Bounce-specific valuation: tokens are great targets, cheap permanents are bad
        let bounce_destination = effects.iter().find_map(|e| match e {
            Effect::Bounce { destination, .. } => Some(*destination),
            _ => None,
        });
        if let Some(destination) = bounce_destination {
            if !beneficial {
                let is_tuck = matches!(destination, Some(Zone::Library));
                if object.is_token || is_tuck {
                    // Tokens cease to exist when bounced; tuck is permanent removal
                    score += ctx.penalties().bounce_token_bonus;
                } else {
                    let mv = object.mana_cost.mana_value();
                    if mv <= 2 {
                        score += ctx.penalties().bounce_cheap_discount;
                    } else {
                        score += mv as f64 * ctx.penalties().bounce_expensive_bonus_per_mv;
                    }
                }
            }
        }
    } else {
        // Non-creature permanent valuation: scale by mana value as a proxy for
        // impact. Tokens (Map, Clue, Food, Treasure) are low-value targets;
        // planeswalkers and high-MV enchantments/artifacts are high-value.
        let noncreature_value = if object.is_token {
            0.5
        } else if object
            .card_types
            .core_types
            .contains(&CoreType::Planeswalker)
        {
            // Planeswalkers are high-priority removal targets
            object.mana_cost.mana_value() as f64 + 2.0
        } else {
            // Artifacts/enchantments: scale by mana value (capped)
            (object.mana_cost.mana_value() as f64).min(6.0)
        };
        score += controller_delta * noncreature_value;
    }

    score
}

/// Penalize pump spells during opponent's combat when the AI must tap creature mana
/// sources to pay the cost. Returns a negative penalty proportional to creature
/// blocking value lost.
fn pump_taps_blocker_penalty(ctx: &PolicyContext<'_>) -> f64 {
    let Some(source) = ctx.source_object() else {
        return 0.0;
    };
    let spell_cost = source.mana_cost.mana_value() as usize;
    if spell_cost == 0 {
        return 0.0;
    }

    let pool_mana = ctx.state.players[ctx.ai_player.0 as usize]
        .mana_pool
        .total();
    let remaining_cost = spell_cost.saturating_sub(pool_mana);
    if remaining_cost == 0 {
        return 0.0;
    }

    // Count untapped land sources (auto_tap tier 0 — tapped first before creatures).
    let untapped_land_count = ctx
        .state
        .battlefield
        .iter()
        .filter(|&&id| {
            ctx.state.objects.get(&id).is_some_and(|obj| {
                obj.controller == ctx.ai_player
                    && !obj.tapped
                    && obj.card_types.core_types.contains(&CoreType::Land)
                    && !obj.card_types.core_types.contains(&CoreType::Creature)
            })
        })
        .count();

    if untapped_land_count >= remaining_cost {
        // Lands can cover the cost — auto_tap won't touch creature dorks.
        return 0.0;
    }

    // Shortfall: some non-land tier-1 sources must be tapped. Check if any are creatures
    // that could otherwise block.
    // CR 302.6: Creatures with summoning sickness cannot activate tap abilities.
    let shortfall = remaining_cost - untapped_land_count;
    let creature_mana_source_count = ctx
        .state
        .battlefield
        .iter()
        .filter(|&&id| {
            ctx.state.objects.get(&id).is_some_and(|obj| {
                obj.controller == ctx.ai_player
                    && !obj.tapped
                    && obj.card_types.core_types.contains(&CoreType::Creature)
                    && !obj.card_types.core_types.contains(&CoreType::Land)
                    && !combat::has_summoning_sickness(obj)
                    && obj.abilities.iter().any(mana_abilities::is_mana_ability)
            })
        })
        .count();

    if creature_mana_source_count == 0 {
        return 0.0;
    }

    // Non-land, non-creature tier-1 sources (mana rocks) that auto_tap would use
    // before creatures. Exclude sacrifice-for-mana sources (Treasures) — those are
    // tier 4 in auto_tap and would NOT be tapped before creature dorks.
    let non_creature_tier1_count = ctx
        .state
        .battlefield
        .iter()
        .filter(|&&id| {
            ctx.state.objects.get(&id).is_some_and(|obj| {
                obj.controller == ctx.ai_player
                    && !obj.tapped
                    && !obj.card_types.core_types.contains(&CoreType::Land)
                    && !obj.card_types.core_types.contains(&CoreType::Creature)
                    && obj.abilities.iter().any(|a| {
                        mana_abilities::is_mana_ability(a) && !ability_cost_requires_sacrifice(a)
                    })
            })
        })
        .count();

    let creatures_at_risk = shortfall.saturating_sub(non_creature_tier1_count);
    let creatures_tapped = creatures_at_risk.min(creature_mana_source_count);
    if creatures_tapped == 0 {
        return 0.0;
    }

    // Each creature tapped loses its blocking value during this combat.
    -(5.0 * creatures_tapped as f64)
}

/// Check if an ability's cost includes self-sacrifice (Treasure-style `{T}, Sacrifice`).
/// Mirrors `mana_sources::cost_requires_sacrifice` which is private to the engine module.
fn ability_cost_requires_sacrifice(ability: &engine::types::ability::AbilityDefinition) -> bool {
    match &ability.cost {
        Some(AbilityCost::Composite { costs }) => costs.iter().any(|c| {
            matches!(
                c,
                AbilityCost::Sacrifice {
                    target: TargetFilter::SelfRef,
                    ..
                }
            )
        }),
        _ => false,
    }
}

/// Extract the fixed damage amount from the pending spell's DealDamage effect.
/// Returns None for variable damage or non-damage spells.
/// Returns true if `object_id` is the source of an activated ability whose cost
/// includes sacrificing itself. Targeting such an object is wasteful because the
/// source will be gone before the ability resolves.
fn target_is_sacrificed_source(ctx: &PolicyContext<'_>, object_id: ObjectId) -> bool {
    let WaitingFor::TargetSelection { pending_cast, .. } = &ctx.decision.waiting_for else {
        return false;
    };

    // The source object for the pending ability
    if pending_cast.object_id != object_id {
        return false;
    }

    // Check if the activation cost includes sacrifice-self
    let Some(activation_cost) = &pending_cast.activation_cost else {
        return false;
    };

    cost_includes_sacrifice_self(activation_cost)
}

fn cost_includes_sacrifice_self(cost: &AbilityCost) -> bool {
    match cost {
        AbilityCost::Sacrifice {
            target: TargetFilter::SelfRef,
            ..
        } => true,
        AbilityCost::Composite { costs } => costs.iter().any(cost_includes_sacrifice_self),
        _ => false,
    }
}

fn extract_damage_amount(effects: &[&Effect]) -> Option<i32> {
    effects.iter().find_map(|effect| match effect {
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value },
            ..
        } => Some(*value),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::config::AiConfig;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        ContinuousModification, FilterProp, PtValue, ResolvedAbility, StaticDefinition,
        TargetFilter, TypeFilter, TypedFilter,
    };
    use engine::types::game_state::{GameState, PendingCast, TargetSelectionSlot, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::keywords::Keyword;
    use engine::types::mana::ManaCost;
    use engine::types::player::PlayerId;
    use engine::types::statics::StaticMode;
    use engine::types::zones::Zone;

    fn make_state() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state
    }

    fn add_creature(
        state: &mut GameState,
        owner: PlayerId,
        name: &str,
        power: i32,
        toughness: i32,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(power);
        obj.toughness = Some(toughness);
        id
    }

    fn make_target_selection_ctx(
        _state: &GameState,
        effect: Effect,
        legal_targets: Vec<TargetRef>,
        candidate_target: Option<TargetRef>,
    ) -> (AiDecisionContext, CandidateAction) {
        let ability = ResolvedAbility::new(effect, Vec::new(), ObjectId(100), PlayerId(0));
        let pending_cast = PendingCast::new(ObjectId(100), CardId(100), ability, ManaCost::zero());
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TargetSelection {
                player: PlayerId(0),
                pending_cast: Box::new(pending_cast),
                target_slots: vec![TargetSelectionSlot {
                    legal_targets,
                    optional: false,
                }],
                selection: Default::default(),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::ChooseTarget {
                target: candidate_target,
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Target,
            },
        };
        (decision, candidate)
    }

    #[test]
    fn beneficial_pump_prefers_own_creature() {
        let mut state = make_state();
        let own_id = add_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let opp_id = add_creature(&mut state, PlayerId(1), "Goblin", 2, 2);
        let config = AiConfig::default();

        let effect = Effect::Pump {
            power: PtValue::Fixed(3),
            toughness: PtValue::Fixed(3),
            target: TargetFilter::Any,
        };

        // Score targeting own creature
        let (decision, candidate) = make_target_selection_ctx(
            &state,
            effect.clone(),
            vec![TargetRef::Object(own_id), TargetRef::Object(opp_id)],
            Some(TargetRef::Object(own_id)),
        );
        let ctx_own = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };
        let score_own = AntiSelfHarmPolicy.score(&ctx_own);

        // Score targeting opponent's creature
        let (decision, candidate) = make_target_selection_ctx(
            &state,
            effect,
            vec![TargetRef::Object(own_id), TargetRef::Object(opp_id)],
            Some(TargetRef::Object(opp_id)),
        );
        let ctx_opp = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };
        let score_opp = AntiSelfHarmPolicy.score(&ctx_opp);

        assert!(
            score_own > score_opp,
            "Pump +3/+3 should prefer own creature: own={score_own}, opp={score_opp}"
        );
        assert!(score_own > 0.0, "Own creature score should be positive");
        assert!(
            score_opp < 0.0,
            "Opponent creature score should be negative"
        );
    }

    #[test]
    fn negative_pump_prefers_opponent_creature() {
        let mut state = make_state();
        let own_id = add_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let opp_id = add_creature(&mut state, PlayerId(1), "Goblin", 2, 2);
        let config = AiConfig::default();

        let effect = Effect::Pump {
            power: PtValue::Fixed(-3),
            toughness: PtValue::Fixed(-3),
            target: TargetFilter::Any,
        };

        let (decision, candidate) = make_target_selection_ctx(
            &state,
            effect.clone(),
            vec![TargetRef::Object(own_id), TargetRef::Object(opp_id)],
            Some(TargetRef::Object(own_id)),
        );
        let ctx_own = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };
        let score_own = AntiSelfHarmPolicy.score(&ctx_own);

        let (decision, candidate) = make_target_selection_ctx(
            &state,
            effect,
            vec![TargetRef::Object(own_id), TargetRef::Object(opp_id)],
            Some(TargetRef::Object(opp_id)),
        );
        let ctx_opp = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };
        let score_opp = AntiSelfHarmPolicy.score(&ctx_opp);

        assert!(
            score_opp > score_own,
            "Pump -3/-3 should prefer opponent creature: own={score_own}, opp={score_opp}"
        );
    }

    #[test]
    fn harmful_destroy_prefers_opponent_creature() {
        let mut state = make_state();
        let own_id = add_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let opp_id = add_creature(&mut state, PlayerId(1), "Goblin", 2, 2);
        let config = AiConfig::default();

        let effect = Effect::Destroy {
            target: TargetFilter::Any,
            cant_regenerate: false,
        };

        let (decision, candidate) = make_target_selection_ctx(
            &state,
            effect.clone(),
            vec![TargetRef::Object(own_id), TargetRef::Object(opp_id)],
            Some(TargetRef::Object(own_id)),
        );
        let ctx_own = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };
        let score_own = AntiSelfHarmPolicy.score(&ctx_own);

        let (decision, candidate) = make_target_selection_ctx(
            &state,
            effect,
            vec![TargetRef::Object(own_id), TargetRef::Object(opp_id)],
            Some(TargetRef::Object(opp_id)),
        );
        let ctx_opp = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };
        let score_opp = AntiSelfHarmPolicy.score(&ctx_opp);

        assert!(
            score_opp > score_own,
            "Destroy should prefer opponent creature: own={score_own}, opp={score_opp}"
        );
    }

    #[test]
    fn beneficial_player_target_prefers_self() {
        let state = make_state();
        let config = AiConfig::default();

        let effect = Effect::Pump {
            power: PtValue::Fixed(3),
            toughness: PtValue::Fixed(3),
            target: TargetFilter::Any,
        };

        let (decision, candidate) = make_target_selection_ctx(
            &state,
            effect.clone(),
            vec![
                TargetRef::Player(PlayerId(0)),
                TargetRef::Player(PlayerId(1)),
            ],
            Some(TargetRef::Player(PlayerId(0))),
        );
        let ctx_self = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };
        let score_self = AntiSelfHarmPolicy.score(&ctx_self);

        let (decision, candidate) = make_target_selection_ctx(
            &state,
            effect,
            vec![
                TargetRef::Player(PlayerId(0)),
                TargetRef::Player(PlayerId(1)),
            ],
            Some(TargetRef::Player(PlayerId(1))),
        );
        let ctx_opp = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };
        let score_opp = AntiSelfHarmPolicy.score(&ctx_opp);

        assert!(
            score_self > score_opp,
            "Beneficial spell targeting player should prefer self: self={score_self}, opp={score_opp}"
        );
    }

    #[test]
    fn discard_then_draw_player_target_prefers_self() {
        let state = make_state();
        let config = AiConfig::default();
        let sub = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 2 },
                target: engine::types::ability::TargetFilter::Controller,
            },
            Vec::new(),
            ObjectId(100),
            PlayerId(0),
        );
        let ability = ResolvedAbility::new(
            Effect::DiscardCard {
                count: 1,
                target: TargetFilter::Any,
            },
            Vec::new(),
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(sub);
        let legal_targets = vec![
            TargetRef::Player(PlayerId(0)),
            TargetRef::Player(PlayerId(1)),
        ];
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TargetSelection {
                player: PlayerId(0),
                pending_cast: Box::new(PendingCast::new(
                    ObjectId(100),
                    CardId(100),
                    ability.clone(),
                    ManaCost::zero(),
                )),
                target_slots: vec![TargetSelectionSlot {
                    legal_targets: legal_targets.clone(),
                    optional: false,
                }],
                selection: Default::default(),
            },
            candidates: Vec::new(),
        };
        let self_candidate = CandidateAction {
            action: GameAction::ChooseTarget {
                target: Some(TargetRef::Player(PlayerId(0))),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Target,
            },
        };
        let self_ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &self_candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };
        let opp_candidate = CandidateAction {
            action: GameAction::ChooseTarget {
                target: Some(TargetRef::Player(PlayerId(1))),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Target,
            },
        };
        let opp_ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &opp_candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };

        let self_score = AntiSelfHarmPolicy.score(&self_ctx);
        let opp_score = AntiSelfHarmPolicy.score(&opp_ctx);
        assert!(
            self_score > opp_score,
            "Net card-positive discard/draw should prefer self: self={self_score}, opp={opp_score}"
        );
    }

    #[test]
    fn opponent_discards_and_you_draw_prefers_opponent() {
        let state = make_state();
        let config = AiConfig::default();
        let sub = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: engine::types::ability::TargetFilter::Controller,
            },
            Vec::new(),
            ObjectId(100),
            PlayerId(0),
        );
        let ability = ResolvedAbility::new(
            Effect::DiscardCard {
                count: 1,
                target: TargetFilter::Any,
            },
            Vec::new(),
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(sub);
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TargetSelection {
                player: PlayerId(0),
                pending_cast: Box::new(PendingCast::new(
                    ObjectId(100),
                    CardId(100),
                    ability,
                    ManaCost::zero(),
                )),
                target_slots: vec![TargetSelectionSlot {
                    legal_targets: vec![
                        TargetRef::Player(PlayerId(0)),
                        TargetRef::Player(PlayerId(1)),
                    ],
                    optional: false,
                }],
                selection: Default::default(),
            },
            candidates: Vec::new(),
        };
        let self_candidate = CandidateAction {
            action: GameAction::ChooseTarget {
                target: Some(TargetRef::Player(PlayerId(0))),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Target,
            },
        };
        let self_ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &self_candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };
        let opp_candidate = CandidateAction {
            action: GameAction::ChooseTarget {
                target: Some(TargetRef::Player(PlayerId(1))),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Target,
            },
        };
        let opp_ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &opp_candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };

        let self_score = AntiSelfHarmPolicy.score(&self_ctx);
        let opp_score = AntiSelfHarmPolicy.score(&opp_ctx);
        assert!(
            opp_score > self_score,
            "Targeted discard plus untargeted draw should still prefer opponent: self={self_score}, opp={opp_score}"
        );
    }

    #[test]
    fn plus_counter_is_beneficial() {
        let effect = Effect::AddCounter {
            counter_type: "+1/+1".to_string(),
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Any,
        };
        assert_eq!(effect_polarity(&effect), EffectPolarity::Beneficial);
    }

    #[test]
    fn minus_counter_is_harmful() {
        let effect = Effect::AddCounter {
            counter_type: "-1/-1".to_string(),
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Any,
        };
        assert_eq!(effect_polarity(&effect), EffectPolarity::Harmful);
    }

    /// Regression: Katsumasa, the Animator upkeep trigger uses `Effect::PutCounter`
    /// with a `+1/+1` counter. Prior to the classifier fix, `effect_polarity`
    /// fell through to the default `Contextual` arm, flipping the AI's
    /// anti-self-harm preference and making it target opponents' artifacts.
    #[test]
    fn put_counter_plus_is_beneficial() {
        let effect = Effect::PutCounter {
            counter_type: "+1/+1".to_string(),
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Any,
        };
        assert_eq!(effect_polarity(&effect), EffectPolarity::Beneficial);
    }

    #[test]
    fn put_counter_all_minus_is_harmful() {
        let effect = Effect::PutCounterAll {
            counter_type: "-1/-1".to_string(),
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Any,
        };
        assert_eq!(effect_polarity(&effect), EffectPolarity::Harmful);
    }

    #[test]
    fn proliferate_is_contextual_before_target_selection() {
        assert_eq!(
            effect_polarity(&Effect::Proliferate),
            EffectPolarity::Contextual
        );
    }

    /// CR 122.1: Removing a +1/+1 counter harms its bearer; removing a
    /// -1/-1 counter helps it (Hexcaster's Mark, Vampire Hexmage). Prior
    /// to the fix RemoveCounter was lumped under the catch-all "harmful"
    /// arm, inverting AI target preference for -1/-1 removal.
    #[test]
    fn remove_plus_counter_is_harmful() {
        let effect = Effect::RemoveCounter {
            counter_type: "+1/+1".to_string(),
            count: 1,
            target: TargetFilter::Any,
        };
        assert_eq!(effect_polarity(&effect), EffectPolarity::Harmful);
    }

    #[test]
    fn remove_minus_counter_is_beneficial() {
        let effect = Effect::RemoveCounter {
            counter_type: "-1/-1".to_string(),
            count: 1,
            target: TargetFilter::Any,
        };
        assert_eq!(effect_polarity(&effect), EffectPolarity::Beneficial);
    }

    #[test]
    fn unknown_effect_defaults_to_contextual() {
        let effect = Effect::GenericEffect {
            static_abilities: Vec::new(),
            target: None,
            duration: None,
        };
        assert_eq!(effect_polarity(&effect), EffectPolarity::Contextual);
    }

    /// Regression: AI should not cast a pump spell when it has no creatures,
    /// since the only targets would be opponent creatures.
    #[test]
    fn pre_cast_penalises_duplicate_legendary() {
        let mut state = make_state();

        // AI already controls a legendary creature on the battlefield
        let existing = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Thalia".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&existing).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.card_types.supertypes.push(Supertype::Legendary);
        obj.power = Some(2);
        obj.toughness = Some(1);

        // AI tries to cast a second copy from hand
        let spell_id = create_object(
            &mut state,
            CardId(201),
            PlayerId(0),
            "Thalia".to_string(),
            Zone::Hand,
        );
        let obj2 = state.objects.get_mut(&spell_id).unwrap();
        obj2.card_types.core_types.push(CoreType::Creature);
        obj2.card_types.supertypes.push(Supertype::Legendary);
        obj2.power = Some(2);
        obj2.toughness = Some(1);
        obj2.abilities = Arc::new(vec![engine::types::ability::AbilityDefinition::new(
            engine::types::ability::AbilityKind::Spell,
            Effect::Draw {
                count: engine::types::ability::QuantityExpr::Fixed { value: 0 },
                target: engine::types::ability::TargetFilter::Controller,
            },
        )]);

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: spell_id,
                card_id: CardId(201),
                targets: Vec::new(),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Spell,
            },
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };

        let score = AntiSelfHarmPolicy.score(&ctx);
        assert!(
            score < -5.0,
            "Casting a duplicate legendary should be heavily penalised, got {score}"
        );
    }

    #[test]
    fn pre_cast_allows_first_legendary() {
        let mut state = make_state();

        // No existing legendary on battlefield — casting should be fine
        let spell_id = create_object(
            &mut state,
            CardId(202),
            PlayerId(0),
            "Thalia".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&spell_id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.card_types.supertypes.push(Supertype::Legendary);
        obj.power = Some(2);
        obj.toughness = Some(1);
        obj.abilities = Arc::new(vec![engine::types::ability::AbilityDefinition::new(
            engine::types::ability::AbilityKind::Spell,
            Effect::Draw {
                count: engine::types::ability::QuantityExpr::Fixed { value: 0 },
                target: engine::types::ability::TargetFilter::Controller,
            },
        )]);

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: spell_id,
                card_id: CardId(202),
                targets: Vec::new(),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Spell,
            },
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };

        let score = AntiSelfHarmPolicy.score(&ctx);
        assert!(
            score >= -1.0,
            "Casting first copy of a legendary should not be penalised, got {score}"
        );
    }

    #[test]
    fn pre_cast_penalises_pump_with_no_friendly_creatures() {
        let mut state = make_state();
        // Only opponent has a creature — AI has none.
        add_creature(&mut state, PlayerId(1), "Goblin", 2, 2);

        // Put Giant Growth in AI's hand so source_object() finds it.
        let spell_id = create_object(
            &mut state,
            CardId(300),
            PlayerId(0),
            "Giant Growth".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&spell_id).unwrap();
        obj.abilities = Arc::new(vec![engine::types::ability::AbilityDefinition::new(
            engine::types::ability::AbilityKind::Spell,
            Effect::Pump {
                power: PtValue::Fixed(3),
                toughness: PtValue::Fixed(3),
                target: TargetFilter::Typed(engine::types::ability::TypedFilter::new(
                    TypeFilter::Creature,
                )),
            },
        )]);

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: spell_id,
                card_id: CardId(300),
                targets: Vec::new(),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Spell,
            },
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };

        let score = AntiSelfHarmPolicy.score(&ctx);
        assert!(
            score < -5.0,
            "Casting pump with no friendly creatures should be heavily penalised, got {score}"
        );
    }

    #[test]
    fn pre_cast_penalises_bounce_with_only_friendly_targets() {
        let mut state = make_state();
        add_creature(&mut state, PlayerId(0), "Otter", 1, 1);

        let spell_id = create_object(
            &mut state,
            CardId(301),
            PlayerId(0),
            "Boomerang Basics".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&spell_id).unwrap();
        obj.abilities = Arc::new(vec![engine::types::ability::AbilityDefinition::new(
            engine::types::ability::AbilityKind::Spell,
            Effect::Bounce {
                target: TargetFilter::Typed(
                    engine::types::ability::TypedFilter::new(TypeFilter::Permanent)
                        .with_type(TypeFilter::Non(Box::new(TypeFilter::Land))),
                ),
                destination: None,
            },
        )]);

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: spell_id,
                card_id: CardId(301),
                targets: Vec::new(),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Spell,
            },
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };

        let score = AntiSelfHarmPolicy.score(&ctx);
        assert!(
            score < -5.0,
            "Casting bounce with only friendly targets should be heavily penalised, got {score}"
        );
    }

    #[test]
    fn pre_cast_allows_explicit_self_bounce_patterns() {
        let mut state = make_state();
        add_creature(&mut state, PlayerId(0), "Otter", 1, 1);

        let spell_id = create_object(
            &mut state,
            CardId(302),
            PlayerId(0),
            "Deputy of Acquittals".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&spell_id).unwrap();
        obj.abilities = Arc::new(vec![engine::types::ability::AbilityDefinition::new(
            engine::types::ability::AbilityKind::Spell,
            Effect::Bounce {
                target: TargetFilter::Typed(
                    engine::types::ability::TypedFilter::new(TypeFilter::Creature)
                        .controller(engine::types::ability::ControllerRef::You),
                ),
                destination: None,
            },
        )]);

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: spell_id,
                card_id: CardId(302),
                targets: Vec::new(),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Spell,
            },
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };

        let score = AntiSelfHarmPolicy.score(&ctx);
        assert!(
            score >= 0.0,
            "Explicit self-bounce patterns should not be treated as self-harm, got {score}"
        );
    }

    /// When the AI controls at least one creature, the pre-cast check should
    /// not penalise casting a pump spell.
    #[test]
    fn pre_cast_allows_pump_with_friendly_creatures() {
        let mut state = make_state();
        add_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        add_creature(&mut state, PlayerId(1), "Goblin", 2, 2);

        let spell_id = create_object(
            &mut state,
            CardId(300),
            PlayerId(0),
            "Giant Growth".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&spell_id).unwrap();
        obj.abilities = Arc::new(vec![engine::types::ability::AbilityDefinition::new(
            engine::types::ability::AbilityKind::Spell,
            Effect::Pump {
                power: PtValue::Fixed(3),
                toughness: PtValue::Fixed(3),
                target: TargetFilter::Typed(engine::types::ability::TypedFilter::new(
                    TypeFilter::Creature,
                )),
            },
        )]);

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: spell_id,
                card_id: CardId(300),
                targets: Vec::new(),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Spell,
            },
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };

        let score = AntiSelfHarmPolicy.score(&ctx);
        assert!(
            score >= 0.0,
            "Casting pump with own creatures should not be penalised, got {score}"
        );
    }

    /// Casting a creature-only destruction spell when only the AI's own
    /// creatures exist should be penalised (symmetric to the pump check).
    #[test]
    fn pre_cast_penalises_destroy_with_no_opponent_creatures() {
        let mut state = make_state();
        // Only AI has a creature — opponent has none.
        add_creature(&mut state, PlayerId(0), "Bear", 2, 2);

        let spell_id = create_object(
            &mut state,
            CardId(400),
            PlayerId(0),
            "Murder".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&spell_id).unwrap();
        obj.abilities = Arc::new(vec![engine::types::ability::AbilityDefinition::new(
            engine::types::ability::AbilityKind::Spell,
            Effect::Destroy {
                target: TargetFilter::Typed(engine::types::ability::TypedFilter::new(
                    TypeFilter::Creature,
                )),
                cant_regenerate: false,
            },
        )]);

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: spell_id,
                card_id: CardId(400),
                targets: Vec::new(),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Spell,
            },
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };

        let score = AntiSelfHarmPolicy.score(&ctx);
        assert!(
            score < -5.0,
            "Casting destroy with only own creatures should be penalised, got {score}"
        );
    }

    /// Burn spells with TargetFilter::Any can still target the opponent player,
    /// so they should NOT be penalised even when no opponent creatures exist.
    #[test]
    fn pre_cast_allows_burn_with_any_target_and_no_opponent_creatures() {
        let mut state = make_state();
        // Only AI has creatures — but burn can go face.
        add_creature(&mut state, PlayerId(0), "Bear", 2, 2);

        let spell_id = create_object(
            &mut state,
            CardId(500),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&spell_id).unwrap();
        obj.abilities = Arc::new(vec![engine::types::ability::AbilityDefinition::new(
            engine::types::ability::AbilityKind::Spell,
            Effect::DealDamage {
                amount: engine::types::ability::QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
            },
        )]);

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: spell_id,
                card_id: CardId(500),
                targets: Vec::new(),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Spell,
            },
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };

        let score = AntiSelfHarmPolicy.score(&ctx);
        assert!(
            score >= 0.0,
            "Burn with Any target should not be penalised (can go face), got {score}"
        );
    }

    fn add_aura(state: &mut GameState, owner: PlayerId, name: &str) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            name.to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.card_types.subtypes.push("Aura".to_string());
        obj.keywords
            .push(Keyword::Enchant(TargetFilter::Typed(TypedFilter::new(
                TypeFilter::Creature,
            ))));
        // Rancor-style: enchanted creature gets +2/+0 and has trample
        obj.static_definitions.push(
            StaticDefinition::continuous()
                .affected(TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Creature)
                        .properties(vec![FilterProp::EnchantedBy]),
                ))
                .modifications(vec![
                    ContinuousModification::AddPower { value: 2 },
                    ContinuousModification::AddToughness { value: 0 },
                    ContinuousModification::AddKeyword {
                        keyword: Keyword::Trample,
                    },
                ]),
        );
        id
    }

    /// Regression: AI should enchant its own creatures with beneficial auras,
    /// not opponent creatures. Rancor (+2/+0 and trample) is beneficial.
    #[test]
    fn beneficial_aura_prefers_own_creature() {
        let mut state = make_state();
        let own_id = add_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let opp_id = add_creature(&mut state, PlayerId(1), "Goblin", 2, 2);
        let aura_id = add_aura(&mut state, PlayerId(0), "Rancor");
        let config = AiConfig::default();

        let score_own = score_aura_target(&state, &config, aura_id, own_id, opp_id, own_id);
        let score_opp = score_aura_target(&state, &config, aura_id, own_id, opp_id, opp_id);

        assert!(
            score_own > score_opp,
            "Beneficial aura should prefer own creature: own={score_own}, opp={score_opp}"
        );
        assert!(score_own > 0.0, "Own creature score should be positive");
        assert!(
            score_opp < 0.0,
            "Opponent creature score should be negative"
        );
    }

    fn score_aura_target(
        state: &GameState,
        config: &AiConfig,
        aura_id: ObjectId,
        own_id: ObjectId,
        opp_id: ObjectId,
        target_id: ObjectId,
    ) -> f64 {
        let (decision, candidate) = make_aura_target_selection_ctx(
            state,
            aura_id,
            vec![TargetRef::Object(own_id), TargetRef::Object(opp_id)],
            Some(TargetRef::Object(target_id)),
        );
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };
        AntiSelfHarmPolicy.score(&ctx)
    }

    /// Pre-cast check: AI should not cast a beneficial aura when it has no creatures.
    #[test]
    fn pre_cast_penalises_beneficial_aura_with_no_friendly_creatures() {
        let mut state = make_state();
        add_creature(&mut state, PlayerId(1), "Goblin", 2, 2);
        let aura_id = add_aura(&mut state, PlayerId(0), "Rancor");
        let card_id = state.objects[&aura_id].card_id;
        let config = AiConfig::default();

        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: aura_id,
                card_id,
                targets: Vec::new(),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Spell,
            },
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };

        let score = AntiSelfHarmPolicy.score(&ctx);
        assert!(
            score < -5.0,
            "Casting beneficial aura with no friendly creatures should be penalised, got {score}"
        );
    }

    fn add_harmful_aura(state: &mut GameState, owner: PlayerId, name: &str) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            name.to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.card_types.subtypes.push("Aura".to_string());
        obj.keywords
            .push(Keyword::Enchant(TargetFilter::Typed(TypedFilter::new(
                TypeFilter::Creature,
            ))));
        // Pacifism-style: enchanted creature can't attack or block
        obj.static_definitions
            .push(StaticDefinition::new(StaticMode::CantAttack).affected(TargetFilter::SelfRef));
        id
    }

    fn add_unblockable_aura(state: &mut GameState, owner: PlayerId, name: &str) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            name.to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.card_types.subtypes.push("Aura".to_string());
        obj.keywords
            .push(Keyword::Enchant(TargetFilter::Typed(TypedFilter::new(
                TypeFilter::Creature,
            ))));
        // Aqueous Form-style: enchanted creature can't be blocked
        obj.static_definitions
            .push(
                StaticDefinition::new(StaticMode::CantBeBlocked).affected(TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Creature)
                        .properties(vec![FilterProp::EnchantedBy]),
                )),
            );
        id
    }

    /// Harmful auras (Pacifism) should target opponent creatures, not own.
    #[test]
    fn harmful_aura_prefers_opponent_creature() {
        let mut state = make_state();
        let own_id = add_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let opp_id = add_creature(&mut state, PlayerId(1), "Goblin", 2, 2);
        let aura_id = add_harmful_aura(&mut state, PlayerId(0), "Pacifism");
        let config = AiConfig::default();

        let score_own = score_aura_target(&state, &config, aura_id, own_id, opp_id, own_id);
        let score_opp = score_aura_target(&state, &config, aura_id, own_id, opp_id, opp_id);

        assert!(
            score_opp > score_own,
            "Harmful aura should prefer opponent creature: own={score_own}, opp={score_opp}"
        );
    }

    /// Beneficial non-modification auras (Aqueous Form: "can't be blocked")
    /// should target own creatures.
    #[test]
    fn beneficial_cant_be_blocked_aura_prefers_own_creature() {
        let mut state = make_state();
        let own_id = add_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let opp_id = add_creature(&mut state, PlayerId(1), "Goblin", 2, 2);
        let aura_id = add_unblockable_aura(&mut state, PlayerId(0), "Aqueous Form");
        let config = AiConfig::default();

        let score_own = score_aura_target(&state, &config, aura_id, own_id, opp_id, own_id);
        let score_opp = score_aura_target(&state, &config, aura_id, own_id, opp_id, opp_id);

        assert!(
            score_own > score_opp,
            "CantBeBlocked aura should prefer own creature: own={score_own}, opp={score_opp}"
        );
        assert!(score_own > 0.0, "Own creature score should be positive");
    }

    /// Pre-cast: harmful aura (Pacifism) with only own creatures should be penalised.
    #[test]
    fn pre_cast_penalises_harmful_aura_with_no_opponent_creatures() {
        let mut state = make_state();
        add_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let aura_id = add_harmful_aura(&mut state, PlayerId(0), "Pacifism");
        let card_id = state.objects[&aura_id].card_id;
        let config = AiConfig::default();

        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: aura_id,
                card_id,
                targets: Vec::new(),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Spell,
            },
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };

        let score = AntiSelfHarmPolicy.score(&ctx);
        assert!(
            score < -5.0,
            "Casting harmful aura with only own creatures should be penalised, got {score}"
        );
    }

    /// Helper to create a target selection context for an aura (no active effects).
    fn make_aura_target_selection_ctx(
        state: &GameState,
        aura_id: ObjectId,
        legal_targets: Vec<TargetRef>,
        candidate_target: Option<TargetRef>,
    ) -> (AiDecisionContext, CandidateAction) {
        // Auras have no active abilities — use a GenericEffect placeholder since
        // the policy should fall through to static_definitions for polarity.
        let ability = ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: Vec::new(),
                target: None,
                duration: None,
            },
            Vec::new(),
            aura_id,
            PlayerId(0),
        );
        let card_id = state.objects[&aura_id].card_id;
        let pending_cast = PendingCast::new(aura_id, card_id, ability, ManaCost::zero());
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TargetSelection {
                player: PlayerId(0),
                pending_cast: Box::new(pending_cast),
                target_slots: vec![TargetSelectionSlot {
                    legal_targets,
                    optional: false,
                }],
                selection: Default::default(),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::ChooseTarget {
                target: candidate_target,
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Target,
            },
        };
        (decision, candidate)
    }

    /// Fix 1: Pumping during opponent's combat when the only way to pay is by tapping
    /// a creature mana source (e.g., Llanowar Elves paying for Giant Growth) should be
    /// penalized — the tapped creature can't block, wasting the pump.
    #[test]
    fn pre_cast_penalizes_pump_when_creature_mana_source_must_tap() {
        use engine::types::ability::{AbilityCost, AbilityKind, ManaContribution, ManaProduction};
        use engine::types::mana::ManaColor;

        let mut state = make_state();
        state.active_player = PlayerId(1); // opponent's turn
        state.phase = Phase::DeclareAttackers;

        // AI has a creature mana source (Llanowar Elves) — no untapped lands.
        let dork_id = add_creature(&mut state, PlayerId(0), "Llanowar Elves", 1, 1);
        let dork_obj = state.objects.get_mut(&dork_id).unwrap();
        // Played on a previous turn — no summoning sickness.
        dork_obj.entered_battlefield_turn = Some(0);
        let mut mana_ability = engine::types::ability::AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![ManaColor::Green],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
            },
        );
        mana_ability.cost = Some(AbilityCost::Tap);
        Arc::make_mut(&mut dork_obj.abilities).push(mana_ability);

        // Also add an opponent creature so the "no opponent creatures" penalty doesn't fire
        add_creature(&mut state, PlayerId(1), "Goblin", 2, 2);

        // Pump spell in hand
        let spell_id = create_object(
            &mut state,
            CardId(500),
            PlayerId(0),
            "Giant Growth".to_string(),
            Zone::Hand,
        );
        let spell_obj = state.objects.get_mut(&spell_id).unwrap();
        spell_obj.card_types.core_types.push(CoreType::Instant);
        spell_obj.mana_cost = ManaCost::Cost {
            shards: vec![engine::types::mana::ManaCostShard::Green],
            generic: 0,
        };
        spell_obj.abilities = Arc::new(vec![engine::types::ability::AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Pump {
                power: PtValue::Fixed(3),
                toughness: PtValue::Fixed(3),
                target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
            },
        )]);

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: spell_id,
                card_id: CardId(500),
                targets: Vec::new(),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Spell,
            },
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };

        let score = AntiSelfHarmPolicy.score(&ctx);
        assert!(
            score < -4.0,
            "Should penalize pump spell that must tap creature blocker, got {score}"
        );
    }

    /// Fix 1 counterpart: if there are enough lands to pay, no penalty should apply.
    #[test]
    fn pre_cast_no_penalty_when_lands_cover_pump_cost() {
        use engine::types::ability::{AbilityCost, AbilityKind, ManaContribution, ManaProduction};
        use engine::types::mana::ManaColor;

        let mut state = make_state();
        state.active_player = PlayerId(1); // opponent's turn
        state.phase = Phase::DeclareAttackers;

        // AI has a creature mana source AND an untapped land.
        let dork_id = add_creature(&mut state, PlayerId(0), "Llanowar Elves", 1, 1);
        let dork_obj = state.objects.get_mut(&dork_id).unwrap();
        dork_obj.entered_battlefield_turn = Some(0);
        let mut mana_ability = engine::types::ability::AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![ManaColor::Green],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
            },
        );
        mana_ability.cost = Some(AbilityCost::Tap);
        Arc::make_mut(&mut dork_obj.abilities).push(mana_ability);

        // Add an untapped land (enough to pay for Giant Growth)
        let land_id = create_object(
            &mut state,
            CardId(501),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        let land_obj = state.objects.get_mut(&land_id).unwrap();
        land_obj.card_types.core_types.push(CoreType::Land);

        add_creature(&mut state, PlayerId(1), "Goblin", 2, 2);

        // Pump spell in hand
        let spell_id = create_object(
            &mut state,
            CardId(502),
            PlayerId(0),
            "Giant Growth".to_string(),
            Zone::Hand,
        );
        let spell_obj = state.objects.get_mut(&spell_id).unwrap();
        spell_obj.card_types.core_types.push(CoreType::Instant);
        spell_obj.mana_cost = ManaCost::Cost {
            shards: vec![engine::types::mana::ManaCostShard::Green],
            generic: 0,
        };
        spell_obj.abilities = Arc::new(vec![engine::types::ability::AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Pump {
                power: PtValue::Fixed(3),
                toughness: PtValue::Fixed(3),
                target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
            },
        )]);

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: spell_id,
                card_id: CardId(502),
                targets: Vec::new(),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Spell,
            },
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };

        let score = AntiSelfHarmPolicy.score(&ctx);
        assert!(
            score >= -1.0,
            "Should not penalize when lands can cover cost, got {score}"
        );
    }

    /// Fix 3: Pumping a tapped creature during combat should still be penalized
    /// if the creature is not participating in combat (not an attacker or blocker).
    #[test]
    fn penalizes_pump_on_tapped_non_combatant_during_combat() {
        use engine::game::combat::CombatState;

        let mut state = make_state();
        state.phase = Phase::DeclareBlockers;
        state.combat = Some(CombatState::default());

        // AI has a tapped creature NOT in combat
        let creature_id = add_creature(&mut state, PlayerId(0), "Tapped Dork", 1, 1);
        let creature = state.objects.get_mut(&creature_id).unwrap();
        creature.tapped = true;

        let config = AiConfig::default();
        let effect = Effect::Pump {
            power: PtValue::Fixed(3),
            toughness: PtValue::Fixed(3),
            target: TargetFilter::Any,
        };
        let (decision, candidate) = make_target_selection_ctx(
            &state,
            effect,
            vec![TargetRef::Object(creature_id)],
            Some(TargetRef::Object(creature_id)),
        );
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };

        let score = AntiSelfHarmPolicy.score(&ctx);
        // Base targeting score for own 1/1 creature is ~+3.0, minus the -6.0 penalty = ~-3.0
        assert!(
            score < -2.0,
            "Should penalize pump on tapped non-combatant during DeclareBlockers, got {score}"
        );
    }

    /// Fix 3 counterpart: pumping a tapped creature that IS an attacker is fine.
    #[test]
    fn allows_pump_on_tapped_attacker_during_combat() {
        use engine::game::combat::{AttackerInfo, CombatState};

        let mut state = make_state();
        state.phase = Phase::DeclareBlockers;

        let attacker_id = add_creature(&mut state, PlayerId(0), "Attacker", 3, 3);
        let attacker = state.objects.get_mut(&attacker_id).unwrap();
        attacker.tapped = true;

        // Set up combat with this creature as an attacker
        let mut combat = CombatState::default();
        combat.attackers.push(AttackerInfo {
            object_id: attacker_id,
            defending_player: PlayerId(1),
            attack_target: engine::game::combat::AttackTarget::Player(PlayerId(1)),
            blocked: false,
        });
        state.combat = Some(combat);

        let config = AiConfig::default();
        let effect = Effect::Pump {
            power: PtValue::Fixed(3),
            toughness: PtValue::Fixed(3),
            target: TargetFilter::Any,
        };
        let (decision, candidate) = make_target_selection_ctx(
            &state,
            effect,
            vec![TargetRef::Object(attacker_id)],
            Some(TargetRef::Object(attacker_id)),
        );
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };

        let score = AntiSelfHarmPolicy.score(&ctx);
        // The score should be positive (pump on own attacker) or at worst mildly negative
        // from other policies, but NOT the -6.0 tapped-creature penalty
        assert!(
            score > -4.0,
            "Should not heavily penalize pump on tapped attacker in combat, got {score}"
        );
    }

    #[test]
    fn trigger_target_prefers_creature_over_token() {
        let mut state = make_state();
        let creature = add_creature(&mut state, PlayerId(1), "Menace Bear", 2, 2);
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .keywords
            .push(Keyword::Menace);

        // Create a Map token (artifact, non-creature)
        let token_card_id = CardId(state.next_object_id);
        let token = create_object(
            &mut state,
            token_card_id,
            PlayerId(1),
            "Map".to_string(),
            Zone::Battlefield,
        );
        let token_obj = state.objects.get_mut(&token).unwrap();
        token_obj
            .card_types
            .core_types
            .push(engine::types::card_type::CoreType::Artifact);
        token_obj.is_token = true;

        // Set up pending trigger with exile effect (like Seam Rip)
        state.pending_trigger = Some(engine::game::triggers::PendingTrigger {
            source_id: ObjectId(200),
            controller: PlayerId(0),
            condition: None,
            ability: ResolvedAbility::new(
                Effect::ChangeZone {
                    origin: None,
                    destination: Zone::Exile,
                    target: TargetFilter::Any,
                    owner_library: false,
                    enter_transformed: false,
                    under_your_control: false,
                    enter_tapped: false,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                },
                Vec::new(),
                ObjectId(200),
                PlayerId(0),
            ),
            timestamp: 1,
            target_constraints: Vec::new(),
            trigger_event: None,
            modal: None,
            mode_abilities: vec![],
            description: None,
        });

        let config = AiConfig::default();
        let legal_targets = vec![TargetRef::Object(creature), TargetRef::Object(token)];
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TriggerTargetSelection {
                player: PlayerId(0),
                target_slots: vec![TargetSelectionSlot {
                    legal_targets: legal_targets.clone(),
                    optional: false,
                }],
                target_constraints: Vec::new(),
                selection: Default::default(),
                source_id: Some(ObjectId(200)),
                description: None,
            },
            candidates: Vec::new(),
        };

        // Score targeting the creature
        let creature_candidate = CandidateAction {
            action: GameAction::ChooseTarget {
                target: Some(TargetRef::Object(creature)),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Target,
            },
        };
        let creature_ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &creature_candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };
        let creature_score = AntiSelfHarmPolicy.score(&creature_ctx);

        // Score targeting the token
        let token_candidate = CandidateAction {
            action: GameAction::ChooseTarget {
                target: Some(TargetRef::Object(token)),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Target,
            },
        };
        let token_ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &token_candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };
        let token_score = AntiSelfHarmPolicy.score(&token_ctx);

        assert!(
            creature_score > token_score,
            "Should prefer exiling creature ({creature_score}) over token ({token_score})"
        );
        // Creature should score significantly higher (at least 2.0 gap)
        assert!(
            creature_score - token_score > 2.0,
            "Gap should be significant: creature={creature_score}, token={token_score}, gap={}",
            creature_score - token_score
        );
    }

    #[test]
    fn trigger_target_effects_are_extracted() {
        let mut state = make_state();
        state.pending_trigger = Some(engine::game::triggers::PendingTrigger {
            source_id: ObjectId(200),
            controller: PlayerId(0),
            condition: None,
            ability: ResolvedAbility::new(
                Effect::ChangeZone {
                    origin: None,
                    destination: Zone::Exile,
                    target: TargetFilter::Any,
                    owner_library: false,
                    enter_transformed: false,
                    under_your_control: false,
                    enter_tapped: false,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                },
                Vec::new(),
                ObjectId(200),
                PlayerId(0),
            ),
            timestamp: 1,
            target_constraints: Vec::new(),
            trigger_event: None,
            modal: None,
            mode_abilities: vec![],
            description: None,
        });

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TriggerTargetSelection {
                player: PlayerId(0),
                target_slots: vec![],
                target_constraints: Vec::new(),
                selection: Default::default(),
                source_id: Some(ObjectId(200)),
                description: None,
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::ChooseTarget { target: None },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Target,
            },
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };

        let effects = ctx.effects();
        assert_eq!(
            effects.len(),
            1,
            "Should extract effects from pending trigger"
        );
        assert!(
            matches!(
                effects[0],
                Effect::ChangeZone {
                    destination: Zone::Exile,
                    ..
                }
            ),
            "Should see ChangeZone Exile effect"
        );
    }

    #[test]
    fn sacrifice_self_ability_penalizes_targeting_source() {
        // Mogg Fanatic pattern: "Sacrifice ~: ~ deals 1 damage to any target."
        // The AI must not target the source creature — it will be sacrificed as cost.
        let mut state = make_state();
        let fanatic_id = add_creature(&mut state, PlayerId(0), "Mogg Fanatic", 1, 1);
        let opp_creature = add_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        let config = AiConfig::default();

        let effect = Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Any,
            damage_source: None,
        };
        let ability = ResolvedAbility::new(effect, Vec::new(), fanatic_id, PlayerId(0));
        let mut pending_cast = PendingCast::new(fanatic_id, CardId(100), ability, ManaCost::zero());
        pending_cast.activation_cost = Some(AbilityCost::Composite {
            costs: vec![AbilityCost::Sacrifice {
                target: TargetFilter::SelfRef,
                count: 1,
            }],
        });

        let legal_targets = vec![
            TargetRef::Object(fanatic_id),
            TargetRef::Object(opp_creature),
            TargetRef::Player(PlayerId(1)),
        ];
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TargetSelection {
                player: PlayerId(0),
                pending_cast: Box::new(pending_cast),
                target_slots: vec![TargetSelectionSlot {
                    legal_targets,
                    optional: false,
                }],
                selection: Default::default(),
            },
            candidates: Vec::new(),
        };

        // Score targeting the source (Mogg Fanatic itself)
        let candidate_self = CandidateAction {
            action: GameAction::ChooseTarget {
                target: Some(TargetRef::Object(fanatic_id)),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Target,
            },
        };
        let ctx_self = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate_self,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };
        let score_self = AntiSelfHarmPolicy.score(&ctx_self);

        // Score targeting opponent creature
        let candidate_opp = CandidateAction {
            action: GameAction::ChooseTarget {
                target: Some(TargetRef::Object(opp_creature)),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Target,
            },
        };
        let ctx_opp = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate_opp,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };
        let score_opp = AntiSelfHarmPolicy.score(&ctx_opp);

        // Score targeting opponent player
        let candidate_player = CandidateAction {
            action: GameAction::ChooseTarget {
                target: Some(TargetRef::Player(PlayerId(1))),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Target,
            },
        };
        let ctx_player = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate_player,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };
        let score_player = AntiSelfHarmPolicy.score(&ctx_player);

        assert!(
            score_self < -50.0,
            "Targeting sacrificed source should be heavily penalized, got {score_self}"
        );
        assert!(
            score_opp > score_self,
            "Opponent creature should score higher than sacrificed source: opp={score_opp}, self={score_self}"
        );
        assert!(
            score_player > score_self,
            "Opponent player should score higher than sacrificed source: player={score_player}, self={score_self}"
        );
    }

    /// Regression: Escape Tunnel's "target creature can't be blocked" is a GenericEffect
    /// with CantBeBlocked static. The AI must recognise this as beneficial and prefer
    /// its own creature, not grant unblockable to the opponent's creature.
    #[test]
    fn generic_effect_cant_be_blocked_prefers_own_creature() {
        let mut state = make_state();
        let own_id = add_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let opp_id = add_creature(&mut state, PlayerId(1), "Goblin", 2, 2);
        let config = AiConfig::default();

        let effect = Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::new(StaticMode::CantBeBlocked)
                .affected(TargetFilter::Typed(TypedFilter::creature()))],
            duration: Some(engine::types::ability::Duration::UntilEndOfTurn),
            target: Some(TargetFilter::Typed(TypedFilter::creature())),
        };

        // Score targeting own creature
        let (decision, candidate) = make_target_selection_ctx(
            &state,
            effect.clone(),
            vec![TargetRef::Object(own_id), TargetRef::Object(opp_id)],
            Some(TargetRef::Object(own_id)),
        );
        let ctx_own = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };
        let score_own = AntiSelfHarmPolicy.score(&ctx_own);

        // Score targeting opponent's creature
        let (decision, candidate) = make_target_selection_ctx(
            &state,
            effect,
            vec![TargetRef::Object(own_id), TargetRef::Object(opp_id)],
            Some(TargetRef::Object(opp_id)),
        );
        let ctx_opp = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };
        let score_opp = AntiSelfHarmPolicy.score(&ctx_opp);

        assert!(
            score_own > score_opp,
            "GenericEffect CantBeBlocked should prefer own creature: own={score_own}, opp={score_opp}"
        );
        assert!(score_own > 0.0, "Own creature score should be positive");
        assert!(
            score_opp < 0.0,
            "Opponent creature score should be negative"
        );
    }

    /// Regression: AI burned an opponent's tapped creature pre-combat with non-lethal
    /// damage. Two compounding mistakes:
    /// 1. Tapped creature can't block — no combat lane to open
    /// 2. Non-lethal burn wastes the card entirely
    #[test]
    fn penalizes_non_lethal_burn_on_tapped_creature_pre_combat() {
        let mut state = make_state();
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);

        add_creature(&mut state, PlayerId(0), "Attacker", 3, 3);
        let opp_id = add_creature(&mut state, PlayerId(1), "Defender", 4, 4);
        // Opponent's creature is tapped — can't block
        state.objects.get_mut(&opp_id).unwrap().tapped = true;
        let config = AiConfig::default();

        // 2 damage to a 4-toughness creature: non-lethal + tapped
        let effect = Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 2 },
            target: TargetFilter::Any,
            damage_source: None,
        };

        let (decision, candidate) = make_target_selection_ctx(
            &state,
            effect.clone(),
            vec![TargetRef::Object(opp_id), TargetRef::Player(PlayerId(1))],
            Some(TargetRef::Object(opp_id)),
        );
        let ctx_creature = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };
        let score_creature = AntiSelfHarmPolicy.score(&ctx_creature);

        // Compare: burn to opponent's face
        let (decision, candidate) = make_target_selection_ctx(
            &state,
            effect,
            vec![TargetRef::Object(opp_id), TargetRef::Player(PlayerId(1))],
            Some(TargetRef::Player(PlayerId(1))),
        );
        let ctx_face = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };
        let score_face = AntiSelfHarmPolicy.score(&ctx_face);

        assert!(
            score_face > score_creature,
            "Going face should beat non-lethal burn on tapped creature: face={score_face}, creature={score_creature}"
        );
        assert!(
            score_creature < 0.0,
            "Non-lethal burn on tapped creature pre-combat should be negative, got {score_creature}"
        );
    }

    /// Lethal burn on a tapped creature should NOT be penalized — killing it
    /// removes a future threat that untaps next turn.
    #[test]
    fn lethal_burn_on_tapped_creature_not_penalized() {
        let mut state = make_state();
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);

        let opp_id = add_creature(&mut state, PlayerId(1), "Goblin", 2, 2);
        state.objects.get_mut(&opp_id).unwrap().tapped = true;
        let config = AiConfig::default();

        // 3 damage to a 2-toughness creature: lethal + tapped
        let effect = Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 3 },
            target: TargetFilter::Any,
            damage_source: None,
        };

        let (decision, candidate) = make_target_selection_ctx(
            &state,
            effect,
            vec![TargetRef::Object(opp_id), TargetRef::Player(PlayerId(1))],
            Some(TargetRef::Object(opp_id)),
        );
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };
        let score = AntiSelfHarmPolicy.score(&ctx);

        assert!(
            score > 0.0,
            "Lethal burn on tapped creature should be positive (removing a threat), got {score}"
        );
    }
}
