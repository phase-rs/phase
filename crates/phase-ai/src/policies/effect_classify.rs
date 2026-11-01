use engine::game::filter::{matches_target_filter, FilterContext};
use engine::game::game_object::GameObject;
use engine::game::quantity::try_resolve_quantity_in_source_context;
use engine::game::restrictions::{
    ability_confined_to_sorcery_timing, spell_confined_to_sorcery_timing,
};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ContinuousModification, ControllerRef, Duration,
    Effect, EffectScope, PerpetualGrantModification, PerpetualModification, PlayerFilter, PtValue,
    QuantityExpr, ResolvedAbility, StaticDefinition, SubAbilityLink, TapStateChange,
    TargetChoiceTiming, TargetFilter, TargetRef, TriggerDefinition, TypeFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastingVariant, GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, KeywordKind};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use super::context::PolicyContext;
use crate::cast_facts::{cast_object_for_action, collect_definition_effects, CastFacts};

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
        // A granted one-time boon carries the direction of the trigger it
        // grants: a pump boon is beneficial to its recipient, a stun-counter
        // boon (Loch Larent, granted to an opponent) is harmful. Read it
        // through the shared granted-trigger seam so target selection aims
        // beneficial boons at self and harmful boons at opponents.
        Effect::CreateBoon { trigger, .. } => boon_granted_trigger_polarity(trigger),
        Effect::Adapt { .. }
        | Effect::AdditionalPhase { .. }
        | Effect::AddPendingETBCounters { .. }
        | Effect::AddPendingEntersModifications { .. }
        // CR 701.33: abandoning an ongoing scheme turns it face down onto the
        // bottom of its owner's scheme deck — whether that is good depends on
        // the scheme (ongoing pressure vs. cycling to the next scheme).
        | Effect::AbandonScheme
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
        | Effect::NoteNumber { .. }
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
        | Effect::PhysicalContest { .. }
        | Effect::Double { .. }
        | Effect::ModifyAnnouncedX { .. }
        | Effect::DraftFromSpellbook { .. }
        | Effect::EachDealsDamageEqualToPower { .. }
        | Effect::EachPlayerCopyChosen { .. }
        | Effect::EachSourceDealsDamage { .. }
        | Effect::EmpowerJace { .. }
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
        | Effect::FlipCoins { .. }
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
        | Effect::ExileTopCardContest { .. }
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
        // CR 115.1a + CR 115.10a: only a declared prevention recipient exposes a
        // selectable target; the mass (`All`) scope is an untargeted population.
        Effect::PreventDamage {
            recipient_scope: EffectScope::Single,
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

/// What a [`TargetFilter`] can select, as three independent axes.
///
/// A filter is a *set* of legal targets, so the three composite variants are
/// exactly set operations on that set: [`TargetFilter::Or`] is a union,
/// [`TargetFilter::And`] an intersection, and [`TargetFilter::Not`] a
/// complement. Representing the domain as independent booleans rather than a
/// flat enum is what makes those three compose field-wise instead of needing a
/// hand-written combination table.
///
/// This exists because the predicates below used to answer only for
/// [`TargetFilter::Typed`] and silently returned "no" for every composite. A
/// card as ordinary as "target attacking or blocking creature" parses to an
/// `Or` of two `Typed` legs, so every consumer of `targets_creatures_only`
/// — the anti-self-harm whiff gate, removal timing — read it as
/// *not* creature-targeting and skipped its own logic entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FilterDomain {
    /// A creature could be selected.
    creatures: bool,
    /// An object that is not a creature could be selected.
    non_creature_objects: bool,
    /// A player could be selected (CR 115.4).
    players: bool,
}

impl FilterDomain {
    /// Nothing is selectable.
    const NOTHING: Self = Self {
        creatures: false,
        non_creature_objects: false,
        players: false,
    };
    /// Anything is selectable — also the fail-open answer for a filter whose
    /// domain is not statically knowable.
    const ANYTHING: Self = Self {
        creatures: true,
        non_creature_objects: true,
        players: true,
    };
    /// Some object, of statically unknown type. Runtime-bound object references
    /// (`SelfRef`, `LastCreated`, a tracked set) land here: they never name a
    /// player, but which object they resolve to is not knowable from the filter.
    const SOME_OBJECT: Self = Self {
        creatures: true,
        non_creature_objects: true,
        players: false,
    };
    /// Exactly one player.
    const SOME_PLAYER: Self = Self {
        creatures: false,
        non_creature_objects: false,
        players: true,
    };
    /// An object on the stack (CR 405.1) — never a creature permanent.
    const STACK_OBJECT: Self = Self {
        creatures: false,
        non_creature_objects: true,
        players: false,
    };

    /// Union — [`TargetFilter::Or`].
    fn union(self, other: Self) -> Self {
        Self {
            creatures: self.creatures || other.creatures,
            non_creature_objects: self.non_creature_objects || other.non_creature_objects,
            players: self.players || other.players,
        }
    }

    /// Intersection — [`TargetFilter::And`].
    fn intersect(self, other: Self) -> Self {
        Self {
            creatures: self.creatures && other.creatures,
            non_creature_objects: self.non_creature_objects && other.non_creature_objects,
            players: self.players && other.players,
        }
    }
}

/// The domain of a [`TypeFilter`] — one CONJUNCT of a `Typed` filter's
/// `type_filters` list — evaluated structurally, the same existential-domain
/// approach `FilterDomain` takes one level up. Independent booleans rather
/// than a flat enum for the same reason: `AnyOf` is a union and the list-level
/// conjunction (see `filter_domain`'s `Typed` arm) is an intersection, and
/// both compose field-wise this way with no hand-written combination table.
///
/// Review finding on this PR: the predecessor of this type answered only for
/// a LITERAL `TypeFilter::Creature` and treated every other variant as
/// creature-EXCLUDING by omission. `Permanent`, `Card`, `Any` and `AnyOf` are
/// not narrower categories that merely CO-OCCUR with `Creature` on some
/// cards — CR 608.2b makes `AnyOf` an explicit disjunction, and
/// `engine::game::filter::type_filter_matches` implements `Permanent` as a
/// union that lists `CoreType::Creature` as one of its six admitted core
/// types, and `Card`/`Any` as unconditionally `true` — so a creature is
/// STRUCTURALLY one of the alternatives these four accept, not an incidental
/// overlap. `filter_admits_creature(TargetFilter::Typed { type_filters:
/// vec![TypeFilter::Permanent], .. })` must therefore be `true`, and it was
/// `false`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TypeDomain {
    /// A creature could satisfy this `TypeFilter`.
    admits_creature: bool,
    /// A non-creature object could satisfy this `TypeFilter`.
    admits_non_creature: bool,
}

impl TypeDomain {
    /// Satisfying this ALONE proves creature — never a non-creature.
    const CREATURE_ONLY: Self = Self {
        admits_creature: true,
        admits_non_creature: false,
    };
    /// Structurally incapable of admitting a creature.
    const NON_CREATURE_ONLY: Self = Self {
        admits_creature: false,
        admits_non_creature: true,
    };
    /// Admits nothing — the identity element for [`Self::union`] (an empty or
    /// not-yet-folded disjunction), symmetric with [`Self::UNCONSTRAINED`]
    /// being the identity for [`Self::intersect`].
    const NEITHER: Self = Self {
        admits_creature: false,
        admits_non_creature: false,
    };
    /// Could go either way — the safe default whenever a `TypeFilter` cannot
    /// be resolved to one of the two categorical extremes above from its
    /// shape alone. Never produces a false `is_creature_only`, and never
    /// produces a false "no creature possible" whiff-detector veto.
    const EITHER: Self = Self {
        admits_creature: true,
        admits_non_creature: true,
    };
    /// The identity element for [`Self::intersect`] — the fold seed for a
    /// non-empty conjunction, standing in for "no constraint imposed yet".
    const UNCONSTRAINED: Self = Self::EITHER;

    /// Union — [`TypeFilter::AnyOf`] (CR 608.2b: disjunction).
    fn union(self, other: Self) -> Self {
        Self {
            admits_creature: self.admits_creature || other.admits_creature,
            admits_non_creature: self.admits_non_creature || other.admits_non_creature,
        }
    }

    /// Intersection — one `TypeFilter` narrowing another inside the SAME
    /// `type_filters` conjunction (e.g. `[Artifact, Creature]`, "target
    /// artifact creature"). Every conjunct must be satisfied by the same
    /// object at once, so a category this predicate cannot verify admits
    /// creatures (like plain `Artifact`) does not by itself disqualify a
    /// LITERAL `Creature` conjunct sitting beside it: `intersect` only
    /// narrows `admits_creature` to `false` when SOME conjunct is
    /// `NON_CREATURE_ONLY` — provably, not merely unverified.
    fn intersect(self, other: Self) -> Self {
        Self {
            admits_creature: self.admits_creature && other.admits_creature,
            admits_non_creature: self.admits_non_creature && other.admits_non_creature,
        }
    }
}

/// The domain of one `TypeFilter` conjunct. Exhaustive over `TypeFilter` with
/// no wildcard arm, mirroring `filter_domain`'s own discipline one level down.
fn type_filter_domain(tf: &TypeFilter) -> TypeDomain {
    match tf {
        TypeFilter::Creature => TypeDomain::CREATURE_ONLY,

        // CR 300.1: a card resolving as one of these two spell-only types is
        // never simultaneously a creature permanent — no printed card
        // combines Instant or Sorcery with Creature, and this engine's
        // `core_types` set does not carry both at once for any live object.
        // Provably creature-excluding, unlike the permanent types below.
        TypeFilter::Instant | TypeFilter::Sorcery => TypeDomain::NON_CREATURE_ONLY,

        // These five permanent types are NOT structurally creature-excluding
        // — real printed cards double them with Creature (Dryad Arbor is a
        // Land Creature; artifact creatures and enchantment creatures are
        // commonplace; creature planeswalkers and creature battles exist).
        // `EITHER` is the correct domain, not an over-cautious fallback: a
        // bare `TypeFilter::Artifact` conjunct genuinely admits BOTH a plain
        // artifact and an artifact creature.
        TypeFilter::Land
        | TypeFilter::Artifact
        | TypeFilter::Enchantment
        | TypeFilter::Planeswalker
        | TypeFilter::Battle
        | TypeFilter::Kindred => TypeDomain::EITHER,

        // CR 403.3 (as implemented, zone-agnostic — see
        // `engine::game::filter::type_filter_matches`): `Permanent` is a
        // union over six core types that explicitly lists `Creature` as one
        // of them, so it admits creatures by construction, not by omission.
        TypeFilter::Permanent => TypeDomain::EITHER,
        // Unconditionally `true` in `type_filter_matches` — admits anything.
        TypeFilter::Card | TypeFilter::Any => TypeDomain::EITHER,

        // CR 608.2b: disjunction — the domain is the union of every branch.
        // `AnyOf([Creature, Enchantment])` is the review's worked example:
        // union(CREATURE_ONLY, EITHER) = EITHER, so `admits_creature` is
        // `true` and `is_creature_only` stays `false` — reached exactly.
        TypeFilter::AnyOf(filters) => filters
            .iter()
            .map(type_filter_domain)
            .fold(TypeDomain::NEITHER, TypeDomain::union),

        // CR 205.2a + CR 205.3: negation. One case resolves cleanly without
        // deeper semantic modeling: `Non(Creature)` ("noncreature") — EVERY
        // creature trivially satisfies the un-negated `Creature`, so NO
        // creature can satisfy its negation. Every other inner filter is at
        // best `EITHER` under this same function, which carries no universal
        // ("does EVERY creature satisfy it") fact to negate — so the general
        // case falls open to `EITHER` rather than guess. `Subtype` is the
        // same shape as the general `Non` case: MTG has 250+ creature
        // subtypes (CR 205.3m) and at least as many non-creature ones with no
        // catalog available here to tell them apart, so `EITHER` is the only
        // sound answer without one.
        TypeFilter::Non(inner) => match inner.as_ref() {
            TypeFilter::Creature => TypeDomain::NON_CREATURE_ONLY,
            _ => TypeDomain::EITHER,
        },
        TypeFilter::Subtype(_) => TypeDomain::EITHER,
    }
}

/// The domain of a [`TargetFilter`], evaluated structurally.
///
/// Exhaustive over `TargetFilter` with no wildcard arm, so a new variant is a
/// compile error here rather than a silent fail-open at the three call sites.
pub(crate) fn filter_domain(filter: &TargetFilter) -> FilterDomain {
    match filter {
        TargetFilter::None => FilterDomain::NOTHING,

        // CR 115.4: "any target" admits creatures, players, planeswalkers and
        // battles.
        TargetFilter::Any => FilterDomain::ANYTHING,

        // CR 115.10a: a compound "you and permanents you control" recipient.
        TargetFilter::ControllerAndControlledPermanents { .. } => FilterDomain::ANYTHING,

        // CR 506.3: the player or planeswalker an attacker is attacking. A planeswalker
        // may itself be a creature, so this fails open rather than claiming a player.
        TargetFilter::AttackedPlayerOrPlaneswalker => FilterDomain::ANYTHING,

        TargetFilter::Player
        | TargetFilter::Controller
        | TargetFilter::SourceController
        | TargetFilter::Opponent
        | TargetFilter::SpecificPlayer { .. }
        | TargetFilter::PlayerWhoChoseLabel { .. }
        | TargetFilter::PlayerMatching { .. }
        | TargetFilter::Neighbor { .. }
        | TargetFilter::ScopedPlayer
        | TargetFilter::TriggeringSpellController
        | TargetFilter::TriggeringSpellOwner
        | TargetFilter::TriggeringPlayer
        | TargetFilter::TriggeringSourceController
        | TargetFilter::ParentTargetController
        | TargetFilter::DeclaredPlayer { .. }
        // CR 120.1 + CR 109.4: the damage recipient's CONTROLLER is a player,
        // unlike `EventTarget` (the recipient object itself) below.
        | TargetFilter::EventTargetController
        | TargetFilter::ParentTargetOwner
        // CR 108.3 + CR 508.1d: per-member owner anchor — denotes players,
        // never the affected objects themselves.
        | TargetFilter::AffectedObjectOwner
        | TargetFilter::SourceChosenPlayer
        | TargetFilter::OriginalController
        | TargetFilter::PostReplacementSourceController
        | TargetFilter::PostReplacementDamageTargetOwner
        | TargetFilter::DefendingPlayer
        | TargetFilter::Owner
        | TargetFilter::AllPlayers => FilterDomain::SOME_PLAYER,

        // CR 405.1: objects on the stack are spells and abilities, never
        // creature permanents — a counterspell does not target creatures.
        TargetFilter::StackAbility { .. } | TargetFilter::StackSpell => FilterDomain::STACK_OBJECT,

        // CR 120.1 + CR 614.1: a damage event's recipient may be a permanent or
        // a player, so these two reach both axes.
        TargetFilter::EventTarget | TargetFilter::PostReplacementDamageTarget => {
            FilterDomain::ANYTHING
        }

        // Runtime-bound object references. The filter names no type line, so the
        // object axis stays open and the player axis is closed.
        TargetFilter::SelfRef
        | TargetFilter::GrantingObject { .. }
        | TargetFilter::SourceOrPaired
        | TargetFilter::SpecificObject { .. }
        | TargetFilter::AttachedTo
        | TargetFilter::LastCreated
        | TargetFilter::LastRevealed
        | TargetFilter::LastZoneChanged
        | TargetFilter::CostPaidObject
        | TargetFilter::AmassedArmy
        | TargetFilter::ChosenCard
        | TargetFilter::TrackedSet { .. }
        | TargetFilter::ExiledBySource
        | TargetFilter::ExiledCardByIndex { .. }
        | TargetFilter::TriggeringSource
        | TargetFilter::ParentTarget
        | TargetFilter::ParentTargetSlot { .. }
        | TargetFilter::OriginalSource
        | TargetFilter::PostReplacementDamageSource
        | TargetFilter::HasChosenName
        | TargetFilter::ChosenDamageSource { .. }
        | TargetFilter::Named { .. } => FilterDomain::SOME_OBJECT,

        // A tracked set narrowed by an inner filter — the narrowing decides.
        TargetFilter::TrackedSetFiltered { filter, .. } => filter_domain(filter),

        TargetFilter::Typed(typed) => {
            // Repo invariant (not a CR rule): `type_filters` is a CONJUNCTION,
            // and an EMPTY list is an empty conjunction — "no type-line
            // constraint", not "matches nothing" (see the invariant on
            // `TypedFilter::type_filters`). An unrestricted `Typed` therefore
            // also matches players, in the same direction as
            // `engine::game::filter::player_matches_target_filter_with`.
            if typed.type_filters.is_empty() {
                return FilterDomain::ANYTHING;
            }
            // Every element of the conjunction must admit the SAME object
            // simultaneously, so the list's domain is the AND (not the OR) of
            // each element's own domain — mirroring `TypeDomain::intersect`'s
            // doc, one level down from `FilterDomain::intersect` above.
            let domain = typed
                .type_filters
                .iter()
                .map(type_filter_domain)
                .fold(TypeDomain::UNCONSTRAINED, TypeDomain::intersect);
            FilterDomain {
                creatures: domain.admits_creature,
                non_creature_objects: domain.admits_non_creature,
                // A `Typed` filter carrying a type line never matches a player.
                players: false,
            }
        }

        TargetFilter::Or { filters } => filters
            .iter()
            .map(filter_domain)
            .fold(FilterDomain::NOTHING, FilterDomain::union),

        TargetFilter::And { filters } => filters
            .iter()
            .map(filter_domain)
            .fold(FilterDomain::ANYTHING, FilterDomain::intersect),

        // The complement of a set is not recoverable from these three axes —
        // "not a creature card" and "not a Goblin" negate to very different
        // domains. Fail open rather than guess.
        TargetFilter::Not { .. } => FilterDomain::ANYTHING,
    }
}

/// Could a creature be a legal target under this filter?
pub(crate) fn filter_admits_creature(filter: &TargetFilter) -> bool {
    filter_domain(filter).creatures
}

/// Is EVERY legal target under this filter a creature?
pub(crate) fn filter_is_creature_only(filter: &TargetFilter) -> bool {
    let domain = filter_domain(filter);
    domain.creatures && !domain.non_creature_objects && !domain.players
}

/// Could a player be a legal target under this filter? (CR 115.4)
pub(crate) fn filter_admits_player(filter: &TargetFilter) -> bool {
    filter_domain(filter).players
}

/// Returns true if the effect exclusively targets creatures (not "any target").
/// Used for harmful spells: burn with TargetFilter::Any can still go face.
pub(crate) fn targets_creatures_only(effect: &Effect) -> bool {
    extract_target_filter(effect).is_some_and(filter_is_creature_only)
}

/// Returns true if an effect's target filter can admit a creature.
pub(crate) fn targets_creatures(effect: &Effect) -> bool {
    extract_target_filter(effect).is_some_and(filter_admits_creature)
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
        && node.illegal_targets_disposition == Default::default()
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
        // Boon recipient semantics: the granted body's impact lands on the
        // RECIPIENT, so route it to the chosen candidate — but only when the
        // recipient IS a chosen slot. Contextual holders (`Controller` "you
        // get", `TriggeringPlayer` "that player gets") resolve independently
        // of target selection and must not score (they would pollute
        // unrelated slots in mixed abilities); declared player slots
        // (`Player` "target player", player-shaped `Typed` "target opponent")
        // and inherited chain targets (`ParentTarget`) do.
        Effect::CreateBoon { recipient, .. } => {
            boon_recipient_is_selected(recipient).then_some(recipient)
        }
        _ => extract_target_filter(effect),
    }
}

fn chosen_player_binding(filter: &TargetFilter) -> bool {
    matches!(filter, TargetFilter::Player | TargetFilter::ParentTarget)
        || filter_names_the_chosen_players_permanents(filter)
}

/// Whether a boon `recipient` is a slot target selection chooses (as opposed
/// to a contextual holder that resolves on its own). Mirrors the recipient
/// shapes `Effect::CreateBoon` documents: `Player` ("target player") and
/// player-shaped `Typed` ("target opponent", Loch Larent) declare slots;
/// `ParentTarget` inherits the chain target being chosen now. `Controller`
/// ("you get"), `TriggeringPlayer` ("that player gets"), and every other
/// shape resolve independently of selection. Object-shaped `Typed` recipients
/// fail closed later in the player matcher.
fn boon_recipient_is_selected(recipient: &TargetFilter) -> bool {
    matches!(
        recipient,
        TargetFilter::Player | TargetFilter::ParentTarget | TargetFilter::Typed(_)
    )
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
/// The `CreateBoon` recipient-routing reading. Unlike
/// `granted_trigger_polarity` (the committed target-relative reading kept for
/// non-boon granted triggers), the boon holder RECEIVES the body, so the inner
/// is re-based into the holder's frame: a weapon aimed at the holder's enemies
/// is a benefit to hold, a debuff on the holder's own creatures a harm. The
/// two paths stay split because event anaphors (`TriggeringSource`) need
/// trigger-shape analysis to place — self-referential in a Malice-style grant,
/// anyone's in a general boon — which is out of scope for the boon routing.
fn boon_granted_trigger_polarity(trigger: &TriggerDefinition) -> EffectPolarity {
    let Some(exec) = trigger.execute.as_deref() else {
        return EffectPolarity::Contextual;
    };
    // A granted body that installs a floating replacement carries its
    // direction in the REPLACEMENT body, not the install wrapper (which is
    // Contextual): Loch Larent's boon installs "enters tapped with a stun
    // counter", whose tap-first root is Harmful to the entering creature's
    // controller. Without this descent the harmful grant would read
    // Contextual and the recipient routing below would score it 0.0.
    if let Effect::AddTargetReplacement { replacement, .. } = exec.effect.as_ref() {
        return replacement
            .execute
            .as_deref()
            .map(boon_executable_polarity_for_holder)
            .unwrap_or(EffectPolarity::Contextual);
    }
    boon_executable_polarity_for_holder(exec)
}

fn granted_trigger_polarity(trigger: &TriggerDefinition) -> EffectPolarity {
    let Some(exec) = trigger.execute.as_deref() else {
        return EffectPolarity::Contextual;
    };
    // Same replacement descent as the boon path, but the committed
    // target-relative reading: non-boon granted triggers (Undying Malice)
    // keep their pinned polarity; only the `CreateBoon` recipient router
    // re-bases into the holder's frame.
    if let Effect::AddTargetReplacement { replacement, .. } = exec.effect.as_ref() {
        return replacement
            .execute
            .as_deref()
            .map(|body| effect_polarity(&body.effect))
            .unwrap_or(EffectPolarity::Contextual);
    }
    effect_polarity(&exec.effect)
}

/// Which side of a granted boon body an inner target sits on, in the HOLDER's
/// frame. The install re-stamp makes the holder the body's controller, so
/// holder-relative is CR 608.2c controller-relative at fire time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BoonTargetSide {
    /// The holder's own objects, or the holder (`Controller`, `SelfRef`,
    /// `ParentTarget`, and untargeted verbs that default to the controller).
    Holder,
    /// The holder's opponents or their objects (`Typed{Opponent}`, opponent
    /// players).
    Enemy,
    /// A slot the holder aims at fire time (`Player`, `Any`, an unrestricted
    /// `Typed`): the holder picks the target that favors them.
    Chosen,
    /// Anything else — fail closed to `Contextual`.
    Unknown,
}

/// Impact of a granted boon body ON ITS HOLDER. `effect_polarity` reads impact
/// on the inner TARGET; a boon holder RECEIVES the body, so that reading must
/// be re-based into the holder's frame: a weapon aimed at the holder's enemies
/// is a benefit to hold, a debuff on the holder's own creatures a harm.
/// Without the flip, "target opponent loses 3 life" would read Harmful and the
/// recipient routing would aim the weapon-grant AT the opponent.
///
/// Classified at the EXECUTABLE level, not the bare effect: the executable
/// carries the `player_scope` a targetless verb inherits ("each opponent
/// loses 5 life" harms the holder's enemies, not the holder) and the
/// `sub_ability` / `else_ability` continuations a single-effect reading
/// cannot see. A chained or conditional body fails closed to Contextual —
/// the continuation's direction is uninspected — while simple scoped and
/// unscoped bodies keep their directed readings.
fn boon_executable_polarity_for_holder(exec: &AbilityDefinition) -> EffectPolarity {
    if exec.sub_ability.is_some() || exec.else_ability.is_some() {
        return EffectPolarity::Contextual;
    }
    let effect = exec.effect.as_ref();
    // Perpetual bodies carry no global polarity (`ApplyPerpetual` is
    // Contextual by default — the modified card's owner varies), so the boon
    // path reads the modification's own direction instead of `effect_polarity`.
    if let Effect::ApplyPerpetual { modification, .. } = effect {
        return holder_relative_polarity(
            perpetual_modification_direction(modification),
            boon_inner_target_side(exec),
        );
    }
    holder_relative_polarity(effect_polarity(effect), boon_inner_target_side(exec))
}

/// Re-base a target-relative reading into the boon holder's frame.
fn holder_relative_polarity(
    target_relative: EffectPolarity,
    side: BoonTargetSide,
) -> EffectPolarity {
    if target_relative == EffectPolarity::Contextual {
        return EffectPolarity::Contextual;
    }
    match side {
        BoonTargetSide::Holder => target_relative,
        BoonTargetSide::Enemy => invert(target_relative),
        // The holder aims a chosen slot at fire time and picks the target
        // that favors them, so any directed weapon or gift is a benefit to
        // hold regardless of which way the verb itself points.
        BoonTargetSide::Chosen => EffectPolarity::Beneficial,
        BoonTargetSide::Unknown => EffectPolarity::Contextual,
    }
}

/// Side of a granted body's primary target via the engine's own slot accessor:
/// `target_filter()` already resolves the Token / CopyTokenOf / ApplyPerpetual
/// axes. Precedence is explicit target, then executable `player_scope`, then
/// the CR 608.2c controller default: a targetless verb under "each opponent"
/// harms the holder's enemies (not the holder), while an untargeted unscoped
/// verb (or the perpetual self-subject `Any`) defaults to the resolving
/// controller — the holder once the install re-stamp lands.
fn boon_inner_target_side(exec: &AbilityDefinition) -> BoonTargetSide {
    if let Some(filter) = exec.effect.target_filter() {
        return boon_filter_side(filter);
    }
    if let Some(scope) = exec.player_scope.as_ref() {
        return boon_player_filter_side(scope);
    }
    BoonTargetSide::Holder
}

/// Side of one `PlayerFilter` in the boon holder's frame. Shared by the
/// `PlayerMatching` target arm and the executable-scope arm so the two
/// recipient axes cannot drift: holder-affiliated filters keep the inner
/// reading, enemy-side filters flip it, anything else fails closed.
fn boon_player_filter_side(player: &PlayerFilter) -> BoonTargetSide {
    match player {
        PlayerFilter::Controller => BoonTargetSide::Holder,
        PlayerFilter::Opponent
        | PlayerFilter::OpponentLostLife
        | PlayerFilter::OpponentGainedLife => BoonTargetSide::Enemy,
        _ => BoonTargetSide::Unknown,
    }
}

/// Classify one target filter into the boon holder's frame. Holder-affiliated
/// context refs keep the inner reading; enemy-side filters flip it; slots the
/// holder aims read as weapons to hold. `TriggeringPlayer` is whoever's event
/// fired the boon — which may be an opponent ("whenever an opponent casts") —
/// and `CostPaidObject` names no one in a trigger fire, so both fail closed.
fn boon_filter_side(filter: &TargetFilter) -> BoonTargetSide {
    match filter {
        TargetFilter::Controller
        | TargetFilter::SourceController
        | TargetFilter::ControllerAndControlledPermanents { .. }
        | TargetFilter::SelfRef
        | TargetFilter::ParentTarget => BoonTargetSide::Holder,
        TargetFilter::Opponent => BoonTargetSide::Enemy,
        TargetFilter::Player | TargetFilter::Any => BoonTargetSide::Chosen,
        TargetFilter::Typed(typed) => match typed.controller {
            // Unrestricted "target creature" — the holder aims it.
            None => BoonTargetSide::Chosen,
            Some(ControllerRef::You) => BoonTargetSide::Holder,
            Some(ControllerRef::Opponent | ControllerRef::TargetOpponent) => BoonTargetSide::Enemy,
            // The holder targets the player first, then aims within.
            Some(ControllerRef::TargetPlayer) => BoonTargetSide::Chosen,
            _ => BoonTargetSide::Unknown,
        },
        TargetFilter::PlayerMatching { player } => boon_player_filter_side(player.as_ref()),
        _ => BoonTargetSide::Unknown,
    }
}

/// Direction of a perpetual edit for its TARGET (before the holder-relative
/// re-base): signed P/T deltas, keyword grants, and cost changes each carry
/// their own sign. Absolute sets (`Become`, `SetBasePowerToughness`) are
/// directionless without the target's base stats, and dynamic (`X`) deltas
/// freeze only at application time, so both stay Contextual. A zero-delta edit
/// with a keyword rider (the Jaheira combined shape with no P/T movement)
/// takes the rider's sign.
fn perpetual_modification_direction(modification: &PerpetualModification) -> EffectPolarity {
    match modification {
        PerpetualModification::ModifyPowerToughness {
            power,
            toughness,
            keywords,
        } => {
            let deltas = match (power, toughness) {
                (QuantityExpr::Fixed { value: p }, QuantityExpr::Fixed { value: t }) => {
                    Some((*p, *t))
                }
                _ => None,
            };
            match deltas {
                Some((0, 0)) if !keywords.is_empty() => perpetual_keyword_sign(keywords),
                Some((p, t)) if p >= 0 && t >= 0 && (p, t) != (0, 0) => EffectPolarity::Beneficial,
                Some((p, t)) if p <= 0 && t <= 0 && (p, t) != (0, 0) => EffectPolarity::Harmful,
                _ => EffectPolarity::Contextual,
            }
        }
        PerpetualModification::GrantKeywords { keywords } => perpetual_keyword_sign(keywords),
        PerpetualModification::Become { .. }
        | PerpetualModification::SetBasePowerToughness { .. } => EffectPolarity::Contextual,
        PerpetualModification::ModifyCost { mode, .. } => match mode {
            engine::types::statics::CostModifyMode::Reduce => EffectPolarity::Beneficial,
            // A raise — or a floor, which can only ever raise what is paid —
            // hurts the modified card's caster.
            engine::types::statics::CostModifyMode::Raise
            | engine::types::statics::CostModifyMode::Minimum => EffectPolarity::Harmful,
        },
        PerpetualModification::GrantAbility { modifications } => {
            // First directed grant wins, mirroring the `GenericEffect` arm.
            for grant in modifications {
                let polarity = match grant {
                    PerpetualGrantModification::AddKeyword { keyword } => {
                        perpetual_keyword_sign(std::slice::from_ref(keyword))
                    }
                    PerpetualGrantModification::AddStaticMode { mode } => {
                        static_mode_polarity(mode)
                    }
                    PerpetualGrantModification::GrantAbility { definition } => {
                        effect_polarity(&definition.effect)
                    }
                    PerpetualGrantModification::GrantTrigger { trigger } => {
                        granted_trigger_polarity(trigger)
                    }
                };
                if polarity != EffectPolarity::Contextual {
                    return polarity;
                }
            }
            EffectPolarity::Contextual
        }
    }
}

/// Sign of granted keywords for the card that bears them: CR 702.147a decayed
/// is the one keyword grant that is a drawback, mirroring the keyword-counter
/// rule in `counter_sign_polarity`.
fn perpetual_keyword_sign(keywords: &[Keyword]) -> EffectPolarity {
    if keywords.is_empty() {
        return EffectPolarity::Contextual;
    }
    if keywords.iter().any(|k| matches!(k, Keyword::Decayed)) {
        EffectPolarity::Harmful
    } else {
        EffectPolarity::Beneficial
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

/// CR 611.2a + CR 514.2: a continuous effect from resolution lasts as stated, and "until end of
/// turn" effects end in the cleanup step, so a P/T (CR 613.4c) or keyword grant that lasts until
/// end of turn is bound to this turn, whatever the keyword. [`effect_is_combat_trick_payload`]
/// is the narrower reading of which such grants are priced as combat tricks.
pub(crate) fn ability_is_temporary_combat_modifier(ability: &AbilityDefinition) -> bool {
    ability_effect_is_temporary_combat_modifier(ability)
        && ability
            .sub_ability
            .as_deref()
            .is_none_or(ability_is_temporary_combat_modifier)
        && ability
            .else_ability
            .as_deref()
            .is_none_or(ability_is_temporary_combat_modifier)
        && ability
            .mode_abilities
            .iter()
            .all(ability_is_temporary_combat_modifier)
}

fn ability_effect_is_temporary_combat_modifier(ability: &AbilityDefinition) -> bool {
    match &*ability.effect {
        Effect::Pump { .. } => matches!(ability.duration, Some(Duration::UntilEndOfTurn)),
        effect => effect_is_temporary_combat_modifier(effect),
    }
}

pub(crate) fn effect_is_temporary_combat_modifier(effect: &Effect) -> bool {
    match effect {
        Effect::GenericEffect {
            static_abilities,
            duration: Some(Duration::UntilEndOfTurn),
            ..
        } => {
            // An empty grant modifies nothing, so it is no combat modifier.
            !static_abilities.is_empty()
                && static_abilities
                    .iter()
                    .all(static_definition_is_temporary_combat_modifier)
        }
        _ => false,
    }
}

fn static_definition_is_temporary_combat_modifier(static_def: &StaticDefinition) -> bool {
    matches!(static_def.mode, StaticMode::Continuous)
        && !static_def.modifications.is_empty()
        && static_def
            .modifications
            .iter()
            .all(continuous_modification_is_temporary_combat_modifier)
}

fn continuous_modification_is_temporary_combat_modifier(
    modification: &ContinuousModification,
) -> bool {
    matches!(
        modification,
        ContinuousModification::AddPower { .. }
            | ContinuousModification::AddToughness { .. }
            | ContinuousModification::AddKeyword { .. }
    )
}

/// Is this keyword's value bound to this turn's combat (CR 506.1), so that granting it until
/// end of turn is a combat trick's payload? The rules read each of these as attackers or
/// blockers are declared or as combat damage is dealt: evasion and blocking restrictions,
/// combat-damage modifiers, and attack, block and combat-damage triggers. Deathtouch, lifelink,
/// wither, infect and haste are not confined to combat by their rules, but combat is where a
/// grant of them pays out. Double team is Alchemy's and has no CR entry; it is an attack trigger.
///
/// A keyword whose value lands elsewhere is refused by not being listed: a dies trigger pays
/// off in any phase (undying, persist, afterlife: CR 702.93a, CR 702.79a, CR 702.135a), a
/// graveyard keyword pays off from the graveyard (flashback, escape, embalm: CR 702.34a,
/// CR 702.138a, CR 702.128a), and a protective keyword pays off against removal in any phase
/// (hexproof, indestructible, protection, shroud: CR 702.11b, CR 702.12b, CR 702.16b,
/// CR 702.18a). Like `keyword_is_defensive`, this is an allowlist rather than an exhaustive
/// `match`, so that an unclassified or future keyword fails closed and takes no combat-window
/// penalty.
fn keyword_is_bound_to_combat(keyword: &Keyword) -> bool {
    matches!(
        keyword,
        Keyword::Flying
            | Keyword::Fear
            | Keyword::Intimidate
            | Keyword::Skulk
            | Keyword::Shadow
            | Keyword::Horsemanship
            | Keyword::Menace
            | Keyword::Landwalk(_)
            | Keyword::Reach
            | Keyword::Defender
            | Keyword::Banding
            | Keyword::BandsWithOther(_)
            | Keyword::FirstStrike
            | Keyword::DoubleStrike
            | Keyword::Trample
            | Keyword::TrampleOverPlaneswalkers
            | Keyword::Deathtouch
            | Keyword::Lifelink
            | Keyword::Wither
            | Keyword::Infect
            | Keyword::Toxic(_)
            | Keyword::Poisonous(_)
            | Keyword::Vigilance
            | Keyword::Haste
            | Keyword::Afflict(_)
            | Keyword::Annihilator(_)
            | Keyword::Battlecry
            | Keyword::Bushido(_)
            | Keyword::Decayed
            | Keyword::Dethrone
            | Keyword::DoubleTeam
            | Keyword::Enlist
            | Keyword::Exalted
            | Keyword::Firebending(_)
            | Keyword::Flanking
            | Keyword::Frenzy(_)
            | Keyword::Ingest
            | Keyword::Melee
            | Keyword::Mentor
            | Keyword::Mobilize(_)
            | Keyword::Myriad
            | Keyword::Provoke
            | Keyword::Rampage(_)
            | Keyword::Renown(_)
            | Keyword::Training
    )
}

fn modification_is_bound_to_combat(modification: &ContinuousModification) -> bool {
    match modification {
        ContinuousModification::AddPower { .. } | ContinuousModification::AddToughness { .. } => {
            true
        }
        ContinuousModification::AddKeyword { keyword } => keyword_is_bound_to_combat(keyword),
        _ => false,
    }
}

/// CR 702.73a: a creature-type grant pays off neither in combat nor anywhere else in
/// particular. Beside a P/T change it leaves a trick a trick (Blades of Velis Vel), and alone
/// it is none (Amoeboid Changeling).
fn modification_is_creature_type_rider(modification: &ContinuousModification) -> bool {
    matches!(
        modification,
        ContinuousModification::AddKeyword {
            keyword: Keyword::Changeling
        }
    )
}

/// The payload of a combat trick, read one effect at a time: a P/T change (`Pump`,
/// `DoublePT`) or an until-end-of-turn `GenericEffect` made only of P/T and keyword
/// modifications (`effect_is_temporary_combat_modifier`), whose value is bound to the
/// combat window (CR 611.2a + CR 514.2). A `GenericEffect` needs a modification bound to
/// that window and none whose value lands elsewhere ([`keyword_is_bound_to_combat`]), so
/// a grant that also protects its creature or brings it back is not a trick in any part.
/// `Pump`/`DoublePT` carry their duration on the ability, not the effect, so at this
/// granularity they are accepted as the effect-timing and hint call sites always have;
/// the ability-level [`ability_is_temporary_combat_modifier`] stays the authority where
/// the definition is in hand. [`CombatBinding`] reads a candidate's whole resolution.
pub(crate) fn effect_is_combat_trick_payload(effect: &Effect) -> bool {
    match effect {
        Effect::Pump { .. } | Effect::DoublePT { .. } => true,
        Effect::GenericEffect {
            static_abilities, ..
        } => {
            let modifications = || {
                static_abilities
                    .iter()
                    .flat_map(|static_def| &static_def.modifications)
            };
            effect_is_temporary_combat_modifier(effect)
                && modifications().any(modification_is_bound_to_combat)
                && modifications().all(|modification| {
                    modification_is_bound_to_combat(modification)
                        || modification_is_creature_type_rider(modification)
                })
        }
        _ => false,
    }
}

/// How much of a candidate's resolution is bound to the combat window. CR 601.2b + CR 700.2a:
/// the caller's walk decides which modes are read: every mode of a spell not yet announced,
/// only the chosen modes at the prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CombatBinding {
    /// No combat-trick payload.
    Unbound,
    /// A payload beside value that lands elsewhere. Combat still pays the payload, but
    /// waiting costs the rest, so timing takes no penalty out of the window.
    Partial,
    /// A payload and only riders of it: the combat window is the candidate's whole value
    /// (CR 611.2a + CR 514.2).
    Whole,
}

impl CombatBinding {
    pub(crate) fn of_resolution(effects: &[&Effect]) -> Self {
        if !effects.iter().copied().any(effect_is_combat_trick_payload) {
            return Self::Unbound;
        }
        if effects.iter().copied().all(|effect| {
            effect_is_combat_trick_payload(effect) || effect_has_no_value_outside_combat(effect)
        }) {
            Self::Whole
        } else {
            Self::Partial
        }
    }

    /// The binding of the candidate that resolves `effects`: [`Self::of_resolution`], except
    /// that an activation putting loyalty counters on its permanent keeps them whatever the
    /// phase (CR 606.4), and the permanent's one loyalty activation of the turn (CR 606.3) is
    /// not banked by passing, so combat is not its whole value. The cost is read through
    /// [`PolicyContext::effective_activated_ability`], so a granted loyalty ability counts.
    pub(crate) fn of_candidate(ctx: &PolicyContext<'_>, effects: &[&Effect]) -> Self {
        match Self::of_resolution(effects) {
            Self::Whole
                if ctx
                    .effective_activated_ability()
                    .and_then(|ability| ability.cost)
                    .is_some_and(|cost| net_loyalty_cost(&cost) > 0) =>
            {
                Self::Partial
            }
            binding => binding,
        }
    }
}

/// CR 606.4 + CR 606.5: the loyalty counters a cost puts on its permanent, net of those it
/// removes, its loyalty costs combining into one. A `[−X]` ability, modeled as removing counters,
/// reads 0: it puts none on.
fn net_loyalty_cost(cost: &AbilityCost) -> i32 {
    match cost {
        AbilityCost::Loyalty { amount } => *amount,
        AbilityCost::Composite { costs } => costs.iter().map(net_loyalty_cost).sum(),
        _ => 0,
    }
}

/// Does this effect, beside a combat-trick payload, add no value outside the combat window?
/// This is an allowlist, as [`keyword_is_bound_to_combat`] is, so that an unclassified or
/// future effect fails closed: it reads as value elsewhere and takes no combat-window penalty.
fn effect_has_no_value_outside_combat(effect: &Effect) -> bool {
    match effect {
        // CR 613.4c: a group's P/T change, in the window a `Pump` is.
        Effect::PumpAll { .. }
        // CR 701.26a + CR 701.26b: a tap or untap beside a pump decides a combat's attackers
        // and blockers.
        | Effect::SetTapState { .. }
        // CR 509.1c: a requirement on this turn's blocks.
        | Effect::ForceBlock { .. } => true,
        Effect::GenericEffect {
            static_abilities,
            duration: Some(Duration::UntilEndOfTurn),
            ..
        } => {
            !static_abilities.is_empty()
                && static_abilities.iter().all(|static_def| {
                    !static_def.modifications.is_empty()
                        && static_def
                            .modifications
                            .iter()
                            .all(modification_rides_a_trick)
                })
        }
        // CR 603.7: a delayed removal of what was pumped is the pump's drawback, not a payoff.
        Effect::CreateDelayedTrigger { effect, .. } => collect_definition_effects(effect)
            .into_iter()
            .all(effect_removes_its_own_subject),
        _ => false,
    }
}

fn modification_rides_a_trick(modification: &ContinuousModification) -> bool {
    modification_is_bound_to_combat(modification)
        || modification_is_creature_type_rider(modification)
        || matches!(
            modification,
            ContinuousModification::AddStaticMode { mode } if static_mode_is_bound_to_combat(mode)
        )
}

/// CR 508.1c + CR 508.1d + CR 509.1b + CR 509.1c: the restrictions and requirements read as
/// attackers and blockers are declared, among them goad's (CR 701.15b), plus the permissions to
/// attack despite defender (CR 702.3b) and to block more than the one attacker of CR 509.1a.
/// Unlisted modes fail closed.
fn static_mode_is_bound_to_combat(mode: &StaticMode) -> bool {
    matches!(
        mode,
        StaticMode::CantAttack
            | StaticMode::CantBlock
            | StaticMode::CantAttackOrBlock
            | StaticMode::AttackOnlyNeighbor
            | StaticMode::MaxAttackersEachCombat { .. }
            | StaticMode::MaxBlockersEachCombat { .. }
            | StaticMode::MustAttack
            | StaticMode::MustAttackDefender { .. }
            | StaticMode::MustAttackAwayFromSource
            | StaticMode::Goaded
            | StaticMode::MustBlock
            | StaticMode::MustBlockAttacker { .. }
            | StaticMode::ExtraBlockers { .. }
            | StaticMode::BlockRestriction { .. }
            | StaticMode::CombatAlone { .. }
            | StaticMode::CantBeBlocked
            | StaticMode::CantBeBlockedExceptBy { .. }
            | StaticMode::CantBeBlockedBy { .. }
            | StaticMode::CantBeBlockedByMoreThan { .. }
            | StaticMode::CantBeBlockedUnlessAllBlock
            | StaticMode::MustBeBlocked { .. }
            | StaticMode::MustBeBlockedByAll { .. }
            | StaticMode::CanAttackWithDefender
    )
}

/// CR 701.21a + CR 701.8a: sacrificing, destroying or bouncing the creature that was pumped, or
/// the source, only takes it away. Any other subject is a payoff, or fails closed.
fn effect_removes_its_own_subject(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::Sacrifice { target, .. }
            | Effect::Destroy { target, .. }
            | Effect::Bounce { target, .. }
            if matches!(target, TargetFilter::ParentTarget | TargetFilter::SelfRef)
    )
}

/// The effects a cast spell itself resolves: its spell abilities (CR 113.3a + CR 608.2), read
/// through `CastFacts::primary_effects`' `AbilityKind::Spell` filter. A permanent spell's
/// activated and triggered abilities (CR 113.3b + CR 113.3c) resolve only once activated or
/// triggered, after the permanent has entered (CR 608.3a + CR 603.6a), so a combat-window
/// payload among them is not the cast's.
pub(crate) fn spell_resolution_effects<'a>(facts: &CastFacts<'a>) -> Vec<&'a Effect> {
    facts
        .primary_effects
        .iter()
        .copied()
        .flat_map(collect_definition_effects)
        .collect()
}

/// The object whose timing governs a candidate's own resolution: a spell being cast
/// (CR 601.2a), or having its modes chosen (CR 601.2b + CR 700.2a), or an activated ability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimingSubject {
    Spell(ObjectId),
    Ability {
        source_id: ObjectId,
        ability_index: usize,
    },
}

impl TimingSubject {
    /// A cast through the AI's cast object authority, an activation by its engine index, a
    /// spell's mode prompt by the pending cast's object. Anything else has no subject, so a
    /// modal ability's prompt keeps its pricing.
    pub(crate) fn of_candidate(ctx: &PolicyContext<'_>) -> Option<Self> {
        match (&ctx.candidate.action, &ctx.decision.waiting_for) {
            (GameAction::CastSpell { .. }, _) => {
                cast_object_for_action(ctx.state, &ctx.candidate.action, ctx.ai_player)
                    .map(|object| Self::Spell(object.id))
            }
            (
                GameAction::ActivateAbility {
                    source_id,
                    ability_index,
                    ..
                },
                _,
            ) => Some(Self::Ability {
                source_id: *source_id,
                ability_index: *ability_index,
            }),
            (GameAction::SelectModes { .. }, WaitingFor::ModeChoice { pending_cast, .. }) => {
                Some(Self::Spell(pending_cast.object_id))
            }
            _ => None,
        }
    }

    fn confined_to_sorcery_timing(self, state: &GameState, player: PlayerId) -> bool {
        match self {
            Self::Spell(object_id) => spell_confined_to_sorcery_timing(state, player, object_id),
            Self::Ability {
                source_id,
                ability_index,
            } => ability_confined_to_sorcery_timing(state, player, source_id, ability_index),
        }
    }
}

/// CR 505.1a + CR 307.1 + CR 307.5: is this the precombat main phase, the first main phase of
/// the turn, for a subject confined to sorcery timing? Then no later window reaches this turn's
/// combat, and an until-end-of-turn payload lasts through it (CR 611.2a + CR 514.2).
pub(crate) fn precombat_main_is_last_trick_window(
    state: &GameState,
    player: PlayerId,
    subject: TimingSubject,
) -> bool {
    state.phase == Phase::PreCombatMain && subject.confined_to_sorcery_timing(state, player)
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
    use engine::types::ability::{PreventionAmount, PreventionScope, TypedFilter};

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

    // CR 115.10a: a mass prevention recipient is an untargeted population; the
    // AI's target-filter extraction must mirror the engine's `target_filter()`.
    #[test]
    fn extract_target_filter_only_for_single_scope_prevent_damage() {
        let prevent = |recipient_scope| Effect::PreventDamage {
            amount: PreventionAmount::All,
            amount_dynamic: None,
            target: TargetFilter::Typed(TypedFilter::creature()),
            recipient_scope,
            scope: PreventionScope::AllDamage,
            damage_source_filter: None,
            prevention_duration: None,
        };
        assert!(
            extract_target_filter(&prevent(EffectScope::Single)).is_some(),
            "declared-recipient PreventDamage must expose a selectable target"
        );
        assert!(
            extract_target_filter(&prevent(EffectScope::All)).is_none(),
            "mass PreventDamage{{All}} is a population effect, not target-filtered"
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
mod temporary_combat_modifier_tests {
    use super::*;
    use crate::cast_facts::{collect_definition_effects_with, ModeWalk};
    use engine::types::ability::{
        BounceSelection, DelayedTriggerCondition, EffectKind, TypedFilter,
    };
    use engine::types::identifiers::ObjectIncarnationRef;
    use engine::types::keywords::{EmbalmCost, EscapeCost, FlashbackCost};
    use engine::types::statics::{
        BlockExceptionKind, CombatAloneAction, CombatAloneRequirement, RequiredDefender,
    };

    fn until_end_of_turn(static_abilities: Vec<StaticDefinition>) -> Effect {
        Effect::GenericEffect {
            static_abilities,
            target: Some(TargetFilter::ParentTarget),
            duration: Some(Duration::UntilEndOfTurn),
            end_cost: None,
        }
    }

    fn pump(modifications: Vec<ContinuousModification>) -> StaticDefinition {
        StaticDefinition::continuous()
            .affected(TargetFilter::ParentTarget)
            .modifications(modifications)
    }

    #[test]
    fn empty_until_end_of_turn_grant_is_no_combat_modifier() {
        let control = until_end_of_turn(vec![pump(vec![ContinuousModification::AddPower {
            value: 1,
        }])]);
        assert!(
            effect_is_temporary_combat_modifier(&control),
            "reach-guard: a +1/+0 grant is a combat modifier"
        );
        assert!(!effect_is_temporary_combat_modifier(&until_end_of_turn(
            vec![]
        )));
        assert!(!effect_is_temporary_combat_modifier(&until_end_of_turn(
            vec![pump(vec![])]
        )));
        assert!(!effect_is_combat_trick_payload(&until_end_of_turn(vec![])));
    }

    fn grant(keyword: Keyword) -> ContinuousModification {
        ContinuousModification::AddKeyword { keyword }
    }

    fn trick_payload(modifications: Vec<ContinuousModification>) -> bool {
        effect_is_combat_trick_payload(&until_end_of_turn(vec![pump(modifications)]))
    }

    /// Every keyword kind an until-end-of-turn grant adds in the card pool whose value is bound
    /// to combat, plus the class's other attack, block and combat-damage keywords.
    #[test]
    fn a_keyword_bound_to_combat_is_a_trick_payload_alone() {
        for keyword in [
            Keyword::Flying,
            Keyword::Fear,
            Keyword::Intimidate,
            Keyword::Skulk,
            Keyword::Shadow,
            Keyword::Horsemanship,
            Keyword::Menace,
            Keyword::Landwalk("Island".to_string()),
            Keyword::Reach,
            Keyword::Defender,
            Keyword::FirstStrike,
            Keyword::DoubleStrike,
            Keyword::Trample,
            Keyword::TrampleOverPlaneswalkers,
            Keyword::Deathtouch,
            Keyword::Lifelink,
            Keyword::Wither,
            Keyword::Infect,
            Keyword::Toxic(1),
            Keyword::Banding,
            Keyword::Vigilance,
            Keyword::Haste,
            Keyword::Annihilator(1),
            Keyword::Myriad,
            Keyword::Melee,
            Keyword::Firebending(QuantityExpr::Fixed { value: 5 }),
            Keyword::DoubleTeam,
            Keyword::Exalted,
            Keyword::Battlecry,
            Keyword::Rampage(2),
            Keyword::Bushido(1),
            Keyword::Flanking,
            Keyword::Provoke,
        ] {
            assert!(
                trick_payload(vec![grant(keyword.clone())]),
                "{keyword:?} pays out in combat"
            );
        }
    }

    /// CR 702.93a + CR 702.79a + CR 702.135a: a dies trigger pays off in any phase. CR 702.34a +
    /// CR 702.138a + CR 702.128a: a graveyard keyword pays off from the graveyard. CR 702.73a: a
    /// creature-type grant has no combat value. An unclassified keyword fails closed. Beside a
    /// +1/+0 reach guard, none of them is a payload alone.
    #[test]
    fn a_keyword_whose_value_lands_elsewhere_is_no_trick_payload() {
        let mana = || engine::types::mana::ManaCost::zero();
        assert!(
            trick_payload(vec![ContinuousModification::AddPower { value: 1 }]),
            "reach-guard: a +1/+0 grant is a trick payload"
        );
        for keyword in [
            Keyword::Undying,
            Keyword::Persist,
            Keyword::Afterlife(1),
            Keyword::Flashback(FlashbackCost::Mana(mana())),
            Keyword::Escape(EscapeCost::Mana(mana())),
            Keyword::Embalm(EmbalmCost::Mana(mana())),
            Keyword::JumpStart,
            Keyword::Unearth(mana()),
            Keyword::Changeling,
            Keyword::Flash,
            Keyword::Absorb(1),
            Keyword::Unknown("unclassified".to_string()),
        ] {
            assert!(
                !trick_payload(vec![grant(keyword.clone())]),
                "{keyword:?} is not bound to combat"
            );
        }
    }

    /// A mixed grant reads as the protective one does: value that lands elsewhere refuses the
    /// whole payload, however much of it is bound to combat. A creature-type grant does not,
    /// since it pays off nowhere else either, but it does not make a payload by itself.
    #[test]
    fn a_mixed_grant_is_a_trick_payload_only_if_nothing_in_it_lands_elsewhere() {
        let plus_one = || ContinuousModification::AddPower { value: 1 };
        assert!(trick_payload(vec![plus_one(), grant(Keyword::Trample)]));
        assert!(trick_payload(vec![plus_one(), grant(Keyword::Changeling)]));
        assert!(trick_payload(vec![
            grant(Keyword::Flying),
            grant(Keyword::Changeling)
        ]));
        assert!(!trick_payload(vec![plus_one(), grant(Keyword::Undying)]));
        assert!(!trick_payload(vec![
            grant(Keyword::Flying),
            grant(Keyword::Persist)
        ]));
        assert!(!trick_payload(vec![
            plus_one(),
            grant(Keyword::Indestructible)
        ]));
    }

    fn parsed(oracle_text: &str, name: &str, core_type: &str) -> Vec<AbilityDefinition> {
        engine::parser::oracle::parse_oracle_text(
            oracle_text,
            name,
            &[],
            &[core_type.to_string()],
            &[],
        )
        .abilities
    }

    /// A cast reads every Spell ability and every mode (CR 601.2b).
    fn cast_walk(abilities: &[AbilityDefinition]) -> Vec<&Effect> {
        abilities
            .iter()
            .filter(|ability| ability.kind == AbilityKind::Spell)
            .flat_map(collect_definition_effects)
            .collect()
    }

    /// An activation reads the one ability it activates, here the card's first.
    fn activation_walk(abilities: &[AbilityDefinition]) -> Vec<&Effect> {
        let ability = abilities
            .iter()
            .find(|ability| ability.kind == AbilityKind::Activated)
            .expect("the card prints an activated ability");
        collect_definition_effects_with(ability, ModeWalk::RootOnly)
    }

    /// `Whole` needs every effect to be a payload or a rider of one; the premise pins each
    /// card's parsed shape, so a row cannot pass by parsing into a different chain.
    #[test]
    fn combat_binding_reads_the_whole_resolution() {
        use CombatBinding::{Partial, Whole};
        use EffectKind as K;

        type Row = (
            &'static str,
            &'static str,
            &'static str,
            &'static [EffectKind],
            CombatBinding,
        );

        let instant_or_sorcery: [Row; 22] = [
            (
                "Giant Growth",
                "Instant",
                "Target creature gets +3/+3 until end of turn.",
                &[K::Pump],
                Whole,
            ),
            (
                "Barge In",
                "Instant",
                "Target attacking creature gets +2/+2 until end of turn. Each attacking non-Human creature gains trample until end of turn.",
                &[K::Pump, K::GenericEffect],
                Whole,
            ),
            (
                "Arm the Cathars",
                "Sorcery",
                "Until end of turn, target creature gets +3/+3, up to one other target creature gets +2/+2, and up to one other target creature gets +1/+1. Those creatures gain vigilance until end of turn.",
                &[K::Pump, K::Pump, K::Pump, K::GenericEffect],
                Whole,
            ),
            (
                "Coordinated Assault",
                "Instant",
                "Up to two target creatures each get +1/+0 and gain first strike until end of turn. (They deal combat damage before creatures without first strike.)",
                &[K::GenericEffect],
                Whole,
            ),
            (
                "Gerrard's Command",
                "Instant",
                "Untap target creature. It gets +3/+3 until end of turn.",
                &[K::Untap, K::Pump],
                Whole,
            ),
            (
                "Stabbing Pain",
                "Instant",
                "Target creature gets -1/-1 until end of turn. Tap that creature.",
                &[K::Pump, K::Tap],
                Whole,
            ),
            (
                "Echoing Courage",
                "Instant",
                "Target creature and all other creatures with the same name as that creature get +2/+2 until end of turn.",
                &[K::Pump, K::PumpAll],
                Whole,
            ),
            (
                "Monstrous Step",
                "Sorcery",
                "Target creature gets +7/+7 until end of turn. Up to one other target creature blocks it this turn if able.\nCycling {2} ({2}, Discard this card: Draw a card.)",
                &[K::Pump, K::ForceBlock],
                Whole,
            ),
            (
                "Give No Ground",
                "Instant",
                "Target creature gets +2/+6 until end of turn and can block any number of creatures this turn.",
                &[K::Pump, K::GenericEffect],
                Whole,
            ),
            (
                "Emergent Growth",
                "Sorcery",
                "Target creature gets +5/+5 until end of turn and must be blocked this turn if able.",
                &[K::Pump, K::GenericEffect],
                Whole,
            ),
            (
                "Blood Frenzy",
                "Instant",
                "Cast this spell only before the combat damage step.\nTarget attacking or blocking creature gets +4/+0 until end of turn. Destroy that creature at the beginning of the next end step.",
                &[K::Pump, K::CreateDelayedTrigger],
                Whole,
            ),
            (
                "Invigorated Rampage",
                "Instant",
                "Choose one —\n• Target creature gets +4/+0 and gains trample until end of turn.\n• Two target creatures each get +2/+0 and gain trample until end of turn.",
                &[K::GenericEffect, K::GenericEffect],
                Whole,
            ),
            (
                "Heroic Reinforcements",
                "Sorcery",
                "Create two 1/1 white Soldier creature tokens. Until end of turn, creatures you control get +1/+1 and gain haste. (They can attack and {T} this turn.)",
                &[K::Token, K::GenericEffect],
                Partial,
            ),
            (
                "Deafening Clarion",
                "Sorcery",
                "Choose one or both —\n• Deafening Clarion deals 3 damage to each creature.\n• Creatures you control gain lifelink until end of turn.",
                &[K::DamageAll, K::GenericEffect],
                Partial,
            ),
            (
                "Goblin War Party",
                "Sorcery",
                "Choose one —\n• Create three 1/1 red Goblin creature tokens.\n• Creatures you control get +1/+1 and gain haste until end of turn.\nEntwine {2}{R} (Choose both if you pay the entwine cost.)",
                &[K::Token, K::GenericEffect],
                Partial,
            ),
            (
                "Monstrous Rage",
                "Instant",
                "Target creature gets +2/+0 until end of turn. Create a Monster Role token attached to it. (If you control another Role on it, put that one into the graveyard. Enchanted creature gets +1/+1 and has trample.)",
                &[K::Pump, K::Token],
                Partial,
            ),
            (
                "Afflict",
                "Instant",
                "Target creature gets -1/-1 until end of turn.\nDraw a card.",
                &[K::Pump, K::Draw],
                Partial,
            ),
            (
                "Moment of Triumph",
                "Instant",
                "Target creature gets +2/+2 until end of turn. You gain 2 life.",
                &[K::Pump, K::GainLife],
                Partial,
            ),
            (
                "A Good Day to Die",
                "Instant",
                "Target creature gets +2/+0 until end of turn. When that creature dies this turn, you draw a card.",
                &[K::Pump, K::CreateDelayedTrigger],
                Partial,
            ),
            (
                "Karametra's Blessing",
                "Instant",
                "Target creature gets +2/+2 until end of turn. If it's an enchanted creature or enchantment creature, it also gains hexproof and indestructible until end of turn. (It can't be the target of spells or abilities your opponents control. Damage and effects that say \"destroy\" don't destroy it.)",
                &[K::Pump, K::GenericEffect],
                Partial,
            ),
            (
                "Unnatural Endurance",
                "Instant",
                "Devoid (This card has no color.)\nTarget creature gets +2/+0 until end of turn. Regenerate it.",
                &[K::Pump, K::Regenerate],
                Partial,
            ),
            (
                "Act of Treason",
                "Sorcery",
                "Gain control of target creature until end of turn. Untap that creature. It gains haste until end of turn. (It can attack and {T} this turn.)",
                &[K::GainControl, K::Untap, K::GenericEffect],
                Partial,
            ),
        ];
        let activated: [Row; 5] = [
            (
                "Vectis Agents",
                "Creature",
                "{U}{B}: This creature gets -2/-0 until end of turn and can't be blocked this turn.",
                &[K::Pump, K::GenericEffect],
                Whole,
            ),
            (
                "Mobile Fort",
                "Creature",
                "Defender (This creature can't attack.)\n{3}: This creature gets +3/-1 until end of turn and can attack this turn as though it didn't have defender. Activate only once each turn.",
                &[K::Pump, K::GenericEffect],
                Whole,
            ),
            (
                "Pyric Salamander",
                "Creature",
                "{R}: This creature gets +1/+0 until end of turn. Sacrifice this creature at the beginning of the next end step.",
                &[K::Pump, K::CreateDelayedTrigger],
                Whole,
            ),
            (
                "Dragon Mask",
                "Artifact",
                "{3}, {T}: Target creature you control gets +2/+2 until end of turn. Return it to its owner's hand at the beginning of the next end step. (Return it only if it's on the battlefield.)",
                &[K::Pump, K::CreateDelayedTrigger],
                Whole,
            ),
            (
                "The Wandering Emperor",
                "Planeswalker",
                "Flash\nAs long as The Wandering Emperor entered this turn, you may activate her loyalty abilities any time you could cast an instant.\n[+1]: Put a +1/+1 counter on up to one target creature. It gains first strike until end of turn.\n[−1]: Create a 2/2 white Samurai creature token with vigilance.\n[−2]: Exile target tapped creature. You gain 2 life.",
                &[K::PutCounter, K::GenericEffect],
                Partial,
            ),
        ];

        let check = |name: &str, walk: Vec<&Effect>, kinds: &[EffectKind], binding| {
            let parsed_kinds: Vec<EffectKind> = walk
                .iter()
                .map(|effect| EffectKind::from(*effect))
                .collect();
            assert_eq!(parsed_kinds, kinds, "premise: {name} parses to this chain");
            assert_eq!(CombatBinding::of_resolution(&walk), binding, "{name}");
        };
        for (name, core_type, oracle_text, kinds, binding) in instant_or_sorcery {
            let abilities = parsed(oracle_text, name, core_type);
            check(name, cast_walk(&abilities), kinds, binding);
        }
        for (name, core_type, oracle_text, kinds, binding) in activated {
            let abilities = parsed(oracle_text, name, core_type);
            check(name, activation_walk(&abilities), kinds, binding);
        }
    }

    /// CR 606.5: loyalty costs combine into one, so its example's `[−4]` with an additional
    /// `[+1]` removes counters and an added mana cost changes nothing. A `[0]` puts no counter.
    #[test]
    fn net_loyalty_cost_combines_the_loyalty_parts_of_a_cost() {
        let loyalty = |amount| AbilityCost::Loyalty { amount };
        let mana = || AbilityCost::Mana {
            cost: engine::types::mana::ManaCost::generic(2),
        };
        let composite = |costs| AbilityCost::Composite { costs };

        for (cost, net) in [
            (loyalty(1), 1),
            (loyalty(0), 0),
            (loyalty(-3), -3),
            (mana(), 0),
            (AbilityCost::Tap, 0),
            (composite(vec![mana(), loyalty(1)]), 1),
            (composite(vec![mana(), loyalty(-1)]), -1),
            (composite(vec![loyalty(-4), loyalty(1)]), -3),
            (composite(vec![loyalty(1), loyalty(1)]), 2),
            (composite(vec![mana(), composite(vec![loyalty(2)])]), 2),
        ] {
            assert_eq!(net_loyalty_cost(&cost), net, "{cost:?}");
        }
    }

    fn bare_pump() -> Effect {
        Effect::Pump {
            power: PtValue::Fixed(1),
            toughness: PtValue::Fixed(1),
            target: TargetFilter::ParentTarget,
        }
    }

    fn static_grant(mode: StaticMode, duration: Option<Duration>) -> Effect {
        Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::new(mode.clone())
                .affected(TargetFilter::ParentTarget)
                .modifications(vec![ContinuousModification::AddStaticMode { mode }])],
            target: Some(TargetFilter::ParentTarget),
            duration,
            end_cost: None,
        }
    }

    fn delayed(ability: AbilityDefinition) -> Effect {
        Effect::CreateDelayedTrigger {
            condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
            effect: Box::new(ability),
            uses_tracked_set: false,
        }
    }

    fn sacrifice(target: TargetFilter) -> Effect {
        Effect::Sacrifice {
            target,
            count: QuantityExpr::Fixed { value: 1 },
            min_count: 0,
        }
    }

    fn draw() -> Effect {
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        }
    }

    fn set_tap_state() -> Effect {
        Effect::SetTapState {
            target: TargetFilter::ParentTarget,
            scope: EffectScope::Single,
            state: TapStateChange::Untap,
        }
    }

    fn pump_all() -> Effect {
        Effect::PumpAll {
            power: PtValue::Fixed(1),
            toughness: PtValue::Fixed(1),
            target: TargetFilter::ParentTarget,
        }
    }

    fn beside_a_pump(effect: &Effect) -> CombatBinding {
        CombatBinding::of_resolution(&[&bare_pump(), effect])
    }

    /// The riders read in full: every attack and block static of CR 508.1c, CR 508.1d, CR 509.1b
    /// and CR 509.1c, with the permissions beside them, and a delayed removal of what was pumped.
    /// A rider alone is no payload, and anything else beside a payload fails closed.
    #[test]
    fn only_attack_and_block_statics_and_self_removal_ride_a_trick() {
        use CombatBinding::{Partial, Unbound, Whole};

        let until_eot = Some(Duration::UntilEndOfTurn);
        for mode in [
            StaticMode::CantAttack,
            StaticMode::CantBlock,
            StaticMode::CantAttackOrBlock,
            StaticMode::AttackOnlyNeighbor,
            StaticMode::MaxAttackersEachCombat {
                max: 1,
                defender: None,
            },
            StaticMode::MaxBlockersEachCombat { max: 1 },
            StaticMode::MustAttack,
            StaticMode::MustAttackDefender {
                defender: RequiredDefender::Fixed {
                    player: PlayerId(1),
                },
            },
            StaticMode::MustAttackAwayFromSource,
            StaticMode::Goaded,
            StaticMode::MustBlock,
            StaticMode::MustBlockAttacker {
                attacker: ObjectIncarnationRef::of(ObjectId(1), 0),
            },
            StaticMode::ExtraBlockers { count: None },
            StaticMode::BlockRestriction {
                filter: TargetFilter::Any,
            },
            StaticMode::CombatAlone {
                action: CombatAloneAction::Attack,
                requirement: CombatAloneRequirement::NeedsCompanion,
            },
            StaticMode::CantBeBlocked,
            StaticMode::CantBeBlockedExceptBy {
                kind: BlockExceptionKind::MinBlockers { min: 2 },
            },
            StaticMode::CantBeBlockedBy {
                filter: TargetFilter::Any,
            },
            StaticMode::CantBeBlockedByMoreThan { max: 1 },
            StaticMode::CantBeBlockedUnlessAllBlock,
            StaticMode::MustBeBlocked { by: None },
            StaticMode::MustBeBlockedByAll { blockers: None },
            StaticMode::CanAttackWithDefender,
        ] {
            let rider = static_grant(mode.clone(), until_eot.clone());
            assert_eq!(beside_a_pump(&rider), Whole, "{mode:?} rides a pump");
            assert_eq!(
                CombatBinding::of_resolution(&[&rider]),
                Unbound,
                "{mode:?} alone is no payload"
            );
        }

        assert_eq!(CombatBinding::of_resolution(&[]), Unbound);
        let forced_block = Effect::ForceBlock {
            target: TargetFilter::ParentTarget,
            attacker: None,
            duration: Duration::UntilEndOfTurn,
        };
        for rider in [set_tap_state(), pump_all(), forced_block] {
            assert_eq!(beside_a_pump(&rider), Whole, "{rider:?} rides a pump");
            assert_eq!(
                CombatBinding::of_resolution(&[&rider]),
                Unbound,
                "{rider:?} alone is no payload"
            );
        }

        for target in [TargetFilter::ParentTarget, TargetFilter::SelfRef] {
            for removal in [
                sacrifice(target.clone()),
                Effect::Destroy {
                    target: target.clone(),
                    cant_regenerate: false,
                },
                Effect::Bounce {
                    target: target.clone(),
                    destination: None,
                    selection: BounceSelection::Targeted,
                },
            ] {
                let rider = delayed(AbilityDefinition::new(AbilityKind::Spell, removal));
                assert_eq!(beside_a_pump(&rider), Whole, "{rider:?} rides a pump");
                assert_eq!(
                    CombatBinding::of_resolution(&[&rider]),
                    Unbound,
                    "{rider:?} alone is no payload"
                );
            }
        }

        for (why, effect) in [
            (
                "a rule modification outside declarations",
                static_grant(StaticMode::CantGainLife, until_eot.clone()),
            ),
            (
                "a protective grant",
                until_end_of_turn(vec![pump(vec![grant(Keyword::Hexproof)])]),
            ),
            (
                "a grant that outlasts the turn",
                static_grant(StaticMode::CantBlock, Some(Duration::Permanent)),
            ),
            ("an empty grant", until_end_of_turn(vec![])),
            ("empty modifications", until_end_of_turn(vec![pump(vec![])])),
            (
                "an empty static beside a rider",
                until_end_of_turn(vec![
                    pump(vec![]),
                    pump(vec![ContinuousModification::AddStaticMode {
                        mode: StaticMode::CantBlock,
                    }]),
                ]),
            ),
            (
                "another creature's sacrifice",
                delayed(AbilityDefinition::new(
                    AbilityKind::Spell,
                    sacrifice(TargetFilter::Typed(TypedFilter::default())),
                )),
            ),
            (
                "a token's sacrifice",
                delayed(AbilityDefinition::new(
                    AbilityKind::Spell,
                    sacrifice(TargetFilter::LastCreated),
                )),
            ),
            (
                "a delayed draw",
                delayed(AbilityDefinition::new(AbilityKind::Spell, draw())),
            ),
            (
                "a delayed draw behind a removal",
                delayed(
                    AbilityDefinition::new(
                        AbilityKind::Spell,
                        sacrifice(TargetFilter::ParentTarget),
                    )
                    .sub_ability(AbilityDefinition::new(AbilityKind::Spell, draw())),
                ),
            ),
            ("a draw", draw()),
        ] {
            assert_eq!(beside_a_pump(&effect), Partial, "{why} is value elsewhere");
        }
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
mod create_boon_polarity_tests {
    use super::*;
    use engine::game::scenario::{GameScenario, P0, P1};
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ControllerRef, PerpetualGrantModification,
        PerpetualModification, PlayerFilter, QuantityRef, ReplacementDefinition, TapStateChange,
        TypedFilter,
    };
    use engine::types::mana::ManaCost;
    use engine::types::replacements::ReplacementEvent;
    use engine::types::statics::CostModifyMode;

    fn boon_with_body(recipient: TargetFilter, exec: Option<Effect>) -> Effect {
        let mut trigger = TriggerDefinition::new(TriggerMode::SpellCast);
        trigger.execute = exec.map(|e| Box::new(AbilityDefinition::new(AbilityKind::Spell, e)));
        Effect::CreateBoon {
            recipient,
            trigger: Box::new(trigger),
        }
    }

    fn boon_with_exec(recipient: TargetFilter, exec: AbilityDefinition) -> Effect {
        let mut trigger = TriggerDefinition::new(TriggerMode::SpellCast);
        trigger.execute = Some(Box::new(exec));
        Effect::CreateBoon {
            recipient,
            trigger: Box::new(trigger),
        }
    }

    fn scoped_exec(effect: Effect, scope: PlayerFilter) -> AbilityDefinition {
        let mut exec = AbilityDefinition::new(AbilityKind::Spell, effect);
        exec.player_scope = Some(scope);
        exec
    }

    fn opponent_recipient() -> TargetFilter {
        TargetFilter::Typed(TypedFilter {
            type_filters: Vec::new(),
            controller: Some(ControllerRef::Opponent),
            properties: Vec::new(),
        })
    }

    /// Loch Larent's boon body: a floating install whose replacement taps the
    /// entering creature first, then places the stun counter.
    fn loch_install() -> Effect {
        let tap = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            },
        );
        Effect::AddTargetReplacement {
            replacement: Box::new(ReplacementDefinition {
                execute: Some(Box::new(tap)),
                ..ReplacementDefinition::new(ReplacementEvent::ChangeZone)
            }),
            target: TargetFilter::None,
        }
    }

    #[test]
    fn create_boon_polarity_follows_granted_body() {
        let gain = boon_with_body(
            TargetFilter::Player,
            Some(Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::Controller,
            }),
        );
        assert_eq!(effect_polarity(&gain), EffectPolarity::Beneficial);
        let drain = boon_with_body(
            TargetFilter::Player,
            Some(Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 1 },
                target: None,
            }),
        );
        assert_eq!(effect_polarity(&drain), EffectPolarity::Harmful);
        let bodiless = boon_with_body(TargetFilter::Player, None);
        assert_eq!(effect_polarity(&bodiless), EffectPolarity::Contextual);
    }

    /// F3: a targetless harm under opponent scope ("each opponent loses 5
    /// life") harms the holder's enemies, so holding the boon is beneficial —
    /// not harmful to the holder. The unscoped targetless shape keeps its
    /// Harmful reading (see `create_boon_polarity_follows_granted_body`).
    #[test]
    fn boon_opponent_scoped_targetless_harm_reads_beneficial() {
        let drain = boon_with_exec(
            TargetFilter::Player,
            scoped_exec(
                Effect::LoseLife {
                    amount: QuantityExpr::Fixed { value: 5 },
                    target: None,
                },
                PlayerFilter::Opponent,
            ),
        );
        assert_eq!(effect_polarity(&drain), EffectPolarity::Beneficial);
    }

    /// F3 control: the same targetless harm under holder scope ("you lose 5
    /// life") stays Harmful — scope names the victim, it does not excuse it.
    #[test]
    fn boon_holder_scoped_targetless_harm_reads_harmful() {
        let drain = boon_with_exec(
            TargetFilter::Player,
            scoped_exec(
                Effect::LoseLife {
                    amount: QuantityExpr::Fixed { value: 5 },
                    target: None,
                },
                PlayerFilter::Controller,
            ),
        );
        assert_eq!(effect_polarity(&drain), EffectPolarity::Harmful);
    }

    /// F3: Underbridge Warlock's full body — opponent-scoped loss chained to a
    /// holder gain — fails closed to Contextual: the single-effect reading
    /// cannot see the continuation's direction.
    #[test]
    fn boon_mixed_continuation_body_reads_contextual() {
        let mut exec = scoped_exec(
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 5 },
                target: None,
            },
            PlayerFilter::Opponent,
        );
        exec.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 5 },
                player: TargetFilter::Controller,
            },
        )));
        let mixed = boon_with_exec(TargetFilter::Player, exec);
        assert_eq!(effect_polarity(&mixed), EffectPolarity::Contextual);
    }

    /// F3: a conditional body ("... Otherwise, ...") fails closed to
    /// Contextual: the untaken branch's direction is uninspected.
    #[test]
    fn boon_conditional_body_reads_contextual() {
        let mut exec = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::Controller,
            },
        );
        exec.else_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 3 },
                target: None,
            },
        )));
        let conditional = boon_with_exec(TargetFilter::Player, exec);
        assert_eq!(effect_polarity(&conditional), EffectPolarity::Contextual);
    }

    #[test]
    fn create_boon_loch_replacement_body_reads_harmful() {
        // The install wrapper is Contextual; the seam must descend into the
        // replacement body (tap-first root: Harmful) or the harmful grant
        // scores 0.0 and aims nowhere.
        let loch = boon_with_body(opponent_recipient(), Some(loch_install()));
        assert_eq!(effect_polarity(&loch), EffectPolarity::Harmful);
    }

    #[test]
    fn boon_harmful_grant_scores_opponent_negative() {
        // Loch Larent: "target opponent gets [stun] boon" must score -1.0 for
        // the opponent candidate, so selection aims the harmful grant away
        // from self. (It must NOT read beneficial-for-self.)
        let runner = GameScenario::new().build();
        let state = runner.state();
        let loch = boon_with_body(opponent_recipient(), Some(loch_install()));
        assert_eq!(
            targeted_player_impact_in(
                state,
                Some(P0),
                Some(engine::types::identifiers::ObjectId(0)),
                &[&loch],
                P1,
            ),
            Some(-1.0),
        );
    }

    #[test]
    fn boon_beneficial_grant_prefers_self_and_ignores_contextual_holder() {
        let runner = GameScenario::new().build();
        let state = runner.state();
        let source = Some(engine::types::identifiers::ObjectId(0));
        let gain = boon_with_body(
            TargetFilter::Player,
            Some(Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::Controller,
            }),
        );
        // "Target player gets [beneficial] boon": +1.0 for self, so selection
        // keeps the good boon instead of gifting it to an opponent.
        assert_eq!(
            targeted_player_impact_in(state, Some(P0), source, &[&gain], P0),
            Some(1.0),
        );
        // "You get [beneficial] boon": the Controller holder is contextual —
        // it resolves independently of selection and must not score (no
        // per-candidate signal, so it can never pollute an unrelated slot in
        // a mixed ability).
        let self_gain = boon_with_body(
            TargetFilter::Controller,
            Some(Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::Controller,
            }),
        );
        assert_eq!(
            targeted_player_impact_in(state, Some(P0), source, &[&self_gain], P0),
            None,
        );
        assert_eq!(
            targeted_player_impact_in(state, Some(P0), source, &[&self_gain], P1),
            None,
        );
    }

    fn drain_some(target: TargetFilter) -> Effect {
        Effect::LoseLife {
            amount: QuantityExpr::Fixed { value: 3 },
            target: Some(target),
        }
    }

    fn typed_controller(controller: Option<ControllerRef>) -> TargetFilter {
        TargetFilter::Typed(TypedFilter {
            type_filters: Vec::new(),
            controller,
            properties: Vec::new(),
        })
    }

    fn perpetual(target: TargetFilter, modification: PerpetualModification) -> Effect {
        Effect::ApplyPerpetual {
            target,
            modification,
        }
    }

    fn modify_pt(power: i32, toughness: i32) -> PerpetualModification {
        PerpetualModification::ModifyPowerToughness {
            power: QuantityExpr::Fixed { value: power },
            toughness: QuantityExpr::Fixed { value: toughness },
            keywords: Vec::new(),
        }
    }

    #[test]
    fn boon_opponent_aimed_harm_reads_beneficial_to_hold() {
        // A weapon aimed at the holder's enemies is a benefit to RECEIVE: the
        // enemy-side inner flips, or selection would aim the weapon-grant at
        // the opponent.
        for target in [
            typed_controller(Some(ControllerRef::Opponent)),
            typed_controller(Some(ControllerRef::TargetOpponent)),
            TargetFilter::PlayerMatching {
                player: Box::new(PlayerFilter::Opponent),
            },
            TargetFilter::Opponent,
        ] {
            let weapon = boon_with_body(TargetFilter::Player, Some(drain_some(target.clone())));
            assert_eq!(
                effect_polarity(&weapon),
                EffectPolarity::Beneficial,
                "opponent-aimed drain must flip to Beneficial for the holder, got {target:?}"
            );
        }
        let burn = boon_with_body(
            TargetFilter::Player,
            Some(Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: typed_controller(Some(ControllerRef::Opponent)),
                damage_source: None,
                excess: None,
            }),
        );
        assert_eq!(effect_polarity(&burn), EffectPolarity::Beneficial);
    }

    #[test]
    fn boon_chosen_slot_reads_beneficial_holder_aims() {
        // The holder aims a chosen slot at fire time and picks the target that
        // favors them, so any directed verb on a chosen slot is a benefit to
        // hold regardless of which way the verb points.
        let chosen_drain =
            boon_with_body(TargetFilter::Player, Some(drain_some(TargetFilter::Player)));
        assert_eq!(effect_polarity(&chosen_drain), EffectPolarity::Beneficial);
        let any_burn = boon_with_body(
            TargetFilter::Player,
            Some(Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Any,
                damage_source: None,
                excess: None,
            }),
        );
        assert_eq!(effect_polarity(&any_burn), EffectPolarity::Beneficial);
        let open_destroy = boon_with_body(
            TargetFilter::Player,
            Some(Effect::Destroy {
                target: typed_controller(None),
                cant_regenerate: false,
            }),
        );
        assert_eq!(effect_polarity(&open_destroy), EffectPolarity::Beneficial);
        let chosen_gift = boon_with_body(
            TargetFilter::Player,
            Some(Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::Player,
            }),
        );
        assert_eq!(effect_polarity(&chosen_gift), EffectPolarity::Beneficial);
    }

    #[test]
    fn boon_self_side_readings_keep_inner_direction() {
        // Holder-side inners keep their direction: a self-drain is a curse to
        // hold, a self-gift a benefit.
        for target in [TargetFilter::Controller, TargetFilter::ParentTarget] {
            let curse = boon_with_body(TargetFilter::Player, Some(drain_some(target.clone())));
            assert_eq!(
                effect_polarity(&curse),
                EffectPolarity::Harmful,
                "holder-side drain must stay Harmful, got {target:?}"
            );
        }
        let own_destroy = boon_with_body(
            TargetFilter::Player,
            Some(Effect::Destroy {
                target: typed_controller(Some(ControllerRef::You)),
                cant_regenerate: false,
            }),
        );
        assert_eq!(effect_polarity(&own_destroy), EffectPolarity::Harmful);
    }

    #[test]
    fn boon_perpetual_pt_deltas_signal_direction() {
        // Perpetual bodies read their modification's own P/T deltas (the
        // global ApplyPerpetual default stays Contextual): buffs on the
        // holder's side are benefits, debuffs harms, enemy-side debuffs flip.
        let buff = boon_with_body(
            TargetFilter::Player,
            Some(perpetual(TargetFilter::ParentTarget, modify_pt(1, 0))),
        );
        assert_eq!(effect_polarity(&buff), EffectPolarity::Beneficial);
        let self_buff = boon_with_body(
            TargetFilter::Player,
            Some(perpetual(TargetFilter::Any, modify_pt(3, 3))),
        );
        assert_eq!(effect_polarity(&self_buff), EffectPolarity::Beneficial);
        let shrink = boon_with_body(
            TargetFilter::Player,
            Some(perpetual(TargetFilter::ParentTarget, modify_pt(-1, -2))),
        );
        assert_eq!(effect_polarity(&shrink), EffectPolarity::Harmful);
        // "Target creature an opponent controls perpetually gets -1/-2": an
        // enemy-side debuff is a weapon to hold.
        let enemy_shrink = boon_with_body(
            TargetFilter::Player,
            Some(perpetual(
                typed_controller(Some(ControllerRef::Opponent)),
                modify_pt(-1, -2),
            )),
        );
        assert_eq!(effect_polarity(&enemy_shrink), EffectPolarity::Beneficial);
        // Dynamic (X) deltas freeze only at application time — no static sign.
        let dynamic = boon_with_body(
            TargetFilter::Player,
            Some(perpetual(
                TargetFilter::ParentTarget,
                PerpetualModification::ModifyPowerToughness {
                    power: QuantityExpr::Ref {
                        qty: QuantityRef::Variable {
                            name: "X".to_string(),
                        },
                    },
                    toughness: QuantityExpr::Fixed { value: 0 },
                    keywords: Vec::new(),
                },
            )),
        );
        assert_eq!(effect_polarity(&dynamic), EffectPolarity::Contextual);
        // Mixed deltas and absolute sets are directionless without a base.
        let mixed = boon_with_body(
            TargetFilter::Player,
            Some(perpetual(TargetFilter::ParentTarget, modify_pt(1, -1))),
        );
        assert_eq!(effect_polarity(&mixed), EffectPolarity::Contextual);
        let set_base = boon_with_body(
            TargetFilter::Player,
            Some(perpetual(
                TargetFilter::ParentTarget,
                PerpetualModification::SetBasePowerToughness {
                    power: 4,
                    toughness: 4,
                },
            )),
        );
        assert_eq!(effect_polarity(&set_base), EffectPolarity::Contextual);
    }

    #[test]
    fn boon_perpetual_keyword_cost_and_grant_signals() {
        let flying = boon_with_body(
            TargetFilter::Player,
            Some(perpetual(
                TargetFilter::ParentTarget,
                PerpetualModification::GrantKeywords {
                    keywords: vec![Keyword::Flying],
                },
            )),
        );
        assert_eq!(effect_polarity(&flying), EffectPolarity::Beneficial);
        // Decayed is the one keyword grant that is a drawback for its bearer.
        let decayed = boon_with_body(
            TargetFilter::Player,
            Some(perpetual(
                TargetFilter::ParentTarget,
                PerpetualModification::GrantKeywords {
                    keywords: vec![Keyword::Decayed],
                },
            )),
        );
        assert_eq!(effect_polarity(&decayed), EffectPolarity::Harmful);
        let discount = boon_with_body(
            TargetFilter::Player,
            Some(perpetual(
                TargetFilter::Any,
                PerpetualModification::ModifyCost {
                    mode: CostModifyMode::Reduce,
                    amount: ManaCost::default(),
                },
            )),
        );
        assert_eq!(effect_polarity(&discount), EffectPolarity::Beneficial);
        let tax = boon_with_body(
            TargetFilter::Player,
            Some(perpetual(
                TargetFilter::Any,
                PerpetualModification::ModifyCost {
                    mode: CostModifyMode::Raise,
                    amount: ManaCost::default(),
                },
            )),
        );
        assert_eq!(effect_polarity(&tax), EffectPolarity::Harmful);
        // Quoted grants route through the shared static inspector.
        let evasive = boon_with_body(
            TargetFilter::Player,
            Some(perpetual(
                TargetFilter::ParentTarget,
                PerpetualModification::GrantAbility {
                    modifications: vec![PerpetualGrantModification::AddStaticMode {
                        mode: StaticMode::CantBeBlocked,
                    }],
                },
            )),
        );
        assert_eq!(effect_polarity(&evasive), EffectPolarity::Beneficial);
    }

    #[test]
    fn boon_unresolvable_sides_fail_closed() {
        // `TriggeringPlayer` is whoever's event fired the boon — possibly an
        // opponent — and `CostPaidObject` / fixed ids name no side at all.
        for target in [
            TargetFilter::TriggeringPlayer,
            TargetFilter::CostPaidObject,
            TargetFilter::SpecificObject {
                id: engine::types::identifiers::ObjectId(7),
            },
        ] {
            let murky = boon_with_body(TargetFilter::Player, Some(drain_some(target.clone())));
            assert_eq!(
                effect_polarity(&murky),
                EffectPolarity::Contextual,
                "unresolvable side must fail closed, got {target:?}"
            );
        }
    }

    #[test]
    fn boon_weapon_grant_scores_self_positive() {
        // "Target player gets [opponent-draining] boon": the flipped weapon
        // reads Beneficial, so selection scores keeping it +1.0 for self —
        // the same keep-the-good-boon signal as the gift test above.
        let runner = GameScenario::new().build();
        let state = runner.state();
        let weapon = boon_with_body(
            TargetFilter::Player,
            Some(drain_some(typed_controller(Some(ControllerRef::Opponent)))),
        );
        assert_eq!(effect_polarity(&weapon), EffectPolarity::Beneficial);
        assert_eq!(
            targeted_player_impact_in(
                state,
                Some(P0),
                Some(engine::types::identifiers::ObjectId(0)),
                &[&weapon],
                P0,
            ),
            Some(1.0),
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
        CopyCountStatus, DetachedRemainder, Duration, EffectKind, FilterProp,
        IllegalTargetsDisposition, ModalChoice, MultiTargetSpec, OpponentMayScope,
        ParentTargetMissingReason, PlayerFilter, PlayerScope, QuantityRef, RepeatContinuation,
        ResolvedAbility, SiblingCondition, SubAbilityLink, TargetChoiceTiming, TargetRef,
        TargetSelectionMode, TypedFilter, UnlessPayModifier,
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
        // CR 608.2b + CR 101.1: a node that still resolves with illegal targets is
        // not the plain shape the exact classifier models.
        assert_ineligible_on_root_and_fixed_child!(
            "illegal_targets_disposition",
            |node: &mut ResolvedAbility| {
                node.illegal_targets_disposition = IllegalTargetsDisposition::StillResolves;
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

#[cfg(test)]
mod filter_domain_tests {
    use super::*;
    use engine::parser::oracle::parse_oracle_text;
    use engine::types::ability::{AbilityDefinition, TypedFilter};

    /// The shipped parse of a real card's Oracle text. Anchoring on this rather
    /// than a hand-written AST means a parser shape change fails these tests
    /// instead of leaving them green on a shape no card actually produces.
    fn parsed_abilities(
        card_name: &str,
        oracle_text: &str,
        keywords: &[&str],
        types: &[&str],
    ) -> Vec<AbilityDefinition> {
        let keywords: Vec<String> = keywords.iter().map(|k| (*k).to_string()).collect();
        let types: Vec<String> = types.iter().map(|t| (*t).to_string()).collect();
        parse_oracle_text(oracle_text, card_name, &keywords, &types, &[]).abilities
    }

    fn creature_filter() -> TargetFilter {
        TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Creature],
            ..Default::default()
        })
    }

    fn land_filter() -> TargetFilter {
        TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Land],
            ..Default::default()
        })
    }

    /// The regression this whole seam exists for. Expendable Troops' "{T},
    /// Sacrifice this creature: It deals 2 damage to target attacking or
    /// blocking creature" parses its target to an `Or` of two `Typed` legs.
    /// Before composite support, `targets_creatures_only` matched only bare
    /// `Typed` and answered **false** here, which silently disabled
    /// `anti_self_harm::score_pre_cast`'s entire whiff gate and let
    /// `effect_timing::removal_score` fall back to scoring the activation by
    /// opponent creatures the ability cannot legally target.
    #[test]
    fn attacking_or_blocking_creature_is_creature_only() {
        let abilities = parsed_abilities(
            "Expendable Troops",
            "{T}, Sacrifice this creature: It deals 2 damage to target attacking or blocking creature.",
            &[],
            &["Creature"],
        );
        let effect = &*abilities
            .first()
            .expect("Expendable Troops parses one activated ability")
            .effect;
        let filter = extract_target_filter(effect).expect("DealDamage carries a target filter");

        assert!(
            matches!(filter, TargetFilter::Or { .. }),
            "premise of this test: the parser emits a composite Or here, got {filter:?}"
        );
        assert!(
            filter_is_creature_only(filter),
            "every leg names Creature, so every legal target is a creature"
        );
        assert!(filter_admits_creature(filter));
        assert!(
            !filter_admits_player(filter),
            "CR 115.4: a typed creature filter never admits a player — this is the \
             answer `self_cost::deal_damage_is_trivial` used to fail open on, letting an \
             opponent's life total decide a verdict the ability could never act on"
        );
        assert!(targets_creatures_only(effect));
        assert!(targets_creatures(effect));
    }

    /// Serra Advocate is the polarity twin: identical `Or` filter, beneficial
    /// effect. Both halves of the reported board must read as creature-only.
    #[test]
    fn beneficial_twin_reads_the_same_filter_the_same_way() {
        let abilities = parsed_abilities(
            "Serra Advocate",
            "Flying\n{T}: Target attacking or blocking creature gets +2/+2 until end of turn.",
            &["Flying"],
            &["Creature"],
        );
        let effect = &*abilities
            .iter()
            .find(|a| extract_target_filter(&a.effect).is_some())
            .expect("Serra Advocate parses a targeted pump")
            .effect;
        assert!(filter_is_creature_only(
            extract_target_filter(effect).expect("pump carries a filter")
        ));
        assert!(targets_creatures(effect));
    }

    #[test]
    fn any_target_admits_everything_but_is_not_creature_only() {
        // CR 115.4: "any target" is the burn-can-go-face case the creature-only
        // gate must keep answering `false` for.
        assert!(filter_admits_creature(&TargetFilter::Any));
        assert!(filter_admits_player(&TargetFilter::Any));
        assert!(!filter_is_creature_only(&TargetFilter::Any));
    }

    #[test]
    fn player_filters_admit_no_objects() {
        for filter in [
            TargetFilter::Player,
            TargetFilter::Opponent,
            TargetFilter::Controller,
            TargetFilter::AllPlayers,
        ] {
            assert!(filter_admits_player(&filter), "{filter:?}");
            assert!(!filter_admits_creature(&filter), "{filter:?}");
            assert!(!filter_is_creature_only(&filter), "{filter:?}");
        }
    }

    #[test]
    fn or_is_a_union() {
        // Creature ∪ Land reaches creatures, but not ONLY creatures.
        let mixed = TargetFilter::Or {
            filters: vec![creature_filter(), land_filter()],
        };
        assert!(filter_admits_creature(&mixed));
        assert!(!filter_is_creature_only(&mixed));

        // Creature ∪ Player reaches a player, so it is not creature-only either.
        let with_player = TargetFilter::Or {
            filters: vec![creature_filter(), TargetFilter::Player],
        };
        assert!(filter_admits_player(&with_player));
        assert!(!filter_is_creature_only(&with_player));
    }

    #[test]
    fn and_is_an_intersection() {
        // Narrowing a creature filter by a second creature filter stays
        // creature-only; narrowing "any target" by a creature filter BECOMES
        // creature-only, because the intersection drops the player leg.
        let narrowed = TargetFilter::And {
            filters: vec![TargetFilter::Any, creature_filter()],
        };
        assert!(filter_is_creature_only(&narrowed));
        assert!(!filter_admits_player(&narrowed));
    }

    #[test]
    fn nested_composites_recurse() {
        let nested = TargetFilter::Or {
            filters: vec![
                TargetFilter::Or {
                    filters: vec![creature_filter(), creature_filter()],
                },
                creature_filter(),
            ],
        };
        assert!(filter_is_creature_only(&nested));
    }

    #[test]
    fn negation_fails_open() {
        // The complement of a set is not recoverable from three booleans:
        // "not a creature" and "not a Goblin" negate to very different domains.
        // Fail open rather than guess — never claim creature-only.
        let negated = TargetFilter::Not {
            filter: Box::new(creature_filter()),
        };
        assert!(!filter_is_creature_only(&negated));
        assert!(filter_admits_creature(&negated));
        assert!(filter_admits_player(&negated));
    }

    #[test]
    fn stack_filters_are_not_creatures() {
        // CR 405.1: objects on the stack are spells and abilities. A
        // counterspell must not read as creature-targeting.
        assert!(!filter_admits_creature(&TargetFilter::StackSpell));
        assert!(!filter_is_creature_only(&TargetFilter::StackSpell));
        assert!(!filter_admits_player(&TargetFilter::StackSpell));
    }

    #[test]
    fn untyped_typed_filter_still_reaches_players() {
        // Repo invariant (not a CR rule): an EMPTY `type_filters` is an empty
        // conjunction — "no type-line constraint" — and the engine's own player
        // matcher admits a player for it. The old `Typed(_) => false` blanket
        // answered this wrong.
        let untyped = TargetFilter::Typed(TypedFilter::default());
        assert!(filter_admits_player(&untyped));
        assert!(filter_admits_creature(&untyped));
        assert!(!filter_is_creature_only(&untyped));
    }

    #[test]
    fn nothing_admits_nothing() {
        assert!(!filter_admits_creature(&TargetFilter::None));
        assert!(!filter_admits_player(&TargetFilter::None));
        assert!(!filter_is_creature_only(&TargetFilter::None));
    }

    /// Review finding: `filter_domain`'s `Typed` arm answered only for a
    /// LITERAL `TypeFilter::Creature` and treated every other `TypeFilter`
    /// as creature-excluding by omission. `AnyOf` (CR 608.2b's own
    /// disjunction) and `Permanent` (a union that lists `CoreType::Creature`
    /// as one of its six admitted core types in
    /// `engine::game::filter::type_filter_matches`) both admit a creature by
    /// construction, not incidentally — so both must report
    /// `filter_admits_creature == true`.
    #[test]
    fn any_of_creature_and_enchantment_admits_a_creature() {
        let filter =
            TargetFilter::Typed(TypedFilter::default().with_type(TypeFilter::AnyOf(vec![
                TypeFilter::Creature,
                TypeFilter::Enchantment,
            ])));
        assert!(
            filter_admits_creature(&filter),
            "AnyOf([Creature, Enchantment]) must admit a creature — Creature is literally              one of its two disjuncts (CR 608.2b)"
        );
        assert!(
            !filter_is_creature_only(&filter),
            "the Enchantment branch means a non-creature (a plain enchantment) can ALSO              satisfy this filter, so it must not read as creature-only"
        );
    }

    #[test]
    fn permanent_admits_a_creature() {
        let filter = TargetFilter::Typed(TypedFilter::default().with_type(TypeFilter::Permanent));
        assert!(
            filter_admits_creature(&filter),
            "TypeFilter::Permanent's own matcher lists CoreType::Creature as one of the six              core types it admits — a creature satisfies 'target permanent' by construction"
        );
        assert!(
            !filter_is_creature_only(&filter),
            "a land, artifact, enchantment, planeswalker or battle ALSO satisfies              'target permanent', so it must not read as creature-only"
        );
    }

    /// A conjunction (NOT a disjunction) of two categorical `TypeFilter`s
    /// where one is the literal `Creature` — "target artifact creature"
    /// (`type_filters: [Artifact, Creature]`). Discriminates
    /// `TypeDomain::intersect` from a hypothetical implementation that
    /// treated any non-`CREATURE_ONLY` conjunct as disqualifying: `Artifact`
    /// alone is `EITHER` (real artifact creatures exist), and ANDing it with
    /// `Creature`'s `CREATURE_ONLY` must still land on creature-only, not on
    /// "admits neither".
    #[test]
    fn artifact_creature_conjunction_is_still_creature_only() {
        let filter = TargetFilter::Typed(
            TypedFilter::default()
                .with_type(TypeFilter::Artifact)
                .with_type(TypeFilter::Creature),
        );
        assert!(filter_admits_creature(&filter));
        assert!(
            filter_is_creature_only(&filter),
            "'target artifact creature' is creature-only: the literal Creature conjunct              proves it regardless of what the Artifact conjunct alone could admit"
        );
    }

    /// The provably creature-excluding categories stay excluded: no printed
    /// card combines a spell-only type with Creature (CR 300.1).
    #[test]
    fn instant_and_sorcery_stay_non_creature_only() {
        for tf in [TypeFilter::Instant, TypeFilter::Sorcery] {
            let filter = TargetFilter::Typed(TypedFilter::default().with_type(tf.clone()));
            assert!(!filter_admits_creature(&filter), "{tf:?}");
            assert!(!filter_is_creature_only(&filter), "{tf:?}");
        }
    }

    /// "target noncreature permanent" — `Non(Creature)` is the one negation
    /// shape this module resolves precisely rather than falling open: every
    /// creature trivially satisfies the un-negated `Creature`, so none can
    /// satisfy its negation.
    #[test]
    fn non_creature_excludes_creatures() {
        let filter = TargetFilter::Typed(
            TypedFilter::default().with_type(TypeFilter::Non(Box::new(TypeFilter::Creature))),
        );
        assert!(!filter_admits_creature(&filter));
        assert!(!filter_is_creature_only(&filter));
    }
}

#[cfg(test)]
mod attacked_referent_domain_tests {
    use super::*;

    /// CR 506.3: a planeswalker may itself be a creature, so the referent fails open.
    #[test]
    fn attacked_referent_domain_is_anything() {
        assert_eq!(
            filter_domain(&TargetFilter::AttackedPlayerOrPlaneswalker),
            FilterDomain::ANYTHING
        );
    }
}
