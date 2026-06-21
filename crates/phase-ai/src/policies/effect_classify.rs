use engine::game::filter::{matches_target_filter, FilterContext};
use engine::game::game_object::GameObject;
use engine::types::ability::{
    ContinuousModification, Effect, EffectScope, PtValue, QuantityExpr, TapStateChange,
    TargetFilter, TypeFilter,
};
use engine::types::counter::CounterType;
use engine::types::game_state::{CastingVariant, GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::triggers::TriggerMode;
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
fn counter_sign_polarity(counter_type: &CounterType) -> EffectPolarity {
    match counter_type {
        CounterType::Plus1Plus1 => EffectPolarity::Beneficial,
        CounterType::Minus1Minus1 => EffectPolarity::Harmful,
        CounterType::Generic(s) if s.starts_with('+') => EffectPolarity::Beneficial,
        CounterType::Generic(s) if s.starts_with('-') => EffectPolarity::Harmful,
        _ => EffectPolarity::Contextual,
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
        Effect::PutCounter { counter_type, .. } | Effect::PutCounterAll { counter_type, .. } => {
            counter_sign_polarity(counter_type)
        }
        // CR 122.1 + CR 121: Removing counters inverts the placement polarity —
        // removing a +1/+1 counter harms the bearer, removing a -1/-1 counter
        // helps it (Hexcaster's Mark, Solemnity-style interactions, Vampire
        // Hexmage). Same building-block class as PutCounter, opposite sign.
        Effect::RemoveCounter { counter_type, .. } => counter_type
            .as_ref()
            .map(counter_sign_polarity)
            .map(invert)
            .unwrap_or(EffectPolarity::Contextual),
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
        Effect::SkipNextTurn { .. } | Effect::SkipNextStep { .. } => EffectPolarity::Contextual,
        Effect::Regenerate { .. }
        | Effect::RemoveAllDamage { .. }
        | Effect::PreventDamage { .. }
        | Effect::Animate { .. }
        | Effect::DoublePT { .. } => EffectPolarity::Beneficial,
        // CR 701.26b: untapping a single permanent is beneficial. The mass
        // (`All`) scope is left Contextual via the catch-all, matching the
        // legacy `UntapAll`.
        Effect::SetTapState {
            scope: EffectScope::Single,
            state: TapStateChange::Untap,
            ..
        } => EffectPolarity::Beneficial,
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
        // CR 701.26a: tapping a single permanent is harmful (denies its use).
        // The mass (`All`) scope is left Contextual via the catch-all, matching
        // the legacy `TapAll`.
        Effect::SetTapState {
            scope: EffectScope::Single,
            state: TapStateChange::Tap,
            ..
        } => EffectPolarity::Harmful,
        // Harmful: removal, disruption, and forced actions
        Effect::Destroy { .. }
        | Effect::DealDamage { .. }
        | Effect::Sacrifice { .. }
        | Effect::DiscardCard { .. }
        | Effect::Mill { .. }
        | Effect::LoseLife { .. }
        | Effect::Bounce { .. }
        | Effect::Counter { .. }
        | Effect::PhaseOut { .. }
        | Effect::Fight { .. }
        | Effect::Goad { .. }
        | Effect::ForceBlock { .. }
        | Effect::DestroyAll { .. }
        | Effect::DamageAll { .. }
        | Effect::BounceAll { .. }
        | Effect::LoseTheGame { .. } => EffectPolarity::Harmful,
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
        | Effect::ExchangeControl { .. }
        | Effect::ExchangeLifeTotals { .. } => EffectPolarity::Contextual,
        _ => EffectPolarity::Contextual,
    }
}

/// Extract the target filter from an effect, if present.
pub(crate) fn extract_target_filter(effect: &Effect) -> Option<&TargetFilter> {
    match effect {
        // Beneficial effects
        Effect::Pump { target, .. }
        | Effect::PutCounter { target, .. }
        | Effect::PutCounterAll { target, .. }
        | Effect::MultiplyCounter { target, .. }
        | Effect::Animate { target, .. }
        | Effect::DoublePT { target, .. }
        | Effect::DoublePTAll { target, .. }
        | Effect::Regenerate { target, .. }
        | Effect::RemoveAllDamage { target, .. }
        | Effect::PreventDamage { target, .. }
        // Harmful effects
        | Effect::Destroy { target, .. }
        | Effect::DealDamage { target, .. }
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
        | Effect::ForceBlock { target, .. }
        | Effect::Exploit { target, .. }
        | Effect::Attach { target, .. }
        | Effect::GivePlayerCounter { target, .. }
        | Effect::BecomeCopy { target, .. }
        | Effect::ExtraTurn { target, .. }
        | Effect::SkipNextStep { target, .. }
        | Effect::MoveCounters { target, .. } => Some(target),
        // CR 701.26a/b: only single-permanent tap/untap exposes a selectable
        // target. The mass (`All`) scope's filter is a population filter and is
        // not surfaced as a target (matching the legacy `TapAll`/`UntapAll`,
        // which fell through to `None`).
        Effect::SetTapState {
            scope: EffectScope::Single,
            target,
            ..
        } => Some(target),
        // CR 701.60a: only single-permanent suspect/unsuspect exposes a
        // selectable target. The mass (`All`) scope (e.g. Absolving Lammasu)
        // is a non-targeting population effect — its filter is not surfaced as
        // a target (mirrors `SetTapState`'s `Single`/`All` split).
        Effect::Suspect {
            scope: EffectScope::Single,
            target,
            ..
        }
        | Effect::Unsuspect {
            scope: EffectScope::Single,
            target,
            ..
        } => Some(target),
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

/// Whether `effects`, all applied to the creature `object_id`, will kill it.
///
/// Models the three toughness-reducing removal modalities the AI must reason
/// about when deciding whether a removal spell is worth casting:
/// - direct damage (`Effect::DealDamage`),
/// - negative `Pump` (covers `-X/-X` and `-0/-X`),
/// - `-1/-1` (and other negative P/T) counters (`Effect::PutCounter`).
///
/// All such effects in the spell are assumed to hit this creature, which is the
/// single-target removal case the callers care about.
///
/// Returns:
/// - `Some(true)`  — provably lethal,
/// - `Some(false)` — provably non-lethal (every relevant amount is fixed and
///   the creature survives),
/// - `None`        — not determinable: either no damage/shrink effect is present
///   (e.g. `Destroy`, `Bounce`, `Exile`), or a relevant amount is variable (an
///   `X` spell whose value the caster chooses). Callers fail open on `None`.
pub(crate) fn lethal_to_creature(
    state: &GameState,
    object_id: ObjectId,
    effects: &[&Effect],
) -> Option<bool> {
    let object = state.objects.get(&object_id)?;
    let base_toughness = object.toughness?;

    let mut toughness_reduction = 0i32; // from negative pumps and -1/-1 counters
    let mut total_damage = 0i32;
    let mut saw_relevant = false;

    for effect in effects {
        match effect {
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value },
                ..
            } => {
                total_damage += *value;
                saw_relevant = true;
            }
            // Variable (X) damage — the caster picks the amount, so lethality
            // can't be decided here.
            Effect::DealDamage { .. } => return None,
            Effect::Pump { toughness, .. } => match toughness {
                PtValue::Fixed(v) if *v < 0 => {
                    toughness_reduction += -*v;
                    saw_relevant = true;
                }
                PtValue::Fixed(_) => {}
                // -X/-X: variable shrink, undecidable here.
                PtValue::Variable(_) | PtValue::Quantity(_) => return None,
            },
            Effect::PutCounter {
                counter_type,
                count,
                ..
            } => {
                if let Some((_, t_delta)) = counter_type.power_toughness_delta() {
                    if t_delta < 0 {
                        match count {
                            QuantityExpr::Fixed { value } => {
                                toughness_reduction += -t_delta * *value;
                                saw_relevant = true;
                            }
                            _ => return None,
                        }
                    }
                }
            }
            _ => {}
        }
    }

    if !saw_relevant {
        return None;
    }

    let new_toughness = base_toughness - toughness_reduction;
    // CR 704.5f: a creature with toughness 0 or less is put into its owner's
    // graveyard. This is not destruction, so indestructible does not prevent it.
    if new_toughness <= 0 {
        return Some(true);
    }
    // CR 704.5g + CR 702.12b: lethal marked damage destroys the creature — but
    // an indestructible permanent ignores that state-based action, so damage
    // alone can never kill it.
    if total_damage > 0 && !object.has_keyword(&Keyword::Indestructible) {
        let marked = object.damage_marked as i32;
        if marked + total_damage >= new_toughness {
            return Some(true);
        }
    }
    Some(false)
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
    // CR 702.140a: Mutate targets a non-Human creature with the same owner as
    // the spell — treat it as beneficial so targeting prefers our creatures over
    // opponents' creatures when evaluating candidate targets.
    if let WaitingFor::TargetSelection { pending_cast, .. } = &ctx.decision.waiting_for {
        if pending_cast.casting_variant == CastingVariant::Mutate {
            return true;
        }
    }

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
        if engine::game::filter::player_matches_target_filter_in_state(
            ctx.state,
            filter,
            player,
            source_controller,
        ) {
            found_targeted_effect = true;
            impact += player_impact(effect);
        }
    }

    found_targeted_effect.then_some(impact)
}

pub(crate) fn targeted_object_impact(ctx: &PolicyContext<'_>, object_id: ObjectId) -> Option<f64> {
    let mut found_targeted_effect = false;
    let mut impact = 0.0;

    for effect in ctx.effects() {
        if effect_targets_object(ctx, effect, object_id) {
            found_targeted_effect = true;
            impact += player_impact(effect);
        }
    }

    found_targeted_effect.then_some(impact)
}

pub(crate) fn effect_targets_object(
    ctx: &PolicyContext<'_>,
    effect: &Effect,
    object_id: ObjectId,
) -> bool {
    let source_id = effect_source_id(ctx);
    let filter_ctx = FilterContext::from_source(ctx.state, source_id);
    extract_target_filter(effect)
        .is_some_and(|filter| object_matches_effect_filter(ctx, object_id, filter, &filter_ctx))
}

fn effect_source_id(ctx: &PolicyContext<'_>) -> ObjectId {
    match &ctx.decision.waiting_for {
        engine::types::game_state::WaitingFor::TargetSelection { pending_cast, .. } => {
            pending_cast.object_id
        }
        engine::types::game_state::WaitingFor::MultiTargetSelection {
            pending_ability, ..
        } => pending_ability.source_id,
        engine::types::game_state::WaitingFor::TriggerTargetSelection { source_id, .. } => {
            source_id.unwrap_or(ObjectId(0))
        }
        _ => ctx
            .source_object()
            .map(|object| object.id)
            .unwrap_or(ObjectId(0)),
    }
}

fn object_matches_effect_filter(
    ctx: &PolicyContext<'_>,
    object_id: ObjectId,
    filter: &TargetFilter,
    filter_ctx: &FilterContext,
) -> bool {
    match filter {
        TargetFilter::StackSpell => ctx.state.stack.iter().any(|entry| entry.id == object_id),
        TargetFilter::StackAbility { .. } => false,
        TargetFilter::And { filters } => filters
            .iter()
            .all(|filter| object_matches_effect_filter(ctx, object_id, filter, filter_ctx)),
        TargetFilter::Or { filters } => filters
            .iter()
            .any(|filter| object_matches_effect_filter(ctx, object_id, filter, filter_ctx)),
        TargetFilter::Not { filter } => {
            !object_matches_effect_filter(ctx, object_id, filter, filter_ctx)
        }
        _ => matches_target_filter(ctx.state, object_id, filter, filter_ctx),
    }
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

    // CR 109.5 + CR 605.1b: Some Auras carry their benefit on a triggered
    // ability that routes the effect to the enchanted permanent's controller
    // ("its controller adds an additional one mana of any color" — Fertile
    // Ground, Wild Growth, Utopia Sprawl, Verdant Haven, Trace of Abundance,
    // Market Festival, Weirding Wood, Overgrowth). Without inspecting
    // triggers, these auras appear `Contextual` and the AI cannot tell that
    // gifting one to an opponent is a strict negative for itself. A
    // `TapsForMana` trigger that adds mana is unambiguously beneficial to
    // the host's controller.
    for trigger in source.trigger_definitions.iter_unchecked() {
        match trigger_mode_polarity_for_host(trigger) {
            EffectPolarity::Contextual => continue,
            polarity => return polarity,
        }
    }

    EffectPolarity::Contextual
}

/// Classify a trigger on an Aura as beneficial/harmful to the *enchanted
/// permanent's controller* (the host's controller — not the aura's controller).
/// Used by `aura_polarity` to flag auras whose value accrues to the host owner
/// (e.g. mana-doubling auras: Fertile Ground class) so the AI prefers attaching
/// them to its own permanents and avoids gifting them to opponents.
fn trigger_mode_polarity_for_host(
    trigger: &engine::types::ability::TriggerDefinition,
) -> EffectPolarity {
    let Some(execute) = trigger.execute.as_deref() else {
        return EffectPolarity::Contextual;
    };
    match trigger.mode {
        // "Whenever enchanted land is tapped for mana, its controller adds …"
        // — bonus mana goes to the host's controller.
        TriggerMode::TapsForMana if matches!(&*execute.effect, Effect::Mana { .. }) => {
            EffectPolarity::Beneficial
        }
        _ => EffectPolarity::Contextual,
    }
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
        | StaticMode::CantBeActivated { .. }
        | StaticMode::CantActivateDuring { .. } => EffectPolarity::Harmful,
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

#[cfg(test)]
mod lethality_tests {
    use super::*;
    use engine::game::scenario::{GameScenario, P1};
    use engine::types::keywords::Keyword;

    fn deal_damage(value: i32) -> Effect {
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value },
            target: TargetFilter::Any,
            damage_source: None,
        }
    }

    fn shrink(power: i32, toughness: i32) -> Effect {
        Effect::Pump {
            power: PtValue::Fixed(power),
            toughness: PtValue::Fixed(toughness),
            target: TargetFilter::Any,
        }
    }

    fn minus_counters(count: i32) -> Effect {
        Effect::PutCounter {
            counter_type: CounterType::Minus1Minus1,
            count: QuantityExpr::Fixed { value: count },
            target: TargetFilter::Any,
        }
    }

    #[test]
    fn damage_lethal_only_at_or_above_remaining_toughness() {
        let mut scenario = GameScenario::new();
        let bear = scenario.add_creature(P1, "Bear", 2, 2).id();
        let runner = scenario.build();
        let state = runner.state();
        assert_eq!(
            lethal_to_creature(state, bear, &[&deal_damage(2)]),
            Some(true)
        );
        assert_eq!(
            lethal_to_creature(state, bear, &[&deal_damage(1)]),
            Some(false)
        );
    }

    #[test]
    fn abrade_on_zero_four_reads_non_lethal() {
        // Regression: 3 damage on a 0/4 must not look lethal (the reported bug).
        let mut scenario = GameScenario::new();
        let wall = scenario.add_creature(P1, "Wall", 0, 4).id();
        let runner = scenario.build();
        assert_eq!(
            lethal_to_creature(runner.state(), wall, &[&deal_damage(3)]),
            Some(false)
        );
    }

    #[test]
    fn damage_accounts_for_marked_damage() {
        // CR 704.5g: total marked damage (prior + this) vs toughness.
        let mut scenario = GameScenario::new();
        let wall = scenario.add_creature(P1, "Wall", 0, 4).id();
        let mut runner = scenario.build();
        runner
            .state_mut()
            .objects
            .get_mut(&wall)
            .unwrap()
            .damage_marked = 2;
        let state = runner.state();
        assert_eq!(
            lethal_to_creature(state, wall, &[&deal_damage(2)]),
            Some(true)
        );
        assert_eq!(
            lethal_to_creature(state, wall, &[&deal_damage(1)]),
            Some(false)
        );
    }

    #[test]
    fn negative_pump_kills_via_zero_toughness() {
        // CR 704.5f: -0/-4 brings a 0/4 to 0 toughness -> dies; -0/-3 survives.
        let mut scenario = GameScenario::new();
        let wall = scenario.add_creature(P1, "Wall", 0, 4).id();
        let runner = scenario.build();
        let state = runner.state();
        assert_eq!(
            lethal_to_creature(state, wall, &[&shrink(0, -4)]),
            Some(true)
        );
        assert_eq!(
            lethal_to_creature(state, wall, &[&shrink(0, -3)]),
            Some(false)
        );
    }

    #[test]
    fn minus_one_counters_kill_by_toughness() {
        let mut scenario = GameScenario::new();
        let bear = scenario.add_creature(P1, "Bear", 2, 2).id();
        let runner = scenario.build();
        let state = runner.state();
        assert_eq!(
            lethal_to_creature(state, bear, &[&minus_counters(2)]),
            Some(true)
        );
        assert_eq!(
            lethal_to_creature(state, bear, &[&minus_counters(1)]),
            Some(false)
        );
    }

    #[test]
    fn indestructible_survives_damage_but_dies_to_zero_toughness() {
        let mut scenario = GameScenario::new();
        let wall = scenario
            .add_creature(P1, "Wall", 0, 4)
            .with_keyword(Keyword::Indestructible)
            .id();
        let runner = scenario.build();
        let state = runner.state();
        // CR 702.12b: damage never destroys an indestructible creature.
        assert_eq!(
            lethal_to_creature(state, wall, &[&deal_damage(10)]),
            Some(false)
        );
        // CR 704.5f: toughness 0 bypasses indestructible.
        assert_eq!(
            lethal_to_creature(state, wall, &[&shrink(0, -4)]),
            Some(true)
        );
    }

    #[test]
    fn variable_and_non_shrink_effects_are_undecidable() {
        let mut scenario = GameScenario::new();
        let bear = scenario.add_creature(P1, "Bear", 2, 2).id();
        let runner = scenario.build();
        let state = runner.state();
        // Variable (X) shrink: the caster picks the amount -> undecidable.
        let variable_shrink = Effect::Pump {
            power: PtValue::Fixed(0),
            toughness: PtValue::Variable("X".to_string()),
            target: TargetFilter::Any,
        };
        assert_eq!(lethal_to_creature(state, bear, &[&variable_shrink]), None);
        // Destroy has no toughness-reducing component -> undecidable here (the
        // gate handles it; lethality math doesn't apply).
        let destroy = Effect::Destroy {
            target: TargetFilter::Any,
            cant_regenerate: false,
        };
        assert_eq!(lethal_to_creature(state, bear, &[&destroy]), None);
    }
}

#[cfg(test)]
mod suspect_scope_tests {
    use super::*;

    // CR 701.60a: mass un-designation ("all suspected creatures are no longer
    // suspected", Absolving Lammasu) is a non-targeting population effect. The
    // AI's target-filter extraction must mirror the engine's `target_filter()`,
    // which surfaces a selectable target only for `EffectScope::Single`.
    #[test]
    fn extract_target_filter_only_for_single_scope_suspect() {
        // Same non-None filter on both; only `scope` differs, so a pass proves
        // the scope gate (not the filter) drives target-filter extraction.
        let single_suspect = Effect::Suspect {
            target: TargetFilter::Any,
            scope: EffectScope::Single,
        };
        let all_suspect = Effect::Suspect {
            target: TargetFilter::Any,
            scope: EffectScope::All,
        };
        assert!(
            extract_target_filter(&single_suspect).is_some(),
            "single-scope Suspect must expose a selectable target"
        );
        assert!(
            extract_target_filter(&all_suspect).is_none(),
            "mass Suspect{{All}} is a population effect, not target-filtered"
        );
    }

    #[test]
    fn extract_target_filter_only_for_single_scope_unsuspect() {
        let single_unsuspect = Effect::Unsuspect {
            target: TargetFilter::Any,
            scope: EffectScope::Single,
        };
        let all_unsuspect = Effect::Unsuspect {
            target: TargetFilter::Any,
            scope: EffectScope::All,
        };
        assert!(
            extract_target_filter(&single_unsuspect).is_some(),
            "single-scope Unsuspect must expose a selectable target"
        );
        assert!(
            extract_target_filter(&all_unsuspect).is_none(),
            "mass Unsuspect{{All}} (Absolving Lammasu) is a population effect, not target-filtered"
        );
    }
}
