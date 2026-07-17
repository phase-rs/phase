//! Net-value gate for self-cost ability activations.
//!
//! An activated ability whose *cost* spends the AI's own resources — it
//! sacrifices a permanent, pays life, discards, or exiles cards from the AI's
//! own hand/graveyard — should only be activated when its *effect* buys
//! something worth the loss. The free-outlet policy only prices creature
//! sacrifice for aristocrats outlets; land-sacrifice lifegain (Zuran Orb),
//! pay-life pingers, discard-to-grant loops, and self-exile-from-graveyard
//! grants all slip past it, so the AI cracks them every turn for nothing.
//!
//! This module is the single authority that (1) recognizes those four cost
//! shapes on the `AbilityCost` tree, (2) prices the self-inflicted cost, and
//! (3) decides whether the ability's immediate payoff is trivial. It is
//! deliberately conservative: anything whose payoff scales or is ambiguous —
//! mana production, land search, large or power-derived damage, beneficial
//! counters — is treated as non-trivial, so ramp, fixing, burn finishers, and
//! counter payoffs are never suppressed. Off-ability synergy (an aristocrats
//! board that turns each death into value, a lifegain/reanimator shell that
//! wants the resource spent) stands the gate down entirely.
//!
//! Only the *scoring* half lives here; the thin `SelfCostValuePolicy` adapter
//! (`self_cost_value.rs`) fetches the activated ability and turns these
//! predicates into a `PolicyVerdict`.

use engine::game::bracket_estimate::CommanderBracketTier;
use engine::game::filter::{matches_target_filter, FilterContext};
use engine::game::game_object::GameObject;
use engine::game::players;
use engine::game::quantity::resolve_quantity;
use engine::types::ability::{AbilityCost, AbilityDefinition, Effect, QuantityExpr, TargetFilter};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::ability_chain::collect_chain_effects;
use crate::config::PolicyPenalties;
use crate::eval::board_stats;
use crate::features::landfall::ability_searches_library_for_land;
use crate::features::mana_ramp::target_filter_references_land;
use crate::features::DeckFeatures;

use super::effect_classify::lethal_to_creature;
use super::self_protection_classify::{
    any_immediate_threat, is_self_protection_effect, self_protection_effect_payoff,
};
use super::strategy_helpers::{sacrifice_cost, targetable_threat_value};

/// A fixed face-damage payoff at or below this value is trivial — a 1- or
/// 2-point ping for no board effect is not worth spending a real resource on.
const FACE_DAMAGE_TRIVIAL_CEILING: i32 = 2;
/// Gaining this much life or less, with the AI not under life pressure, is not
/// worth a real self-cost (Zuran Orb's 2 is the flagship case).
const TRIVIAL_LIFEGAIN_CEILING: i32 = 3;
/// Multiplier applied to the per-point pay-life cost when the AI's life is a
/// pressured resource (mirrors `LifeTotalResourcePolicy`'s criticality test).
const PAY_LIFE_CRITICALITY_MULT: f64 = 4.0;
/// Deck-commitment floor above which an off-ability synergy payoff (lifegain,
/// reanimator) justifies paying the corresponding self-cost. Mirrors
/// `FreeOutletActivationPolicy::COMMITMENT_FLOOR`.
const SYNERGY_COMMITMENT_FLOOR: f32 = 0.1;

/// True when the ability's cost spends one of the four self-resources this gate
/// prices. Recurses `Composite`/`OneOf`. Only `Exile` from the AI's own hand or
/// graveyard is in scope; the `ExileMaterials` / `CollectEvidence` /
/// `ExileWithAggregate` / `Behold` cost siblings (and every other cost variant)
/// deliberately do not fire the gate — they are structurally different payment
/// shapes, so the catch-all is fail-open (a new cost variant simply gets no
/// gate rather than a spurious veto).
pub(crate) fn self_cost_in_scope(cost: &AbilityCost) -> bool {
    match cost {
        AbilityCost::Sacrifice(_) | AbilityCost::PayLife { .. } | AbilityCost::Discard { .. } => {
            true
        }
        // Exile-as-cost: only the AI's own hand (a discard by another name) or
        // graveyard is a self-resource loss. Library/other zones and a bare
        // `None` zone are out of scope.
        AbilityCost::Exile { zone, .. } => {
            matches!(zone, Some(Zone::Graveyard) | Some(Zone::Hand))
        }
        AbilityCost::Composite { costs } | AbilityCost::OneOf { costs } => {
            costs.iter().any(self_cost_in_scope)
        }
        _ => false,
    }
}

/// Price the self-inflicted portion of `cost` in card-equivalent units.
/// `Composite` sums its sub-costs (you pay them all); `OneOf` takes the minimum
/// (the payer chooses the cheapest). Out-of-scope sub-costs (mana, tap) price 0.
pub(crate) fn real_self_cost(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    cost: &AbilityCost,
    penalties: &PolicyPenalties,
) -> f64 {
    match cost {
        AbilityCost::Sacrifice(sacrifice) => {
            sacrifice_leaf_cost(state, ai_player, source_id, &sacrifice.target, penalties)
        }
        // `amount` is a QuantityExpr — resolve it, then weight by the per-point
        // life cost and by runtime life pressure.
        AbilityCost::PayLife { amount } => {
            let points = resolve_quantity(state, amount, ai_player, source_id).max(0) as f64;
            points
                * penalties.self_cost_pay_life_per_point
                * pay_life_criticality_mult(state, ai_player)
        }
        // Discard `count` is a QuantityExpr (unlike `Exile.count`).
        AbilityCost::Discard { count, .. } => {
            let cards = resolve_quantity(state, count, ai_player, source_id).max(0) as f64;
            cards * penalties.self_cost_discard_per_card
        }
        // `Exile.count` is a plain `u32` here, so multiply directly —
        // `resolve_quantity` takes a `&QuantityExpr` and does not apply. Hand
        // exile is priced as a discard; graveyard exile is cheap.
        AbilityCost::Exile { count, zone, .. } => match zone {
            Some(Zone::Graveyard) => (*count as f64) * penalties.self_cost_exile_graveyard_per_card,
            Some(Zone::Hand) => (*count as f64) * penalties.self_cost_discard_per_card,
            _ => 0.0,
        },
        AbilityCost::Composite { costs } => costs
            .iter()
            .map(|c| real_self_cost(state, ai_player, source_id, c, penalties))
            .sum(),
        AbilityCost::OneOf { costs } => {
            let min = costs
                .iter()
                .map(|c| real_self_cost(state, ai_player, source_id, c, penalties))
                .fold(f64::INFINITY, f64::min);
            if min.is_finite() {
                min
            } else {
                0.0
            }
        }
        _ => 0.0,
    }
}

/// Cost of the permanent(s) the sacrifice would consume. A `SelfRef` sacrifice
/// is priced against the ability's own source (never the cheapest permanent);
/// any other filter takes the cheapest AI-controlled match.
fn sacrifice_leaf_cost(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    target: &TargetFilter,
    penalties: &PolicyPenalties,
) -> f64 {
    if matches!(target, TargetFilter::SelfRef) {
        return sacrifice_cost(state, source_id, penalties);
    }
    let filter_ctx = FilterContext::from_source(state, source_id);
    let min = state
        .battlefield
        .iter()
        .filter_map(|&id| {
            let obj = state.objects.get(&id)?;
            (obj.controller == ai_player && matches_target_filter(state, id, target, &filter_ctx))
                .then(|| sacrifice_cost(state, id, penalties))
        })
        .fold(f64::INFINITY, f64::min);
    if min.is_finite() {
        min
    } else {
        0.0
    }
}

/// True when every effect the ability produces is trivial — no meaningful
/// immediate advantage that would justify the self-cost.
pub(crate) fn benefit_is_trivial(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    ability: &AbilityDefinition,
) -> bool {
    collect_chain_effects(ability)
        .iter()
        .all(|effect| effect_is_trivial(state, ai_player, source_id, ability, effect))
}

/// Whether a single effect carries no meaningful immediate advantage. Shared
/// with the X-cast no-op gate (`x_cast_gate.rs`) for pricing the *non-X*
/// residual effects of an {X}-cost payoff whose only affordable X is 0.
pub(crate) fn effect_is_trivial(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    ability: &AbilityDefinition,
    effect: &Effect,
) -> bool {
    match effect {
        // Drawing a card is real card advantage.
        Effect::Draw { count, .. } => resolve_quantity(state, count, ai_player, source_id) < 1,
        // Damage is non-trivial if it is dynamic (power-derived, e.g. Fling),
        // lethal to a player, kills a real creature, or exceeds the face-ping
        // ceiling.
        Effect::DealDamage { amount, target, .. } => {
            deal_damage_is_trivial(state, ai_player, source_id, amount, target)
        }
        // Small lifegain is trivial unless the AI is under life pressure.
        Effect::GainLife { amount, .. } => {
            resolve_quantity(state, amount, ai_player, source_id) <= TRIVIAL_LIFEGAIN_CEILING
                && !ai_life_critical(state, ai_player)
        }
        // Removal is non-trivial when a worthwhile opponent creature can be hit.
        Effect::Destroy { target, .. } | Effect::Bounce { target, .. } => {
            removal_is_trivial(state, ai_player, source_id, target)
        }
        Effect::ChangeZone {
            destination: Zone::Exile | Zone::Graveyard,
            target,
            ..
        } => removal_is_trivial(state, ai_player, source_id, target),
        // A beneficial counter (e.g. an indestructible keyword counter) is
        // non-trivial by default; a harmful counter is removal. The one trivial
        // case is a self-counter that fizzles because paying the cost removes
        // its only recipient (Carrion Feeder into an empty board).
        Effect::PutCounter {
            counter_type,
            target,
            ..
        } => put_counter_is_trivial(state, ai_player, source_id, ability, counter_type, target),
        // A mass counter mirrors the single-`PutCounter` classification: a
        // harmful mass counter (e.g. "-1/-1 counter on each creature") is real
        // board interaction and non-trivial whenever it wipes/shrinks a
        // worthwhile opponent creature — only trivial when it has no worthwhile
        // opponent-board impact. A beneficial mass counter is non-trivial by
        // default (conservative), consistent with single `PutCounter`.
        Effect::PutCounterAll {
            counter_type,
            target,
            ..
        } => {
            if counter_is_harmful(counter_type) {
                removal_is_trivial(state, ai_player, source_id, target)
            } else {
                false
            }
        }
        // Mana production is ramp — never trivial (Ashnod's/Phyrexian Altar).
        Effect::Mana { .. } => false,
        // A library search for a land is ramp/fixing (sacrifice-a-land fetch
        // chains) — non-trivial.
        Effect::SearchLibrary { filter, .. } => {
            !(ability_searches_library_for_land(ability) || target_filter_references_land(filter))
        }
        // A self-protection grant is only worth a cost when a threat is live.
        effect if is_self_protection_effect(effect) => {
            match self_protection_effect_payoff(state, ai_player, source_id, effect) {
                Some(has_payoff) => !has_payoff,
                None => !any_immediate_threat(state, ai_player),
            }
        }
        // No modeled board impact → trivial.
        _ => true,
    }
}

/// A fixed face ping at or below the ceiling, that neither kills a
/// player nor a real creature, is trivial. Dynamic (non-`Fixed`) damage is
/// power-derived (Fling and friends) and always treated as non-trivial so burn
/// finishers are never suppressed.
fn deal_damage_is_trivial(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    amount: &QuantityExpr,
    target: &TargetFilter,
) -> bool {
    let QuantityExpr::Fixed { value } = amount else {
        return false;
    };
    let value = *value;
    if value > FACE_DAMAGE_TRIVIAL_CEILING {
        return false;
    }
    if filter_can_target_player(target) && damage_lethal_to_opponent(state, ai_player, value) {
        return false;
    }
    if damage_kills_creature(state, ai_player, source_id, target, value) {
        return false;
    }
    true
}

fn filter_can_target_player(target: &TargetFilter) -> bool {
    match target {
        TargetFilter::Any | TargetFilter::Player => true,
        // Typed filters select permanents, not players.
        TargetFilter::Typed(_) => false,
        // Unknown/compound filters: fail-open (assume a player could be hit).
        _ => true,
    }
}

fn damage_lethal_to_opponent(state: &GameState, ai_player: PlayerId, value: i32) -> bool {
    players::opponents(state, ai_player).iter().any(|&opp| {
        let player = &state.players[opp.0 as usize];
        !player.is_eliminated && player.life <= value
    })
}

/// True when `value` fixed damage would be lethal to at least one opponent
/// creature the filter admits (via `lethal_to_creature`).
fn damage_kills_creature(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    target: &TargetFilter,
    value: i32,
) -> bool {
    let opponents = players::opponents(state, ai_player);
    let filter_ctx = FilterContext::from_source(state, source_id);
    let damage = Effect::DealDamage {
        amount: QuantityExpr::Fixed { value },
        target: TargetFilter::Any,
        damage_source: None,
        excess: None,
    };
    state.battlefield.iter().any(|&id| {
        let Some(obj) = state.objects.get(&id) else {
            return false;
        };
        opponents.contains(&obj.controller)
            && obj.card_types.core_types.contains(&CoreType::Creature)
            && matches_target_filter(state, id, target, &filter_ctx)
            && lethal_to_creature(state, id, &[&damage]) == Some(true)
    })
}

fn removal_is_trivial(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    target: &TargetFilter,
) -> bool {
    targetable_threat_value(state, ai_player, target, source_id) <= 0.0
}

/// A placed counter is beneficial-by-default (indestructible, +1/+1)
/// unless its sign is negative. A harmful counter routes through removal
/// semantics; a beneficial counter is trivial only when it fizzles.
fn put_counter_is_trivial(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    ability: &AbilityDefinition,
    counter_type: &CounterType,
    target: &TargetFilter,
) -> bool {
    if counter_is_harmful(counter_type) {
        return removal_is_trivial(state, ai_player, source_id, target);
    }
    put_counter_fizzles(state, ai_player, source_id, ability, target)
}

/// A self-targeted beneficial counter fizzles when paying the cost necessarily
/// removes the source — sacrificing the only recipient (Carrion Feeder with no
/// other creature to feed it).
fn put_counter_fizzles(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    ability: &AbilityDefinition,
    counter_target: &TargetFilter,
) -> bool {
    if !matches!(counter_target, TargetFilter::SelfRef) {
        return false;
    }
    ability
        .cost
        .as_ref()
        .is_some_and(|cost| sacrifice_must_remove_source(state, ai_player, source_id, cost))
}

/// True when paying `cost` necessarily sacrifices the ability's source — either
/// a `SelfRef` sacrifice, or a filtered sacrifice whose only legal AI-controlled
/// target is the source itself.
fn sacrifice_must_remove_source(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    cost: &AbilityCost,
) -> bool {
    match cost {
        AbilityCost::Sacrifice(sacrifice) => {
            if matches!(sacrifice.target, TargetFilter::SelfRef) {
                return true;
            }
            let filter_ctx = FilterContext::from_source(state, source_id);
            let mut matched_any = false;
            let mut matched_other = false;
            for &id in &state.battlefield {
                let Some(obj) = state.objects.get(&id) else {
                    continue;
                };
                if obj.controller == ai_player
                    && matches_target_filter(state, id, &sacrifice.target, &filter_ctx)
                {
                    matched_any = true;
                    if id != source_id {
                        matched_other = true;
                    }
                }
            }
            matched_any && !matched_other
        }
        AbilityCost::Composite { costs } | AbilityCost::OneOf { costs } => costs
            .iter()
            .any(|c| sacrifice_must_remove_source(state, ai_player, source_id, c)),
        _ => false,
    }
}

/// Harmful counters are the negative-sign counters (`-1/-1`, negative
/// power/toughness, or a generic counter whose name reads negative). Everything
/// else — `+1/+1`, keyword counters (indestructible), and other typed counters —
/// is treated as beneficial-by-default for the gate.
fn counter_is_harmful(counter_type: &CounterType) -> bool {
    match counter_type {
        CounterType::Minus1Minus1 => true,
        CounterType::PowerToughness { power, toughness } => *power < 0 || *toughness < 0,
        CounterType::Generic(name) => name.starts_with('-'),
        _ => false,
    }
}

/// The AI's life is a pressured resource when it is at or
/// below 5, or at or below the opponents' combined board power. Mirrors
/// `LifeTotalResourcePolicy`'s `ai_critical` test.
fn ai_life_critical(state: &GameState, ai_player: PlayerId) -> bool {
    let ai_life = state.players[ai_player.0 as usize].life;
    let opp_total_power: i32 = players::opponents(state, ai_player)
        .iter()
        .map(|&opp| board_stats(state, opp).1)
        .sum();
    ai_life <= 5 || ai_life <= opp_total_power
}

fn pay_life_criticality_mult(state: &GameState, ai_player: PlayerId) -> f64 {
    if ai_life_critical(state, ai_player) {
        PAY_LIFE_CRITICALITY_MULT
    } else {
        1.0
    }
}

/// Whether off-ability deck synergy justifies paying this self-cost even though
/// the ability's own effect is trivial. Complements the intrinsic-payoff check
/// in [`benefit_is_trivial`] — it covers value that lands elsewhere (aristocrats
/// death triggers, a lifegain/reanimator engine fed by the resource spent).
pub(crate) fn synergy_justifies_self_cost(
    features: &DeckFeatures,
    state: &GameState,
    ai_player: PlayerId,
    ability: &AbilityDefinition,
) -> bool {
    // cEDH lists run tight combo/engine lines where routine self-costs are the
    // intended fuel; never veto self-costs for a Cedh-bracket deck.
    if features.bracket_tier == CommanderBracketTier::Cedh {
        return true;
    }
    ability
        .cost
        .as_ref()
        .is_some_and(|cost| synergy_justifies_cost(features, state, ai_player, cost))
}

fn synergy_justifies_cost(
    features: &DeckFeatures,
    state: &GameState,
    ai_player: PlayerId,
    cost: &AbilityCost,
) -> bool {
    match cost {
        // Board-gated aristocrats payoff only. NO landfall stand-down: landfall
        // triggers when a land ENTERS the battlefield, never when one is
        // sacrificed, so "sacrifice a land" yields zero landfall value —
        // including it would reopen Zuran Orb in landfall decks. Genuine
        // sacrifice-a-land ramp is already non-trivial via the mana / land-search
        // arms of `benefit_is_trivial`.
        AbilityCost::Sacrifice(_) => {
            count_death_triggers_on_board(
                state,
                ai_player,
                &features.aristocrats.death_trigger_names,
            ) > 0
        }
        AbilityCost::PayLife { .. } => features.lifegain.commitment >= SYNERGY_COMMITMENT_FLOOR,
        AbilityCost::Discard { .. } => features.reanimator.commitment >= SYNERGY_COMMITMENT_FLOOR,
        // Exile from the AI's own hand/graveyard: no synergy stand-down. Graveyard
        // exile is a strict loss for a reanimator deck (it removes fuel), and no
        // real card pairs a trivial payoff with a hand-exile self-cost.
        AbilityCost::Exile { .. } => false,
        AbilityCost::Composite { costs } | AbilityCost::OneOf { costs } => costs
            .iter()
            .any(|c| synergy_justifies_cost(features, state, ai_player, c)),
        _ => false,
    }
}

/// Count AI-controlled death-trigger payoff objects currently on the battlefield.
/// Uses `death_trigger_names` as an identity-lookup list — the structural
/// classification already happened at deck-build time in `aristocrats::detect`.
/// Shared with `FreeOutletActivationPolicy` (the aristocrats sac-outlet path).
pub(crate) fn count_death_triggers_on_board(
    state: &GameState,
    player: PlayerId,
    death_trigger_names: &[String],
) -> usize {
    if death_trigger_names.is_empty() {
        return 0;
    }
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|obj: &&GameObject| obj.controller == player && obj.zone == Zone::Battlefield)
        .filter(|obj| death_trigger_names.iter().any(|name| name == &obj.name))
        .count()
}
