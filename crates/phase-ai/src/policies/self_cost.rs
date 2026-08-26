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
//! (3) decides whether the ability's immediate payoff is trivial, and prices
//! the payoffs it can price confidently (draws, out-of-pressure lifegain) for
//! the net-value comparison — a draw the policy can classify as **AI-only**
//! is priced only when the engine can complete an exact present-state
//! draw-delivery preview. An exact partial or zero delivery is priced exactly;
//! a replacement order, optional replacement, or continuation-owned choice is
//! `Unpriced` and therefore leaves the comparison neutral. Only AI-only
//! recipients are previewed — mixed-recipient draws are `Unpriced`, while
//! opponent-only draws are trivial churn.
//!
//! It is deliberately conservative: anything whose
//! payoff scales or is ambiguous — mana production, land search, large or
//! power-derived damage, beneficial counters — is treated as non-trivial, so
//! ramp, fixing, burn finishers, and counter payoffs are never suppressed.
//! That conservatism is typed as [`BenefitAppraisal::Unpriced`], which covers
//! both a non-trivial effect with no confident price and an unmodeled rider
//! sitting beside a priced effect — in either case no directional conclusion
//! about the trade is sound, so the comparison stands down. Off-ability
//! synergy for a resource that is actually spent (a lifegain/reanimator shell)
//! stands the gate down entirely. A sacrifice is never a generic discount:
//! death-trigger value must be represented by the activated ability's real
//! payoff before it can outweigh that cost.
//!
//! Cost-vs-benefit for a self-cost activation is answered **here and only
//! here**: `FreeOutletActivationPolicy` scores aristocrats death-trigger
//! payoff presence for free outlets and holds no cost authority.
//!
//! Only the *scoring* half lives here; the thin `SelfCostValuePolicy` adapter
//! (`self_cost_value.rs`) fetches the activated ability and turns these
//! predicates into a `PolicyVerdict`.

use engine::game::effects::counters::{preview_counter_addition, CounterAdditionPreview};
use engine::game::effects::draw::{preview_draw_delivery, DrawDeliveryPreview};
use engine::game::filter::{matches_target_filter, FilterContext};
use engine::game::game_object::GameObject;
use engine::game::players;
use engine::game::quantity::resolve_quantity;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, CostCategory, Effect, QuantityExpr, TargetFilter,
};
use engine::types::card_type::CoreType;
use engine::types::counter::{CounterMatch, CounterType};
use engine::types::game_state::GameState;
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::ability_chain::collect_chain_effects;
use crate::config::PolicyPenalties;
use crate::eval::board_stats;
use crate::features::landfall::ability_searches_library_for_land;
use crate::features::mana_ramp::target_filter_references_land;
use crate::features::DeckFeatures;

use super::effect_classify::{
    aggregate_player_impact_in, extract_target_filter, lethal_to_creature,
    targeted_player_impact_in, PLAYER_IMPACT_PREFERENCE_BAND,
};
use super::self_protection_classify::{
    any_immediate_threat, is_self_protection_effect, self_protection_effect_payoff,
};
use super::strategy_helpers::{sacrifice_cost, targetable_threat_value, SINGLE_CARD_VALUE};

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

/// Replacement-aware fact for the narrow "remove N typed counters from this,
/// then put exactly N of that type on this" activated-ability shape.
///
/// This is intentionally not a general counter-value model. It only protects a
/// self-cost that claims to replenish its own exact counter payment, where a
/// replacement preventing the add would turn the activation into a pure loss.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SelfCounterCostPreview {
    Applied,
    Prevented,
    ChoiceRequired,
    Transformed,
    Unsupported,
}

pub(crate) fn self_counter_cost_preview(
    state: &GameState,
    actor: PlayerId,
    source_id: ObjectId,
    ability: &AbilityDefinition,
) -> Option<SelfCounterCostPreview> {
    let (count, counter_type) = self_counter_removal_cost(ability.cost.as_ref()?)?;
    let Effect::PutCounter {
        counter_type: replenished_type,
        count: QuantityExpr::Fixed { value },
        target: TargetFilter::SelfRef,
    } = &*ability.effect
    else {
        return None;
    };
    if ability.sub_ability.is_some()
        || ability.else_ability.is_some()
        || counter_type != replenished_type
        || *value != i32::try_from(count).ok()?
    {
        return None;
    }

    let source = state.objects.get(&source_id)?;
    if source.controller != actor {
        return None;
    }
    match preview_counter_addition(
        state,
        actor,
        ObjectIncarnationRef::from_object(source),
        counter_type.clone(),
        count,
    )? {
        CounterAdditionPreview::Applied { .. } => Some(SelfCounterCostPreview::Applied),
        CounterAdditionPreview::Prevented => Some(SelfCounterCostPreview::Prevented),
        CounterAdditionPreview::ChoiceRequired { .. } => {
            Some(SelfCounterCostPreview::ChoiceRequired)
        }
        CounterAdditionPreview::Transformed { .. } => Some(SelfCounterCostPreview::Transformed),
        CounterAdditionPreview::Unsupported => Some(SelfCounterCostPreview::Unsupported),
    }
}

/// Extract the narrow self-counter payment this preview understands. Parser
/// output may wrap one cost in `Composite`; multi-cost composites are outside
/// this exact replenishment check because the preview does not value their
/// additional payment components.
fn self_counter_removal_cost(cost: &AbilityCost) -> Option<(u32, &CounterType)> {
    match cost {
        AbilityCost::RemoveCounter {
            count,
            counter_type: CounterMatch::OfType(counter_type),
            target: None,
            ..
        } => Some((*count, counter_type)),
        AbilityCost::Composite { costs } if costs.len() == 1 => {
            self_counter_removal_cost(&costs[0])
        }
        _ => None,
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
///
/// **KNOWN MISPRICING on the `SelfRef` branch when the source is a land.**
/// `sacrifice_cost` charges `sacrifice_land_penalty`, whose stated rationale is
/// CR 305.2's one-land-per-turn rate limit on the replacement drop. CR 305.4
/// (`docs/MagicCompRules.txt:1700`) refutes that for a fetchland: "Effects may
/// also allow players to 'put' lands onto the battlefield. This isn't the same
/// as 'playing a land' and doesn't count as a land played during the current
/// turn." A fetchland puts its replacement onto the battlefield, so it consumes
/// no land drop and is close to manabase-neutral — yet this path prices it as a
/// full land lost, and the AI under-activates it.
///
/// Not corrected here, but the correction is **cheap and the parts already
/// exist** — this is a scoping decision, not a research problem. The discount
/// cannot be "a land sacrificing itself is cheap": a land that sacrifices itself
/// for a non-land effect really does lose a source. The discriminator is whether
/// the ability *replaces* the land, and `policies::fetch_land_patience` already
/// carries both halves of that predicate:
///
/// - `cost_sacrifices_self` — matches `AbilityCost::Sacrifice(sac)` with
///   `sac.target == TargetFilter::SelfRef`, recursing through `Composite`, which
///   is exactly the shape this function short-circuits on;
/// - `effects_are_tapped_land_fetch` — `Effect::SearchLibrary` with a
///   land-referencing filter plus a `ChangeZone` to the battlefield. Relaxing
///   its `EtbTapState::Tapped` requirement generalizes Evolving Wilds to true
///   fetchlands (Flooded Strand), which is the case this docstring is about.
///   `features::mana_ramp::{chain_searches_for_land, chain_puts_land_to_safe_zone,
///   target_filter_references_land}` are the same building blocks.
///
/// Deferred because it is an **unmeasured AI behaviour change** across this
/// function's call sites, `scripts/ai-gate.sh` is 2-player-only and cannot reach
/// the Commander regime the reports come from, and
/// `crates/engine/data/card-data.json` is not generated in this tree, so a
/// printed fetchland's parsed representation was never observed end to end.
/// Owned follow-up; see UNIT2-IMPL-R3 §5.
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

/// Outcome of pricing an in-scope self-cost ability's payoff chain.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum BenefitAppraisal {
    /// Every effect in the chain is trivial or unmodeled — no real payoff.
    Trivial,
    /// Every effect is explicitly classified, every non-trivial effect carries
    /// a confident card-equivalent price, and `value` is their sum.
    Priced { value: f64 },
    /// The chain has a real payoff but no sound comparison basis: either a
    /// classifier-non-trivial effect has no confident price (mana, land search,
    /// dynamic damage, removal, counters, self-protection under threat, ...) or
    /// an unmodeled rider sits beside a priced effect. Stand down.
    Unpriced,
}

/// Four-valued classification of a single chain effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EffectTriviality {
    /// An explicit classifier arm judged it: no meaningful immediate advantage.
    Trivial,
    /// An explicit classifier arm judged it: a real payoff.
    NonTrivial,
    /// An explicit classifier arm judged it: a priced cost incurred while
    /// receiving another effect's payoff.
    Drawback,
    /// No classifier arm models this effect at all (the old catch-all).
    Unmodeled,
}

/// AI's prediction of which player receives one effect in the chain.
///
/// This is intentionally private policy state: engine target matching remains
/// authoritative for actual resolution, while the policy only predicts its
/// own target chooser's preference before targets are bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecipientClass {
    AiOnly,
    OpponentOnly,
    Mixed,
}

/// Ordered state shared by the classifier while it walks one payoff chain.
/// Opponent draws can only make later mandatory parent-target discards trivial
/// up to the cards they supplied.
#[derive(Default)]
struct BenefitClassificationContext {
    remaining_opponent_draw_churn: u32,
}

/// Ordered state shared by pricing while it walks one classified payoff chain.
#[derive(Default)]
struct BenefitPricingContext {
    ai_draws_so_far: u32,
}

impl EffectTriviality {
    /// Map a classifier arm's existing `is_trivial` bool onto the typed form.
    /// Every explicit arm keeps its exact judgement; only the catch-all becomes
    /// [`EffectTriviality::Unmodeled`].
    fn from_is_trivial(is_trivial: bool) -> Self {
        if is_trivial {
            Self::Trivial
        } else {
            Self::NonTrivial
        }
    }
}

/// Appraise the whole payoff chain: is there a real payoff, and can it be
/// priced confidently enough to compare against the self-cost?
///
/// Complements the off-ability synergy check in [`synergy_justifies_self_cost`]
/// — this one measures the ability's *intrinsic* payoff.
pub(crate) fn appraise_benefit(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    ability: &AbilityDefinition,
    penalties: &PolicyPenalties,
) -> BenefitAppraisal {
    let effects = collect_chain_effects(ability);
    let classified = classify_effects(state, ai_player, source_id, ability, &effects);
    let mut total = 0.0;
    let mut any_nontrivial = false;
    let mut any_unmodeled = false;
    let mut pricing = BenefitPricingContext::default();
    for (effect, recipient, triviality) in classified {
        match triviality {
            EffectTriviality::Trivial => continue,
            EffectTriviality::Unmodeled => any_unmodeled = true,
            EffectTriviality::NonTrivial => {
                any_nontrivial = true;
                match effect_benefit_value(
                    state,
                    ai_player,
                    source_id,
                    effect,
                    recipient,
                    &mut pricing,
                    penalties,
                ) {
                    Some(value) => total += value,
                    None => return BenefitAppraisal::Unpriced,
                }
            }
            EffectTriviality::Drawback => {
                match effect_benefit_value(
                    state,
                    ai_player,
                    source_id,
                    effect,
                    recipient,
                    &mut pricing,
                    penalties,
                ) {
                    Some(value) => total += value,
                    None => return BenefitAppraisal::Unpriced,
                }
            }
        }
    }
    if !any_nontrivial {
        // All-trivial AND all-unmodeled chains land here — preserving the old
        // `benefit_is_trivial() == true` behaviour byte for byte (the catch-all
        // mapped to trivial), so the reject/marginal gate is not weakened.
        return BenefitAppraisal::Trivial;
    }
    if any_unmodeled {
        // An unmodeled rider beside a priced effect makes the sum neither a
        // lower nor an upper bound — the measured rider population carries both
        // signs (Token/Surveil benefits, Discard/LoseLife drawbacks), so no
        // directional conclusion is sound. Decided here, during the walk,
        // independent of what the net would have been.
        return BenefitAppraisal::Unpriced;
    }
    BenefitAppraisal::Priced { value: total }
}

/// Confident card-equivalent price of a single NON-TRIVIAL effect.
/// `None` = real but not confidently priceable — this module's documented
/// conservatism, typed as [`BenefitAppraisal::Unpriced`] by the caller.
fn effect_benefit_value(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    effect: &Effect,
    recipient: RecipientClass,
    pricing: &mut BenefitPricingContext,
    penalties: &PolicyPenalties,
) -> Option<f64> {
    match effect {
        // CR 121.1 + CR 121.2: one delivered card is one registry score unit,
        // but a multi-card instruction may deliver only a partial count. The
        // engine-owned preview runs that exact cloned instruction rather than
        // duplicating draw or replacement logic here.
        //
        // CR 614.6 + CR 614.11a: prevention and fully settled non-draw
        // substitutions are exact zeroes. A choice required to order or accept
        // a replacement—or raised by its continuation—is not a zero; the policy
        // returns `None` so `BenefitAppraisal::Unpriced` fails open.
        Effect::Draw { count, .. } if recipient == RecipientClass::AiOnly => {
            // `preview_draw_delivery` starts from its input state. A later
            // chained draw therefore cannot be priced independently without
            // replaying the earlier delivery; stand down rather than double-count.
            if pricing.ai_draws_so_far > 0 {
                return None;
            }
            let requested = resolve_quantity(state, count, ai_player, source_id).max(0) as u32;
            match preview_draw_delivery(state, ai_player, requested) {
                DrawDeliveryPreview::Exact { delivered } => {
                    pricing.ai_draws_so_far = pricing.ai_draws_so_far.saturating_add(delivered);
                    Some(f64::from(delivered) * SINGLE_CARD_VALUE)
                }
                DrawDeliveryPreview::Unknown => None,
            }
        }
        Effect::Discard {
            count,
            target: TargetFilter::ParentTarget,
            ..
        } if recipient == RecipientClass::AiOnly => {
            let requested = resolve_quantity(state, count, ai_player, source_id).max(0) as usize;
            let hand_size = state.players[ai_player.0 as usize].hand.len();
            // CR 609.3 + CR 701.9a: a mandatory discard does only as much as
            // possible, so cap the hand-to-graveyard discards at cards held
            // after earlier AI draws in this resolving chain.
            let discarded =
                requested.min(hand_size.saturating_add(pricing.ai_draws_so_far as usize));
            Some(-(discarded as f64) * penalties.self_cost_discard_per_card)
        }
        // Lifegain to the controller, life not a pressured resource: priced on
        // the same per-point axis as paying life. Under life pressure the value
        // is genuinely larger and hard to bound — stand down (`None`),
        // preserving the pre-comparison behaviour exactly.
        Effect::GainLife {
            amount,
            player: TargetFilter::Controller,
        } if !ai_life_critical(state, ai_player) => Some(
            resolve_quantity(state, amount, ai_player, source_id).max(0) as f64
                * penalties.self_cost_pay_life_per_point,
        ),
        _ => None,
    }
}

/// Classify a chain's effects in order, preserving the per-effect recipient
/// prediction and opponent-draw churn budget.
fn classify_effects<'a>(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    ability: &AbilityDefinition,
    effects: &[&'a Effect],
) -> Vec<(&'a Effect, RecipientClass, EffectTriviality)> {
    let recipients = recipient_classes(state, ai_player, source_id, ability, effects);
    let mut context = BenefitClassificationContext::default();
    effects
        .iter()
        .copied()
        .zip(recipients)
        .map(|(effect, recipient)| {
            let triviality = effect_triviality(
                state,
                ai_player,
                source_id,
                ability,
                effect,
                recipient,
                &mut context,
            );
            (effect, recipient, triviality)
        })
        .collect()
}

/// Classification view used by policy tests for ordered chain behavior.
#[cfg(test)]
pub(crate) fn chain_effect_trivialities(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    ability: &AbilityDefinition,
) -> Vec<EffectTriviality> {
    let effects = collect_chain_effects(ability);
    classify_effects(state, ai_player, source_id, ability, &effects)
        .into_iter()
        .map(|(_, _, triviality)| triviality)
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResidualVerdict {
    TrivialAtXZero,
    MeaningfulAtXZero,
    Unknown,
}

/// Classifies residual effects at X=0. The caller removes X-scaled effects
/// before calling, so an omitted X draw cannot contribute opponent-discard churn.
pub(crate) fn residual_effects_at_x_zero(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    ability: &AbilityDefinition,
    effects: &[&Effect],
) -> ResidualVerdict {
    let mut verdict = ResidualVerdict::TrivialAtXZero;
    for (_, _, triviality) in classify_effects(state, ai_player, source_id, ability, effects) {
        match triviality {
            EffectTriviality::Trivial => {}
            EffectTriviality::Unmodeled => return ResidualVerdict::Unknown,
            EffectTriviality::NonTrivial | EffectTriviality::Drawback => {
                verdict = ResidualVerdict::MeaningfulAtXZero;
            }
        }
    }
    verdict
}

fn recipient_classes(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    ability: &AbilityDefinition,
    effects: &[&Effect],
) -> Vec<RecipientClass> {
    let root_player_recipient = matches!(
        recipient_filter(&ability.effect),
        Some(TargetFilter::Player)
    )
    .then(|| predicted_root_player_recipient(state, ai_player, source_id, effects));

    effects
        .iter()
        .map(|effect| match recipient_filter(effect) {
            Some(TargetFilter::Controller) => RecipientClass::AiOnly,
            Some(TargetFilter::ParentTarget) => {
                root_player_recipient.unwrap_or(RecipientClass::Mixed)
            }
            Some(TargetFilter::Player) if std::ptr::eq(*effect, &*ability.effect) => {
                root_player_recipient.unwrap_or(RecipientClass::Mixed)
            }
            Some(filter) => recipient_class_for_filter(state, ai_player, source_id, filter),
            None => RecipientClass::Mixed,
        })
        .collect()
}

fn recipient_filter(effect: &Effect) -> Option<&TargetFilter> {
    match effect {
        Effect::Draw { target, .. } | Effect::Discard { target, .. } => Some(target),
        _ => extract_target_filter(effect),
    }
}

fn predicted_root_player_recipient(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    effects: &[&Effect],
) -> RecipientClass {
    let source_controller = state
        .objects
        .get(&source_id)
        .map(|object| object.controller);
    let aggregate = aggregate_player_impact_in(effects);
    let mut ai_candidate = false;
    let mut ai_accepted = false;
    let mut opponent_candidate = false;
    let mut opponent_accepted = false;

    for player in state.players.iter().filter(|player| !player.is_eliminated) {
        let impact = targeted_player_impact_in(
            state,
            source_controller,
            Some(source_id),
            effects,
            player.id,
        )
        .unwrap_or(aggregate);
        let prefers_self = if impact > PLAYER_IMPACT_PREFERENCE_BAND {
            true
        } else if impact < -PLAYER_IMPACT_PREFERENCE_BAND {
            false
        } else {
            return RecipientClass::Mixed;
        };
        let accepted = prefers_self == (player.id == ai_player);

        if player.id == ai_player {
            ai_candidate = true;
            ai_accepted = accepted;
        } else if players::is_opponent(state, ai_player, player.id) {
            opponent_candidate = true;
            opponent_accepted |= accepted;
        } else {
            return RecipientClass::Mixed;
        }
    }

    if !ai_candidate || !opponent_candidate {
        return RecipientClass::Mixed;
    }
    match (ai_accepted, opponent_accepted) {
        (true, false) => RecipientClass::AiOnly,
        (false, true) => RecipientClass::OpponentOnly,
        (true, true) | (false, false) => RecipientClass::Mixed,
    }
}

fn recipient_class_for_filter(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    filter: &TargetFilter,
) -> RecipientClass {
    let source_controller = state
        .objects
        .get(&source_id)
        .map(|object| object.controller);
    let ai_matches = engine::game::filter::player_matches_target_filter_in_state(
        state,
        filter,
        ai_player,
        source_controller,
        Some(source_id),
    );
    let mut opponent_matches = false;
    for player in state.players.iter().filter(|player| !player.is_eliminated) {
        if player.id == ai_player {
            continue;
        }
        if !engine::game::filter::player_matches_target_filter_in_state(
            state,
            filter,
            player.id,
            source_controller,
            Some(source_id),
        ) {
            continue;
        }
        if players::is_opponent(state, ai_player, player.id) {
            opponent_matches = true;
        } else {
            return RecipientClass::Mixed;
        }
    }
    match (ai_matches, opponent_matches) {
        (true, false) => RecipientClass::AiOnly,
        (false, true) => RecipientClass::OpponentOnly,
        (true, true) | (false, false) => RecipientClass::Mixed,
    }
}

/// Whether a single effect carries a meaningful immediate advantage, and
/// whether this module models it at all.
fn effect_triviality(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    ability: &AbilityDefinition,
    effect: &Effect,
    recipient: RecipientClass,
    context: &mut BenefitClassificationContext,
) -> EffectTriviality {
    match effect {
        Effect::Draw { count, .. } => match recipient {
            RecipientClass::OpponentOnly => {
                if count.is_up_to() {
                    return EffectTriviality::Trivial;
                }
                let count = resolve_quantity(state, count, ai_player, source_id).max(0) as u32;
                context.remaining_opponent_draw_churn =
                    context.remaining_opponent_draw_churn.saturating_add(count);
                EffectTriviality::Trivial
            }
            RecipientClass::AiOnly => EffectTriviality::from_is_trivial(
                resolve_quantity(state, count, ai_player, source_id) < 1,
            ),
            RecipientClass::Mixed => EffectTriviality::NonTrivial,
        },
        Effect::Discard {
            count: count_expr,
            target: TargetFilter::ParentTarget,
            ..
        } => match recipient {
            RecipientClass::AiOnly => {
                let count = resolve_quantity(state, count_expr, ai_player, source_id);
                if count_expr.is_up_to() || count <= 0 {
                    EffectTriviality::Trivial
                } else {
                    EffectTriviality::Drawback
                }
            }
            RecipientClass::OpponentOnly => {
                let count = resolve_quantity(state, count_expr, ai_player, source_id);
                if count_expr.is_up_to() || count < 0 {
                    return EffectTriviality::NonTrivial;
                }
                let count = count as u32;
                let trivial = count <= context.remaining_opponent_draw_churn;
                context.remaining_opponent_draw_churn =
                    context.remaining_opponent_draw_churn.saturating_sub(count);
                EffectTriviality::from_is_trivial(trivial)
            }
            RecipientClass::Mixed => EffectTriviality::NonTrivial,
        },
        // Damage is non-trivial if it is dynamic (power-derived, e.g. Fling),
        // lethal to a player, kills a real creature, or exceeds the face-ping
        // ceiling.
        Effect::DealDamage { amount, target, .. } => EffectTriviality::from_is_trivial(
            deal_damage_is_trivial(state, ai_player, source_id, amount, target),
        ),
        // Small lifegain is trivial unless the AI is under life pressure.
        Effect::GainLife { amount, .. } => EffectTriviality::from_is_trivial(
            resolve_quantity(state, amount, ai_player, source_id) <= TRIVIAL_LIFEGAIN_CEILING
                && !ai_life_critical(state, ai_player),
        ),
        // Removal is non-trivial when a worthwhile opponent creature can be hit.
        Effect::Destroy { target, .. } | Effect::Bounce { target, .. } => {
            EffectTriviality::from_is_trivial(removal_is_trivial(
                state, ai_player, source_id, target,
            ))
        }
        Effect::ChangeZone {
            destination: Zone::Exile | Zone::Graveyard,
            target,
            ..
        } => EffectTriviality::from_is_trivial(removal_is_trivial(
            state, ai_player, source_id, target,
        )),
        // A beneficial counter (e.g. an indestructible keyword counter) is
        // non-trivial by default; a harmful counter is removal. The one trivial
        // case is a self-counter that fizzles because paying the cost removes
        // its only recipient (Carrion Feeder into an empty board).
        Effect::PutCounter {
            counter_type,
            target,
            ..
        } => EffectTriviality::from_is_trivial(put_counter_is_trivial(
            state,
            ai_player,
            source_id,
            ability,
            counter_type,
            target,
        )),
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
        } => EffectTriviality::from_is_trivial(if counter_is_harmful(counter_type) {
            removal_is_trivial(state, ai_player, source_id, target)
        } else {
            false
        }),
        // Mana production is ramp — never trivial (Ashnod's/Phyrexian Altar).
        Effect::Mana { .. } => EffectTriviality::NonTrivial,
        // A library search for a land is ramp/fixing (sacrifice-a-land fetch
        // chains) — non-trivial.
        Effect::SearchLibrary { filter, .. } => EffectTriviality::from_is_trivial(
            !(ability_searches_library_for_land(ability) || target_filter_references_land(filter)),
        ),
        // A self-protection grant is only worth a cost when a threat is live.
        effect if is_self_protection_effect(effect) => EffectTriviality::from_is_trivial(
            match self_protection_effect_payoff(state, ai_player, source_id, effect) {
                Some(has_payoff) => !has_payoff,
                None => !any_immediate_threat(state, ai_player),
            },
        ),
        // No modeled board impact at all — the honest third answer.
        _ => EffectTriviality::Unmodeled,
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
        .map(|&opp| board_stats(state, opp).power)
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
/// in [`appraise_benefit`] — it covers value that lands elsewhere in a
/// lifegain/reanimator engine fed by the resource spent.
pub(crate) fn synergy_justifies_self_cost(
    features: &DeckFeatures,
    ability: &AbilityDefinition,
) -> bool {
    ability.cost.as_ref().is_some_and(|cost| {
        !contains_sacrifice_cost(cost) && synergy_justifies_cost(features, cost)
    })
}

/// A sacrifice leaf remains a real cost even when nested in a composite or a
/// choice of costs.  Treating a sibling lifegain/reanimator cost as a reason to
/// waive the whole tree would reintroduce the aristocrats exception indirectly.
fn contains_sacrifice_cost(cost: &AbilityCost) -> bool {
    cost.categories()
        .contains(&CostCategory::SacrificesPermanent)
}

fn synergy_justifies_cost(features: &DeckFeatures, cost: &AbilityCost) -> bool {
    match cost {
        // Sacrificing the source is a concrete cost, not a generic aristocrats
        // discount.  Death triggers remain part of an ability's actual payoff
        // appraisal; they must not make an otherwise trivial activation free.
        AbilityCost::Sacrifice(_) => false,
        AbilityCost::PayLife { .. } => features.lifegain.commitment >= SYNERGY_COMMITMENT_FLOOR,
        AbilityCost::Discard { .. } => features.reanimator.commitment >= SYNERGY_COMMITMENT_FLOOR,
        // Exile from the AI's own hand/graveyard: no synergy stand-down. Graveyard
        // exile is a strict loss for a reanimator deck (it removes fuel), and no
        // real card pairs a trivial payoff with a hand-exile self-cost.
        AbilityCost::Exile { .. } => false,
        AbilityCost::Composite { costs } | AbilityCost::OneOf { costs } => costs
            .iter()
            .any(|cost| synergy_justifies_cost(features, cost)),
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
