use engine::game::game_object::GameObject;
use engine::types::ability::{
    ContinuousModification, Effect, PtValue, QuantityExpr, TargetFilter, TypeFilter,
};
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

use super::context::PolicyContext;

/// Three-valued polarity: whether an effect benefits or harms its target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EffectPolarity {
    /// Target benefits (pump, regenerate, +1/+1 counters, untap, animate)
    Beneficial,
    /// Target is harmed (destroy, damage, -1/-1 counters, sacrifice)
    Harmful,
    /// Depends on context — fall through to default "assume harmful" behavior
    Contextual,
}

/// Flip a polarity. `Contextual` stays put — there's no opposite of "unknown."
fn invert(polarity: EffectPolarity) -> EffectPolarity {
    match polarity {
        EffectPolarity::Beneficial => EffectPolarity::Harmful,
        EffectPolarity::Harmful => EffectPolarity::Beneficial,
        EffectPolarity::Contextual => EffectPolarity::Contextual,
    }
}

/// CR 122.1: Counters sign — `+1/+1` is beneficial to the bearer, `-1/-1`
/// harmful. Non-P/T counter types (poison, loyalty, charge, etc.) are classified
/// as Contextual because their value to the bearer depends on card semantics.
fn counter_sign_polarity(counter_type: &str) -> EffectPolarity {
    if counter_type.starts_with('+') {
        EffectPolarity::Beneficial
    } else if counter_type.starts_with('-') {
        EffectPolarity::Harmful
    } else {
        EffectPolarity::Contextual
    }
}

pub(crate) fn effect_polarity(effect: &Effect) -> EffectPolarity {
    match effect {
        // Pump: beneficial only if both values are non-negative
        Effect::Pump {
            power, toughness, ..
        } => {
            let p_ok = matches!(power, PtValue::Fixed(v) if *v >= 0)
                || matches!(power, PtValue::Variable(_) | PtValue::Quantity(_));
            let t_ok = matches!(toughness, PtValue::Fixed(v) if *v >= 0)
                || matches!(toughness, PtValue::Variable(_) | PtValue::Quantity(_));
            if p_ok && t_ok {
                EffectPolarity::Beneficial
            } else {
                EffectPolarity::Harmful
            }
        }
        // CR 122.1: Counter placement. Sign drives polarity: +1/+1 is beneficial,
        // -1/-1 is harmful. AddCounter, PutCounter, and PutCounterAll share the
        // same semantics — the effect puts counters of `counter_type` onto a target.
        // MultiplyCounter (e.g., Doubling Season) amplifies existing counters: its
        // polarity is context-dependent (doubling -1/-1 on a creature is harmful).
        Effect::AddCounter { counter_type, .. }
        | Effect::PutCounter { counter_type, .. }
        | Effect::PutCounterAll { counter_type, .. } => counter_sign_polarity(counter_type),
        // CR 122.1 + CR 121: Removing counters inverts the placement polarity —
        // removing a +1/+1 counter harms the bearer, removing a -1/-1 counter
        // helps it (Hexcaster's Mark, Solemnity-style interactions, Vampire
        // Hexmage). Same building-block class as PutCounter, opposite sign.
        Effect::RemoveCounter { counter_type, .. } => invert(counter_sign_polarity(counter_type)),
        Effect::MultiplyCounter {
            counter_type,
            multiplier,
            ..
        } => {
            // Doubling +1/+1 is beneficial; halving (-1) or erasing (0) inverts.
            let base = counter_sign_polarity(counter_type);
            if *multiplier > 1 {
                base
            } else if *multiplier < 1 {
                // Both negative multipliers and zero erase/invert counters —
                // erasing +1/+1 is harmful, erasing -1/-1 is beneficial.
                invert(base)
            } else {
                // multiplier == 1 is a no-op
                EffectPolarity::Contextual
            }
        }
        // CR 122.5: Moving counters is target-relative (moving -1/-1 off your
        // own creature onto an opponent's is beneficial), so leave as
        // Contextual — the call site must inspect source/target controllers.
        Effect::MoveCounters { .. } => EffectPolarity::Contextual,
        // CR 701.34: Proliferate adds a counter of each existing kind on
        // chosen permanents/players. Polarity depends on the counter mix on
        // the chosen targets at resolution time — classify as Contextual;
        // target-selection must inspect the target's counter sign.
        Effect::Proliferate => EffectPolarity::Contextual,
        // CR 701.10a: Doubling base P/T on a filter set is beneficial to that set.
        Effect::DoublePTAll { .. } => EffectPolarity::Beneficial,
        Effect::Regenerate { .. }
        | Effect::PreventDamage { .. }
        | Effect::Animate { .. }
        | Effect::DoublePT { .. } => EffectPolarity::Beneficial,
        Effect::Untap { .. } => EffectPolarity::Beneficial,
        // Beneficial: resource generation and card advantage
        Effect::GainLife { .. }
        | Effect::Draw { .. }
        | Effect::Token { .. }
        | Effect::Scry { .. }
        | Effect::Explore
        | Effect::Investigate
        | Effect::Mana { .. }
        | Effect::SearchLibrary { .. }
        | Effect::Surveil { .. }
        | Effect::Connive { .. }
        | Effect::BecomeMonarch
        | Effect::ExtraTurn { .. } => EffectPolarity::Beneficial,
        // Harmful: removal, disruption, and forced actions
        Effect::Destroy { .. }
        | Effect::DealDamage { .. }
        | Effect::Sacrifice { .. }
        | Effect::DiscardCard { .. }
        | Effect::Mill { .. }
        | Effect::LoseLife { .. }
        | Effect::Tap { .. }
        | Effect::Bounce { .. }
        | Effect::Counter { .. }
        | Effect::PhaseOut { .. }
        | Effect::Fight { .. }
        | Effect::Goad { .. }
        | Effect::ForceBlock { .. }
        | Effect::DestroyAll { .. }
        | Effect::DamageAll { .. }
        | Effect::BounceAll { .. }
        | Effect::LoseTheGame => EffectPolarity::Harmful,
        // ChangeZone: depends on destination
        Effect::ChangeZone { destination, .. } => match destination {
            Zone::Exile | Zone::Graveyard => EffectPolarity::Harmful,
            Zone::Battlefield => EffectPolarity::Beneficial,
            _ => EffectPolarity::Contextual,
        },
        // GenericEffect: inspect the static abilities it grants to determine polarity.
        // e.g. CantBeBlocked → Beneficial, CantAttack → Harmful.
        Effect::GenericEffect {
            static_abilities, ..
        } => {
            for sd in static_abilities {
                match static_mode_polarity(&sd.mode) {
                    EffectPolarity::Contextual => {
                        // Check modifications within this static definition
                        for m in &sd.modifications {
                            match modification_polarity(m) {
                                EffectPolarity::Contextual => continue,
                                polarity => return polarity,
                            }
                        }
                    }
                    polarity => return polarity,
                }
            }
            EffectPolarity::Contextual
        }
        // Contextual: depends on usage context
        Effect::GainControl { .. }
        | Effect::GiftDelivery { .. }
        | Effect::Suspect { .. }
        | Effect::GivePlayerCounter { .. }
        | Effect::ExchangeControl { .. } => EffectPolarity::Contextual,
        _ => EffectPolarity::Contextual,
    }
}

/// Extract the target filter from an effect, if present.
pub(crate) fn extract_target_filter(effect: &Effect) -> Option<&TargetFilter> {
    match effect {
        // Beneficial effects
        Effect::Pump { target, .. }
        | Effect::AddCounter { target, .. }
        | Effect::PutCounter { target, .. }
        | Effect::PutCounterAll { target, .. }
        | Effect::MultiplyCounter { target, .. }
        | Effect::Animate { target, .. }
        | Effect::DoublePT { target, .. }
        | Effect::DoublePTAll { target, .. }
        | Effect::Regenerate { target, .. }
        | Effect::Untap { target }
        | Effect::PreventDamage { target, .. }
        // Harmful effects
        | Effect::Destroy { target, .. }
        | Effect::DealDamage { target, .. }
        | Effect::Tap { target }
        | Effect::RemoveCounter { target, .. }
        // Removal / disruption
        | Effect::Bounce { target, .. }
        | Effect::Counter { target, .. }
        | Effect::GainControl { target, .. }
        | Effect::PhaseOut { target }
        | Effect::Fight { target, .. }
        | Effect::Goad { target }
        | Effect::ChangeZone { target, .. }
        | Effect::Connive { target, .. }
        | Effect::Suspect { target, .. }
        | Effect::ForceBlock { target, .. }
        | Effect::Exploit { target, .. }
        | Effect::Attach { target, .. }
        | Effect::GivePlayerCounter { target, .. }
        | Effect::BecomeCopy { target, .. }
        | Effect::ExtraTurn { target, .. }
        | Effect::MoveCounters { target, .. } => Some(target),
        // GenericEffect and LoseLife have Option<TargetFilter>
        Effect::GenericEffect { target, .. } | Effect::LoseLife { target, .. } => {
            target.as_ref()
        }
        // NOTE: ExchangeControl carries two distinct target filters (target_a/target_b).
        // Its slot collection is special-cased; no single filter is meaningful here.
        // NOTE: GiftDelivery { kind } has no target field.
        // NOTE: SearchLibrary uses `filter`, not `target`.
        _ => None,
    }
}

/// Returns true if the effect exclusively targets creatures (not "any target").
/// Used for harmful spells: burn with TargetFilter::Any can still go face.
pub(crate) fn targets_creatures_only(effect: &Effect) -> bool {
    let filter = extract_target_filter(effect);
    matches!(
        filter,
        Some(TargetFilter::Typed(typed))
            if typed.type_filters.iter().any(|t| matches!(t, TypeFilter::Creature))
    )
}

/// Returns true if an effect's target filter is creature-typed (or Any).
pub(crate) fn targets_creatures(effect: &Effect) -> bool {
    let Some(filter) = extract_target_filter(effect) else {
        return false;
    };
    match filter {
        TargetFilter::Any => true,
        TargetFilter::Typed(typed) => typed
            .type_filters
            .iter()
            .any(|t| matches!(t, TypeFilter::Creature)),
        _ => false,
    }
}

/// Returns true if the pending spell's dominant effect is beneficial to its target.
/// Defaults to false (assume harmful) when uncertain — safe fallback since most
/// targeted spells in MTG are removal/damage.
pub(crate) fn is_spell_beneficial(ctx: &PolicyContext<'_>) -> bool {
    let player_impact = aggregate_player_impact(ctx);
    if player_impact > 0.25 {
        return true;
    }
    if player_impact < -0.25 {
        return false;
    }

    let effects = ctx.effects();

    // Check active effects for a clear polarity signal.
    let dominant_polarity = effects.first().map(|e| effect_polarity(e));
    match dominant_polarity {
        Some(EffectPolarity::Beneficial) => return true,
        Some(EffectPolarity::Harmful) => return false,
        _ => {}
    }

    // TargetOnly marks a target without direct effect — check sub-effects for polarity.
    // If a subsequent harmful mass effect (ChangeZoneAll, DestroyAll, DamageAll) excludes
    // the parent target via Not(ParentTarget), the target is being SAVED from the mass effect.
    if matches!(effects.first(), Some(Effect::TargetOnly { .. })) {
        for effect in effects.iter().skip(1) {
            if is_harmful_all_excluding_target(effect) {
                return true; // Target is the survivor — beneficial
            }
        }
    }

    // No clear polarity from active effects (empty or Contextual).
    // Auras carry their beneficial/harmful nature in static definitions.
    if let Some(source) = ctx.source_object() {
        if source.card_types.subtypes.iter().any(|s| s == "Aura") {
            return matches!(aura_polarity(source), EffectPolarity::Beneficial);
        }
    }

    false
}

pub(crate) fn aggregate_player_impact(ctx: &PolicyContext<'_>) -> f64 {
    ctx.effects()
        .iter()
        .map(|effect| player_impact(effect))
        .sum()
}

pub(crate) fn targeted_player_impact(ctx: &PolicyContext<'_>, player: PlayerId) -> Option<f64> {
    let source_controller = ctx.source_object().map(|object| object.controller);
    let mut found_targeted_effect = false;
    let mut impact = 0.0;

    for effect in ctx.effects() {
        let Some(filter) = extract_target_filter(effect) else {
            continue;
        };
        if engine::game::filter::player_matches_target_filter(filter, player, source_controller) {
            found_targeted_effect = true;
            impact += player_impact(effect);
        }
    }

    found_targeted_effect.then_some(impact)
}

fn player_impact(effect: &Effect) -> f64 {
    match effect {
        Effect::Draw { count, .. } => quantity_weight(count, 1.25),
        Effect::Discard { count, .. } => -quantity_weight(count, 1.5),
        Effect::DiscardCard { count, .. } => -(*count as f64 * 1.5),
        Effect::GainLife { amount, .. } => quantity_weight(amount, 0.15),
        Effect::LoseLife { amount, .. } => -quantity_weight(amount, 0.15),
        _ => match effect_polarity(effect) {
            EffectPolarity::Beneficial => 1.0,
            EffectPolarity::Harmful => -1.0,
            EffectPolarity::Contextual => 0.0,
        },
    }
}

fn quantity_weight(quantity: &QuantityExpr, factor: f64) -> f64 {
    factor
        * match quantity {
            QuantityExpr::Fixed { value } => (*value).max(0) as f64,
            _ => 1.0,
        }
}

/// Determines whether an Aura is beneficial or harmful to its target by inspecting
/// both static modes (CantAttack, CantBeBlocked, etc.) and continuous modifications.
pub(crate) fn aura_polarity(source: &GameObject) -> EffectPolarity {
    // First check static modes — these carry clear polarity independent of modifications.
    for sd in source.static_definitions.iter_unchecked() {
        match static_mode_polarity(&sd.mode) {
            EffectPolarity::Contextual => continue,
            polarity => return polarity,
        }
    }

    // Then check continuous modifications (AddPower, AddKeyword, etc.).
    for sd in source.static_definitions.iter_unchecked() {
        for m in &sd.modifications {
            match modification_polarity(m) {
                EffectPolarity::Contextual => continue,
                polarity => return polarity,
            }
        }
    }

    EffectPolarity::Contextual
}

/// Classify a static mode as beneficial/harmful to the enchanted permanent.
pub(crate) fn static_mode_polarity(mode: &StaticMode) -> EffectPolarity {
    match mode {
        // Harmful: restricts the enchanted permanent
        StaticMode::CantAttack
        | StaticMode::CantBlock
        | StaticMode::CantUntap
        | StaticMode::MustAttack
        | StaticMode::MustBlock
        | StaticMode::CantGainLife
        | StaticMode::CantBeActivated { .. } => EffectPolarity::Harmful,
        // Beneficial: enhances the enchanted permanent
        StaticMode::CantBeBlocked
        | StaticMode::CantBeBlockedExceptBy { .. }
        | StaticMode::CantBeTargeted
        | StaticMode::CantBeCountered
        | StaticMode::CantBeCopied
        | StaticMode::Protection
        | StaticMode::CastWithFlash => EffectPolarity::Beneficial,
        // Continuous, cost changes, and others depend on modifications/context
        _ => EffectPolarity::Contextual,
    }
}

/// Classify a continuous modification as beneficial/harmful to its target.
pub(crate) fn modification_polarity(m: &ContinuousModification) -> EffectPolarity {
    match m {
        ContinuousModification::AddPower { value }
        | ContinuousModification::AddToughness { value } => {
            if *value > 0 {
                EffectPolarity::Beneficial
            } else if *value < 0 {
                EffectPolarity::Harmful
            } else {
                EffectPolarity::Contextual
            }
        }
        ContinuousModification::AddDynamicPower { .. }
        | ContinuousModification::AddDynamicToughness { .. } => EffectPolarity::Beneficial,
        ContinuousModification::AddKeyword { .. }
        | ContinuousModification::GrantAbility { .. }
        | ContinuousModification::AddAllCreatureTypes
        | ContinuousModification::AddColor { .. }
        | ContinuousModification::AddType { .. }
        | ContinuousModification::AddSubtype { .. } => EffectPolarity::Beneficial,
        ContinuousModification::RemoveKeyword { .. }
        | ContinuousModification::RemoveAllAbilities
        | ContinuousModification::RemoveType { .. }
        | ContinuousModification::RemoveSubtype { .. } => EffectPolarity::Harmful,
        // SetPower/SetToughness, SetColor, etc. are contextual — could go either way.
        _ => EffectPolarity::Contextual,
    }
}

/// Returns true if the effect is a harmful mass effect (ChangeZoneAll, DestroyAll, DamageAll)
/// whose filter excludes the parent ability's target via `Not(ParentTarget)`.
/// This pattern means the targeted creature is the survivor, not the victim.
fn is_harmful_all_excluding_target(effect: &Effect) -> bool {
    let filter = match effect {
        Effect::ChangeZoneAll {
            destination: Zone::Exile | Zone::Graveyard,
            target,
            ..
        } => Some(target),
        Effect::DestroyAll { target, .. }
        | Effect::DamageAll { target, .. }
        | Effect::BounceAll { target, .. } => Some(target),
        _ => return false,
    };
    filter.is_some_and(filter_excludes_parent_target)
}

/// Recursively checks if a target filter contains `Not(ParentTarget)`.
fn filter_excludes_parent_target(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Not { filter: inner } => matches!(inner.as_ref(), TargetFilter::ParentTarget),
        TargetFilter::And { filters } => filters.iter().any(filter_excludes_parent_target),
        _ => false,
    }
}
