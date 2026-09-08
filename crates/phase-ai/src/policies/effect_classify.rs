use engine::game::filter::{matches_target_filter, FilterContext};
use engine::game::game_object::GameObject;
use engine::game::quantity::try_resolve_quantity_in_source_context;
use engine::types::ability::{
    AbilityKind, ContinuousModification, ControllerRef, Effect, EffectScope, PtValue, QuantityExpr,
    ResolvedAbility, SubAbilityLink, TapStateChange, TargetChoiceTiming, TargetFilter, TargetRef,
    TriggerDefinition, TypeFilter,
};
use engine::types::counter::CounterType;
use engine::types::game_state::{CastingVariant, GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, KeywordKind};
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use super::context::PolicyContext;

/// Player-impact magnitude above which target selection has a directional
/// preference rather than falling back to the spell's broader polarity.
pub(crate) const PLAYER_IMPACT_PREFERENCE_BAND: f64 = 0.25;

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

/// CR 122.1: A counter's polarity for the permanent that bears it.
///
/// Exhaustive over `CounterType` — deliberately no wildcard — so a new counter
/// kind is a compile error here rather than a silent `Contextual`. `Contextual`
/// reads as impact 0.0 at the call sites, which inverts the AI's target
/// preference (it happily shielded and buffed opponents' creatures).
fn counter_sign_polarity(counter_type: &CounterType) -> EffectPolarity {
    match counter_type {
        // CR 122.1a: `+X/+Y` adds to power and toughness, `-X/-Y` subtracts.
        CounterType::Plus1Plus1 => EffectPolarity::Beneficial,
        CounterType::Minus1Minus1 => EffectPolarity::Harmful,
        // CR 122.1a: the parameterized asymmetric counter carries both deltas,
        // so the sign is derived rather than assumed. A mixed counter (+1/-1)
        // is a genuine trade-off and an all-zero one a no-op, so both stay
        // Contextual.
        CounterType::PowerToughness { power, toughness } => match (*power, *toughness) {
            (0, 0) => EffectPolarity::Contextual,
            (p, t) if p >= 0 && t >= 0 => EffectPolarity::Beneficial,
            (p, t) if p <= 0 && t <= 0 => EffectPolarity::Harmful,
            _ => EffectPolarity::Contextual,
        },
        // CR 122.1c: shield counters only ever protect their bearer — they
        // replace a destruction and prevent damage — so they are strictly
        // beneficial to it.
        CounterType::Shield => EffectPolarity::Beneficial,
        // CR 122.1d: a stun counter stops the permanent from untapping, a
        // strict penalty on its controller.
        CounterType::Stun => EffectPolarity::Harmful,
        // CR 702.147a: decayed is the one keyword a counter can grant that is a
        // drawback ("can't block", and sacrifice at end of combat when it
        // attacks), so it inverts the keyword-counter default below.
        CounterType::Keyword(KeywordKind::Decayed) => EffectPolarity::Harmful,
        // CR 122.1b: every other keyword a counter can grant (flying, first
        // strike, hexproof, lifelink, …) upgrades the bearer.
        CounterType::Keyword(_) => EffectPolarity::Beneficial,
        // Resource and clock counters carry no intrinsic polarity for the
        // bearer: CR 122.1e loyalty is a planeswalker's own currency;
        // CR 122.1g defense is a battle's, and a battle is attacked by its
        // protector's opponents; CR 714.3 lore advances a Saga toward both its
        // payoff chapters and its sacrifice; CR 702.62a/CR 702.63a time counts
        // down to a free cast (suspend) or a sacrifice (vanishing);
        // CR 702.32a fade and CR 702.24a age each bring the sacrifice or the
        // escalating upkeep closer on a permanent that was printed to carry
        // them; CR 122.1h finality swaps the graveyard for exile, which denies
        // the bearer's controller recursion but also denies an opponent's.
        CounterType::Loyalty
        | CounterType::Defense
        | CounterType::Lore
        | CounterType::Time
        | CounterType::Fade
        | CounterType::Age
        | CounterType::Finality => EffectPolarity::Contextual,
        // The legacy string counter keeps its `+`/`-` spelling rule; every
        // other name (charge, quest, verse, …) is card-specific.
        CounterType::Generic(s) if s.starts_with('+') => EffectPolarity::Beneficial,
        CounterType::Generic(s) if s.starts_with('-') => EffectPolarity::Harmful,
        CounterType::Generic(_) => EffectPolarity::Contextual,
    }
}

/// Whether a `Pump` power/toughness modifier is non-negative.
///
/// The parser carries a pump's sign *inside* the value, in two encodings the AI
/// must read alike:
/// - `PtValue::Variable` names the signed variable directly — "target creature
///   gets -X/-X" parses to `Variable("-X")` (Slice from the Shadows), while
///   "gets +X/+X" parses to `Variable("X")`.
/// - `PtValue::Quantity` negates by multiplying — "gets -X/-X, where X is the
///   number of …" parses to `Multiply { factor: -1, inner }`.
///
/// Every other `Quantity` shape counts up from a non-negative game quantity, and
/// a `Fixed` modifier carries its own sign.
fn pt_value_is_non_negative(value: &PtValue) -> bool {
    match value {
        PtValue::Fixed(v) => *v >= 0,
        PtValue::Variable(name) => !name.starts_with('-'),
        PtValue::Quantity(QuantityExpr::Multiply { factor, .. }) => *factor >= 0,
        PtValue::Quantity(_) => true,
    }
}

pub(crate) fn effect_polarity(effect: &Effect) -> EffectPolarity {
    match effect {
        // Pump: beneficial only if both modifiers are non-negative. The sign
        // lives in the value (see `pt_value_is_non_negative`), so a `-X/-X`
        // shrink must not be read as a buff just because it is variable.
        Effect::Pump {
            power, toughness, ..
        } => {
            if pt_value_is_non_negative(power) && pt_value_is_non_negative(toughness) {
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
        // CR 122.1: the reproduced counter KIND is event-derived at resolution —
        // there is no static `counter_type` to sign, and the triggering event can
        // carry a harmful kind (e.g. -1/-1). The `target` is also not necessarily
        // self: Aragorn, Company Leader reproduces onto "up to one OTHER target
        // creature", so the effect can land on a creature the controller does not
        // want buffed/debuffed. Neither the sign nor the recipient is knowable
        // until the policy holds the selected target and the triggering multiset,
        // so classify as Contextual and let the call site (e.g. anti_self_harm)
        // inspect both rather than assuming a self-buff.
        Effect::ReproduceEventCounters { .. } => EffectPolarity::Contextual,
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
        | Effect::ArrangePlanarDeckTop { .. }
        | Effect::Explore
        | Effect::Investigate
        | Effect::Mana { .. }
        | Effect::SearchLibrary { .. }
        | Effect::Surveil { .. }
        | Effect::Connive { .. }
        // CR 725.1 + CR 725.2: the monarch draws an extra card each turn, so
        // crowning YOURSELF is beneficial. Crowning someone else ("target
        // opponent becomes the monarch") hands that advantage away and is NOT,
        // so every other subject scope falls through to the `Contextual`
        // catch-all rather than inheriting this arm.
        | Effect::BecomeMonarch {
            target: TargetFilter::Controller,
        }
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
        // Remaining variants have no fixed polarity for target-selection purposes
        // (their benefit/harm depends on usage context). Enumerated exhaustively
        // rather than caught by `_` so a newly added `Effect` variant fails to
        // compile here until its polarity is deliberately classified — the
        // forcing function that prevents silent `Contextual` misclassification.
        // `SetTapState { .. }` here catches only the non-Single scopes; the
        // beneficial (Single+Untap) and harmful (Single+Tap) cases are handled
        // by the guarded arms above.
        Effect::Adapt { .. }
        | Effect::AdditionalPhase { .. }
        | Effect::AddPendingETBCounters { .. }
        | Effect::AddPendingEntersModifications { .. }
        | Effect::AddRestriction { .. }
        | Effect::AddTargetReplacement { .. }
        | Effect::Amass { .. }
        | Effect::ApplyPerpetual { .. }
        | Effect::ApplyPostReplacementDamage { .. }
        | Effect::ApplySticker { .. }
        | Effect::AssembleContraptionOnSprocket { .. }
        | Effect::AssembleContraptions { .. }
        | Effect::AssembleContraptionsFromRollDifference
        | Effect::Attach { .. }
        | Effect::BecomeCopy { .. }
        | Effect::BecomePrepared { .. }
        | Effect::BecomeSaddled { .. }
        | Effect::BecomeUnprepared { .. }
        | Effect::Behold { .. }
        | Effect::BlightEffect { .. }
        | Effect::Bolster { .. }
        | Effect::Cascade
        | Effect::CastCopyOfCard { .. }
        | Effect::CastFromZone { .. }
        | Effect::ChangeSpeed { .. }
        | Effect::ChangeTargets { .. }
        | Effect::ChangeZoneAll { .. }
        | Effect::ChaosEnsues
        | Effect::Choose { .. }
        | Effect::ChooseAndSacrificeRest { .. }
        | Effect::ChooseAugmentAndCombineWithHost { .. }
        | Effect::ChooseCard { .. }
        | Effect::ChooseCounterAdjustment { .. }
        | Effect::ChooseCounterKind { .. }
        | Effect::ChooseDamageSource { .. }
        | Effect::ChooseDrawnThisTurnPayOrTopdeck { .. }
        | Effect::ChooseFromZone { .. }
        | Effect::ChooseObjectsIntoTrackedSet { .. }
        | Effect::ChooseOneOf { .. }
        | Effect::ChoosePermanent { .. }
        | Effect::Clash
        | Effect::Cleanup { .. }
        | Effect::Cloak { .. }
        | Effect::CollectEvidence { .. }
        | Effect::CombineHost { .. }
        | Effect::Conjure { .. }
        | Effect::ControlNextTurn { .. }
        | Effect::CopySpell { .. }
        | Effect::CopyTokenBlockingAttacker { .. }
        | Effect::CopyTokenOf { .. }
        | Effect::CounterAll { .. }
        | Effect::CrankContraptions { .. }
        | Effect::CreateDamageReplacement { .. }
        | Effect::CreateDelayedTrigger { .. }
        | Effect::CreateDrawReplacement { .. }
        | Effect::CreateEmblem { .. }
        | Effect::CreatePlaneswalkReplacement { .. }
        | Effect::CreateTokenCopyFromPool { .. }
        | Effect::DamageEachPlayer { .. }
        | Effect::Detain { .. }
        | Effect::Dig { .. }
        | Effect::Discard { .. }
        | Effect::Discover { .. }
        | Effect::Double { .. }
        | Effect::DraftFromSpellbook { .. }
        | Effect::EachDealsDamageEqualToPower { .. }
        | Effect::EachPlayerCopyChosen { .. }
        | Effect::EachSourceDealsDamage { .. }
        | Effect::Encore
        | Effect::EndCombatPhase
        | Effect::EndTheTurn
        | Effect::Endure { .. }
        | Effect::EpicCopy { .. }
        | Effect::ExchangeLifeWithStat { .. }
        | Effect::ExileFromTopUntil { .. }
        | Effect::ExileHaunting { .. }
        | Effect::ExileResolvingSpellInsteadOfGraveyard { .. }
        | Effect::ExileTop { .. }
        | Effect::ExileFaceDownPile { .. }
        | Effect::Exploit { .. }
        | Effect::ExploreAll { .. }
        | Effect::FlipCoin { .. }
        | Effect::FlipCoins { .. }
        | Effect::FlipCoinUntilLose { .. }
        | Effect::Forage
        | Effect::CompletePlayerAction { .. }
        | Effect::ForceAttack { .. }
        | Effect::ForEachCategory { .. }
        | Effect::FreeCastFromZones { .. }
        | Effect::GainActivatedAbilitiesOfTarget { .. }
        | Effect::GainControlAll { .. }
        | Effect::GainEnergy { .. }
        | Effect::GiveControl { .. }
        | Effect::GoadAll { .. }
        | Effect::GrantCastingPermission { .. }
        | Effect::GrantExtraLoyaltyActivations { .. }
        | Effect::GrantNextSpellAbility { .. }
        | Effect::Harness
        | Effect::Heist { .. }
        | Effect::HeistExile
        | Effect::HideawayConceal { .. }
        | Effect::Incubate { .. }
        | Effect::Intensify { .. }
        | Effect::Learn
        | Effect::LoseAllPlayerCounters { .. }
        | Effect::MadnessCast { .. }
        | Effect::Manifest { .. }
        | Effect::ManifestDread
        | Effect::Meld { .. }
        | Effect::MiracleCast { .. }
        | Effect::Monstrosity { .. }
        | Effect::Myriad
        | Effect::NoOp
        | Effect::NoteManaSpent
        | Effect::OpenAttractions { .. }
        | Effect::OpponentGuess { .. }
        | Effect::PairWith { .. }
        | Effect::PayCost { .. }
        | Effect::PhaseIn { .. }
        | Effect::Planeswalk
        | Effect::Populate
        | Effect::ProcessRadCounters
        | Effect::ProliferateTarget { .. }
        | Effect::PumpAll { .. }
        | Effect::PutAtLibraryPosition { .. }
        | Effect::PutChosenCounter { .. }
        | Effect::PutOnTopOrBottom { .. }
        | Effect::PutSticker { .. }
        | Effect::ReassembleContraption { .. }
        | Effect::ReassembleContraptionOnSprocket { .. }
        | Effect::ReduceNextSpellCost { .. }
        | Effect::RedistributeLifeTotals
        | Effect::RegisterBending { .. }
        | Effect::RememberCard { .. }
        | Effect::RemoveFromCombat { .. }
        | Effect::BecomeBlocked { .. }
        // CR 725.1: crowning a player OTHER than yourself ("target opponent
        // becomes the monarch"). Whether handing out the designation helps you
        // is card-specific — Jared Carthalion wants an opponent crowned so it
        // can take it back — so it is Contextual, never the `Beneficial` arm
        // above, which is scoped to `PlayerScope::Controller`.
        | Effect::BecomeMonarch { .. }
        | Effect::Renown { .. }
        | Effect::ReturnAsAura { .. }
        | Effect::Reveal { .. }
        // CR 101.4: publishing already-chosen numbers moves no card and changes
        // no board state, so it is neither good nor bad on its own — the damage
        // and wheel clauses that READ those numbers carry the polarity.
        | Effect::RevealChosenNumbers { .. }
        | Effect::RevealFromHand { .. }
        | Effect::RevealHand { .. }
        | Effect::RevealTop { .. }
        | Effect::RevealUntil { .. }
        | Effect::ReverseTurnOrder
        | Effect::RingTemptsYou
        | Effect::Ripple { .. }
        | Effect::RollDie { .. }
        | Effect::RollToVisitAttractions
        | Effect::RuntimeHandled { .. }
        | Effect::OpenBoosterPack { .. }
        | Effect::SearchOutsideGame { .. }
        | Effect::Seek { .. }
        | Effect::SeparateIntoPiles { .. }
        | Effect::SetClassLevel { .. }
        | Effect::SetDayNight { .. }
        | Effect::SetLifeTotal { .. }
        | Effect::SetRoomDoorLock { .. }
        | Effect::SetTapState { .. }
        | Effect::Shuffle { .. }
        | Effect::SolveCase
        | Effect::Specialize
        | Effect::StartYourEngines { .. }
        | Effect::SwapChosenLabels { .. }
        | Effect::SwitchPT { .. }
        | Effect::TakeTheInitiative
        | Effect::TargetOnly { .. }
        | Effect::TimeTravel
        | Effect::Transform { .. }
        // CR 710.4: like Transform, flipping swaps a permanent's characteristics
        // wholesale — whether the alternative half is better is card-specific.
        | Effect::FlipPermanent { .. }
        | Effect::Tribute { .. }
        | Effect::TurnFaceDown { .. }
        | Effect::TurnFaceUp { .. }
        | Effect::UnattachAll { .. }
        | Effect::Unimplemented { .. }
        | Effect::Unsuspect { .. }
        | Effect::VentureInto { .. }
        | Effect::VentureIntoDungeon
        | Effect::Vote { .. }
        | Effect::WinTheGame { .. } => EffectPolarity::Contextual,
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
        // GainLife's player axis is always its target filter; GenericEffect
        // and LoseLife carry optional target filters.
        Effect::GainLife { player, .. } => Some(player),
        Effect::GenericEffect { target, .. } | Effect::LoseLife { target, .. } => target.as_ref(),
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
    if player_impact > PLAYER_IMPACT_PREFERENCE_BAND {
        return true;
    }
    if player_impact < -PLAYER_IMPACT_PREFERENCE_BAND {
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
    aggregate_player_impact_in(&ctx.effects())
}

pub(crate) fn aggregate_player_impact_in(effects: &[&Effect]) -> f64 {
    effects
        .iter()
        .map(|effect| player_impact(None, None, None, effect))
        .sum()
}

pub(crate) fn targeted_player_impact(ctx: &PolicyContext<'_>, player: PlayerId) -> Option<f64> {
    let source = ctx.source_object();
    targeted_player_impact_in(
        ctx.state,
        source.map(|object| object.controller),
        source.map(|object| object.id),
        &ctx.effects(),
        player,
    )
}

pub(crate) fn targeted_player_impact_in(
    state: &GameState,
    source_controller: Option<PlayerId>,
    source_id: Option<ObjectId>,
    effects: &[&Effect],
    player: PlayerId,
) -> Option<f64> {
    targeted_player_impact_in_with_parent_target_binding(
        state,
        source_controller,
        source_id,
        effects,
        player,
        None,
    )
}

/// Compatibility seam for callers that have already proved that `player` is
/// the direct root target. A flat effect slice cannot establish that ownership
/// itself, so ordinary target scoring must continue to pass `None` here.
pub(crate) fn targeted_player_impact_in_with_bound_parent_target(
    state: &GameState,
    source_controller: Option<PlayerId>,
    source_id: Option<ObjectId>,
    effects: &[&Effect],
    player: PlayerId,
    bound_parent_target: PlayerId,
) -> Option<f64> {
    targeted_player_impact_in_with_parent_target_binding(
        state,
        source_controller,
        source_id,
        effects,
        player,
        Some(bound_parent_target),
    )
}

fn targeted_player_impact_in_with_parent_target_binding(
    state: &GameState,
    source_controller: Option<PlayerId>,
    source_id: Option<ObjectId>,
    effects: &[&Effect],
    player: PlayerId,
    bound_parent_target: Option<PlayerId>,
) -> Option<f64> {
    let mut found_targeted_effect = false;
    let mut impact = 0.0;

    for effect in effects {
        let Some(filter) = selected_player_target_filter(effect) else {
            continue;
        };
        if matches!(filter, TargetFilter::ParentTarget) && bound_parent_target == Some(player)
            || filter_names_the_chosen_players_permanents(filter)
            || engine::game::filter::player_matches_target_filter_in_state(
                state,
                filter,
                player,
                source_controller,
                source_id,
            )
        {
            found_targeted_effect = true;
            impact += player_impact(Some(state), source_controller, source_id, effect);
        }
    }

    found_targeted_effect.then_some(impact)
}

/// Preview a player-targeting pending cast only while the exact root target
/// selection is still live. This is intentionally not a general ability-graph
/// interpreter: unsupported modifiers, branches, or recipient authorities
/// leave the established heuristic in control.
pub(crate) fn exact_pending_player_impact(
    ctx: &PolicyContext<'_>,
    target: &TargetRef,
) -> Option<f64> {
    let TargetRef::Player(player) = target else {
        return None;
    };
    let WaitingFor::TargetSelection {
        pending_cast,
        target_slots,
        selection,
        ..
    } = &ctx.decision.waiting_for
    else {
        return None;
    };
    if target_slots.len() != 1
        || selection.current_slot != 0
        || !selection.selected_slots.is_empty()
        || !selection.current_legal_targets.contains(target)
        || target_slots[0].chooser.is_some()
    {
        return None;
    }

    let root = &pending_cast.ability;
    if pending_cast.object_id != root.source_id
        || root.kind != AbilityKind::Spell
        || !ctx.state.objects.contains_key(&pending_cast.object_id)
        || !exact_pending_node_is_eligible(root)
    {
        return None;
    }
    let source_controller = root.original_controller.unwrap_or(root.controller);
    let root_filter = exact_player_effect_filter(&root.effect)?;
    if !is_exact_root_player_selector(root_filter)
        || !engine::game::filter::player_matches_target_filter_in_state(
            ctx.state,
            root_filter,
            *player,
            Some(source_controller),
            Some(root.source_id),
        )
    {
        return None;
    }

    let ExactPendingNodeContribution::Impact(mut impact) = exact_pending_node_impact(
        root,
        ExactPendingNodeRole::Root,
        ctx.state,
        source_controller,
    )?
    else {
        return None;
    };
    let mut node: &ResolvedAbility = root;
    while let Some(next) = node.sub_ability.as_deref() {
        if next.sub_link != SubAbilityLink::ContinuationStep
            || next.source_id != root.source_id
            || !exact_pending_node_is_eligible(next)
        {
            return None;
        }
        match exact_pending_node_impact(
            next,
            ExactPendingNodeRole::Continuation,
            ctx.state,
            source_controller,
        )? {
            ExactPendingNodeContribution::Impact(contribution) => impact += contribution,
            ExactPendingNodeContribution::Independent => {}
        }
        node = next;
    }
    Some(impact)
}

fn exact_pending_node_is_eligible(node: &ResolvedAbility) -> bool {
    node.kind == AbilityKind::Spell
        && node.targets.is_empty()
        && node.else_ability.is_none()
        && node.duration.is_none()
        && node.condition.is_none()
        && !node.optional_targeting
        && !node.optional
        && node.optional_player.is_none()
        && node.optional_for.is_none()
        && node.multi_target.is_none()
        && node.target_constraints.is_empty()
        && node.target_choice_timing == TargetChoiceTiming::Stack
        && node.selected_mode_labels.is_empty()
        && node.modal_instruction_ordinal.is_none()
        && node.detached_remainder == Default::default()
        && node.repeat_for.is_none()
        && node.min_x_value == 0
        && node.announced_x.is_none()
        && !node.cant_be_copied
        && node.copy_count_status == Default::default()
        && !node.forward_result
        && node.unless_pay.is_none()
        && node.distribution.is_none()
        && node.distribute.is_none()
        && node.player_scope.is_none()
        && node.starting_with.is_none()
        && node.chosen_x.is_none()
        && node.target_chooser.is_none()
        && node.chosen_players.is_empty()
        && node.repeat_until.is_none()
        && node.replacement_applied.is_empty()
        && node.target_selection_mode == Default::default()
        && node.sibling_condition == Default::default()
        && node.modal.is_none()
        && node.mode_abilities.is_empty()
        && node.parent_target_missing_reason.is_none()
}

fn exact_player_effect_filter(effect: &Effect) -> Option<&TargetFilter> {
    match effect {
        Effect::Draw { target, .. } | Effect::Discard { target, .. } => Some(target),
        Effect::GainLife { player, .. } => Some(player),
        Effect::LoseLife {
            target: Some(target),
            ..
        } => Some(target),
        _ => None,
    }
}

fn is_exact_root_player_selector(filter: &TargetFilter) -> bool {
    matches!(
        filter,
        TargetFilter::Player | TargetFilter::PlayerMatching { .. }
    ) || matches!(
        filter,
        TargetFilter::Typed(typed)
            if typed.type_filters.is_empty()
                && typed.properties.is_empty()
                && matches!(
                    typed.controller,
                    Some(
                        ControllerRef::You
                            | ControllerRef::Opponent
                            | ControllerRef::SpecificPlayer { .. }
                    )
                )
    )
}

#[derive(Debug, Clone, Copy)]
enum ExactPendingNodeRole {
    Root,
    Continuation,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum ExactPendingNodeContribution {
    Impact(f64),
    Independent,
}

fn exact_pending_node_impact(
    node: &ResolvedAbility,
    role: ExactPendingNodeRole,
    state: &GameState,
    source_controller: PlayerId,
) -> Option<ExactPendingNodeContribution> {
    let (quantity, coefficient, filter, discard_is_simple) = match &node.effect {
        Effect::Draw { count, target } => (count, 1.25, target, true),
        Effect::Discard {
            count,
            target,
            filter,
            selection,
            unless_filter,
        } => (
            count,
            -1.5,
            target,
            filter.is_none()
                && unless_filter.is_none()
                && matches!(selection, engine::types::ability::CardSelectionMode::Chosen),
        ),
        Effect::GainLife { amount, player } => (amount, 0.15, player, true),
        Effect::LoseLife {
            amount,
            target: Some(target),
        } => (amount, -0.15, target, true),
        _ => return None,
    };
    if !discard_is_simple {
        return None;
    }

    match role {
        ExactPendingNodeRole::Root if !is_exact_root_player_selector(filter) => return None,
        ExactPendingNodeRole::Root => {}
        ExactPendingNodeRole::Continuation => match filter {
            TargetFilter::Controller => return Some(ExactPendingNodeContribution::Independent),
            TargetFilter::ParentTarget => {}
            _ => return None,
        },
    }

    let value = match role {
        ExactPendingNodeRole::Root => try_resolve_quantity_in_source_context(
            state,
            quantity,
            source_controller,
            node.source_id,
        )?,
        ExactPendingNodeRole::Continuation => {
            let QuantityExpr::Fixed { value } = quantity else {
                return None;
            };
            *value
        }
    };
    Some(ExactPendingNodeContribution::Impact(
        f64::from(value.max(0)) * coefficient,
    ))
}

/// Returns the player filter that is bound by target selection, if this effect
/// has one. Contextual recipients such as `Controller` occur independently of
/// the selected player and therefore must not affect target preference.
fn selected_player_target_filter(effect: &Effect) -> Option<&TargetFilter> {
    match effect {
        Effect::Draw { target, .. } | Effect::Discard { target, .. } => {
            chosen_player_binding(target).then_some(target)
        }
        Effect::GainLife { player, .. } => chosen_player_binding(player).then_some(player),
        Effect::LoseLife {
            target: Some(target),
            ..
        } => chosen_player_binding(target).then_some(target),
        _ => extract_target_filter(effect),
    }
}

fn chosen_player_binding(filter: &TargetFilter) -> bool {
    matches!(filter, TargetFilter::Player | TargetFilter::ParentTarget)
        || filter_names_the_chosen_players_permanents(filter)
}

/// "each creature target player controls" (Requisition
/// Raid) or "each creature target opponent controls" has exactly one instance
/// of the word "target", and it names a PLAYER. The objects the effect touches
/// are described *by reference to* that chosen player, so the slot being
/// filled is the player slot and the effect's impact lands on whichever player
/// is chosen. `player_matches_target_filter_in_state` deliberately fails closed
/// on both `ControllerRef::TargetPlayer` and `ControllerRef::TargetOpponent`
/// (it has no ability context to resolve the reference against); here the
/// candidate player *is* that target, so the effect must be counted for them
/// rather than dropped — dropping it left the whole spell reading as "no
/// per-player signal".
fn filter_names_the_chosen_players_permanents(filter: &TargetFilter) -> bool {
    matches!(
        filter,
        TargetFilter::Typed(typed)
            if matches!(
                typed.controller,
                Some(ControllerRef::TargetPlayer | ControllerRef::TargetOpponent)
            )
    )
}

pub(crate) fn targeted_object_impact(ctx: &PolicyContext<'_>, object_id: ObjectId) -> Option<f64> {
    let mut found_targeted_effect = false;
    let mut impact = 0.0;

    for effect in ctx.effects() {
        if effect_targets_object(ctx, effect, object_id) {
            found_targeted_effect = true;
            impact += player_impact(
                Some(ctx.state),
                ctx.source_object().map(|object| object.controller),
                Some(effect_source_id(ctx)),
                effect,
            );
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

fn player_impact(
    state: Option<&GameState>,
    source_controller: Option<PlayerId>,
    source_id: Option<ObjectId>,
    effect: &Effect,
) -> f64 {
    match effect {
        Effect::Draw { count, .. } => {
            quantity_weight(state, source_controller, source_id, count, 1.25)
        }
        Effect::Discard { count, .. } => {
            -quantity_weight(state, source_controller, source_id, count, 1.5)
        }
        Effect::DiscardCard { count, .. } => -(*count as f64 * 1.5),
        Effect::GainLife { amount, .. } => {
            quantity_weight(state, source_controller, source_id, amount, 0.15)
        }
        Effect::LoseLife { amount, .. } => {
            -quantity_weight(state, source_controller, source_id, amount, 0.15)
        }
        _ => match effect_polarity(effect) {
            EffectPolarity::Beneficial => 1.0,
            EffectPolarity::Harmful => -1.0,
            EffectPolarity::Contextual => 0.0,
        },
    }
}

fn quantity_weight(
    state: Option<&GameState>,
    source_controller: Option<PlayerId>,
    source_id: Option<ObjectId>,
    quantity: &QuantityExpr,
    factor: f64,
) -> f64 {
    let magnitude = match (state, source_controller, source_id) {
        (Some(state), Some(controller), Some(source_id)) => {
            try_resolve_quantity_in_source_context(state, quantity, controller, source_id)
                .map_or(1, |value| value.max(0))
        }
        _ => match quantity {
            QuantityExpr::Fixed { value } => (*value).max(0),
            _ => 1,
        },
    };
    factor * f64::from(magnitude)
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
    for trigger in source
        .trigger_definitions
        .iter_unchecked()
        .map(|entry| &entry.definition)
    {
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

/// CR 603.1: A `GrantTrigger` confers a triggered ability on its target. The
/// benefit/harm to the bearer is the polarity of the effect that granted trigger
/// *executes* — Undying Malice grants "when this dies, return it to the
/// battlefield" (`ChangeZone`→Battlefield, Beneficial); a downside grant of "at
/// the beginning of your upkeep, you lose 1 life" (`LoseLife`, Harmful) must NOT
/// read Beneficial. Delegating to `effect_polarity` covers the whole class of
/// grant-a-trigger buffs and downside curses (AI heuristic). The polarity is
/// bound statically from the parsed `TriggerDefinition.execute.effect`; no live
/// game-state lookup.
fn granted_trigger_polarity(trigger: &TriggerDefinition) -> EffectPolarity {
    trigger
        .execute
        .as_deref()
        .map(|exec| effect_polarity(&exec.effect))
        .unwrap_or(EffectPolarity::Contextual)
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
        | ContinuousModification::AddKeywordWithDerivedCost { .. }
        | ContinuousModification::GrantAbility { .. }
        | ContinuousModification::AddAllCreatureTypes
        | ContinuousModification::AddColor { .. }
        | ContinuousModification::AddType { .. }
        | ContinuousModification::AddSubtype { .. } => EffectPolarity::Beneficial,
        ContinuousModification::GrantTrigger { trigger } => granted_trigger_polarity(trigger),
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
            excess: None,
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

#[cfg(test)]
mod grant_trigger_polarity_tests {
    use super::*;
    use engine::types::ability::{AbilityDefinition, AbilityKind, StaticDefinition, TypedFilter};
    use engine::types::zones::EtbTapState;

    /// Build a `GenericEffect` that grants its target a triggered ability whose
    /// executed effect is `exec` — the Undying-Malice-shaped AST
    /// (`GenericEffect{ Continuous{ GrantTrigger{ dies → exec } } }`).
    fn grant_trigger_generic(exec: Effect) -> Effect {
        let mut trigger = TriggerDefinition::new(TriggerMode::ChangesZone);
        trigger.execute = Some(Box::new(AbilityDefinition::new(AbilityKind::Spell, exec)));
        Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::continuous()
                .affected(TargetFilter::ParentTarget)
                .modifications(vec![ContinuousModification::GrantTrigger {
                    trigger: Box::new(trigger),
                }])],
            target: Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature))),
            duration: None,
            end_cost: None,
        }
    }

    /// "When this dies, return it to the battlefield" — the Undying Malice grant.
    fn return_to_battlefield() -> Effect {
        Effect::ChangeZone {
            origin: Some(Zone::Graveyard),
            destination: Zone::Battlefield,
            target: TargetFilter::SelfRef,
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: EtbTapState::Unspecified,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
            enters_modified_if: None,
        }
    }

    #[test]
    fn grant_return_trigger_reads_beneficial() {
        // Undying Malice grants "when this dies, return it to the battlefield"
        // (ChangeZone→Battlefield, Beneficial). Pre-fix `GrantTrigger` hit the
        // `_ => Contextual` fallback, so the whole GenericEffect read Contextual.
        let ge = grant_trigger_generic(return_to_battlefield());
        assert_eq!(effect_polarity(&ge), EffectPolarity::Beneficial);
    }

    #[test]
    fn harmful_grant_trigger_not_beneficial() {
        // A downside grant ("at the beginning of your upkeep, you lose 1 life")
        // must read Harmful, NOT a blanket Beneficial — this is the load-bearing
        // discriminator that proves the arm reads the executed-effect polarity
        // rather than labeling every grant beneficial.
        let ge = grant_trigger_generic(Effect::LoseLife {
            amount: QuantityExpr::Fixed { value: 1 },
            target: None,
        });
        assert_eq!(effect_polarity(&ge), EffectPolarity::Harmful);
    }

    #[test]
    fn granted_trigger_without_execute_is_contextual() {
        // A grant whose trigger has no executed effect carries no polarity
        // signal — stays Contextual (the same as the pre-existing fallback).
        let mut trigger = TriggerDefinition::new(TriggerMode::ChangesZone);
        trigger.execute = None;
        assert_eq!(
            modification_polarity(&ContinuousModification::GrantTrigger {
                trigger: Box::new(trigger),
            }),
            EffectPolarity::Contextual
        );
    }

    #[test]
    fn parsed_undying_malice_grant_reads_beneficial() {
        // Production-parser reach guard: the real Undying Malice Oracle text parses
        // to `GenericEffect{ Continuous{ GrantTrigger{ dies → ChangeZone→Battlefield
        // } } }`, so its polarity must read Beneficial through the same classifier
        // the AI target-scorer uses. Guards the fix against future parser AST drift.
        use engine::parser::oracle::parse_oracle_text;

        let parsed = parse_oracle_text(
            "Until end of turn, target creature gains \"When this creature dies, return it to the battlefield tapped under its owner's control with a +1/+1 counter on it.\"",
            "Undying Malice",
            &[],
            &["Instant".to_string()],
            &[],
        );
        let spell = parsed
            .abilities
            .iter()
            .find(|a| a.kind == AbilityKind::Spell)
            .expect("Undying Malice parses to a spell ability");
        assert_eq!(
            effect_polarity(&spell.effect),
            EffectPolarity::Beneficial,
            "Undying Malice's granted return-to-battlefield trigger must read Beneficial"
        );
    }

    #[test]
    fn grant_ability_still_beneficial() {
        // Sibling reach-guard: `GrantAbility` (a granted static/activated ability,
        // no executed-trigger effect to inspect) stays in the Beneficial cluster,
        // unchanged by the new GrantTrigger arm.
        assert_eq!(
            modification_polarity(&ContinuousModification::GrantAbility {
                definition: Box::new(AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::TargetOnly {
                        target: TargetFilter::Any,
                    },
                )),
            }),
            EffectPolarity::Beneficial
        );
    }
}

#[cfg(test)]
mod live_quantity_targeting_tests {
    use super::*;
    use crate::config::AiConfig;
    use crate::policies::anti_self_harm::AntiSelfHarmPolicy;
    use crate::policies::context::SearchDepth;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::quantity::quantity_is_cast_stable_for_pre_cast;
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityCondition, AbilityCost, AbilityDefinition, CardSelectionMode, ControllerRef,
        CopyCountStatus, DetachedRemainder, Duration, EffectKind, FilterProp, ModalChoice,
        MultiTargetSpec, OpponentMayScope, ParentTargetMissingReason, PlayerFilter, PlayerScope,
        QuantityRef, RepeatContinuation, ResolvedAbility, SiblingCondition, SubAbilityLink,
        TargetChoiceTiming, TargetRef, TargetSelectionMode, TypedFilter, UnlessPayModifier,
    };
    use engine::types::actions::GameAction;
    use engine::types::card_type::CoreType;
    use engine::types::game_state::{
        DistributionUnit, PendingCast, TargetEffectDetail, TargetSelectionConstraint,
        TargetSelectionSlot, WaitingFor,
    };
    use engine::types::identifiers::CardId;
    use engine::types::mana::ManaCost;
    use engine::types::proposed_event::AppliedReplacementKey;

    fn player_target_score(
        state: &GameState,
        source: ObjectId,
        effect: Effect,
        action: GameAction,
    ) -> f64 {
        player_target_score_for_ability(
            state,
            source,
            ResolvedAbility::new(effect, Vec::new(), source, PlayerId(0)),
            action,
        )
    }

    fn player_target_score_for_ability(
        state: &GameState,
        source: ObjectId,
        ability: ResolvedAbility,
        action: GameAction,
    ) -> f64 {
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TargetSelection {
                player: PlayerId(0),
                pending_cast: Box::new(PendingCast::new(
                    source,
                    CardId(source.0),
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
                    effect_kind: EffectKind::NoOp,
                    effect_detail: TargetEffectDetail::None,
                }],
                mode_labels: Vec::new(),
                selection: engine::types::game_state::TargetSelectionProgress {
                    current_slot: 0,
                    selected_slots: Vec::new(),
                    current_legal_targets: vec![
                        TargetRef::Player(PlayerId(0)),
                        TargetRef::Player(PlayerId(1)),
                    ],
                },
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action,
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Target),
        };
        let config = AiConfig::default();
        let context = crate::context::AiContext::empty(&config.weights);
        let policy_context = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: SearchDepth::Root,
        };

        AntiSelfHarmPolicy.score(&policy_context)
    }

    fn creature_count_amount() -> QuantityExpr {
        QuantityExpr::Multiply {
            factor: 2,
            inner: Box::new(QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount {
                    filter: TargetFilter::Typed(TypedFilter::creature()),
                },
            }),
        }
    }

    fn exact_pending_impact(
        state: &mut GameState,
        ability: ResolvedAbility,
        legal_targets: Vec<TargetRef>,
        candidate: TargetRef,
    ) -> Option<f64> {
        let source = ability.source_id;
        state.waiting_for = WaitingFor::TargetSelection {
            player: PlayerId(0),
            pending_cast: Box::new(PendingCast::new(
                source,
                CardId(source.0),
                ability,
                ManaCost::zero(),
            )),
            target_slots: vec![TargetSelectionSlot {
                legal_targets: legal_targets.clone(),
                optional: false,
                chooser: None,
                effect_kind: EffectKind::NoOp,
                effect_detail: TargetEffectDetail::None,
            }],
            mode_labels: Vec::new(),
            selection: engine::types::game_state::TargetSelectionProgress {
                current_slot: 0,
                selected_slots: Vec::new(),
                current_legal_targets: legal_targets,
            },
        };
        exact_pending_impact_from_live_state(state, candidate)
    }

    fn exact_pending_impact_from_live_state(
        state: &GameState,
        candidate: TargetRef,
    ) -> Option<f64> {
        let decision = AiDecisionContext {
            waiting_for: state.waiting_for.clone(),
            candidates: Vec::new(),
        };
        let action = CandidateAction {
            action: GameAction::ChooseTarget {
                target: Some(candidate.clone()),
            },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Target),
        };
        let config = AiConfig::default();
        let context = crate::context::AiContext::empty(&config.weights);
        let policy_context = PolicyContext {
            state,
            decision: &decision,
            candidate: &action,
            ai_player: PlayerId(0),
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: SearchDepth::Root,
        };
        exact_pending_player_impact(&policy_context, &candidate)
    }

    fn hand_source(state: &mut GameState, card_id: u64) -> ObjectId {
        create_object(
            state,
            CardId(card_id),
            PlayerId(0),
            "exact pending source".to_string(),
            Zone::Hand,
        )
    }

    fn direct_draw(source: ObjectId, count: QuantityExpr) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Draw {
                count,
                target: TargetFilter::Player,
            },
            Vec::new(),
            source,
            PlayerId(0),
        )
    }

    #[test]
    fn exact_pending_node_contributions_keep_zero_independent_and_unsupported_distinct() {
        let mut state = GameState::new_two_player(7);
        let source = hand_source(&mut state, 409);
        assert_eq!(
            exact_pending_node_impact(
                &direct_draw(source, QuantityExpr::Fixed { value: 0 }),
                ExactPendingNodeRole::Root,
                &state,
                PlayerId(0),
            ),
            Some(ExactPendingNodeContribution::Impact(0.0)),
            "a zero root quantity is an exact owned contribution, not an independent rider"
        );

        let independent = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
            Vec::new(),
            source,
            PlayerId(0),
        );
        assert_eq!(
            exact_pending_node_impact(
                &independent,
                ExactPendingNodeRole::Continuation,
                &state,
                PlayerId(0),
            ),
            Some(ExactPendingNodeContribution::Independent),
            "a controller continuation is independent of the selected player"
        );
        assert_eq!(
            exact_pending_node_impact(
                &ResolvedAbility::new(Effect::NoOp, Vec::new(), source, PlayerId(0)),
                ExactPendingNodeRole::Continuation,
                &state,
                PlayerId(0),
            ),
            None,
            "unsupported continuations leave the whole exact preview unavailable"
        );
    }

    #[test]
    fn exact_pending_player_impact_enforces_selection_and_selector_provenance() {
        let mut state = GameState::new_two_player(7);
        let source = hand_source(&mut state, 410);
        let legal = vec![
            TargetRef::Player(PlayerId(0)),
            TargetRef::Player(PlayerId(1)),
        ];
        let ability = direct_draw(source, QuantityExpr::Fixed { value: 2 });
        assert_eq!(
            exact_pending_impact(
                &mut state,
                ability.clone(),
                legal.clone(),
                TargetRef::Player(PlayerId(1)),
            ),
            Some(2.5),
            "the legal current player slot is the positive reach guard"
        );
        assert_eq!(
            exact_pending_impact(
                &mut state,
                ability.clone(),
                legal.clone(),
                TargetRef::Object(ObjectId(999)),
            ),
            None,
            "an object is not a legal player candidate"
        );
        assert_eq!(
            exact_pending_impact(
                &mut state,
                ability.clone(),
                vec![TargetRef::Player(PlayerId(0))],
                TargetRef::Player(PlayerId(1)),
            ),
            None,
            "a player excluded only from current_legal_targets cannot take the exact path"
        );

        for mutation in ["previous", "later", "multiple", "chooser"] {
            let _ = exact_pending_impact(
                &mut state,
                ability.clone(),
                legal.clone(),
                TargetRef::Player(PlayerId(1)),
            );
            let WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            } = &mut state.waiting_for
            else {
                unreachable!("fixture installs a target selection");
            };
            match mutation {
                "previous" => selection
                    .selected_slots
                    .push(Some(TargetRef::Player(PlayerId(0)))),
                "later" => selection.current_slot = 1,
                "multiple" => target_slots.push(target_slots[0].clone()),
                "chooser" => target_slots[0].chooser = Some(PlayerId(1)),
                _ => unreachable!(),
            }
            let decision = AiDecisionContext {
                waiting_for: state.waiting_for.clone(),
                candidates: Vec::new(),
            };
            let candidate = CandidateAction {
                action: GameAction::ChooseTarget {
                    target: Some(TargetRef::Player(PlayerId(1))),
                },
                metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Target),
            };
            let config = AiConfig::default();
            let context = crate::context::AiContext::empty(&config.weights);
            let ctx = PolicyContext {
                state: &state,
                decision: &decision,
                candidate: &candidate,
                ai_player: PlayerId(0),
                config: &config,
                context: &context,
                cast_facts: None,
                search_depth: SearchDepth::Root,
            };
            assert_eq!(
                exact_pending_player_impact(&ctx, &TargetRef::Player(PlayerId(1))),
                None,
                "{mutation} selection shape must preserve the legacy fallback"
            );
        }

        let typed = |controller| {
            ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 2 },
                    target: TargetFilter::Typed(TypedFilter {
                        type_filters: Vec::new(),
                        controller: Some(controller),
                        properties: Vec::new(),
                    }),
                },
                Vec::new(),
                source,
                PlayerId(0),
            )
        };
        assert_eq!(
            exact_pending_impact(
                &mut state,
                typed(ControllerRef::You),
                legal.clone(),
                TargetRef::Player(PlayerId(0))
            ),
            Some(2.5)
        );
        assert_eq!(
            exact_pending_impact(
                &mut state,
                typed(ControllerRef::You),
                legal.clone(),
                TargetRef::Player(PlayerId(1))
            ),
            None
        );
        assert_eq!(
            exact_pending_impact(
                &mut state,
                typed(ControllerRef::SpecificPlayer { id: PlayerId(1) }),
                legal.clone(),
                TargetRef::Player(PlayerId(1))
            ),
            Some(2.5)
        );
        assert_eq!(
            exact_pending_impact(
                &mut state,
                typed(ControllerRef::SpecificPlayer { id: PlayerId(0) }),
                legal.clone(),
                TargetRef::Player(PlayerId(1)),
            ),
            None,
            "an unmatched SpecificPlayer is a legal slot candidate but not this root selector's recipient"
        );
        assert_eq!(
            exact_pending_impact(
                &mut state,
                typed(ControllerRef::TargetPlayer),
                legal,
                TargetRef::Player(PlayerId(1))
            ),
            None,
            "contextual typed siblings are not direct target provenance"
        );
        assert_eq!(
            exact_pending_impact(
                &mut state,
                typed(ControllerRef::ParentTargetOwner),
                vec![
                    TargetRef::Player(PlayerId(0)),
                    TargetRef::Player(PlayerId(1)),
                ],
                TargetRef::Player(PlayerId(1)),
            ),
            None,
            "ParentTargetOwner is contextual rather than direct root ownership"
        );
        let rider = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::Controller,
            },
            Vec::new(),
            source,
            PlayerId(0),
        );
        let signed = ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 1 },
                target: Some(TargetFilter::Player),
            },
            Vec::new(),
            source,
            PlayerId(0),
        )
        .sub_ability(rider);
        assert_eq!(
            exact_pending_impact(
                &mut state,
                signed,
                vec![
                    TargetRef::Player(PlayerId(0)),
                    TargetRef::Player(PlayerId(1))
                ],
                TargetRef::Player(PlayerId(1)),
            ),
            Some(-0.15),
            "LoseLife(1) keeps its exact unbanded sign when the controller rider is ignored"
        );

        let matching = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::PlayerMatching {
                    player: Box::new(PlayerFilter::Opponent),
                },
            },
            Vec::new(),
            source,
            PlayerId(0),
        );
        assert_eq!(
            exact_pending_impact(
                &mut state,
                matching.clone(),
                vec![
                    TargetRef::Player(PlayerId(0)),
                    TargetRef::Player(PlayerId(1))
                ],
                TargetRef::Player(PlayerId(1)),
            ),
            Some(2.5),
            "PlayerMatching is accepted only when the engine matcher accepts the candidate"
        );
        assert_eq!(
            exact_pending_impact(
                &mut state,
                matching,
                vec![
                    TargetRef::Player(PlayerId(0)),
                    TargetRef::Player(PlayerId(1))
                ],
                TargetRef::Player(PlayerId(0)),
            ),
            None,
            "the matched root selector must not leak to an illegal candidate"
        );
    }

    #[test]
    fn exact_pending_quantity_uses_original_controller_and_matching_source() {
        let mut state = GameState::new_two_player(7);
        let source = hand_source(&mut state, 411);
        let own = create_object(
            &mut state,
            CardId(412),
            PlayerId(0),
            "live controller creature".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&own).unwrap().card_types.core_types = vec![CoreType::Creature];
        for card_id in [413, 414] {
            let opposing = create_object(
                &mut state,
                CardId(card_id),
                PlayerId(1),
                "original-controller creature".to_string(),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&opposing)
                .unwrap()
                .card_types
                .core_types = vec![CoreType::Creature];
        }
        let filter = TargetFilter::Typed(TypedFilter {
            type_filters: Vec::new(),
            controller: Some(ControllerRef::You),
            properties: Vec::new(),
        });
        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount {
                        filter: TargetFilter::Typed(
                            TypedFilter::creature().controller(ControllerRef::You),
                        ),
                    },
                },
                target: filter,
            },
            Vec::new(),
            source,
            PlayerId(0),
        );
        ability.original_controller = Some(PlayerId(1));
        let legal = vec![TargetRef::Player(PlayerId(1))];
        assert_eq!(
            exact_pending_impact(
                &mut state,
                ability.clone(),
                legal.clone(),
                TargetRef::Player(PlayerId(1))
            ),
            Some(2.5),
            "the root's original controller, not its source object's live controller, resolves You"
        );
        let mut wrong_source = ability.clone();
        wrong_source.source_id = ObjectId(9_999);
        assert_eq!(
            exact_pending_impact(
                &mut state,
                wrong_source,
                legal.clone(),
                TargetRef::Player(PlayerId(1))
            ),
            None,
            "a pending object/root source mismatch has no exact provenance"
        );
        let different_existing_source = hand_source(&mut state, 416);
        let _ = exact_pending_impact(
            &mut state,
            ability.clone(),
            legal.clone(),
            TargetRef::Player(PlayerId(1)),
        );
        let WaitingFor::TargetSelection { pending_cast, .. } = &mut state.waiting_for else {
            unreachable!("the exact fixture installs a live pending cast");
        };
        pending_cast.object_id = different_existing_source;
        assert_eq!(
            exact_pending_impact_from_live_state(&state, TargetRef::Player(PlayerId(1))),
            None,
            "two existing source ids still must match before exact source-context resolution"
        );
        let WaitingFor::TargetSelection { pending_cast, .. } = &mut state.waiting_for else {
            unreachable!("the source-mismatch fixture remains live");
        };
        pending_cast.object_id = source;
        pending_cast.ability.sub_ability = Some(Box::new(ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::ParentTarget,
                filter: None,
                selection: CardSelectionMode::Chosen,
                unless_filter: None,
            },
            Vec::new(),
            source,
            PlayerId(0),
        )));
        assert_eq!(
            exact_pending_impact_from_live_state(&state, TargetRef::Player(PlayerId(1))),
            Some(1.0),
            "the matching fixed continuation reaches the child-source provenance guard"
        );
        let WaitingFor::TargetSelection { pending_cast, .. } = &mut state.waiting_for else {
            unreachable!("the matched fixed-child fixture remains live");
        };
        pending_cast
            .ability
            .sub_ability
            .as_mut()
            .expect("the matched chain retains its fixed child")
            .source_id = different_existing_source;
        assert_eq!(
            exact_pending_impact_from_live_state(&state, TargetRef::Player(PlayerId(1))),
            None,
            "a continuation with a different existing source id cannot inherit root authority"
        );
        state.objects.remove(&source);
        assert_eq!(
            exact_pending_impact(&mut state, ability, legal, TargetRef::Player(PlayerId(1))),
            None,
            "a missing source cannot supply exact source-context authority"
        );
    }

    #[test]
    fn exact_pending_typed_opponent_selector_matches_each_three_player_opponent() {
        let mut state = GameState::new_two_player(7);
        let mut third = state.players[1].clone();
        third.id = PlayerId(2);
        state.players.push(third);
        state.seat_order.push(PlayerId(2));
        let source = hand_source(&mut state, 415);
        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Typed(TypedFilter {
                    type_filters: Vec::new(),
                    controller: Some(ControllerRef::Opponent),
                    properties: Vec::new(),
                }),
            },
            Vec::new(),
            source,
            PlayerId(0),
        );
        let legal = vec![
            TargetRef::Player(PlayerId(0)),
            TargetRef::Player(PlayerId(1)),
            TargetRef::Player(PlayerId(2)),
        ];
        for opponent in [PlayerId(1), PlayerId(2)] {
            assert_eq!(
                exact_pending_impact(
                    &mut state,
                    ability.clone(),
                    legal.clone(),
                    TargetRef::Player(opponent)
                ),
                Some(2.5),
                "each live opponent is accepted by the engine player matcher"
            );
        }
        assert_eq!(
            exact_pending_impact(&mut state, ability, legal, TargetRef::Player(PlayerId(0))),
            None,
            "a supplied legal controller remains unmatched in a three-player Opponent selector"
        );
    }

    #[test]
    fn exact_pending_player_impact_keeps_live_roots_and_fixed_parent_continuations_separate() {
        let mut state = GameState::new_two_player(7);
        let source = hand_source(&mut state, 420);
        let legal = vec![
            TargetRef::Player(PlayerId(0)),
            TargetRef::Player(PlayerId(1)),
        ];
        let creature = create_object(
            &mut state,
            CardId(421),
            PlayerId(0),
            "root quantity reach guard".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Creature];
        let root = direct_draw(
            source,
            QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount {
                    filter: TargetFilter::Typed(TypedFilter::creature()),
                },
            },
        );
        assert_eq!(
            exact_pending_impact(
                &mut state,
                root.clone(),
                legal.clone(),
                TargetRef::Player(PlayerId(1))
            ),
            Some(1.25),
            "a live root quantity is previewed at target declaration"
        );

        let fixed_child = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::ParentTarget,
                filter: None,
                selection: CardSelectionMode::Chosen,
                unless_filter: None,
            },
            Vec::new(),
            source,
            PlayerId(0),
        );
        let chain = root.clone().sub_ability(fixed_child.clone());
        assert_eq!(
            exact_pending_impact(
                &mut state,
                chain,
                legal.clone(),
                TargetRef::Player(PlayerId(1))
            ),
            Some(-1.75),
            "a fixed ParentTarget continuation is the only permitted owned child"
        );

        for quantity in [
            QuantityExpr::Ref {
                qty: QuantityRef::HandSize {
                    player: PlayerScope::Controller,
                },
            },
            QuantityExpr::Ref {
                qty: QuantityRef::GraveyardSize {
                    player: PlayerScope::Controller,
                },
            },
            QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount {
                    filter: TargetFilter::Typed(TypedFilter::creature()),
                },
            },
        ] {
            let child = ResolvedAbility::new(
                Effect::Discard {
                    count: quantity,
                    target: TargetFilter::ParentTarget,
                    filter: None,
                    selection: CardSelectionMode::Chosen,
                    unless_filter: None,
                },
                Vec::new(),
                source,
                PlayerId(0),
            );
            assert_eq!(
                exact_pending_impact(
                    &mut state,
                    root.clone().sub_ability(child),
                    legal.clone(),
                    TargetRef::Player(PlayerId(1))
                ),
                None,
                "state-reading continuation quantities cannot be simulated"
            );
        }
        let contextual = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::Controller,
            },
            Vec::new(),
            source,
            PlayerId(0),
        );
        assert_eq!(
            exact_pending_impact(
                &mut state,
                root.clone().sub_ability(contextual),
                legal.clone(),
                TargetRef::Player(PlayerId(1))
            ),
            Some(1.25),
            "a controller rider is independent and deliberately ignored"
        );
        for controller in [
            ControllerRef::ParentTargetController,
            ControllerRef::ParentTargetOwner,
        ] {
            let contextual_parent = ResolvedAbility::new(
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Typed(TypedFilter {
                        type_filters: Vec::new(),
                        controller: Some(controller),
                        properties: Vec::new(),
                    }),
                },
                Vec::new(),
                source,
                PlayerId(0),
            );
            assert_eq!(
                exact_pending_impact(
                    &mut state,
                    root.clone().sub_ability(contextual_parent),
                    legal.clone(),
                    TargetRef::Player(PlayerId(1)),
                ),
                None,
                "contextual parent controller/owner children cannot consume selected-player ownership"
            );
        }
        let independent_player_child = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Player,
            },
            Vec::new(),
            source,
            PlayerId(0),
        );
        assert_eq!(
            exact_pending_impact(
                &mut state,
                root.clone().sub_ability(independent_player_child),
                legal.clone(),
                TargetRef::Player(PlayerId(1)),
            ),
            None,
            "an independently targeting Player child has no ParentTarget ownership"
        );
        let unsupported_child = ResolvedAbility::new(Effect::NoOp, Vec::new(), source, PlayerId(0));
        assert_eq!(
            exact_pending_impact(
                &mut state,
                root.clone().sub_ability(unsupported_child),
                legal.clone(),
                TargetRef::Player(PlayerId(1)),
            ),
            None,
            "an unsupported owning child cannot leave a partial exact sum"
        );
        let mut sibling = fixed_child;
        sibling.sub_link = SubAbilityLink::SequentialSibling;
        assert_eq!(
            exact_pending_impact(
                &mut state,
                root.clone().sub_ability(sibling),
                legal,
                TargetRef::Player(PlayerId(1))
            ),
            None,
            "an independent sibling has no owned ParentTarget provenance"
        );
        let root_parent = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::ParentTarget,
            },
            Vec::new(),
            source,
            PlayerId(0),
        );
        assert_eq!(
            exact_pending_impact(
                &mut state,
                root_parent,
                vec![
                    TargetRef::Player(PlayerId(0)),
                    TargetRef::Player(PlayerId(1))
                ],
                TargetRef::Player(PlayerId(1)),
            ),
            None,
            "a root ParentTarget has no antecedent to bind"
        );
        let contextual_parent = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Typed(TypedFilter {
                    type_filters: Vec::new(),
                    controller: Some(ControllerRef::ParentTargetController),
                    properties: Vec::new(),
                }),
            },
            Vec::new(),
            source,
            PlayerId(0),
        );
        assert_eq!(
            exact_pending_impact(
                &mut state,
                contextual_parent,
                vec![
                    TargetRef::Player(PlayerId(0)),
                    TargetRef::Player(PlayerId(1))
                ],
                TargetRef::Player(PlayerId(1)),
            ),
            None,
            "contextual parent-controller references are not direct root selectors"
        );
        let unknown_child = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
                target: TargetFilter::ParentTarget,
                filter: None,
                selection: CardSelectionMode::Chosen,
                unless_filter: None,
            },
            Vec::new(),
            source,
            PlayerId(0),
        );
        assert_eq!(
            exact_pending_impact(
                &mut state,
                root.sub_ability(unknown_child),
                vec![
                    TargetRef::Player(PlayerId(0)),
                    TargetRef::Player(PlayerId(1))
                ],
                TargetRef::Player(PlayerId(1)),
            ),
            None,
            "mixed known and unknown owning contributions cannot be partially summed"
        );
    }

    #[test]
    fn exact_pending_player_impact_rejects_execution_modifiers_and_unknown_contributions() {
        let mut state = GameState::new_two_player(7);
        let source = hand_source(&mut state, 430);
        let legal = vec![
            TargetRef::Player(PlayerId(0)),
            TargetRef::Player(PlayerId(1)),
        ];
        let root = direct_draw(source, QuantityExpr::Fixed { value: 2 });
        assert_eq!(
            exact_pending_impact(
                &mut state,
                root.clone(),
                legal.clone(),
                TargetRef::Player(PlayerId(1))
            ),
            Some(2.5),
            "the plain eligible root reaches exact classification before each mutation"
        );
        let fixed_parent_discard = || {
            ResolvedAbility::new(
                Effect::Discard {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::ParentTarget,
                    filter: None,
                    selection: CardSelectionMode::Chosen,
                    unless_filter: None,
                },
                Vec::new(),
                source,
                PlayerId(0),
            )
        };
        macro_rules! assert_ineligible_on_root_and_fixed_child {
            ($name:literal, $mutate:expr) => {{
                let mut mutated_root = root.clone();
                $mutate(&mut mutated_root);
                assert_eq!(
                    exact_pending_impact(
                        &mut state,
                        mutated_root,
                        legal.clone(),
                        TargetRef::Player(PlayerId(1)),
                    ),
                    None,
                    concat!($name, " must reject the otherwise-exact root"),
                );
                let mut mutated_child = fixed_parent_discard();
                $mutate(&mut mutated_child);
                assert_eq!(
                    exact_pending_impact(
                        &mut state,
                        root.clone().sub_ability(mutated_child),
                        legal.clone(),
                        TargetRef::Player(PlayerId(1)),
                    ),
                    None,
                    concat!(
                        $name,
                        " must reject the otherwise-exact fixed ParentTarget child"
                    ),
                );
            }};
        }
        assert_ineligible_on_root_and_fixed_child!("kind", |node: &mut ResolvedAbility| {
            node.kind = AbilityKind::Activated;
        });
        assert_ineligible_on_root_and_fixed_child!("targets", |node: &mut ResolvedAbility| {
            node.targets.push(TargetRef::Player(PlayerId(0)));
        });
        assert_ineligible_on_root_and_fixed_child!("duration", |node: &mut ResolvedAbility| {
            node.duration = Some(Duration::UntilEndOfTurn);
        });
        assert_ineligible_on_root_and_fixed_child!(
            "optional_targeting",
            |node: &mut ResolvedAbility| {
                node.optional_targeting = true;
            }
        );
        assert_ineligible_on_root_and_fixed_child!("optional", |node: &mut ResolvedAbility| {
            node.optional = true;
        });
        assert_ineligible_on_root_and_fixed_child!(
            "optional_player",
            |node: &mut ResolvedAbility| {
                node.optional_player = Some(TargetFilter::Controller);
            }
        );
        assert_ineligible_on_root_and_fixed_child!("optional_for", |node: &mut ResolvedAbility| {
            node.optional_for = Some(OpponentMayScope::AnyOpponent);
        });
        assert_ineligible_on_root_and_fixed_child!("multi_target", |node: &mut ResolvedAbility| {
            node.multi_target = Some(MultiTargetSpec::fixed(1, 1));
        });
        assert_ineligible_on_root_and_fixed_child!(
            "target_constraints",
            |node: &mut ResolvedAbility| {
                node.target_constraints
                    .push(TargetSelectionConstraint::DifferentTargetPlayers);
            }
        );
        assert_ineligible_on_root_and_fixed_child!(
            "target_choice_timing",
            |node: &mut ResolvedAbility| {
                node.target_choice_timing = TargetChoiceTiming::Resolution;
            }
        );
        assert_ineligible_on_root_and_fixed_child!(
            "selected_mode_labels",
            |node: &mut ResolvedAbility| {
                node.selected_mode_labels.push("selected mode".to_string());
            }
        );
        assert_ineligible_on_root_and_fixed_child!(
            "target_chooser",
            |node: &mut ResolvedAbility| {
                node.target_chooser = Some(TargetFilter::Any);
            }
        );
        assert_ineligible_on_root_and_fixed_child!("else_ability", |node: &mut ResolvedAbility| {
            node.else_ability = Some(Box::new(root.clone()));
        });
        assert_ineligible_on_root_and_fixed_child!("condition", |node: &mut ResolvedAbility| {
            node.condition = Some(AbilityCondition::IsMonarch);
        });
        assert_ineligible_on_root_and_fixed_child!(
            "modal_instruction_ordinal",
            |node: &mut ResolvedAbility| {
                node.modal_instruction_ordinal = Some(0);
            }
        );
        assert_ineligible_on_root_and_fixed_child!(
            "detached_remainder",
            |node: &mut ResolvedAbility| {
                node.detached_remainder = DetachedRemainder::HoldsPublisher;
            }
        );
        assert_ineligible_on_root_and_fixed_child!("repeat_for", |node: &mut ResolvedAbility| {
            node.repeat_for = Some(QuantityExpr::Fixed { value: 1 });
        });
        assert_ineligible_on_root_and_fixed_child!("min_x_value", |node: &mut ResolvedAbility| {
            node.min_x_value = 1;
        });
        assert_ineligible_on_root_and_fixed_child!("announced_x", |node: &mut ResolvedAbility| {
            node.announced_x = Some(QuantityExpr::Fixed { value: 1 });
        });
        assert_ineligible_on_root_and_fixed_child!(
            "cant_be_copied",
            |node: &mut ResolvedAbility| {
                node.cant_be_copied = true;
            }
        );
        assert_ineligible_on_root_and_fixed_child!(
            "copy_count_status",
            |node: &mut ResolvedAbility| {
                node.copy_count_status = CopyCountStatus::Finalized;
            }
        );
        assert_ineligible_on_root_and_fixed_child!(
            "forward_result",
            |node: &mut ResolvedAbility| {
                node.forward_result = true;
            }
        );
        assert_ineligible_on_root_and_fixed_child!("unless_pay", |node: &mut ResolvedAbility| {
            node.unless_pay = Some(UnlessPayModifier {
                cost: AbilityCost::Tap,
                payer: TargetFilter::Any,
            });
        });
        assert_ineligible_on_root_and_fixed_child!("distribution", |node: &mut ResolvedAbility| {
            node.distribution = Some(Vec::new());
        });
        assert_ineligible_on_root_and_fixed_child!("distribute", |node: &mut ResolvedAbility| {
            node.distribute = Some(DistributionUnit::Life);
        });
        assert_ineligible_on_root_and_fixed_child!("player_scope", |node: &mut ResolvedAbility| {
            node.player_scope = Some(PlayerFilter::Opponent);
        });
        assert_ineligible_on_root_and_fixed_child!(
            "starting_with",
            |node: &mut ResolvedAbility| {
                node.starting_with = Some(ControllerRef::You);
            }
        );
        assert_ineligible_on_root_and_fixed_child!("chosen_x", |node: &mut ResolvedAbility| {
            node.chosen_x = Some(1);
        });
        assert_ineligible_on_root_and_fixed_child!(
            "chosen_players",
            |node: &mut ResolvedAbility| {
                node.chosen_players.push(PlayerId(0));
            }
        );
        assert_ineligible_on_root_and_fixed_child!("repeat_until", |node: &mut ResolvedAbility| {
            node.repeat_until = Some(RepeatContinuation::ControllerChoice);
        });
        assert_ineligible_on_root_and_fixed_child!(
            "replacement_applied",
            |node: &mut ResolvedAbility| {
                node.replacement_applied
                    .insert(AppliedReplacementKey::floating(0));
            }
        );
        assert_ineligible_on_root_and_fixed_child!(
            "target_selection_mode",
            |node: &mut ResolvedAbility| {
                node.target_selection_mode = TargetSelectionMode::Random;
            }
        );
        assert_ineligible_on_root_and_fixed_child!(
            "sibling_condition",
            |node: &mut ResolvedAbility| {
                node.sibling_condition = SiblingCondition::ReplicatedOrBranch;
            }
        );
        assert_ineligible_on_root_and_fixed_child!("modal", |node: &mut ResolvedAbility| {
            node.modal = Some(ModalChoice::default());
        });
        assert_ineligible_on_root_and_fixed_child!(
            "mode_abilities",
            |node: &mut ResolvedAbility| {
                node.mode_abilities
                    .push(AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp));
            }
        );
        assert_ineligible_on_root_and_fixed_child!(
            "parent_target_missing_reason",
            |node: &mut ResolvedAbility| {
                node.parent_target_missing_reason = Some(ParentTargetMissingReason::Dig);
            }
        );
        for (name, filter, selection, unless_filter) in [
            (
                "filter",
                Some(TargetFilter::Any),
                CardSelectionMode::Chosen,
                None,
            ),
            (
                "unless filter",
                None,
                CardSelectionMode::Chosen,
                Some(TargetFilter::Any),
            ),
            ("random selection", None, CardSelectionMode::Random, None),
        ] {
            let restricted = ResolvedAbility::new(
                Effect::Discard {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Player,
                    filter,
                    selection,
                    unless_filter,
                },
                Vec::new(),
                source,
                PlayerId(0),
            );
            assert_eq!(
                exact_pending_impact(
                    &mut state,
                    restricted,
                    legal.clone(),
                    TargetRef::Player(PlayerId(1))
                ),
                None,
                "restricted discard {name} is not a simple chosen-card loss preview"
            );
        }
        let unknown = direct_draw(
            source,
            QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: "X".to_string(),
                },
            },
        );
        assert_eq!(
            exact_pending_impact(&mut state, unknown, legal, TargetRef::Player(PlayerId(1))),
            None,
            "an unknown-only owning contribution must not manufacture an exact magnitude"
        );
    }

    #[test]
    fn targeted_player_impact_uses_live_object_count_without_double_affiliation() {
        let mut state = GameState::new_two_player(7);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Congregate source".to_string(),
            Zone::Hand,
        );
        let gain = Effect::GainLife {
            amount: creature_count_amount(),
            player: TargetFilter::Player,
        };
        let effects = vec![&gain];

        assert_eq!(
            targeted_player_impact_in(
                &state,
                Some(PlayerId(0)),
                Some(source),
                &effects,
                PlayerId(0),
            ),
            Some(0.0),
            "zero is a known recipient impact, not the legacy fabricated +1 magnitude"
        );
        assert_eq!(
            targeted_player_impact_in(
                &state,
                Some(PlayerId(0)),
                Some(source),
                &effects,
                PlayerId(1),
            ),
            Some(0.0)
        );

        let creature = create_object(
            &mut state,
            CardId(101),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .expect("the counted creature exists")
            .card_types
            .core_types = vec![CoreType::Creature];

        for recipient in [PlayerId(0), PlayerId(1)] {
            assert_eq!(
                targeted_player_impact_in(
                    &state,
                    Some(PlayerId(0)),
                    Some(source),
                    &effects,
                    recipient,
                ),
                Some(0.3),
                "this helper is recipient-relative; affiliation belongs to its caller"
            );
        }

        let choose_self = player_target_score(
            &state,
            source,
            gain.clone(),
            GameAction::ChooseTarget {
                target: Some(TargetRef::Player(PlayerId(0))),
            },
        );
        let choose_opponent = player_target_score(
            &state,
            source,
            gain.clone(),
            GameAction::ChooseTarget {
                target: Some(TargetRef::Player(PlayerId(1))),
            },
        );
        assert!(
            choose_self > choose_opponent,
            "the production ChooseTarget policy must apply the one affiliation flip and keep life gain"
        );

        let select_self = player_target_score(
            &state,
            source,
            gain.clone(),
            GameAction::SelectTargets {
                targets: vec![TargetRef::Player(PlayerId(0))],
            },
        );
        let select_opponent = player_target_score(
            &state,
            source,
            gain,
            GameAction::SelectTargets {
                targets: vec![TargetRef::Player(PlayerId(1))],
            },
        );
        assert!(
            select_self > select_opponent,
            "the production SelectTargets policy must preserve the same recipient polarity"
        );

        let lose = Effect::LoseLife {
            amount: creature_count_amount(),
            target: Some(TargetFilter::Player),
        };
        let lose_self = player_target_score(
            &state,
            source,
            lose.clone(),
            GameAction::ChooseTarget {
                target: Some(TargetRef::Player(PlayerId(0))),
            },
        );
        let lose_opponent = player_target_score(
            &state,
            source,
            lose,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Player(PlayerId(1))),
            },
        );
        assert!(
            lose_opponent > lose_self,
            "the same recipient-relative magnitude must make life loss prefer an opponent"
        );
    }

    #[test]
    fn contextual_life_gain_does_not_bind_the_chosen_player() {
        let mut state = GameState::new_two_player(7);
        let source = create_object(
            &mut state,
            CardId(300),
            PlayerId(0),
            "Targeted harm source".to_string(),
            Zone::Hand,
        );
        let harm = Effect::LoseLife {
            amount: QuantityExpr::Fixed { value: 1 },
            target: Some(TargetFilter::Player),
        };
        let gain = Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        };
        let effects = vec![&harm, &gain];
        assert_eq!(
            targeted_player_impact_in(
                &state,
                Some(PlayerId(0)),
                Some(source),
                &effects,
                PlayerId(0)
            ),
            Some(-0.15),
            "the controller gain is not attributed to the self target"
        );
        assert_eq!(
            targeted_player_impact_in(
                &state,
                Some(PlayerId(0)),
                Some(source),
                &effects,
                PlayerId(1)
            ),
            Some(-0.15),
            "the same contextual gain is not attributed to the opponent target"
        );

        let draw = Effect::Draw {
            count: QuantityExpr::Fixed { value: 2 },
            target: TargetFilter::Player,
        };
        let discard = Effect::Discard {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::ParentTarget,
            filter: None,
            selection: engine::types::ability::CardSelectionMode::Chosen,
            unless_filter: None,
        };
        assert_eq!(
            targeted_player_impact_in(
                &state,
                Some(PlayerId(0)),
                Some(source),
                &[&draw],
                PlayerId(1)
            ),
            Some(2.5),
            "a chosen-player Draw retains its live quantity in targeted scoring"
        );
        assert_eq!(
            targeted_player_impact_in(
                &state,
                Some(PlayerId(0)),
                Some(source),
                &[&discard],
                PlayerId(1)
            ),
            None,
            "a flat ParentTarget has no target-graph ownership"
        );
        assert_eq!(
            targeted_player_impact_in_with_bound_parent_target(
                &state,
                Some(PlayerId(0)),
                Some(source),
                &[&discard],
                PlayerId(1),
                PlayerId(1),
            ),
            Some(-1.5),
            "the explicit direct-root binding restores the ParentTarget recipient"
        );

        let mut ability = ResolvedAbility::new(harm, Vec::new(), source, PlayerId(0));
        ability.sub_ability = Some(Box::new(ResolvedAbility::new(
            gain,
            Vec::new(),
            source,
            PlayerId(0),
        )));
        let choose_self = player_target_score_for_ability(
            &state,
            source,
            ability.clone(),
            GameAction::ChooseTarget {
                target: Some(TargetRef::Player(PlayerId(0))),
            },
        );
        let choose_opponent = player_target_score_for_ability(
            &state,
            source,
            ability.clone(),
            GameAction::ChooseTarget {
                target: Some(TargetRef::Player(PlayerId(1))),
            },
        );
        let select_self = player_target_score_for_ability(
            &state,
            source,
            ability.clone(),
            GameAction::SelectTargets {
                targets: vec![TargetRef::Player(PlayerId(0))],
            },
        );
        let select_opponent = player_target_score_for_ability(
            &state,
            source,
            ability,
            GameAction::SelectTargets {
                targets: vec![TargetRef::Player(PlayerId(1))],
            },
        );
        assert!(
            choose_opponent > choose_self,
            "ChooseTarget keeps the harmful target preference"
        );
        assert!(
            select_opponent > select_self,
            "SelectTargets keeps the harmful target preference"
        );
    }

    #[test]
    fn targeted_player_impact_previews_explicit_zone_counts_but_cast_stability_rejects_them() {
        let mut state = GameState::new_two_player(7);
        let source = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Zone-count source".to_string(),
            Zone::Hand,
        );
        let graveyard_creature = create_object(
            &mut state,
            CardId(201),
            PlayerId(0),
            "Graveyard Bear".to_string(),
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&graveyard_creature)
            .expect("the explicit-zone object exists")
            .card_types
            .core_types = vec![CoreType::Creature];
        let amount = QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter::creature().properties(vec![
                    FilterProp::InZone {
                        zone: Zone::Graveyard,
                    },
                ])),
            },
        };
        let gain = Effect::GainLife {
            amount: amount.clone(),
            player: TargetFilter::Player,
        };

        assert_eq!(
            targeted_player_impact_in(
                &state,
                Some(PlayerId(0)),
                Some(source),
                &[&gain],
                PlayerId(0),
            ),
            Some(0.15),
            "live preview accepts an explicit-zone quantity for recipient valuation"
        );
        assert!(
            !quantity_is_cast_stable_for_pre_cast(&amount),
            "the pre-cast veto deliberately has a narrower stability contract"
        );
    }

    #[test]
    fn unknown_quantity_retains_legacy_directional_weight() {
        let unknown = QuantityExpr::Ref {
            qty: QuantityRef::Variable {
                name: "X".to_string(),
            },
        };
        assert_eq!(quantity_weight(None, None, None, &unknown, 0.15), 0.15);
    }
}

#[cfg(test)]
mod pump_polarity_tests {
    use super::*;
    use engine::parser::oracle::parse_oracle_text;
    use engine::types::ability::AbilityKind;

    fn pump(power: PtValue, toughness: PtValue) -> Effect {
        Effect::Pump {
            power,
            toughness,
            target: TargetFilter::Any,
        }
    }

    /// "gets -X/-X" — the parser puts the sign in the VARIABLE NAME, so the
    /// pre-fix `matches!(PtValue::Variable(_))` non-negative assumption read it
    /// as a buff and `anti_self_harm` aimed it at the AI's own creature.
    #[test]
    fn negative_variable_pump_is_harmful() {
        assert_eq!(
            effect_polarity(&pump(
                PtValue::Variable("-X".to_string()),
                PtValue::Variable("-X".to_string()),
            )),
            EffectPolarity::Harmful
        );
    }

    /// The discriminator: the same variable carrier WITHOUT the sign is still a
    /// buff, so the fix reads the name rather than blanket-condemning variables.
    #[test]
    fn positive_variable_pump_is_beneficial() {
        assert_eq!(
            effect_polarity(&pump(
                PtValue::Variable("X".to_string()),
                PtValue::Variable("X".to_string()),
            )),
            EffectPolarity::Beneficial
        );
    }

    /// A single negative slot is enough: "+2/-2" harms the creature it lands on.
    #[test]
    fn one_negative_slot_makes_the_pump_harmful() {
        assert_eq!(
            effect_polarity(&pump(
                PtValue::Fixed(2),
                PtValue::Variable("-X".to_string()),
            )),
            EffectPolarity::Harmful
        );
    }

    /// The parser's SECOND negative encoding: "gets -X/-X, where X is the number
    /// of …" lands as `Quantity(Multiply { factor: -1, .. })`, not as a signed
    /// variable name. 103 Pump/PumpAll slots in card-data carry a `Multiply`.
    #[test]
    fn where_x_negative_quantity_pump_is_harmful() {
        let negated = PtValue::Quantity(QuantityExpr::Multiply {
            factor: -1,
            inner: Box::new(QuantityExpr::Ref {
                qty: engine::types::ability::QuantityRef::Variable {
                    name: "X".to_string(),
                },
            }),
        });
        assert_eq!(
            effect_polarity(&pump(negated.clone(), negated)),
            EffectPolarity::Harmful
        );
    }

    /// Sibling reach-guard: a POSITIVE multiplier ("twice the number of …") is
    /// still a buff, so the arm keys on the factor's sign, not on `Multiply`.
    #[test]
    fn positive_multiply_quantity_pump_is_beneficial() {
        let doubled = PtValue::Quantity(QuantityExpr::Multiply {
            factor: 2,
            inner: Box::new(QuantityExpr::Ref {
                qty: engine::types::ability::QuantityRef::Variable {
                    name: "X".to_string(),
                },
            }),
        });
        assert_eq!(
            effect_polarity(&pump(doubled.clone(), doubled)),
            EffectPolarity::Beneficial
        );
    }

    /// Production-parser reach guard: Slice from the Shadows' real Oracle text
    /// parses to `Pump { power: Variable("-X"), toughness: Variable("-X") }`
    /// (confirmed against `data/card-data.json`), and that shape must read
    /// Harmful through the classifier the AI's target scorer uses.
    #[test]
    fn slice_from_the_shadows_real_parse_is_harmful() {
        let parsed = parse_oracle_text(
            "Target creature gets -X/-X until end of turn.",
            "Slice from the Shadows",
            &[],
            &["Instant".to_string()],
            &[],
        );
        let spell = parsed
            .abilities
            .iter()
            .find(|a| a.kind == AbilityKind::Spell)
            .expect("Slice from the Shadows parses to a spell ability");
        assert!(
            matches!(
                &*spell.effect,
                Effect::Pump { power: PtValue::Variable(p), toughness: PtValue::Variable(t), .. }
                    if p == "-X" && t == "-X"
            ),
            "the sign lives in the variable name: {:?}",
            spell.effect
        );
        assert_eq!(effect_polarity(&spell.effect), EffectPolarity::Harmful);
    }
}

#[cfg(test)]
mod counter_polarity_tests {
    use super::*;
    use engine::types::keywords::KeywordKind;

    fn put(counter_type: CounterType) -> Effect {
        Effect::PutCounter {
            counter_type,
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Any,
        }
    }

    /// CR 122.1c: shield counters only ever protect their bearer. Pre-fix these
    /// fell through the wildcard to `Contextual` (impact 0.0), so the AI put
    /// them on opponents' creatures.
    #[test]
    fn shield_counter_is_beneficial() {
        assert_eq!(
            effect_polarity(&put(CounterType::Shield)),
            EffectPolarity::Beneficial
        );
    }

    /// CR 122.1d: a stun counter stops the permanent from untapping.
    #[test]
    fn stun_counter_is_harmful() {
        assert_eq!(
            effect_polarity(&put(CounterType::Stun)),
            EffectPolarity::Harmful
        );
    }

    /// CR 122.1b: a granted keyword upgrades the bearer …
    #[test]
    fn keyword_counter_is_beneficial() {
        assert_eq!(
            effect_polarity(&put(CounterType::Keyword(KeywordKind::Flying))),
            EffectPolarity::Beneficial
        );
    }

    /// … except CR 702.147a decayed, the one drawback keyword a counter grants.
    #[test]
    fn decayed_keyword_counter_is_harmful() {
        assert_eq!(
            effect_polarity(&put(CounterType::Keyword(KeywordKind::Decayed))),
            EffectPolarity::Harmful
        );
    }

    /// CR 122.1a: the asymmetric counter's sign comes from its own payload.
    #[test]
    fn power_toughness_counter_sign_derived() {
        assert_eq!(
            effect_polarity(&put(CounterType::PowerToughness {
                power: 1,
                toughness: 0,
            })),
            EffectPolarity::Beneficial
        );
        assert_eq!(
            effect_polarity(&put(CounterType::PowerToughness {
                power: 0,
                toughness: -1,
            })),
            EffectPolarity::Harmful
        );
        // Mixed signs are a trade-off, not a direction.
        assert_eq!(
            effect_polarity(&put(CounterType::PowerToughness {
                power: 1,
                toughness: -1,
            })),
            EffectPolarity::Contextual
        );
    }

    /// Removing a counter inverts its polarity, and the inversion now reaches
    /// the newly-signed kinds too (Vampire Hexmage on a shield counter).
    #[test]
    fn removing_a_shield_counter_is_harmful() {
        assert_eq!(
            effect_polarity(&Effect::RemoveCounter {
                counter_type: Some(CounterType::Shield),
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Any,
            }),
            EffectPolarity::Harmful
        );
    }

    /// Unchanged behaviour guard: loyalty stays Contextual, so the exhaustive
    /// match did not turn the whole enum into a direction.
    #[test]
    fn loyalty_counter_stays_contextual() {
        assert_eq!(
            effect_polarity(&put(CounterType::Loyalty)),
            EffectPolarity::Contextual
        );
    }
}
