use engine::game::combat;
use engine::game::keywords;
use engine::game::mana_abilities;
use engine::game::quantity::resolve_quantity;
use engine::game::targeting::find_legal_targets;
use engine::game::turn_control;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, DelayedTriggerCondition, Effect, EffectScope,
    QuantityExpr, ReplacementMode, TapStateChange, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::card_type::{CoreType, Supertype};
#[cfg(test)]
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, WardCost};
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

use crate::cast_facts::collect_definition_effects;
use crate::damage_reflection::{
    is_event_context_damage_to_player, opponent_creature_reflection_penalty,
};
use crate::eval::{evaluate_creature, threat_level};
use engine::game::players;

use super::activation::turn_only;
use super::context::PolicyContext;
use super::copy_value::{
    copy_effect_strips_legendary, copy_target_penalties, score_legend_rule_keep,
};
use super::effect_classify::{
    aggregate_player_impact, aura_polarity, effect_polarity, effect_targets_object,
    extract_target_filter, is_spell_beneficial, lethal_to_creature, targeted_object_impact,
    targeted_player_impact, targets_creatures, targets_creatures_only, EffectPolarity,
};
use super::registry::{
    DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy, CRITICAL_MAX,
};
use super::strategy_helpers::can_pay_ward_cost;
use crate::features::DeckFeatures;
#[cfg(test)]
use engine::types::game_state::CastPaymentMode;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

pub struct AntiSelfHarmPolicy;

// `turn_only` can scale early-game verdicts by 1.3; cap the raw verdict so
// registry-scaled anti-self-harm penalties stay within the critical band.
const ANTI_SELF_HARM_RAW_CRITICAL_CEILING: f64 = CRITICAL_MAX / 1.3;

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
            GameAction::ChooseLegend { keep } => score_legend_rule_keep(ctx.state, *keep),
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
        if let Some(reason) = reject_reason(ctx) {
            return PolicyVerdict::reject(reason);
        }

        PolicyVerdict::score(
            self.score(ctx).clamp(
                -ANTI_SELF_HARM_RAW_CRITICAL_CEILING,
                ANTI_SELF_HARM_RAW_CRITICAL_CEILING,
            ),
            PolicyReason::new("anti_self_harm_score"),
        )
    }
}

fn reject_reason(ctx: &PolicyContext<'_>) -> Option<PolicyReason> {
    match &ctx.candidate.action {
        GameAction::CastSpell { .. } if cast_has_unpayable_self_etb_may_cost(ctx) => {
            Some(PolicyReason::new("anti_self_harm_unpayable_etb_may_cost"))
        }
        GameAction::CastSpell { .. } | GameAction::ActivateAbility { .. }
            if grants_extra_turn_then_self_loss(ctx) =>
        {
            Some(PolicyReason::new("anti_self_harm_extra_turn_self_loss"))
        }
        GameAction::DecideOptionalEffect { accept: true }
            if optional_effect_life_cost_is_lethal(ctx) =>
        {
            Some(PolicyReason::new("anti_self_harm_lethal_life_cost"))
        }
        GameAction::ChooseTarget { target } => target
            .as_ref()
            .and_then(|target| target_reject_reason(ctx, target)),
        GameAction::SelectTargets { targets } => targets
            .iter()
            .find_map(|target| target_reject_reason(ctx, target)),
        _ => None,
    }
}

fn cast_has_unpayable_self_etb_may_cost(ctx: &PolicyContext<'_>) -> bool {
    let GameAction::CastSpell { .. } = &ctx.candidate.action else {
        return false;
    };
    let Some(source) = ctx.source_object() else {
        return false;
    };

    source
        .replacement_definitions
        .iter_unchecked()
        .any(|replacement| {
            if replacement.event != ReplacementEvent::Moved {
                return false;
            }
            let ReplacementMode::MayCost { cost, decline } = &replacement.mode else {
                return false;
            };
            decline
                .as_deref()
                .is_some_and(decline_moves_self_to_graveyard)
                && !cost.is_payable(ctx.state, ctx.ai_player, source.id)
        })
}

fn decline_moves_self_to_graveyard(decline: &AbilityDefinition) -> bool {
    ability_tree_any(decline, |effect| {
        matches!(
            effect,
            Effect::ChangeZone {
                destination: Zone::Graveyard,
                target: TargetFilter::SelfRef,
                ..
            }
        )
    })
}

fn grants_extra_turn_then_self_loss(ctx: &PolicyContext<'_>) -> bool {
    action_ability_definitions(ctx).into_iter().any(|ability| {
        ability_tree_any(ability, effect_grants_ai_extra_turn)
            && ability_tree_any(ability, effect_loses_game_for_controller)
    })
}

fn action_ability_definitions<'a>(ctx: &'a PolicyContext<'_>) -> Vec<&'a AbilityDefinition> {
    match &ctx.candidate.action {
        GameAction::CastSpell { .. } => ctx
            .source_object()
            .into_iter()
            .flat_map(|object| object.abilities.iter())
            .filter(|ability| ability.kind == AbilityKind::Spell)
            .collect(),
        GameAction::ActivateAbility {
            source_id,
            ability_index,
        } => ctx
            .state
            .objects
            .get(source_id)
            .and_then(|object| object.abilities.get(*ability_index))
            .into_iter()
            .collect(),
        _ => Vec::new(),
    }
}

fn ability_tree_any(
    ability: &AbilityDefinition,
    mut predicate: impl FnMut(&Effect) -> bool,
) -> bool {
    ability_tree_any_impl(ability, &mut predicate)
}

fn ability_tree_any_impl(
    ability: &AbilityDefinition,
    predicate: &mut impl FnMut(&Effect) -> bool,
) -> bool {
    predicate(&ability.effect)
        || ability
            .sub_ability
            .as_deref()
            .is_some_and(|sub| ability_tree_any_impl(sub, predicate))
        || ability
            .else_ability
            .as_deref()
            .is_some_and(|sub| ability_tree_any_impl(sub, predicate))
        || ability
            .mode_abilities
            .iter()
            .any(|mode| ability_tree_any_impl(mode, predicate))
}

fn effect_grants_ai_extra_turn(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::ExtraTurn {
            target: TargetFilter::Controller
        }
    )
}

fn effect_loses_game_for_controller(effect: &Effect) -> bool {
    match effect {
        Effect::LoseTheGame { target } => target
            .as_ref()
            .is_none_or(|target| matches!(target, TargetFilter::Controller)),
        Effect::CreateDelayedTrigger {
            condition, effect, ..
        } => {
            matches!(
                condition,
                DelayedTriggerCondition::AtNextPhaseForPlayer {
                    phase: Phase::End,
                    ..
                } | DelayedTriggerCondition::AtNextPhase { phase: Phase::End }
            ) && ability_tree_any(effect, effect_loses_game_for_controller)
        }
        _ => false,
    }
}

/// Penalise casting a targeted spell when the only legal creature targets
/// would hurt the AI.  Two cases:
/// - Beneficial spell (pump/aura buff) but AI has no creatures → would buff opponents.
/// - Harmful spell (destroy) but opponents have no creatures → would kill own.
fn score_pre_cast(ctx: &PolicyContext<'_>) -> f64 {
    // CR 704.5j: Penalise casting a legendary permanent when we already control one
    // with the same name — the legend rule SBA will force us to put one into the
    // graveyard. Skip same-name copies that the engine's legend-rule SBA would
    // exclude under a "legend rule doesn't apply" exemption (Mirror Gallery /
    // Sakashima / Sliver Gravemother class).
    // Reuse the engine's own legend-rule predicate (`engine::game::sba::legend_rule_exempt`)
    // rather than re-deriving the exemption logic in the AI — the engine owns the rule.
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
                    }) && !engine::game::sba::legend_rule_exempt(ctx.state, id)
                })
                .then_some(ctx.penalties().wasted_cast_penalty)
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

    // ETB-only permanents (e.g. Gravedigger): the spell itself has no targets,
    // but the card's value may come from a targeted ETB trigger. If no valid
    // target exists for that ETB trigger, casting wastes the card.
    let etb_whiff_penalty = if let Some(facts) = ctx.cast_facts() {
        if facts.requires_targets_in_immediate_etb
            && !facts.requires_targets_in_spell_text
            && !etb_trigger_has_valid_targets(ctx, &facts)
        {
            ctx.penalties().wasted_cast_penalty
        } else {
            0.0
        }
    } else {
        0.0
    };

    if !has_beneficial_creature_target && !has_harmful_creature_only_target && !has_harmful_bounce {
        return legend_penalty + etb_whiff_penalty;
    }

    let has_own_creature = ctx.state.battlefield.iter().any(|&id| {
        ctx.state.objects.get(&id).is_some_and(|o| {
            o.controller == ctx.ai_player && o.card_types.core_types.contains(&CoreType::Creature)
        })
    });
    // Targeting legality (CR 702.11/702.16/702.18) is owned by the engine.
    // Ask `find_legal_targets` with the spell's own creature-only harmful
    // filter so Shroud, Hexproof-vs-opponents, "Hexproof from [quality]",
    // Protection, and ignore-hexproof effects are all honored — a hand-rolled
    // `!Hexproof && !Shroud` check would whiff on Protection / HexproofFrom and
    // mis-score a fizzling removal spell as castable.
    let has_targetable_opponent_creature = if effects.is_empty() {
        harmful_aura_has_opponent_creature_target(ctx)
    } else {
        effects
            .iter()
            .filter(|effect| {
                !matches!(effect, Effect::Bounce { .. })
                    && matches!(effect_polarity(effect), EffectPolarity::Harmful)
                    && targets_creatures_only(effect)
            })
            .any(|effect| harmful_effect_has_opponent_creature_target(ctx, effect))
    };

    let mut penalty = 0.0;

    // Beneficial creature-targeting spell but no own creatures to buff.
    if has_beneficial_creature_target && !has_own_creature {
        penalty += ctx.penalties().wasted_cast_penalty;
    }

    // Harmful creature-only spell (e.g. Murder) but no targetable opponent creatures.
    if has_harmful_creature_only_target && !has_targetable_opponent_creature {
        penalty += ctx.penalties().wasted_cast_penalty;
    }

    // Harmful bounce with no opposing legal targets will force a self-bounce line.
    if has_harmful_bounce && !has_opponent_bounce_target(ctx, &effects) {
        penalty += ctx.penalties().wasted_cast_penalty;
    }

    penalty += etb_whiff_penalty;

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
fn score_optional_effect_accept(_ctx: &PolicyContext<'_>) -> f64 {
    0.0
}

fn optional_effect_life_cost_is_lethal(ctx: &PolicyContext<'_>) -> bool {
    let WaitingFor::OptionalEffectChoice {
        player, source_id, ..
    } = &ctx.state.waiting_for
    else {
        return false;
    };
    let life = ctx.state.players[player.0 as usize].life;
    let Some(cost) = optional_effect_life_cost(ctx, *source_id) else {
        return false;
    };
    life <= cost
}

/// Worst-case life payment across every reachable branch of a source object's optional
/// replacement definitions.
///
/// CR 119.6 / CR 704.5a: a player at 0 or less life loses the game as a state-based
/// action, so accepting an optional "pay N life" effect that brings the AI to 0 or
/// below is self-lethal. The life cost can live in any branch of the ability tree
/// (`sub_ability` / `else_ability` / modal modes), so we collect *all* reachable
/// `LoseLife` effects via [`collect_definition_effects`] (the shared comprehensive
/// walker) rather than only descending the `sub_ability` chain, and take the MAX
/// payment as the worst case.
///
/// Non-`Fixed` amounts are resolved against live game state via the engine's
/// `resolve_quantity`; a value that resolves non-positive is treated as a 0-life
/// payment (no self-harm) rather than silently dropped.
fn optional_effect_life_cost(ctx: &PolicyContext<'_>, source_id: ObjectId) -> Option<i32> {
    let obj = ctx.state.objects.get(&source_id)?;
    obj.replacement_definitions
        .iter_unchecked()
        .filter(|r| matches!(r.mode, ReplacementMode::Optional { .. }))
        .filter_map(|r| r.execute.as_deref())
        .flat_map(collect_definition_effects)
        .filter_map(|effect| match effect {
            Effect::LoseLife { amount, .. } => {
                Some(resolve_quantity(ctx.state, amount, ctx.ai_player, source_id).max(0))
            }
            _ => None,
        })
        .max()
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
        // Walk the trigger's effect chain looking for targeted effects.
        // CR 702.11/702.16/702.18 + CR 608.2b: targeting legality (and the
        // correct zone enumeration for the filter) is owned by the engine, so
        // ask `find_legal_targets` rather than re-deriving candidate zones and
        // applying a property-only `matches_target_filter` that ignores
        // Shroud/Hexproof/Protection.
        let mut node = Some(execute.as_ref());
        while let Some(def) = node {
            if let Some(filter) = extract_target_filter(&def.effect) {
                if !find_legal_targets(ctx.state, filter, ctx.ai_player, source_id).is_empty() {
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
        // CR 702.11/702.16/702.18: defer targeting legality to the engine.
        // `matches_target_filter` is a property filter only and would not
        // reject Shroud/Hexproof/Protection targets, letting a bounce that can
        // only legally hit our own creatures look like a clean opponent line.
        .any(|target| ctx.has_legal_opponent_creature_target(target, source.id, |_| true))
}

fn harmful_aura_has_opponent_creature_target(ctx: &PolicyContext<'_>) -> bool {
    let Some(source) = ctx.source_object() else {
        return true;
    };
    source
        .keywords
        .iter()
        .find_map(|keyword| match keyword {
            Keyword::Enchant(filter) => Some(filter),
            _ => None,
        })
        .is_none_or(|filter| ctx.has_legal_opponent_creature_target(filter, source.id, |_| true))
}

/// Resolve the harmful creature-only effect's target filter and check, via the
/// engine, whether a legal opponent-creature target exists. Returns `true` when
/// the effect carries no usable filter (fail-open: don't over-penalize an
/// effect we can't analyze).
fn harmful_effect_has_opponent_creature_target(ctx: &PolicyContext<'_>, effect: &Effect) -> bool {
    let Some(filter) = extract_target_filter(effect) else {
        return true;
    };
    let Some(source) = ctx.source_object() else {
        return true;
    };
    let effects = ctx.effects();
    ctx.has_legal_opponent_creature_target(filter, source.id, |id| {
        is_useful_removal_target(ctx, id, &effects)
    })
}

/// Whether a removal target is worth casting at: the AI can pay any ward cost
/// (CR 702.21a — otherwise the spell is merely countered) and the spell can
/// actually kill it. Provably non-lethal damage / shrink (CR 704.5f/g) is a
/// wasted cast. Variable-X effects and non-damage removal (Destroy, Exile,
/// Bounce) stay useful, since `lethal_to_creature` returns `None` for them.
fn is_useful_removal_target(ctx: &PolicyContext<'_>, id: ObjectId, effects: &[&Effect]) -> bool {
    if let Some(object) = ctx.state.objects.get(&id) {
        for keyword in &object.keywords {
            if let Keyword::Ward(ward) = keyword {
                if !can_pay_ward_cost(ctx, ward, object) {
                    return false;
                }
                break;
            }
        }
    }
    lethal_to_creature(ctx.state, id, effects) != Some(false)
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

fn target_reject_reason(ctx: &PolicyContext<'_>, target: &TargetRef) -> Option<PolicyReason> {
    match target {
        TargetRef::Player(player_id) => {
            let beneficial = is_spell_beneficial(ctx);
            let is_self = *player_id == ctx.ai_player;

            if !is_self && !beneficial {
                if let Some(damage) = extract_damage_amount(&ctx.effects()) {
                    let opponent_life = ctx.state.players[player_id.0 as usize].life;
                    if damage >= opponent_life {
                        return None;
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

            (prefers_self != is_self)
                .then(|| PolicyReason::new("anti_self_harm_wrong_player_target"))
        }
        TargetRef::Object(object_id) => target_is_sacrificed_source(ctx, *object_id)
            .then(|| PolicyReason::new("anti_self_harm_sacrificed_source_target")),
    }
}

fn score_target_ref(ctx: &PolicyContext<'_>, target: &TargetRef) -> f64 {
    if target_reject_reason(ctx, target).is_some() {
        return 0.0;
    }

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

            // Spiteful Sliver / Boros Reckoner-style reflection: in multiplayer,
            // concentrate damage on the lowest-life opponent instead of rotating
            // targets each trigger (issue #1364).
            if !is_self
                && !beneficial
                && ctx
                    .effects()
                    .iter()
                    .any(|e| is_event_context_damage_to_player(e))
            {
                let opponents = players::opponents(ctx.state, ctx.ai_player);
                if opponents.len() > 1 {
                    if let Some(weakest) = opponents
                        .iter()
                        .min_by_key(|&&p| ctx.state.players[p.0 as usize].life)
                    {
                        if *player_id == *weakest {
                            return 12.0 + threat_level(ctx.state, ctx.ai_player, *player_id) * 4.0;
                        }
                    }
                }
            }

            4.0 + threat_level(ctx.state, ctx.ai_player, *player_id) * 8.0
        }
        TargetRef::Object(object_id) => {
            let object_beneficial =
                targeted_object_impact(ctx, *object_id).map_or(beneficial, |impact| impact > 0.25);
            score_target_object(ctx, *object_id, object_beneficial)
        }
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
        return 0.0;
    }

    let effects = ctx.effects();

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

    if beneficial
        && effects.iter().any(|effect| {
            // CR 701.26b: only single-target untap (legacy `Effect::Untap`)
            // factors here; the mass scope was never matched.
            matches!(
                effect,
                Effect::SetTapState {
                    scope: EffectScope::Single,
                    state: TapStateChange::Untap,
                    ..
                }
            ) && effect_targets_object(ctx, effect, object_id)
        })
    {
        if object.tapped {
            score += if object.controller == ctx.ai_player {
                ctx.penalties().untap_own_tapped_bonus
            } else {
                ctx.penalties().untap_opponent_tapped_penalty
            };
        } else {
            score += ctx.penalties().untap_untapped_penalty;
        }
    }

    if let Some(copy_effect) = ctx
        .effects()
        .iter()
        .find(|effect| matches!(effect, Effect::CopyTokenOf { .. }))
    {
        if let Some(source) = ctx.source_object() {
            let strips = copy_effect_strips_legendary(copy_effect);
            score -=
                copy_target_penalties(ctx.state, ctx.ai_player, Some(source.id), object, strips);
        }
    }

    if object.card_types.core_types.contains(&CoreType::Creature) {
        score += controller_delta * evaluate_creature(ctx.state, object_id);

        if !beneficial {
            // CR 704.5f/g: penalize removal that provably won't kill its target.
            // `lethal_to_creature` unifies the three shrink/burn modalities (damage,
            // -X/-X, -1/-1 counters); variable-X and non-damage removal return
            // `None` (no penalty). The penalty is a discouragement, not a veto, so
            // a genuine multi-spell kill can still emerge from search lookahead.
            let lethal = lethal_to_creature(ctx.state, object_id, &effects);

            if let Some(damage) = extract_damage_amount(&effects) {
                score += opponent_creature_reflection_penalty(
                    ctx.state,
                    object_id,
                    ctx.ai_player,
                    damage,
                );

                if let Some(toughness) = object.toughness {
                    let remaining = toughness - object.damage_marked as i32;
                    // Graduated non-lethal penalty: almost-lethal burn (leaves 1
                    // toughness) is less wasteful than burn that barely scratches a
                    // large creature.
                    if lethal == Some(false) && remaining > 0 {
                        let survival_ratio =
                            ((remaining - damage).max(0)) as f64 / remaining as f64;
                        // Full penalty (-8.0) when damage is negligible relative to
                        // toughness, reduced penalty (-4.0) when damage is almost lethal.
                        score -= 4.0 + 4.0 * survival_ratio;
                    }
                    // Penalize massive overkill (wasting damage capacity)
                    if remaining > 0 && damage >= remaining && damage > remaining * 2 {
                        let wasted = damage - remaining;
                        let waste_ratio = wasted as f64 / damage as f64;
                        score += ctx.penalties().overkill_base_penalty * waste_ratio.sqrt();
                    }
                }
            } else if lethal == Some(false) {
                // Non-damage shrink (-X/-X, -0/-X, -1/-1 counters) that won't kill
                // the target wastes the spell, mirroring the burn-whiff penalty.
                score -= 8.0;
            }

            // CR 702.16b + CR 702.16e: Protection prevents targeting and damage
            // from sources with the protected quality. Targeting a creature with
            // protection from the spell's qualities wastes the spell entirely.
            if let Some(source) = ctx.source_object() {
                if keywords::protection_prevents_from(object, source) {
                    score -= 100.0;
                }
            }

            // Price the cost of an *affordable* ward (must pay an extra cost).
            // An unaffordable ward is hard-rejected upstream by `tactical_gate`
            // (CR 702.21a — the spell would just be countered), so this judgment
            // layer never double-scores that case.
            for keyword in &object.keywords {
                if let Keyword::Ward(ward_cost) = keyword {
                    if !can_pay_ward_cost(ctx, ward_cost, object) {
                        break;
                    }
                    let severity = match ward_cost {
                        WardCost::Mana(cost) => (cost.mana_value() as f64 / 2.0).min(2.0),
                        WardCost::PayLife(amount) => (*amount as f64 / 3.0).min(2.0),
                        WardCost::PayLifeEqualToPower => {
                            (object.power.unwrap_or(0).max(0) as f64 / 3.0).min(2.0)
                        }
                        WardCost::DiscardCard => 1.5,
                        WardCost::Sacrifice { count, .. } => *count as f64 * 2.0,
                        WardCost::Waterbend(cost) => (cost.mana_value() as f64 / 2.0).min(2.0),
                        // CR 702.21a: Compound costs sum severity of components.
                        WardCost::Compound(costs) => costs
                            .iter()
                            .map(|c| match c {
                                WardCost::Mana(cost) => (cost.mana_value() as f64 / 2.0).min(2.0),
                                WardCost::PayLife(amount) => (*amount as f64 / 3.0).min(2.0),
                                WardCost::PayLifeEqualToPower => {
                                    (object.power.unwrap_or(0).max(0) as f64 / 3.0).min(2.0)
                                }
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
                    score += ctx.penalties().tapped_removal_no_urgency_penalty;
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
                AbilityCost::Sacrifice(cost)
                    if matches!(cost.target, TargetFilter::SelfRef)
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
        AbilityCost::Sacrifice(cost)
            if matches!(cost.target, engine::types::ability::TargetFilter::SelfRef) =>
        {
            true
        }
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
        AbilityCost, AbilityDefinition, AbilityKind, BounceSelection, CardSelectionMode,
        ContinuousModification, ControllerRef, DiscardSelfScope, FilterProp, PtValue, QuantityRef,
        ReplacementDefinition, ResolvedAbility, SacrificeCost, StaticDefinition, TargetFilter,
        TriggerDefinition, TypeFilter, TypedFilter,
    };
    use engine::types::game_state::{
        CastingVariant, GameState, PendingCast, TargetSelectionSlot, WaitingFor,
    };
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::keywords::Keyword;
    use engine::types::mana::ManaCost;
    use engine::types::player::PlayerId;
    use engine::types::replacements::ReplacementEvent;
    use engine::types::statics::StaticMode;
    use engine::types::triggers::TriggerMode;
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

    fn add_land(state: &mut GameState, owner: PlayerId, name: &str, tapped: bool) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.tapped = tapped;
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
                    chooser: None,
                }],
                mode_labels: Vec::new(),
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

    fn make_mutate_target_selection_ctx(
        state: &GameState,
        legal_targets: Vec<TargetRef>,
        candidate_target: Option<TargetRef>,
    ) -> (AiDecisionContext, CandidateAction) {
        let (mut decision, candidate) = make_target_selection_ctx(
            state,
            Effect::TargetOnly {
                target: TargetFilter::Any,
            },
            legal_targets,
            candidate_target,
        );
        if let WaitingFor::TargetSelection { pending_cast, .. } = &mut decision.waiting_for {
            pending_cast.casting_variant = CastingVariant::Mutate;
        }
        (decision, candidate)
    }

    fn graveyard_recursion_creature(state: &mut GameState) -> ObjectId {
        let id = create_object(
            state,
            CardId(900),
            PlayerId(0),
            "Gravedigger".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(2);
        obj.toughness = Some(2);

        let mut creature_card = TypedFilter::creature();
        creature_card.controller = Some(ControllerRef::You);
        creature_card.properties.push(FilterProp::InZone {
            zone: Zone::Graveyard,
        });
        let mut trigger = TriggerDefinition::new(TriggerMode::ChangesZone);
        trigger.valid_card = Some(TargetFilter::SelfRef);
        trigger.destination = Some(Zone::Battlefield);
        trigger.execute = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Hand,
                target: TargetFilter::Typed(creature_card),
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: engine::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: Vec::new(),
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
        )));
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .trigger_definitions
            .push(trigger);
        id
    }

    fn make_cast_spell_decision(
        state: &GameState,
        object_id: ObjectId,
    ) -> (AiDecisionContext, CandidateAction) {
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id,
                card_id: state.objects[&object_id].card_id,
                targets: Vec::new(),

                payment_mode: CastPaymentMode::Auto,
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Spell,
            },
        };
        (decision, candidate)
    }

    #[test]
    fn pre_cast_penalizes_graveyard_etb_with_empty_graveyard() {
        let mut state = make_state();
        let spell_id = graveyard_recursion_creature(&mut state);
        let config = AiConfig::default();
        let (decision, candidate) = make_cast_spell_decision(&state, spell_id);
        let context = crate::context::AiContext::empty(&config.weights);
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };

        let score = AntiSelfHarmPolicy.score(&ctx);

        assert!(
            score < -5.0,
            "Empty graveyard should make targeted recursion ETB a wasted cast, got {score}"
        );
    }

    #[test]
    fn pre_cast_allows_graveyard_etb_with_matching_graveyard_card() {
        let mut state = make_state();
        let spell_id = graveyard_recursion_creature(&mut state);
        let target = create_object(
            &mut state,
            CardId(901),
            PlayerId(0),
            "Dead Bear".to_string(),
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&target)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let config = AiConfig::default();
        let (decision, candidate) = make_cast_spell_decision(&state, spell_id);
        let context = crate::context::AiContext::empty(&config.weights);
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };

        let score = AntiSelfHarmPolicy.score(&ctx);

        assert!(
            score > -1.0,
            "Matching graveyard target should avoid ETB whiff penalty, got {score}"
        );
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
            search_depth: crate::policies::context::SearchDepth::Root,
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
            search_depth: crate::policies::context::SearchDepth::Root,
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
    fn undying_malice_prefers_own_creature() {
        // Undying Malice grants target creature "when this dies, return it to the
        // battlefield" — GenericEffect{ Continuous{ GrantTrigger{ dies →
        // ChangeZone→Battlefield } } }. Pre-fix `modification_polarity(GrantTrigger)`
        // fell to `Contextual`, so `player_impact`/`is_spell_beneficial` read the
        // spell as non-beneficial and `score_target_object` aimed it at an opponent
        // creature. The fix classifies the grant Beneficial (via the executed
        // ChangeZone→Battlefield), flipping the preference to the AI's own creature.
        // Reverting the named `GrantTrigger` arm makes `score_own < score_opp`.
        let mut state = make_state();
        let own_id = add_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let opp_id = add_creature(&mut state, PlayerId(1), "Goblin", 2, 2);
        let config = AiConfig::default();

        let mut trigger = TriggerDefinition::new(TriggerMode::ChangesZone);
        trigger.execute = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Battlefield,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: engine::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
        )));
        let effect = Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::continuous()
                .affected(TargetFilter::ParentTarget)
                .modifications(vec![ContinuousModification::GrantTrigger {
                    trigger: Box::new(trigger),
                }])],
            target: Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature))),
            duration: None,
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
            search_depth: crate::policies::context::SearchDepth::Root,
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
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        let score_opp = AntiSelfHarmPolicy.score(&ctx_opp);

        assert!(
            score_own > score_opp,
            "Undying grant should prefer own creature: own={score_own}, opp={score_opp}"
        );
        assert!(score_own > 0.0, "Own creature score should be positive");
        assert!(
            score_opp < 0.0,
            "Opponent creature score should be negative"
        );
    }

    #[test]
    fn strength_of_tajuru_prefers_own_creature() {
        // VERIFY-ONLY (expected to PASS on unmodified code): Strength of the Tajuru's
        // payoff leaf is `PutCounterAll{ +1/+1 }`, which `counter_sign_polarity`
        // already classifies Beneficial, so `is_spell_beneficial` is true and
        // `score_target_object` prefers the AI's own creature. No code change backs
        // this — it documents the reported "targets opponent" behavior as already
        // correct for the counter payoff (the empty `Typed` target mirrors the real
        // leaf AST). If this ever fails, it is a stop-and-return item, not a fix.
        let mut state = make_state();
        let own_id = add_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let opp_id = add_creature(&mut state, PlayerId(1), "Goblin", 2, 2);
        let config = AiConfig::default();

        let effect = Effect::PutCounterAll {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 2 },
            target: TargetFilter::Typed(TypedFilter::default()),
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
            search_depth: crate::policies::context::SearchDepth::Root,
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
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        let score_opp = AntiSelfHarmPolicy.score(&ctx_opp);

        assert!(
            score_own > score_opp,
            "PutCounterAll{{+1/+1}} should prefer own creature: own={score_own}, opp={score_opp}"
        );
    }

    #[test]
    fn mutate_target_prefers_own_creature() {
        let mut state = make_state();
        let own_id = add_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let opp_id = add_creature(&mut state, PlayerId(1), "Goblin", 2, 2);
        let config = AiConfig::default();
        let context = crate::context::AiContext::empty(&config.weights);

        let (decision, candidate) = make_mutate_target_selection_ctx(
            &state,
            vec![TargetRef::Object(own_id), TargetRef::Object(opp_id)],
            Some(TargetRef::Object(own_id)),
        );
        let ctx_own = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        let score_own = AntiSelfHarmPolicy.score(&ctx_own);

        let (decision, candidate) = make_mutate_target_selection_ctx(
            &state,
            vec![TargetRef::Object(own_id), TargetRef::Object(opp_id)],
            Some(TargetRef::Object(opp_id)),
        );
        let ctx_opp = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        let score_opp = AntiSelfHarmPolicy.score(&ctx_opp);

        assert!(
            score_own > score_opp,
            "Mutate should prefer own creature: own={score_own}, opp={score_opp}"
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
            search_depth: crate::policies::context::SearchDepth::Root,
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
            search_depth: crate::policies::context::SearchDepth::Root,
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
            search_depth: crate::policies::context::SearchDepth::Root,
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
            search_depth: crate::policies::context::SearchDepth::Root,
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
            search_depth: crate::policies::context::SearchDepth::Root,
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
            search_depth: crate::policies::context::SearchDepth::Root,
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
                    chooser: None,
                }],
                mode_labels: Vec::new(),
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
            search_depth: crate::policies::context::SearchDepth::Root,
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
            search_depth: crate::policies::context::SearchDepth::Root,
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
                    chooser: None,
                }],
                mode_labels: Vec::new(),
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
            search_depth: crate::policies::context::SearchDepth::Root,
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
            search_depth: crate::policies::context::SearchDepth::Root,
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
        let effect = Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Any,
        };
        assert_eq!(effect_polarity(&effect), EffectPolarity::Beneficial);
    }

    #[test]
    fn minus_counter_is_harmful() {
        let effect = Effect::PutCounter {
            counter_type: CounterType::Minus1Minus1,
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Any,
        };
        assert_eq!(effect_polarity(&effect), EffectPolarity::Harmful);
    }

    #[test]
    fn generic_positive_pt_counter_is_beneficial() {
        let effect = Effect::PutCounter {
            counter_type: CounterType::Generic("+0/+1".to_string()),
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Any,
        };
        assert_eq!(effect_polarity(&effect), EffectPolarity::Beneficial);
    }

    #[test]
    fn generic_negative_pt_counter_is_harmful() {
        let effect = Effect::PutCounter {
            counter_type: CounterType::Generic("-0/-1".to_string()),
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
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Any,
        };
        assert_eq!(effect_polarity(&effect), EffectPolarity::Beneficial);
    }

    #[test]
    fn put_counter_all_minus_is_harmful() {
        let effect = Effect::PutCounterAll {
            counter_type: CounterType::Minus1Minus1,
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
            counter_type: Some(CounterType::Plus1Plus1),
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Any,
        };
        assert_eq!(effect_polarity(&effect), EffectPolarity::Harmful);
    }

    #[test]
    fn remove_minus_counter_is_beneficial() {
        let effect = Effect::RemoveCounter {
            counter_type: Some(CounterType::Minus1Minus1),
            count: QuantityExpr::Fixed { value: 1 },
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

                payment_mode: CastPaymentMode::Auto,
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
            search_depth: crate::policies::context::SearchDepth::Root,
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

                payment_mode: CastPaymentMode::Auto,
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
            search_depth: crate::policies::context::SearchDepth::Root,
        };

        let score = AntiSelfHarmPolicy.score(&ctx);
        assert!(
            score >= -1.0,
            "Casting first copy of a legendary should not be penalised, got {score}"
        );
    }

    /// CR 704.5j: A "legend rule doesn't apply" exemption (Mirror Gallery's
    /// global static, Sakashima/Sliver Gravemother's scoped variants) means a
    /// second same-name legendary is legal and will NOT be put into the
    /// graveyard. The anti-self-harm legend penalty must defer to the engine's
    /// exemption predicate and apply no penalty when an exemption covers the
    /// controlled copy.
    #[test]
    fn pre_cast_does_not_penalise_duplicate_legendary_under_global_exemption() {
        let mut state = make_state();

        // AI already controls a legendary creature on the battlefield.
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

        // A Mirror-Gallery-class permanent grants a GLOBAL legend-rule exemption
        // (affected = None => applies to every legendary permanent).
        let gallery = create_object(
            &mut state,
            CardId(210),
            PlayerId(0),
            "Mirror Gallery".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&gallery)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::LegendRuleDoesntApply));

        // AI tries to cast a second copy from hand.
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

                payment_mode: CastPaymentMode::Auto,
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
            search_depth: crate::policies::context::SearchDepth::Root,
        };

        let score = AntiSelfHarmPolicy.score(&ctx);
        assert_eq!(
            score, 0.0,
            "Legend-rule exemption must zero the duplicate-legendary penalty, got {score}"
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

                payment_mode: CastPaymentMode::Auto,
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
            search_depth: crate::policies::context::SearchDepth::Root,
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
                selection: BounceSelection::Targeted,
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

                payment_mode: CastPaymentMode::Auto,
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
            search_depth: crate::policies::context::SearchDepth::Root,
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
                selection: BounceSelection::Targeted,
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

                payment_mode: CastPaymentMode::Auto,
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
            search_depth: crate::policies::context::SearchDepth::Root,
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

                payment_mode: CastPaymentMode::Auto,
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
            search_depth: crate::policies::context::SearchDepth::Root,
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

                payment_mode: CastPaymentMode::Auto,
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
            search_depth: crate::policies::context::SearchDepth::Root,
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
                excess: None,
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

                payment_mode: CastPaymentMode::Auto,
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
            search_depth: crate::policies::context::SearchDepth::Root,
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

    #[test]
    fn rewind_land_targets_use_untap_clause_polarity() {
        let mut state = make_state();
        let own_land = add_land(&mut state, PlayerId(0), "Island", true);
        let opp_land = add_land(&mut state, PlayerId(1), "Plains", true);
        let rewind_id = create_object(
            &mut state,
            CardId(500),
            PlayerId(0),
            "Rewind".to_string(),
            Zone::Hand,
        );
        let mut rewind = ResolvedAbility::new(
            Effect::Counter {
                target: TargetFilter::StackSpell,
                source_rider: None,
                countered_spell_zone: None,
            },
            Vec::new(),
            rewind_id,
            PlayerId(0),
        );
        rewind.sub_ability = Some(Box::new(ResolvedAbility::new(
            Effect::SetTapState {
                target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Land)),
                scope: EffectScope::Single,
                state: TapStateChange::Untap,
            },
            Vec::new(),
            rewind_id,
            PlayerId(0),
        )));
        let pending_cast = PendingCast::new(rewind_id, CardId(500), rewind, ManaCost::zero());
        let config = AiConfig::default();

        let score_own = score_rewind_land_target(&state, &config, &pending_cast, own_land);
        let score_opp = score_rewind_land_target(&state, &config, &pending_cast, opp_land);

        assert!(
            score_own > score_opp,
            "Rewind should prefer untapping own land: own={score_own}, opp={score_opp}"
        );
        assert!(
            score_opp < 0.0,
            "Untapping opponent land should be penalised, got {score_opp}"
        );
    }

    fn score_rewind_land_target(
        state: &GameState,
        config: &AiConfig,
        pending_cast: &PendingCast,
        target_id: ObjectId,
    ) -> f64 {
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TargetSelection {
                player: PlayerId(0),
                pending_cast: Box::new(pending_cast.clone()),
                target_slots: vec![TargetSelectionSlot {
                    legal_targets: vec![TargetRef::Object(target_id)],
                    optional: true,
                    chooser: None,
                }],
                mode_labels: Vec::new(),
                selection: Default::default(),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target_id)),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Target,
            },
        };
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        AntiSelfHarmPolicy.score(&ctx)
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
            search_depth: crate::policies::context::SearchDepth::Root,
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

                payment_mode: CastPaymentMode::Auto,
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
            search_depth: crate::policies::context::SearchDepth::Root,
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

                payment_mode: CastPaymentMode::Auto,
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
            search_depth: crate::policies::context::SearchDepth::Root,
        };

        let score = AntiSelfHarmPolicy.score(&ctx);
        assert!(
            score < -5.0,
            "Casting harmful aura with only own creatures should be penalised, got {score}"
        );
    }

    /// Regression: harmful Auras have no active effects, so pre-cast targetability
    /// must come from the Enchant filter rather than the empty effect list.
    #[test]
    fn pre_cast_allows_harmful_aura_with_legal_opponent_creature() {
        let mut state = make_state();
        add_creature(&mut state, PlayerId(1), "Goblin", 2, 2);
        let aura_id = add_harmful_aura(&mut state, PlayerId(0), "Pacifism");

        let score = pre_cast_score_for_spell(&state, aura_id);
        assert!(
            score > -5.0,
            "Casting harmful aura with a legal opponent target should not get the no-target \
             penalty, got {score}"
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
                    chooser: None,
                }],
                mode_labels: Vec::new(),
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
                target: None,
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

                payment_mode: CastPaymentMode::Auto,
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
            search_depth: crate::policies::context::SearchDepth::Root,
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
                target: None,
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

                payment_mode: CastPaymentMode::Auto,
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
            search_depth: crate::policies::context::SearchDepth::Root,
        };

        let score = AntiSelfHarmPolicy.score(&ctx);
        assert!(
            score >= -1.0,
            "Should not penalize when lands can cover cost, got {score}"
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
            band_id: None,
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
            search_depth: crate::policies::context::SearchDepth::Root,
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
                    enters_under: None,
                    enter_tapped: engine::types::zones::EtbTapState::Unspecified,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                    conditional_enter_with_counters: vec![],
                    face_down_profile: None,
                    enters_modified_if: None,
                },
                Vec::new(),
                ObjectId(200),
                PlayerId(0),
            ),
            timestamp: 1,
            target_constraints: Vec::new(),
            distribute: None,
            trigger_event: None,
            modal: None,
            mode_abilities: vec![],
            description: None,
            may_trigger_origin: None,
            subject_match_count: None,
            die_result: None,
        });

        let config = AiConfig::default();
        let legal_targets = vec![TargetRef::Object(creature), TargetRef::Object(token)];
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TriggerTargetSelection {
                player: PlayerId(0),
                trigger_controller: None,
                trigger_event: None,
                trigger_events: Vec::new(),
                target_slots: vec![TargetSelectionSlot {
                    legal_targets: legal_targets.clone(),
                    optional: false,
                    chooser: None,
                }],
                mode_labels: Vec::new(),
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
            search_depth: crate::policies::context::SearchDepth::Root,
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
            search_depth: crate::policies::context::SearchDepth::Root,
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
                    enters_under: None,
                    enter_tapped: engine::types::zones::EtbTapState::Unspecified,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                    conditional_enter_with_counters: vec![],
                    face_down_profile: None,
                    enters_modified_if: None,
                },
                Vec::new(),
                ObjectId(200),
                PlayerId(0),
            ),
            timestamp: 1,
            target_constraints: Vec::new(),
            distribute: None,
            trigger_event: None,
            modal: None,
            mode_abilities: vec![],
            description: None,
            may_trigger_origin: None,
            subject_match_count: None,
            die_result: None,
        });

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TriggerTargetSelection {
                player: PlayerId(0),
                trigger_controller: None,
                trigger_event: None,
                trigger_events: Vec::new(),
                target_slots: vec![],
                mode_labels: Vec::new(),
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
            search_depth: crate::policies::context::SearchDepth::Root,
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
            excess: None,
        };
        let ability = ResolvedAbility::new(effect, Vec::new(), fanatic_id, PlayerId(0));
        let mut pending_cast = PendingCast::new(fanatic_id, CardId(100), ability, ManaCost::zero());
        pending_cast.activation_cost = Some(AbilityCost::Composite {
            costs: vec![AbilityCost::Sacrifice(SacrificeCost::count(
                TargetFilter::SelfRef,
                1,
            ))],
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
                    chooser: None,
                }],
                mode_labels: Vec::new(),
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
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        let verdict_self = AntiSelfHarmPolicy.verdict(&ctx_self);

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
            search_depth: crate::policies::context::SearchDepth::Root,
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
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        let score_player = AntiSelfHarmPolicy.score(&ctx_player);

        assert!(matches!(
            verdict_self,
            PolicyVerdict::Reject { reason }
                if reason.kind == "anti_self_harm_sacrificed_source_target"
        ));
        assert!(
            score_opp > 0.0,
            "Opponent creature should remain a viable target: opp={score_opp}"
        );
        assert!(
            score_player > 0.0,
            "Opponent player should remain a viable target: player={score_player}"
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
            search_depth: crate::policies::context::SearchDepth::Root,
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
            search_depth: crate::policies::context::SearchDepth::Root,
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
            excess: None,
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
            search_depth: crate::policies::context::SearchDepth::Root,
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
            search_depth: crate::policies::context::SearchDepth::Root,
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
            excess: None,
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
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        let score = AntiSelfHarmPolicy.score(&ctx);

        assert!(
            score > 0.0,
            "Lethal burn on tapped creature should be positive (removing a threat), got {score}"
        );
    }

    /// Issue #1364: pinging an opponent's Spiteful-style sliver gives them free
    /// damage triggers — non-lethal damage should be strongly penalized.
    #[test]
    fn non_lethal_damage_on_opponent_spiteful_creature_penalized() {
        let mut state = make_state();
        let spiteful = add_creature(&mut state, PlayerId(1), "Sliver", 2, 3);
        let trigger = TriggerDefinition::new(TriggerMode::DamageReceived)
            .valid_card(TargetFilter::SelfRef)
            .execute(AbilityDefinition::new(
                AbilityKind::Database,
                Effect::DealDamage {
                    amount: QuantityExpr::Ref {
                        qty: QuantityRef::EventContextAmount,
                    },
                    target: TargetFilter::Or {
                        filters: vec![
                            TargetFilter::Player,
                            TargetFilter::Typed(TypedFilter::new(TypeFilter::Planeswalker)),
                        ],
                    },
                    damage_source: None,
                    excess: None,
                },
            ));
        state
            .objects
            .get_mut(&spiteful)
            .unwrap()
            .trigger_definitions
            .push(trigger);

        let effect = Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        };
        let (decision, candidate) = make_target_selection_ctx(
            &state,
            effect,
            vec![TargetRef::Object(spiteful)],
            Some(TargetRef::Object(spiteful)),
        );
        let config = AiConfig::default();
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        let score = AntiSelfHarmPolicy.score(&ctx);
        assert!(
            score <= -10.0,
            "Non-lethal damage on opponent spiteful creature should be heavily penalized, got {score}"
        );
    }

    /// Issue #1364: reflected damage in multiplayer should concentrate on the
    /// lowest-life opponent instead of cycling evenly between opponents.
    #[test]
    fn event_context_damage_prefers_lowest_life_opponent_in_multiplayer() {
        let mut state = GameState::new(engine::types::format::FormatConfig::free_for_all(), 3, 42);
        state.players[0].life = 20;
        state.players[1].life = 5;
        state.players[2].life = 14;

        let effect = Effect::DealDamage {
            amount: QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            },
            target: TargetFilter::Player,
            damage_source: None,
            excess: None,
        };
        let config = AiConfig::default();

        let (decision, candidate_lowest) = make_target_selection_ctx(
            &state,
            effect.clone(),
            vec![
                TargetRef::Player(PlayerId(1)),
                TargetRef::Player(PlayerId(2)),
            ],
            Some(TargetRef::Player(PlayerId(1))),
        );
        let ctx_lowest = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate_lowest,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        let lowest_score = AntiSelfHarmPolicy.score(&ctx_lowest);

        let (decision, candidate_other) = make_target_selection_ctx(
            &state,
            effect,
            vec![
                TargetRef::Player(PlayerId(1)),
                TargetRef::Player(PlayerId(2)),
            ],
            Some(TargetRef::Player(PlayerId(2))),
        );
        let ctx_other = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate_other,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        let other_score = AntiSelfHarmPolicy.score(&ctx_other);

        assert!(
            lowest_score > other_score,
            "Reflected damage should prefer the lowest-life opponent: lowest={lowest_score}, other={other_score}"
        );
    }

    /// Build a white creature-only Destroy spell ("Murder"-style) in the AI's
    /// hand so `score_pre_cast` analyzes a harmful, creature-targeting cast.
    fn white_creature_destroy_spell(state: &mut GameState) -> ObjectId {
        use engine::types::mana::ManaColor;

        let id = create_object(
            state,
            CardId(state.next_object_id),
            PlayerId(0),
            "Murder".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Instant);
        // CR 105.2 + CR 702.16b: the spell's color is the quality a target's
        // "protection from white" checks against.
        obj.color = vec![ManaColor::White];
        obj.abilities = Arc::new(vec![AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Destroy {
                target: TargetFilter::Typed(TypedFilter::creature()),
                cant_regenerate: false,
            },
        )]);
        id
    }

    fn pre_cast_score_for_spell(state: &GameState, spell_id: ObjectId) -> f64 {
        let config = AiConfig::default();
        let (decision, candidate) = make_cast_spell_decision(state, spell_id);
        let context = crate::context::AiContext::empty(&config.weights);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        AntiSelfHarmPolicy.score(&ctx)
    }

    fn pre_cast_verdict_for_spell(state: &GameState, spell_id: ObjectId) -> PolicyVerdict {
        let config = AiConfig::default();
        let (decision, candidate) = make_cast_spell_decision(state, spell_id);
        let context = crate::context::AiContext::empty(&config.weights);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        AntiSelfHarmPolicy.verdict(&ctx)
    }

    fn extra_turn_spell(state: &mut GameState, self_loss: bool) -> ObjectId {
        extra_turn_spell_with_self_loss_phase(
            state,
            self_loss.then_some(DelayedTriggerCondition::AtNextPhaseForPlayer {
                phase: Phase::End,
                player: PlayerId(0),
                gate: engine::types::ability::TurnGate::None,
            }),
        )
    }

    fn extra_turn_spell_with_self_loss_phase(
        state: &mut GameState,
        self_loss_condition: Option<DelayedTriggerCondition>,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            PlayerId(0),
            "Extra Turn Test".to_string(),
            Zone::Hand,
        );
        let mut ability = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ExtraTurn {
                target: TargetFilter::Controller,
            },
        );
        if let Some(condition) = self_loss_condition {
            ability = ability.sub_ability(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::CreateDelayedTrigger {
                    condition,
                    effect: Box::new(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::LoseTheGame { target: None },
                    )),
                    uses_tracked_set: false,
                },
            ));
        }
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Instant);
        obj.abilities = Arc::new(vec![ability]);
        id
    }

    fn mox_diamond_like_spell(state: &mut GameState) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            PlayerId(0),
            "Mox Diamond Test".to_string(),
            Zone::Hand,
        );
        let decline = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Graveyard,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: engine::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: Vec::new(),
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
        );
        let mut land_filter = TypedFilter::new(TypeFilter::Land);
        land_filter
            .properties
            .push(FilterProp::InZone { zone: Zone::Hand });
        let cost = AbilityCost::Discard {
            count: QuantityExpr::Fixed { value: 1 },
            filter: Some(TargetFilter::Typed(land_filter)),
            selection: CardSelectionMode::Chosen,
            self_scope: DiscardSelfScope::FromHand,
        };
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.replacement_definitions
            .push(ReplacementDefinition::new(ReplacementEvent::Moved).mode(
                ReplacementMode::MayCost {
                    cost,
                    decline: Some(Box::new(decline)),
                },
            ));
        id
    }

    fn hand_land(state: &mut GameState) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            PlayerId(0),
            "Discardable Land".to_string(),
            Zone::Hand,
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

    #[test]
    fn rejects_unpayable_self_etb_may_cost_spell() {
        let mut state = make_state();
        let spell_id = mox_diamond_like_spell(&mut state);

        assert!(matches!(
            pre_cast_verdict_for_spell(&state, spell_id),
            PolicyVerdict::Reject { reason }
                if reason.kind == "anti_self_harm_unpayable_etb_may_cost"
        ));
    }

    #[test]
    fn allows_self_etb_may_cost_spell_when_cost_is_payable() {
        let mut state = make_state();
        let spell_id = mox_diamond_like_spell(&mut state);
        hand_land(&mut state);

        assert!(matches!(
            pre_cast_verdict_for_spell(&state, spell_id),
            PolicyVerdict::Score { .. }
        ));
    }

    #[test]
    fn rejects_extra_turn_spell_with_delayed_self_loss() {
        let mut state = make_state();
        state.players[0].life = 38;
        let spell_id = extra_turn_spell(&mut state, true);

        assert!(matches!(
            pre_cast_verdict_for_spell(&state, spell_id),
            PolicyVerdict::Reject { reason }
                if reason.kind == "anti_self_harm_extra_turn_self_loss"
        ));
    }

    #[test]
    fn allows_extra_turn_spell_without_self_loss() {
        let mut state = make_state();
        let spell_id = extra_turn_spell(&mut state, false);

        assert!(matches!(
            pre_cast_verdict_for_spell(&state, spell_id),
            PolicyVerdict::Score { .. }
        ));
    }

    #[test]
    fn allows_extra_turn_spell_with_non_end_step_delayed_self_loss() {
        let mut state = make_state();
        let spell_id = extra_turn_spell_with_self_loss_phase(
            &mut state,
            Some(DelayedTriggerCondition::AtNextPhaseForPlayer {
                phase: Phase::Upkeep,
                player: PlayerId(0),
                gate: engine::types::ability::TurnGate::None,
            }),
        );

        assert!(matches!(
            pre_cast_verdict_for_spell(&state, spell_id),
            PolicyVerdict::Score { .. }
        ));
    }

    /// CR 702.16b: An opponent creature with protection from white is not a
    /// legal target for a white removal spell, so casting it would fizzle.
    /// The engine-backed legality check must surface the no-target penalty —
    /// the old hand-rolled `!Hexproof && !Shroud` check ignored Protection.
    #[test]
    fn pre_cast_penalizes_white_removal_into_protection_from_white() {
        use engine::types::keywords::{Keyword, ProtectionTarget};
        use engine::types::mana::ManaColor;

        let mut state = make_state();
        let opp = add_creature(&mut state, PlayerId(1), "Guardian", 2, 2);
        state
            .objects
            .get_mut(&opp)
            .unwrap()
            .keywords
            .push(Keyword::Protection(ProtectionTarget::Color(
                ManaColor::White,
            )));
        let spell_id = white_creature_destroy_spell(&mut state);

        let score = pre_cast_score_for_spell(&state, spell_id);
        assert!(
            score <= -8.0,
            "White removal with only a protection-from-white target should be penalized, got {score}"
        );
    }

    /// CR 702.11d: An opponent creature with "hexproof from white" can't be
    /// targeted by the white removal spell either.
    #[test]
    fn pre_cast_penalizes_white_removal_into_hexproof_from_white() {
        use engine::types::keywords::{HexproofFilter, Keyword};
        use engine::types::mana::ManaColor;

        let mut state = make_state();
        let opp = add_creature(&mut state, PlayerId(1), "Warden", 2, 2);
        state
            .objects
            .get_mut(&opp)
            .unwrap()
            .keywords
            .push(Keyword::HexproofFrom(HexproofFilter::Color(
                ManaColor::White,
            )));
        let spell_id = white_creature_destroy_spell(&mut state);

        let score = pre_cast_score_for_spell(&state, spell_id);
        assert!(
            score <= -8.0,
            "White removal with only a hexproof-from-white target should be penalized, got {score}"
        );
    }

    /// Control: the same opponent creature with no protection IS a legal
    /// target, so no no-target penalty applies.
    #[test]
    fn pre_cast_allows_white_removal_into_unprotected_creature() {
        let mut state = make_state();
        add_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        let spell_id = white_creature_destroy_spell(&mut state);

        let score = pre_cast_score_for_spell(&state, spell_id);
        assert!(
            score > -8.0,
            "White removal with a legal unprotected target should not be penalized, got {score}"
        );
    }

    // --- Optional-effect life-cost self-harm guard ---------------------------

    /// Build an object on the battlefield carrying an Optional replacement whose
    /// life payment lives in the given branch of the execute ability tree.
    fn make_optional_lose_life_source(
        state: &mut GameState,
        amount: QuantityExpr,
        branch: LifeCostBranch,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            PlayerId(0),
            "Painful Passage".to_string(),
            Zone::Battlefield,
        );

        let lose_life = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::LoseLife {
                amount,
                target: None,
            },
        );
        // A benign primary effect; the life cost sits in a non-primary branch so
        // the test exercises the full tree walk, not just the root effect.
        let benign = || {
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 0 },
                    target: TargetFilter::Controller,
                },
            )
        };
        let mut execute = benign();
        match branch {
            LifeCostBranch::Sub => execute = execute.sub_ability(lose_life),
            LifeCostBranch::Else => execute.else_ability = Some(Box::new(lose_life)),
            LifeCostBranch::Modal => execute.mode_abilities = vec![benign(), lose_life],
        }

        state
            .objects
            .get_mut(&id)
            .unwrap()
            .replacement_definitions
            .push(
                ReplacementDefinition::new(ReplacementEvent::Moved)
                    .mode(ReplacementMode::Optional { decline: None })
                    .execute(execute),
            );
        id
    }

    #[derive(Clone, Copy)]
    enum LifeCostBranch {
        Sub,
        Else,
        Modal,
    }

    fn optional_effect_accept_verdict(state: &GameState) -> PolicyVerdict {
        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::DecideOptionalEffect { accept: true },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Replacement,
            },
        };
        let context = crate::context::AiContext::empty(&config.weights);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        AntiSelfHarmPolicy.verdict(&ctx)
    }

    /// `OptionalEffectChoice` routes through `DecisionKind::ActivateAbility`, so
    /// the registry must still invoke `AntiSelfHarmPolicy` for the production
    /// candidate path rather than only when tests call `score()` directly.
    #[test]
    fn optional_life_cost_accept_is_scored_by_policy_registry() {
        let mut state = make_state();
        let source_id = make_optional_lose_life_source(
            &mut state,
            QuantityExpr::Fixed { value: 5 },
            LifeCostBranch::Else,
        );
        state.players[0].life = 5;
        state.waiting_for = WaitingFor::OptionalEffectChoice {
            player: PlayerId(0),
            source_id,
            description: None,
            may_trigger_key: None,
        };

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: state.waiting_for.clone(),
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::DecideOptionalEffect { accept: true },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Replacement,
            },
        };
        let context = crate::context::AiContext::empty(&config.weights);
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };

        let verdicts = crate::policies::registry::PolicyRegistry::shared().verdicts(&ctx);
        let anti_self_harm_reject = verdicts.into_iter().any(|(id, verdict)| {
            id == PolicyId::AntiSelfHarm
                && matches!(
                    verdict,
                    PolicyVerdict::Reject { reason }
                        if reason.kind == "anti_self_harm_lethal_life_cost"
                )
        });

        assert!(
            anti_self_harm_reject,
            "OptionalEffectChoice accept must be rejected by AntiSelfHarmPolicy"
        );
    }

    /// CR 119.6 / CR 704.5a: accepting an optional life payment that brings the AI
    /// to 0 or less is self-lethal. The guard must fire even when the `LoseLife`
    /// sits in a non-`sub_ability` branch (else / modal mode).
    #[test]
    fn optional_life_cost_in_else_branch_penalises_lethal_accept() {
        let mut state = make_state();
        let source_id = make_optional_lose_life_source(
            &mut state,
            QuantityExpr::Fixed { value: 5 },
            LifeCostBranch::Else,
        );
        state.players[0].life = 5;
        state.waiting_for = WaitingFor::OptionalEffectChoice {
            player: PlayerId(0),
            source_id,
            description: None,
            may_trigger_key: None,
        };
        assert!(matches!(
            optional_effect_accept_verdict(&state),
            PolicyVerdict::Reject { reason }
                if reason.kind == "anti_self_harm_lethal_life_cost"
        ));
    }

    #[test]
    fn optional_life_cost_in_modal_branch_penalises_lethal_accept() {
        let mut state = make_state();
        let source_id = make_optional_lose_life_source(
            &mut state,
            QuantityExpr::Fixed { value: 3 },
            LifeCostBranch::Modal,
        );
        state.players[0].life = 3;
        state.waiting_for = WaitingFor::OptionalEffectChoice {
            player: PlayerId(0),
            source_id,
            description: None,
            may_trigger_key: None,
        };
        assert!(matches!(
            optional_effect_accept_verdict(&state),
            PolicyVerdict::Reject { reason }
                if reason.kind == "anti_self_harm_lethal_life_cost"
        ));
    }

    #[test]
    fn optional_life_cost_with_ample_life_is_accepted() {
        let mut state = make_state();
        let source_id = make_optional_lose_life_source(
            &mut state,
            QuantityExpr::Fixed { value: 2 },
            LifeCostBranch::Else,
        );
        state.players[0].life = 20;
        state.waiting_for = WaitingFor::OptionalEffectChoice {
            player: PlayerId(0),
            source_id,
            description: None,
            may_trigger_key: None,
        };
        assert!(matches!(
            optional_effect_accept_verdict(&state),
            PolicyVerdict::Score { delta, .. } if delta == 0.0
        ));
    }

    /// Non-`Fixed` amount: "lose life equal to the number of creatures you control".
    /// Resolved against live game state via `resolve_quantity`; with N creatures and
    /// N life the payment is lethal and must trigger the guard even though the amount
    /// is not a literal constant.
    #[test]
    fn optional_life_cost_non_fixed_amount_resolves_and_penalises() {
        let mut state = make_state();
        // Three AI creatures makes "for each creature you control" resolve to 3.
        add_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        add_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        add_creature(&mut state, PlayerId(0), "Bear", 2, 2);

        let mut creature_filter = TypedFilter::creature();
        creature_filter.controller = Some(ControllerRef::You);
        let amount = QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(creature_filter),
            },
        };
        let source_id = make_optional_lose_life_source(&mut state, amount, LifeCostBranch::Sub);
        state.players[0].life = 3;
        state.waiting_for = WaitingFor::OptionalEffectChoice {
            player: PlayerId(0),
            source_id,
            description: None,
            may_trigger_key: None,
        };
        assert!(matches!(
            optional_effect_accept_verdict(&state),
            PolicyVerdict::Reject { reason }
                if reason.kind == "anti_self_harm_lethal_life_cost"
        ));
    }
}
