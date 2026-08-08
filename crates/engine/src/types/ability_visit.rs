//! The engine's single complete `AbilityDefinition` / `Effect` traversal.
//!
//! This code was moved verbatim out of `game/printed_cards.rs`, where it was
//! specialized to conjure-name collection, and parameterized by a visitor
//! closure so any "can this ability tree contain effect shape X" question can
//! reuse it. The name-extraction leaf stayed behind in `printed_cards.rs`.
//!
//! The `match`es over `Effect`, `ContinuousModification`, and `AbilityCost` are
//! wildcard-free **on purpose**: a new variant on any of those three enums is a
//! compile error here, which forces a descend-or-leaf decision at the one place
//! that owns the answer.
//!
//! That guarantee is necessary but not sufficient. A new nested **struct field**
//! is field access, not a match arm, so it compiles silently. Two fixtures are
//! the complementary safety nets:
//!
//! - `game::printed_cards::tests::walker_covers_every_nested_carrier`
//! - `ai_support::targeted_exchange::tests::predicate_sees_a_fight_in_every_nested_carrier`
//!
//! Both plant a marker effect in every carrier this module descends into.
//! Extend **both** whenever a carrier is added.
//!
//! Two narrower ad-hoc walkers remain unmigrated and are candidate future
//! consumers: `game::coverage::ability_tree_any` (which has a `_ => {}`
//! wildcard and omits many carriers — broadening it would change the coverage
//! report) and `game::replacement::ability_tree_creates_tokens` (which walks
//! only `Token` / `ChooseOneOf` / `sub_ability` / `else_ability` — broadening it
//! would change replacement behavior). Neither is migrated here, because either
//! migration would change behavior.

use crate::types::ability::{
    AbilityCost, AbilityDefinition, ContinuousModification, CopiableValues, CounterSourceRider,
    Effect, ReplacementDefinition, ReplacementMode, StaticDefinition, TriggerDefinition,
    VoteSubject,
};
use std::ops::ControlFlow;

pub fn visit_ability_def<F>(def: &AbilityDefinition, visit: &mut F) -> ControlFlow<()>
where
    F: FnMut(&Effect) -> ControlFlow<()>,
{
    visit_effect(&def.effect, visit)?;
    if let Some(cost) = &def.cost {
        visit_cost(cost, visit)?;
    }
    if let Some(sub) = &def.sub_ability {
        visit_ability_def(sub, visit)?;
    }
    if let Some(else_ability) = &def.else_ability {
        visit_ability_def(else_ability, visit)?;
    }
    for mode in &def.mode_abilities {
        visit_ability_def(mode, visit)?;
    }
    // "unless [player] pays {cost}" — the cost may be an EffectCost that conjures.
    if let Some(unless_pay) = &def.unless_pay {
        visit_cost(&unless_pay.cost, visit)?;
    }
    ControlFlow::Continue(())
}

pub fn visit_trigger<F>(trigger: &TriggerDefinition, visit: &mut F) -> ControlFlow<()>
where
    F: FnMut(&Effect) -> ControlFlow<()>,
{
    if let Some(execute) = &trigger.execute {
        visit_ability_def(execute, visit)?;
    }
    if let Some(unless_pay) = &trigger.unless_pay {
        visit_cost(&unless_pay.cost, visit)?;
    }
    ControlFlow::Continue(())
}

pub fn visit_replacement<F>(replacement: &ReplacementDefinition, visit: &mut F) -> ControlFlow<()>
where
    F: FnMut(&Effect) -> ControlFlow<()>,
{
    if let Some(execute) = &replacement.execute {
        visit_ability_def(execute, visit)?;
    }
    // The mode carries the decline continuation (and, for MayCost, a cost),
    // either of which may conjure. Descend into both.
    match &replacement.mode {
        ReplacementMode::MayCost { cost, decline } => {
            visit_cost(cost, visit)?;
            if let Some(decline) = decline {
                visit_ability_def(decline, visit)?;
            }
        }
        ReplacementMode::Optional { decline } => {
            if let Some(decline) = decline {
                visit_ability_def(decline, visit)?;
            }
        }
        ReplacementMode::Mandatory => {}
    }
    // `runtime_execute` holds a resolution-time continuation that is never
    // present on a printed/static `CardFace`; skipped intentionally.
    ControlFlow::Continue(())
}

pub fn visit_static<F>(static_def: &StaticDefinition, visit: &mut F) -> ControlFlow<()>
where
    F: FnMut(&Effect) -> ControlFlow<()>,
{
    for modification in &static_def.modifications {
        visit_continuous_mod(modification, visit)?;
    }
    ControlFlow::Continue(())
}

pub fn visit_continuous_mod<F>(
    modification: &ContinuousModification,
    visit: &mut F,
) -> ControlFlow<()>
where
    F: FnMut(&Effect) -> ControlFlow<()>,
{
    match modification {
        ContinuousModification::GrantAbility { definition } => {
            visit_ability_def(definition, visit)?
        }
        ContinuousModification::GrantTrigger { trigger } => visit_trigger(trigger, visit)?,
        ContinuousModification::GrantReplacement { replacement } => {
            visit_replacement(replacement, visit)?
        }
        ContinuousModification::GrantStaticAbility { definition } => {
            visit_static(definition, visit)?
        }
        ContinuousModification::CopyValues { values, .. } => {
            visit_copiable_values(values, visit)?
        }
        // Remaining modifications carry no nested ability/effect carriers.
        // GrantAllActivatedAbilitiesOf / GrantAllTriggeredAbilitiesOf only hold a
        // source `TargetFilter`; the granted abilities/triggers are pulled live
        // from the provider objects at layer collection time, not nested here.
        ContinuousModification::GrantAllActivatedAbilitiesOf { .. }
        | ContinuousModification::GrantAllTriggeredAbilitiesOf { .. }
        // CR 707.2c (Metamorphic Alteration): inert parse-time copy marker — no
        // nested ability/effect carrier to walk (the copy grant is the runtime TCE).
        | ContinuousModification::CopyChosen
        | ContinuousModification::SetName { .. }
        | ContinuousModification::SetTextName { .. }
        | ContinuousModification::AddPower { .. }
        | ContinuousModification::AddToughness { .. }
        | ContinuousModification::SetPower { .. }
        | ContinuousModification::SetToughness { .. }
        | ContinuousModification::AddKeyword { .. }
        | ContinuousModification::AddKeywordWithDerivedCost { .. }
        | ContinuousModification::RemoveKeyword { .. }
        | ContinuousModification::RemoveAllAbilities
        | ContinuousModification::AddType { .. }
        | ContinuousModification::RemoveType { .. }
        | ContinuousModification::AddSubtype { .. }
        | ContinuousModification::RemoveSubtype { .. }
        | ContinuousModification::SetCardTypes { .. }
        | ContinuousModification::RemoveAllSubtypes { .. }
        | ContinuousModification::SetDynamicPower { .. }
        | ContinuousModification::SetDynamicToughness { .. }
        | ContinuousModification::SetPowerDynamic { .. }
        | ContinuousModification::SetToughnessDynamic { .. }
        | ContinuousModification::AddDynamicPower { .. }
        | ContinuousModification::AddDynamicToughness { .. }
        | ContinuousModification::AddDynamicKeyword { .. }
        | ContinuousModification::AddAllCreatureTypes
        | ContinuousModification::AddAllBasicLandTypes
        | ContinuousModification::AddAllLandTypes
        | ContinuousModification::AddChosenSubtype { .. }
        | ContinuousModification::AddChosenColor { .. }
        | ContinuousModification::RemoveChosenKeyword
        | ContinuousModification::AddChosenKeyword
        | ContinuousModification::SetColor { .. }
        | ContinuousModification::AddColor { .. }
        | ContinuousModification::AddStaticMode { .. }
        | ContinuousModification::SwitchPowerToughness
        | ContinuousModification::AssignDamageFromToughness
        | ContinuousModification::AssignDamageAsThoughUnblocked
        | ContinuousModification::AssignNoCombatDamage
        | ContinuousModification::ChangeController
        | ContinuousModification::SetBasicLandType { .. }
        | ContinuousModification::SetChosenBasicLandType
        | ContinuousModification::SetChosenName
        | ContinuousModification::RetainPrintedTriggerFromSource { .. }
        | ContinuousModification::RetainPrintedAbilityFromSource { .. }
        | ContinuousModification::RetainAllOtherAbilitiesFromSource
        | ContinuousModification::AddSupertype { .. }
        | ContinuousModification::RemoveSupertype { .. }
        | ContinuousModification::AddCounterOnEnter { .. }
        | ContinuousModification::SetStartingLoyalty { .. }
        | ContinuousModification::RemoveManaCost => {}
    }
    ControlFlow::Continue(())
}

pub fn visit_copiable_values<F>(values: &CopiableValues, visit: &mut F) -> ControlFlow<()>
where
    F: FnMut(&Effect) -> ControlFlow<()>,
{
    for ability in values.abilities.iter() {
        visit_ability_def(ability, visit)?;
    }
    for trigger in values.trigger_definitions.iter() {
        visit_trigger(trigger, visit)?;
    }
    for static_def in values.static_definitions.iter() {
        visit_static(static_def, visit)?;
    }
    for replacement in values.replacement_definitions.iter() {
        visit_replacement(replacement, visit)?;
    }
    ControlFlow::Continue(())
}

pub fn visit_cost<F>(cost: &AbilityCost, visit: &mut F) -> ControlFlow<()>
where
    F: FnMut(&Effect) -> ControlFlow<()>,
{
    match cost {
        AbilityCost::EffectCost { effect } => visit_effect(effect, visit)?,
        AbilityCost::Composite { costs } | AbilityCost::OneOf { costs } => {
            for sub in costs {
                visit_cost(sub, visit)?;
            }
        }
        AbilityCost::PerCounter { base, .. } => visit_cost(base, visit)?,
        // Remaining costs carry no nested effect/cost carriers.
        AbilityCost::Mana { .. }
        | AbilityCost::ManaDynamic { .. }
        | AbilityCost::Tap
        | AbilityCost::Untap
        | AbilityCost::Loyalty { .. }
        | AbilityCost::Sacrifice(_)
        | AbilityCost::PayLife { .. }
        | AbilityCost::Discard { .. }
        | AbilityCost::Exile { .. }
        | AbilityCost::ExileMaterials { .. }
        | AbilityCost::CollectEvidence { .. }
        | AbilityCost::ExileWithAggregate { .. }
        | AbilityCost::TapCreatures { .. }
        | AbilityCost::RemoveCounter { .. }
        | AbilityCost::PayEnergy { .. }
        | AbilityCost::PaySpeed { .. }
        | AbilityCost::ReturnToHand { .. }
        | AbilityCost::Unattach
        | AbilityCost::UnattachFrom { .. }
        | AbilityCost::Mill { .. }
        | AbilityCost::Exert
        | AbilityCost::Blight { .. }
        | AbilityCost::Reveal { .. }
        | AbilityCost::Behold { .. }
        | AbilityCost::Waterbend { .. }
        | AbilityCost::NinjutsuFamily { .. }
        // CR 118.9: a borrowed keyword cost carries no nested effect/cost carrier.
        | AbilityCost::KeywordCostOfCastSpell { .. }
        | AbilityCost::GetPlayerCounters { .. }
        | AbilityCost::Unimplemented { .. } => {}
    }
    ControlFlow::Continue(())
}

/// Visit `effect` and every effect reachable from its nested ability/effect
/// carriers, pre-order, stopping early on `ControlFlow::Break`. The match is
/// wildcard-free, so a new `Effect` variant forces a decision here (compile
/// error until handled). That guarantee is necessary but not sufficient: a
/// variant wrongly added to the leaf arm, or a new nested *struct field* (which
/// is field access, not a match arm), compiles silently.
/// `printed_cards::tests::walker_covers_every_nested_carrier` and
/// `ai_support::targeted_exchange::tests::predicate_sees_a_fight_in_every_nested_carrier`
/// are the complementary safety nets for those cases — extend both whenever a
/// carrier is added.
pub fn visit_effect<F>(effect: &Effect, visit: &mut F) -> ControlFlow<()>
where
    F: FnMut(&Effect) -> ControlFlow<()>,
{
    visit(effect)?;
    match effect {
        Effect::Intensify { .. } => {}
        Effect::ApplyPerpetual { .. } => {}
        // CR 614.11: A one-shot draw replacement nests its substitute Effect
        // (Words of Worship/Wilding). Walk it so any conjure name it carries is
        // surfaced (GainLife/Token carry none today, but it is a nested carrier).
        Effect::CreateDrawReplacement { replacement_effect } => {
            visit_effect(replacement_effect, visit)?
        }
        // CR 614.1a: A planeswalk replacement nests its substitute Effect (Fixed
        // Point in Time: chaos ensues). Walk it so any conjure name it carries is
        // surfaced (ChaosEnsues carries none today, but it is a nested carrier).
        Effect::CreatePlaneswalkReplacement { replacement_effect } => {
            visit_effect(replacement_effect, visit)?
        }
        // Heist exiles a card from an opponent's library at random; it does not
        // name a conjure card, so there is no static face to preload.
        Effect::Heist { .. } | Effect::HeistExile => {}
        // Carries no nested ability/effect carrier. Only named-conjure has a
        // static card name to extract, and that extraction now lives in the
        // caller's visitor closure (`printed_cards::collect_conjure_names`).
        Effect::Conjure { .. } => {}
        // CR 701.42 / CR 712.4b: the melded permanent presents the `result`
        // card's characteristics, but `result` is an outside-the-game third card.
        // Its name is extracted by the caller's visitor closure
        // (`printed_cards::collect_conjure_names`), which seeds it so
        // `build_conjure_registry` preloads its `CardFace` into
        // `card_face_registry`. `source` and `partner` are live battlefield
        // objects the resolver finds by printed identity — they need no registry
        // seeding, and neither field is a nested ability/effect carrier.
        Effect::Meld { .. } => {}
        // A spellbook draft conjures the chosen card, but the list lives on the
        // card face (`metadata.spellbook`), not in the effect — the registry
        // seed collects it directly from the face (see
        // `collect_conjure_names_from_face`), so nothing to gather here.
        Effect::DraftFromSpellbook { .. } => {}
        Effect::TurnFaceUp { .. } => {}
        Effect::TurnFaceDown { .. } => {}
        // Nested-ability carriers — descend.
        Effect::Vote {
            per_choice_effect,
            subject,
            ..
        } => {
            for sub in per_choice_effect {
                visit_ability_def(sub, visit)?;
            }
            // CR 701.38b: object-pool votes (Council's Judgment, Prime
            // Minister's Cabinet Room) leave `per_choice_effect` empty and
            // carry the sole nested AbilityDefinition in `outcome_template`.
            // Walk it so any conjure name a future object-vote outcome names is
            // surfaced (the current exile-only class carries none).
            if let VoteSubject::Objects {
                outcome_template, ..
            } = subject
            {
                visit_ability_def(outcome_template, visit)?;
            }
        }
        Effect::SeparateIntoPiles {
            chosen_pile_effect,
            unchosen_pile_effect,
            ..
        } => {
            visit_ability_def(chosen_pile_effect, visit)?;
            if let Some(unchosen) = unchosen_pile_effect {
                visit_ability_def(unchosen, visit)?;
            }
        }
        Effect::RevealFromHand { on_decline, .. } => {
            if let Some(sub) = on_decline {
                visit_ability_def(sub, visit)?;
            }
        }
        // Only the delayed `effect` is walked; the `condition`'s embedded
        // TriggerDefinition has `execute: None` by construction (it is a matcher,
        // not a payload), so it carries no conjure name.
        Effect::CreateDelayedTrigger { effect, .. } => visit_ability_def(effect, visit)?,
        Effect::FlipCoin {
            win_effect,
            lose_effect,
            ..
        }
        | Effect::FlipCoins {
            win_effect,
            lose_effect,
            ..
        } => {
            if let Some(sub) = win_effect {
                visit_ability_def(sub, visit)?;
            }
            if let Some(sub) = lose_effect {
                visit_ability_def(sub, visit)?;
            }
        }
        Effect::FlipCoinUntilLose { win_effect } => visit_ability_def(win_effect, visit)?,
        Effect::RollDie { results, .. } => {
            for branch in results {
                visit_ability_def(&branch.effect, visit)?;
            }
        }
        Effect::ChooseOneOf { branches, .. } => {
            for branch in branches {
                visit_ability_def(branch, visit)?;
            }
        }
        // GenericEffect applies static abilities at resolution; their
        // modifications can grant abilities/triggers that themselves conjure.
        // Descend into the granted definitions rather than treating it as a leaf.
        Effect::GenericEffect {
            static_abilities, ..
        } => {
            for static_def in static_abilities {
                visit_static(static_def, visit)?;
            }
        }
        // Carries a nested ReplacementDefinition whose execute/decline/cost may conjure.
        Effect::AddTargetReplacement { replacement, .. } => visit_replacement(replacement, visit)?,
        // Counter's `source_rider` may apply a static to the countered source
        // (LosesAbilities) that grants an ability that conjures. The Destroy
        // rider carries no static.
        Effect::Counter { source_rider, .. } => {
            if let Some(CounterSourceRider::LosesAbilities { static_def, .. }) = source_rider {
                visit_static(static_def, visit)?;
            }
        }
        // Tokens and emblems can host granted static/triggered abilities that conjure.
        Effect::Token {
            static_abilities, ..
        } => {
            for static_def in static_abilities {
                visit_static(static_def, visit)?;
            }
        }
        Effect::CreateEmblem { statics, triggers } => {
            for static_def in statics {
                visit_static(static_def, visit)?;
            }
            for trigger in triggers {
                visit_trigger(trigger, visit)?;
            }
        }
        // Leaf effects with no nested ability/effect carrier.
        Effect::StartYourEngines { .. }
        | Effect::ChangeSpeed { .. }
        | Effect::DealDamage { .. }
        | Effect::ApplyPostReplacementDamage { .. }
        // CR 120.1: leaf effect — the source/recipient filters carry no nested
        // ability or effect to walk.
        | Effect::EachDealsDamageEqualToPower { .. }
        | Effect::EachSourceDealsDamage { .. }
        | Effect::Draw { .. }
        | Effect::Pump { .. }
        | Effect::PairWith { .. }
        | Effect::Destroy { .. }
        | Effect::Regenerate { .. }
        | Effect::RemoveAllDamage { .. }
        | Effect::CounterAll { .. }
        | Effect::GainLife { .. }
        | Effect::LoseLife { .. }
        | Effect::ExchangeLifeWithStat { .. }
        | Effect::ExchangeLifeTotals { .. }
        // CR 701.26a/b: all tap/untap scopes are leaf effects here.
        | Effect::SetTapState { .. }
        | Effect::RemoveCounter { .. }
        | Effect::Sacrifice { .. }
        | Effect::DiscardCard { .. }
        | Effect::Mill { .. }
        | Effect::Scry { .. }
        | Effect::PumpAll { .. }
        | Effect::DamageAll { .. }
        | Effect::DamageEachPlayer { .. }
        | Effect::DestroyAll { .. }
        | Effect::ChangeZone { .. }
        | Effect::ChangeZoneAll { .. }
        | Effect::Dig { .. }
        | Effect::GainControl { .. }
        | Effect::GainControlAll { .. }
        | Effect::ControlNextTurn { .. }
        | Effect::Attach { .. }
        | Effect::UnattachAll { .. }
        | Effect::Surveil { .. }
        | Effect::Fight { .. }
        | Effect::Bounce { .. }
        | Effect::BounceAll { .. }
        | Effect::Explore
        | Effect::ExploreAll { .. }
        | Effect::Investigate
        | Effect::Tribute { .. }
        | Effect::TimeTravel
        | Effect::BecomeMonarch
        | Effect::NoOp
        | Effect::Proliferate
        | Effect::ProliferateTarget { .. }
        | Effect::EndTheTurn
        | Effect::EndCombatPhase
        | Effect::Populate
        | Effect::Clash
        | Effect::Behold { .. }
        | Effect::SwitchPT { .. }
        | Effect::CopySpell { .. }
        | Effect::EpicCopy { .. }
        | Effect::CastCopyOfCard { .. }
        | Effect::CopyTokenOf { .. }
        // owner/type_filter are TargetFilters; no nested ability carrier and the
        // copy source comes from the format pool, so this is a leaf for conjure
        // collection.
        | Effect::CreateTokenCopyFromPool { .. }
        | Effect::Myriad
        | Effect::Encore
        | Effect::ExileHaunting { .. }
        | Effect::HideawayConceal { .. }
        | Effect::CopyTokenBlockingAttacker { .. }
        | Effect::BecomeCopy { .. }
        // CR 707.2c (Metamorphic Alteration): filter-only copy choice; no nested
        // ability carrier to walk — a leaf for printed-card collection.
        | Effect::ChoosePermanent { .. }
        | Effect::GainActivatedAbilitiesOfTarget { .. }
        | Effect::ChooseCard { .. }
        | Effect::PutCounter { .. }
        | Effect::PutCounterAll { .. }
        | Effect::MultiplyCounter { .. }
        // Builds its PutCounter/RemoveCounter branches at resolution — carries no
        // static conjure name to preload.
        | Effect::ChooseCounterAdjustment { .. }
        | Effect::DoublePT { .. }
        | Effect::DoublePTAll { .. }
        | Effect::MoveCounters { .. }
        | Effect::Animate { .. }
        | Effect::RegisterBending { .. }
        | Effect::Cleanup { .. }
        | Effect::Mana { .. }
        | Effect::Discard { .. }
        | Effect::Shuffle { .. }
        | Effect::Transform { .. }
        // CR 710.4: no nested ability carrier and no conjured card name.
        | Effect::FlipPermanent { .. }
        | Effect::SearchLibrary { .. }
        | Effect::SearchOutsideGame { .. }
        | Effect::RevealHand { .. }
        | Effect::Reveal { .. }
        | Effect::RevealTop { .. }
        | Effect::ExileTop { .. }
        | Effect::ExileFaceDownPile { .. }
        | Effect::TargetOnly { .. }
        | Effect::Choose { .. }
        | Effect::OpponentGuess { .. }
        | Effect::SwapChosenLabels { .. }
        | Effect::ChooseDamageSource { .. }
        | Effect::Suspect { .. }
        | Effect::Unsuspect { .. }
        | Effect::Connive { .. }
        | Effect::PhaseOut { .. }
        | Effect::PhaseIn { .. }
        | Effect::ForceBlock { .. }
        | Effect::ForceAttack { .. }
        | Effect::SolveCase
        | Effect::BecomePrepared { .. }
        | Effect::BecomeUnprepared { .. }
        | Effect::BecomeSaddled { .. }
        | Effect::BecomeBlocked { .. }
        | Effect::SetClassLevel { .. }
        | Effect::AddRestriction { .. }
        | Effect::ReduceNextSpellCost { .. }
        | Effect::GrantNextSpellAbility { .. }
        | Effect::AddPendingETBCounters { .. }
        | Effect::AddPendingEntersModifications { .. }
        | Effect::PayCost { .. }
        | Effect::CastFromZone { .. }
        | Effect::FreeCastFromZones { .. }
        | Effect::ExileResolvingSpellInsteadOfGraveyard { .. }
        | Effect::PreventDamage { .. }
        | Effect::LoseTheGame { .. }
        | Effect::WinTheGame { .. }
        | Effect::RingTemptsYou
        | Effect::VentureIntoDungeon
        | Effect::VentureInto { .. }
        | Effect::TakeTheInitiative
        | Effect::ArrangePlanarDeckTop { .. }
        | Effect::Planeswalk
        | Effect::ChaosEnsues
        | Effect::RedistributeLifeTotals
        | Effect::ReverseTurnOrder
        | Effect::OpenAttractions { .. }
        | Effect::RollToVisitAttractions
        | Effect::AssembleContraptions { .. }
        | Effect::AssembleContraptionsFromRollDifference
        | Effect::CrankContraptions { .. }
        | Effect::ReassembleContraption { .. }
        | Effect::AssembleContraptionOnSprocket { .. }
        | Effect::ReassembleContraptionOnSprocket { .. }
        | Effect::PutSticker { .. }
        | Effect::ApplySticker { .. }
        | Effect::ProcessRadCounters
        | Effect::GrantCastingPermission { .. }
        | Effect::ChooseFromZone { .. }
        | Effect::RememberCard { .. }
        | Effect::NoteManaSpent
        | Effect::ForEachCategory { .. }
        | Effect::ChooseObjectsIntoTrackedSet { .. }
        | Effect::ChooseAndSacrificeRest { .. }
        | Effect::EachPlayerCopyChosen { .. }
        | Effect::Exploit { .. }
        | Effect::GainEnergy { .. }
        | Effect::GivePlayerCounter { .. }
        | Effect::LoseAllPlayerCounters { .. }
        | Effect::ExileFromTopUntil { .. }
        | Effect::RevealUntil { .. }
        | Effect::Discover { .. }
        | Effect::Cascade
        | Effect::Ripple { .. }
        | Effect::MiracleCast { .. }
        | Effect::MadnessCast { .. }
        | Effect::PutAtLibraryPosition { .. }
        | Effect::ChooseDrawnThisTurnPayOrTopdeck { .. }
        | Effect::PutOnTopOrBottom { .. }
        | Effect::GiftDelivery { .. }
        | Effect::Goad { .. }
        | Effect::GoadAll { .. }
        | Effect::Detain { .. }
        | Effect::SetRoomDoorLock { .. }
        | Effect::ExchangeControl { .. }
        | Effect::ChangeTargets { .. }
        | Effect::Manifest { .. }
        | Effect::ManifestDread
        | Effect::Cloak { .. }
        | Effect::ExtraTurn { .. }
        | Effect::GrantExtraLoyaltyActivations { .. }
        | Effect::SkipNextTurn { .. }
        | Effect::SkipNextStep { .. }
        | Effect::AdditionalPhase { .. }
        | Effect::Double { .. }
        | Effect::RuntimeHandled { .. }
        | Effect::Incubate { .. }
        | Effect::Amass { .. }
        | Effect::Monstrosity { .. }
        | Effect::Renown { .. }
        | Effect::Bolster { .. }
        | Effect::Adapt { .. }
        | Effect::Learn
        | Effect::Forage
        | Effect::Harness
        | Effect::CollectEvidence { .. }
        | Effect::Endure { .. }
        | Effect::BlightEffect { .. }
        | Effect::Seek { .. }
        | Effect::SetLifeTotal { .. }
        | Effect::SetDayNight { .. }
        | Effect::GiveControl { .. }
        | Effect::RemoveFromCombat { .. }
        | Effect::CreateDamageReplacement { .. }
        | Effect::CombineHost { .. }
        | Effect::ChooseAugmentAndCombineWithHost { .. }
        // CR 614.12 + CR 303.4: ReturnAsAura.grants carry typed
        // ContinuousModifications, never conjured card names.
        | Effect::ReturnAsAura { .. }
        | Effect::Specialize
        // CR 608.2d + CR 122.1: counter-kind choice / consume carry no conjure names.
        | Effect::ChooseCounterKind { .. }
        | Effect::PutChosenCounter { .. }
        | Effect::Unimplemented { .. } => {}
    }
    ControlFlow::Continue(())
}
