//! CR 608.2d + CR 616.1 + CR 732.2a — resolution-time choice-freeness.
//!
//! Split out of `game/ability_scan.rs`, which the module header defines as *"a
//! single compiler-exhaustive, wildcard-free walk of a resolved ability's typed
//! AST"* and which contains ZERO references to `GameState` in any form. Probing a
//! resolution needs a board, so threading `&GameState` into that file would break
//! its stated contract. `ResolutionChoiceFreedom`'s own doc already flagged the
//! mismatch: it *"classifies RESOLVER prompting behavior, not AST reads"*.
//!
//! The verdict carries the EVENTS the resolution proposes, not a class name. A
//! name-derived replacement class map was fail-open in two independent ways —
//! it missed defs drawn through a second registry key, and it could not see a
//! virtual candidate at all, because a virtual has no `ReplacementDefinition`.

use crate::game::engine::SimulationProbeGuard;
use crate::game::{effects, replacement};
use crate::types::ability::{
    Effect, QuantityExpr, RepeatContinuation, ResolvedAbility, TargetChoiceTiming,
};
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::proposed_event::ProposedEvent;

use crate::analysis::resource::ProbeBudget;

/// CR 732.2a + CR 608.2d: resolution-time choice-freeness verdict for the
/// growing-cascade cover gate (`analysis::resource` item 6). NOT an
/// `ability_scan::Axes` axis — this classifies RESOLVER prompting behavior, not
/// AST reads.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum ResolutionChoiceFreedom {
    /// The exact `ProposedEvent`s this resolution runs through the replacement
    /// pipeline, DERIVED by running the real resolver on a throwaway clone.
    ///
    /// The caller discharges CR 616.1 against the pipeline's own candidate
    /// authority (`replacement::proposed_event_prompt_cause`) — never against a
    /// name-derived class map, which was fail-open: a
    /// `ProposedEvent::CreateToken` draws `ReplacementEvent::ChangeZone` defs
    /// too, and no def scan sees a virtual candidate.
    ///
    /// NEVER EMPTY: `probe_resolution` returns `Prompted` on an empty
    /// derivation, so a caller's `events.iter().any(..)` can never discharge
    /// vacuously.
    FreeUnlessReplacements(Vec<ProposedEvent>),
    /// May prompt, or unproven — the fail-closed default.
    MayPrompt,
}

// A `join` combinator lived here, concatenating the event sets of a root verdict and
// the separately-probed `sub_ability` / `else_ability` verdicts. It is deleted rather
// than kept-and-allowed: with the probe running once at the chain root, there are no
// sibling verdicts left to combine, and the union it performed was the mechanism by
// which events from the NOT-taken branch entered the derived set. Keeping a dead
// combinator that documents superseded semantics is how the next reader concludes the
// union still happens.

/// CR 616.1 + CR 614.1a: what ONE resolution asks of the player, observed by
/// RUNNING the real resolver on a throwaway clone. Never a hand-written
/// per-effect event list — measured, one `Effect::DealDamage` resolution
/// proposes `{Damage, LifeLoss}` (CR 120.3a) and one `Effect::Draw` proposes
/// `{Draw, ZoneChange}` (CR 121.1); a hand list omitted both companions.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum ResolutionProbe {
    /// The resolver ran without opening a prompt of its own. These are EVERY
    /// event it handed to the replacement pipeline.
    Events(Vec<ProposedEvent>),
    /// The resolver parked — an intrinsic prompt, or a replacement that already
    /// prompts on THIS board, or a budget/accounting refusal. Fail-closed.
    Prompted,
}

/// CR 616.1 + CR 614.1a: run `a`'s whole chain on a clone of `state` and report
/// what it asked of the player.
///
/// `state` must be the RESOLUTION BOARD the caller built — the entry removed and
/// `stack::bind_resolution_scope(..)` run on it, so CR 608.2k / CR 603.2c /
/// CR 706.2 resolution scope is bound and the CR 603.4 intervening-if has
/// already been re-checked. Handing this function a raw pre-resolution
/// `GameState` resolves every `EventContextAmount` / `Triggering*` reference
/// against an absent context and is FAIL-OPEN for the `> 0`-gated virtual
/// replacement arms.
pub(crate) fn probe_resolution(
    state: &GameState,
    a: &ResolvedAbility,
    budget: &mut ProbeBudget,
) -> ResolutionProbe {
    // CR 732.2a: BUDGET-EXCEEDED ⇒ `Prompted`. Cost is a COVERAGE knob, never a
    // soundness knob — an unaffordable probe degrades to honest-red (no
    // certificate, no offer), never to a wrong certificate and never to an
    // unbounded stall. Same shape as the `is_empty ⇒ Prompted` arm below.
    if !budget.try_charge_one() {
        return ResolutionProbe::Prompted;
    }
    // The SAME re-entrancy guard `apply_confirmed_shortcut` holds across its own
    // clone-and-drive. It suppresses ring accumulation and shortcut detection
    // inside the probe, which is exactly the recursion hazard a speculative
    // resolve would otherwise open.
    let _probe = SimulationProbeGuard::enter();
    let mut work = state.clone();
    let events = replacement::record_proposed_events(|| {
        let mut ev = Vec::new();
        // `resolve_ability_chain`, NOT `resolve_effect`: the chain is the
        // documented production entry and is where `optional`, the chained
        // sub-abilities and the `repeat_for` / `RepeatDecision` prompt sites
        // live. `resolve_effect` is the reserved-for-tests entry and is blind to
        // all of them.
        let _ = effects::resolve_ability_chain(&mut work, a, &mut ev, 0);
    });
    // CR 732.2a: an unanswered prompt is a choice the certificate cannot
    // describe.
    //
    // KEYED ON "IS THERE A PROMPT AT ALL", not merely on "does it differ from the
    // incoming variant". The struck form compared only `WaitingFor` DISCRIMINANTS: if
    // the incoming board already carried a non-priority variant and the resolver parked
    // a NEW prompt of that SAME variant, the discriminants matched and the resolution
    // was reported CHOICE-FREE. That is fail-open in the one direction this function
    // exists to close, and comparing against the incoming variant can never see it —
    // the incoming variant is exactly what masks it.
    //
    // The incoming board is a RESOLUTION BOARD (see this function's contract above), so
    // a non-priority `waiting_for` on entry is itself a reason to refuse rather than a
    // baseline to compare against. The discriminant test is KEPT alongside, so a board
    // that entered at priority and left at a different variant still refuses.
    //
    // Strictly stronger than the struck form on every input, so it can only ever cost
    // COVERAGE (a missed offer), never soundness — the same direction as the
    // budget-exceeded and empty-derivation arms around it.
    if !matches!(work.waiting_for, WaitingFor::Priority { .. })
        || std::mem::discriminant(&work.waiting_for) != std::mem::discriminant(&state.waiting_for)
    {
        return ResolutionProbe::Prompted;
    }
    // FAIL-CLOSED on an empty set. Measured: every empty derivation observed was
    // an entry whose targets were not yet announced, and the resolver still
    // returned `Ok` — so the `Result` does NOT discriminate "proposes nothing"
    // from "could not run".
    if events.is_empty() {
        return ResolutionProbe::Prompted;
    }
    // CR 732.2a: an event whose board effect the certificate cannot account for
    // is a choice surface the certificate cannot describe. `event_is_accounted`
    // is the SAME exhaustive, wildcard-free partition the completeness witness
    // uses — one function, two callers — so the resolver and the witness cannot
    // drift about which variants are accounted.
    if events.iter().any(|ev| !replacement::event_is_accounted(ev)) {
        return ResolutionProbe::Prompted;
    }
    ResolutionProbe::Events(events)
}

/// The one adapter every allow-listed arm funnels through: `Prompted ⇒
/// MayPrompt`, `Events(v) ⇒ FreeUnlessReplacements(v)`.
fn resolution_probe_verdict(
    state: &GameState,
    ability: &ResolvedAbility,
    budget: &mut ProbeBudget,
) -> ResolutionChoiceFreedom {
    match probe_resolution(state, ability, budget) {
        ResolutionProbe::Prompted => ResolutionChoiceFreedom::MayPrompt,
        ResolutionProbe::Events(events) => ResolutionChoiceFreedom::FreeUnlessReplacements(events),
    }
}

/// CR 608.2d: is this `QuantityExpr` a CR 608.2d "up to N" count — a magnitude
/// the resolving player picks rather than one the state determines?
///
/// Recursive and wildcard-free for the same reason the classifier it serves is:
/// a NEW `QuantityExpr` variant must be classified before it compiles, so an
/// `UpTo` can never be smuggled in under a newly-added wrapper.
///
/// MEASURED DISCREPANCY, and the reason this guard exists at all:
/// `game/quantity.rs` resolves `UpTo { max } => recurse(max)` — it SILENTLY
/// ANSWERS the CR 107.1c / CR 608.2d resolution-time count choice as the maximum
/// rather than surfacing it. Only the resolvers that call
/// `QuantityExpr::peel_up_to` honour the flag, and none of the six allow-listed
/// classes does. So an unguarded arm would probe choice-free on an ability whose
/// resolution opens a count prompt.
fn quantity_offers_up_to_choice(q: &QuantityExpr) -> bool {
    match q {
        QuantityExpr::UpTo { .. } => true,
        QuantityExpr::Fixed { .. } | QuantityExpr::Ref { .. } => false,
        QuantityExpr::DivideRounded { inner, .. }
        | QuantityExpr::Offset { inner, .. }
        | QuantityExpr::ClampMin { inner, .. }
        | QuantityExpr::Multiply { inner, .. } => quantity_offers_up_to_choice(inner),
        // `base` is an `i32`, not a `Box<QuantityExpr>` (see `types/ability.rs`), so it
        // is structurally incapable of carrying an `UpTo` and needs no recursion. Stated
        // because the asymmetry with `Difference`/`Sum`/`Max` reads like an omission.
        QuantityExpr::Power { exponent, base: _ } => quantity_offers_up_to_choice(exponent),
        QuantityExpr::Difference { left, right } => {
            quantity_offers_up_to_choice(left) || quantity_offers_up_to_choice(right)
        }
        QuantityExpr::Sum { exprs } | QuantityExpr::Max { exprs } => {
            exprs.iter().any(quantity_offers_up_to_choice)
        }
    }
}

/// CR 608.2d: can resolving this single `Effect` ever offer a resolution-time
/// player choice? Exhaustive `match` with NO wildcard catch-all arm — a NEW
/// `Effect` variant fails to compile here until it is classified.
///
/// THE ALLOW LIST IS A SCOPE/COST FILTER, NOT A SOUNDNESS CARRIER. At HEAD each
/// arm carried a hand-audited "its only prompt is `ReplacementResult::NeedsChoice`"
/// claim in its doc comment. That is a hand enumeration, and it went stale the
/// moment a resolver gained a prompt. The soundness is now carried by
/// `ResolutionProbe::Prompted`, which OBSERVES the real resolver opening a real
/// prompt on the real board. The arm only decides which classes are worth paying
/// a clone for.
fn effect_offers_choice(e: &Effect) -> bool {
    match e {
        // ---- SCOPE FILTER. DESTRUCTURED WITHOUT `..` on every arm, exactly as
        //      HEAD's three allow arms are, so a new field on any of them forces
        //      a re-audit of whether the class is still in scope.
        //      ⚠ `..` IS FORBIDDEN ON EVERY ARM BELOW. If a new `Effect` field
        //      makes one of them stop compiling, THAT E0027 IS THE GUARD FIRING
        //      — it is not a compile error to silence. rustc's own diagnostic
        //      prints a `help:` suggesting `..`, which silently disarms the
        //      guard. Classify the new field against the scope filter, then name
        //      it `_`. The 14-field `Effect::Token` arm is the one that maximally
        //      invites the `..`.
        //
        //      CR 107.1c + CR 608.2d: THE `UpTo` GUARD IS ON *EVERY*
        //      QUANTITY-CARRYING ARM, not only `Draw`. All six allow-listed arms
        //      carry a `QuantityExpr` at their count/amount field, and
        //      `game/quantity.rs` answers `UpTo` as the maximum instead of
        //      prompting — so an unguarded arm would report choice-free on an
        //      ability whose resolution opens a count choice. DIRECTION:
        //      fail-closed. It can only turn a probe verdict into `MayPrompt`,
        //      never the reverse. ----
        Effect::GainLife { amount, player: _ }
        | Effect::LoseLife { amount, target: _ }
        | Effect::DealDamage {
            amount,
            target: _,
            damage_source: _,
            excess: _,
        } => {
            quantity_offers_up_to_choice(amount)
        }
        Effect::PutCounter {
            target: _,
            counter_type: _,
            count,
        }
        | Effect::Token {
            name: _,
            power: _,
            toughness: _,
            types: _,
            colors: _,
            keywords: _,
            tapped: _,
            count,
            owner: _,
            attach_to: _,
            enters_attacking: _,
            supertypes: _,
            static_abilities: _,
            enter_with_counters: _,
        } => {
            quantity_offers_up_to_choice(count)
        }
        // HEAD's own arm shape, kept verbatim (single arm + inner `if`, no match
        // guard). CR 608.2d: an "up to N" draw is a resolution-time COUNT choice
        // the probe would ANSWER rather than surface, because the count is read,
        // not prompted, inside `draw::resolve`.
        Effect::Draw { count, target: _ } => {
            quantity_offers_up_to_choice(count)
        }
        // ---- everything else: fail-closed MayPrompt. HEAD's named list
        //      VERBATIM, minus the three variants promoted above
        //      (`DealDamage`, `PutCounter`, `Token`), and with NO wildcard, so
        //      the compiler still enforces exhaustiveness and a new `Effect`
        //      variant fails to compile until it is classified. ----
        Effect::StartYourEngines { .. }
        | Effect::ChangeSpeed { .. }
        | Effect::ApplyPostReplacementDamage { .. }
        | Effect::EachDealsDamageEqualToPower { .. }
        | Effect::OpponentGuess { .. }
        | Effect::SwapChosenLabels { .. }
        | Effect::Pump { .. }
        | Effect::PairWith { .. }
        | Effect::Destroy { .. }
        | Effect::Regenerate { .. }
        | Effect::RemoveAllDamage { .. }
        | Effect::Counter { .. }
        | Effect::CounterAll { .. }
        | Effect::SetTapState { .. }
        | Effect::RemoveCounter { .. }
        | Effect::ChooseCounterKind { .. }
        | Effect::PutChosenCounter { .. }
        | Effect::Sacrifice { .. }
        | Effect::DiscardCard { .. }
        | Effect::Mill { .. }
        | Effect::Scry { .. }
        | Effect::PumpAll { .. }
        | Effect::DamageAll { .. }
        | Effect::DamageEachPlayer { .. }
        | Effect::EachPlayerCopyChosen { .. }
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
        | Effect::Populate
        | Effect::Clash
        // CR 701.4a + CR 608.2d: behold may prompt (`WaitingFor::BeholdChoice`
        // when 2+ candidates) — fail-closed MayPrompt.
        | Effect::Behold { .. }
        | Effect::EndTheTurn
        | Effect::EndCombatPhase
        | Effect::Vote { .. }
        | Effect::SeparateIntoPiles { .. }
        | Effect::SwitchPT { .. }
        | Effect::CopySpell { .. }
        | Effect::EpicCopy { .. }
        | Effect::CastCopyOfCard { .. }
        | Effect::CopyTokenOf { .. }
        | Effect::CreateTokenCopyFromPool { .. }
        | Effect::Myriad
        | Effect::Encore
        | Effect::CombineHost { .. }
        | Effect::ChooseAugmentAndCombineWithHost { .. }
        | Effect::Meld { .. }
        | Effect::ExileHaunting { .. }
        | Effect::HideawayConceal { .. }
        | Effect::CopyTokenBlockingAttacker { .. }
        | Effect::BecomeCopy { .. }
        // CR 707.6: an object entering as a copy of a permanent does NOT inherit the
        // original's choices — its controller makes the "as [this] enters" choices
        // anew. That fresh choice is what raises `WaitingFor::CopyTargetChoice`, so
        // this is a fail-closed MayPrompt (never resolved through the normal chain,
        // but classified here to keep the match exhaustive).
        | Effect::ChoosePermanent { .. }
        | Effect::GainActivatedAbilitiesOfTarget { .. }
        | Effect::ChooseCard { .. }
        | Effect::PutCounterAll { .. }
        | Effect::MultiplyCounter { .. }
        | Effect::DoublePT { .. }
        | Effect::DoublePTAll { .. }
        | Effect::MoveCounters { .. }
        | Effect::Animate { .. }
        | Effect::ReturnAsAura { .. }
        | Effect::RegisterBending { .. }
        | Effect::GenericEffect { .. }
        | Effect::Cleanup { .. }
        | Effect::Mana { .. }
        | Effect::Discard { .. }
        | Effect::Shuffle { .. }
        | Effect::Transform { .. }
        // CR 710.4: `flip_permanent` offers no resolution-time choice (it is a
        // status change or a silent no-op), exactly like `Transform`.
        | Effect::FlipPermanent { .. }
        | Effect::SearchLibrary { .. }
        | Effect::SearchOutsideGame { .. }
        | Effect::RevealHand { .. }
        | Effect::RevealFromHand { .. }
        | Effect::Reveal { .. }
        | Effect::RevealTop { .. }
        | Effect::ExileTop { .. }
        | Effect::ExileFaceDownPile { .. }
        | Effect::TargetOnly { .. }
        | Effect::Choose { .. }
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
        | Effect::CreateDelayedTrigger { .. }
        | Effect::AddTargetReplacement { .. }
        | Effect::AddRestriction { .. }
        | Effect::ReduceNextSpellCost { .. }
        | Effect::GrantNextSpellAbility { .. }
        | Effect::AddPendingETBCounters { .. }
        | Effect::AddPendingEntersModifications { .. }
        | Effect::CreateEmblem { .. }
        | Effect::PayCost { .. }
        | Effect::CastFromZone { .. }
        | Effect::FreeCastFromZones { .. }
        | Effect::ExileResolvingSpellInsteadOfGraveyard { .. }
        | Effect::PreventDamage { .. }
        | Effect::CreateDamageReplacement { .. }
        | Effect::CreateDrawReplacement { .. }
        | Effect::LoseTheGame { .. }
        | Effect::WinTheGame { .. }
        | Effect::RollDie { .. }
        | Effect::FlipCoin { .. }
        | Effect::FlipCoins { .. }
        | Effect::FlipCoinUntilLose { .. }
        | Effect::RingTemptsYou
        | Effect::VentureIntoDungeon
        | Effect::VentureInto { .. }
        | Effect::TakeTheInitiative
        | Effect::ArrangePlanarDeckTop { .. }
        | Effect::Planeswalk
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
        | Effect::ForEachCategory { .. }
        | Effect::ChooseObjectsIntoTrackedSet { .. }
        | Effect::ChooseAndSacrificeRest { .. }
        | Effect::Exploit { .. }
        | Effect::GainEnergy { .. }
        | Effect::GivePlayerCounter { .. }
        | Effect::LoseAllPlayerCounters { .. }
        | Effect::ExileFromTopUntil { .. }
        | Effect::RevealUntil { .. }
        | Effect::Discover { .. }
        | Effect::Heist { .. }
        | Effect::HeistExile
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
        | Effect::TurnFaceUp { .. }
        | Effect::TurnFaceDown { .. }
        | Effect::ExtraTurn { .. }
        | Effect::GrantExtraLoyaltyActivations { .. }
        | Effect::SkipNextTurn { .. }
        | Effect::SkipNextStep { .. }
        | Effect::AdditionalPhase { .. }
        | Effect::Double { .. }
        | Effect::EachSourceDealsDamage { .. }
        | Effect::RuntimeHandled { .. }
        | Effect::Incubate { .. }
        | Effect::Amass { .. }
        | Effect::Monstrosity { .. }
        | Effect::Specialize
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
        | Effect::ExchangeLifeWithStat { .. }
        | Effect::ExchangeLifeTotals { .. }
        | Effect::SetDayNight { .. }
        | Effect::GiveControl { .. }
        | Effect::RemoveFromCombat { .. }
        | Effect::Conjure { .. }
        | Effect::ApplyPerpetual { .. }
        | Effect::Intensify { .. }
        | Effect::DraftFromSpellbook { .. }
        | Effect::ChooseCounterAdjustment { .. }
        | Effect::CreatePlaneswalkReplacement { .. }
        | Effect::ChaosEnsues
        | Effect::RedistributeLifeTotals
        | Effect::ReverseTurnOrder
        | Effect::ChooseOneOf { .. }
        | Effect::Unimplemented { .. } => true,
    }
}

/// CR 608.2d + CR 732.2a: can resolving this ability's whole chain enter a
/// resolution-time player choice?
///
/// PURE AST WALK — no `GameState`, no clone, no budget. That is what lets the
/// recursion stay exhaustive over `sub_ability` / `else_ability` while the
/// expensive probe runs exactly ONCE, at the chain root, in
/// [`ability_resolution_choice_freedom`].
///
/// The `ResolvedAbility` destructure is EXHAUSTIVE with no `..` —
/// `ability_scan::resolved_ability_axes`'s classifications are deliberately NOT
/// reused (this is a different question: e.g. `optional` is read-free yet
/// choice-bearing). A FUTURE field fails to compile here until classified for the
/// choice question.
pub(crate) fn chain_offers_choice(a: &ResolvedAbility) -> bool {
    let ResolvedAbility {
        // ---- choice-bearing: folded into the verdict below ----
        effect,
        sub_ability,
        else_ability,
        optional,
        optional_for,
        optional_targeting,
        unless_pay,
        target_chooser,
        target_choice_timing,
        modal,
        mode_abilities,
        repeat_until,
        repeat_for,
        // ---- choice-free: bound `_` with a one-line justification ----
        condition: _, // resolution branch selector, pure eval (both branches gate-checked below)
        duration: _,  // continuous-effect lifetime, no prompt
        player_scope: _, // iteration fan-out, pure player-filter eval
        starting_with: _, // APNAP start override, no prompt
        announced_x: _, // CR 601.2b announce-time count, pure quantity eval, no prompt
        multi_target: _, // announce-time variable-count bounds (Resolution case caught by timing)
        target_constraints: _, // announce-time cross-target legality, no resolution prompt
        distribution: _, // CR 601.2d concrete pre-assigned portions (announce-time)
        targets: _,   // concrete announced target refs (already resolved)
        source_id: _, // object id
        source_incarnation: _, // self-transform epoch latch, no resolution-time choice
        trigger_source: _, // exact triggered-source authority, no choice
        trigger_definition_ref: _, // exact trigger occurrence, no choice
        force_block_attacker: _, // exact force-block referent, no choice
        controller: _, // player id
        original_controller: _, // player id
        scoped_player: _, // player id (iteration binding)
        kind: _,      // AbilityKind tag (no payload)
        context: _,   // SpellContext: cast-time fact snapshot, not a live choice
        description: _, // display string
        selected_mode_labels: _, // display strings, no resolution-time choice
        min_x_value: _, // u32
        cant_be_copied: _, // bool
        copy_count_status: _, // status tag
        forward_result: _, // bool
        chosen_x: _,  // concrete cast-time X (chosen at announcement, not resolution)
        cost_paid_object: _, // concrete captured-object snapshot
        cost_paid_object_ids: _, // concrete captured-object ids (issue #4948)
        effect_context_object: _, // concrete captured-object snapshot
        amassed_army_object: _, // concrete captured-object snapshot
        ability_index: _, // usize provenance
        may_trigger_origin: _, // provenance tag
        target_selection_mode: _, // Chosen/Random tag (announce-time)
        chosen_players: _, // concrete chosen player ids (already selected)
        replacement_applied: _, // replacement provenance set, no prompt
        sub_link: _,  // SubAbilityLink kind tag
        sibling_condition: _, // SiblingCondition replication marker, no resolution-time choice
        parent_target_missing_reason: _, // seam flag
    } = a;

    // CR 603.5 + CR 608.2d: an optional effect / optional targeting /
    // opponent-may effect prompts the controller (or opponent) before execution.
    if *optional || *optional_targeting || optional_for.is_some() {
        return true;
    }
    // CR 118.12: "unless a player pays {cost}" is a resolution-time pay prompt.
    if unless_pay.is_some() {
        return true;
    }
    // CR 601.2c + CR 603.3d: a resolution-time target chooser announces targets.
    if target_chooser.is_some() {
        return true;
    }
    // CR 608.2d: resolution-timed target selection is a resolution-time choice
    // even though `targets` is empty on the stack.
    if matches!(target_choice_timing, TargetChoiceTiming::Resolution) {
        return true;
    }
    // CR 700.2b + CR 603.3c: a modal header / reflexive per-mode abilities open a
    // mode choice at resolution (conservative — rejected even when the mode is
    // baked).
    if modal.is_some() || !mode_abilities.is_empty() {
        return true;
    }
    // CR 608.2c + CR 107.1c: only the controller-prompted repeat variant is a
    // player choice; while / until-stop predicates are pure re-evaluation.
    if matches!(repeat_until, Some(RepeatContinuation::ControllerChoice)) {
        return true;
    }
    // CR 608.2d + CR 107.1c: an "up to N" REPEAT COUNT is a resolution-time choice
    // exactly like an "up to N" damage/draw/counter count, and it is answered in the
    // same silent way — `game/quantity.rs` resolves `UpTo { max } => recurse(max)`,
    // taking the maximum, and none of the six allow-listed classes calls
    // `QuantityExpr::peel_up_to`. This field was previously bound `_` and justified as
    // "pure quantity eval (game/quantity.rs)", which cites the very mechanism
    // `quantity_offers_up_to_choice`'s own doc comment exists to distrust: the count is
    // READ, not prompted. An allow-listed repeated ability carrying an `UpTo` repeat
    // count was therefore probed choice-free and admitted to a loop certificate while
    // its resolution opens a count prompt. Guarded with the SAME single authority the
    // per-effect quantity positions use, so a new `QuantityExpr` wrapper is classified
    // in one place rather than three.
    if repeat_for
        .as_ref()
        .is_some_and(quantity_offers_up_to_choice)
    {
        return true;
    }

    // CR 608.2c: gate-check the whole chain — this node's effect, plus the
    // sub_ability / else_ability branches. Recursion is retained HERE, where it is a
    // pure AST walk costing nothing, and dropped from the probe below.
    effect_offers_choice(effect)
        || sub_ability.as_deref().is_some_and(chain_offers_choice)
        || else_ability.as_deref().is_some_and(chain_offers_choice)
}

/// CR 608.2d + CR 732.2a: the resolution-choice verdict for a whole ability chain.
///
/// TWO PHASES, and the split is the point. [`chain_offers_choice`] walks the entire
/// chain as pure AST — free, so it stays exhaustive over both branches. Only if the
/// whole chain is gate-clean does the expensive half run, and then exactly once, at
/// the ROOT: `probe_resolution` drives `resolve_ability_chain`, which is the
/// production entry and already resolves the taken `sub_ability` / `else_ability`
/// branch itself. The previous shape probed the root AND recursed a probe into each
/// branch, so a chain of depth N paid N whole-`GameState` clones and re-resolved every
/// subchain the root resolution had already walked.
///
/// ⚠ ONE MEASURABLE CONSEQUENCE, stated rather than buried. The old form `join`ed the
/// branch verdicts, and `join` UNIONS event sets, so the accumulated set included
/// events from the branch NOT taken on this board. The single root probe reports only
/// what the resolution actually proposes. That is a smaller, more accurate set — the
/// old one described a resolution that cannot happen — but it IS a different set, so
/// any change in certification is a real delta and is reported with the change rather
/// than absorbed silently.
pub(crate) fn ability_resolution_choice_freedom(
    state: &GameState,
    a: &ResolvedAbility,
    budget: &mut ProbeBudget,
) -> ResolutionChoiceFreedom {
    if chain_offers_choice(a) {
        return ResolutionChoiceFreedom::MayPrompt;
    }
    resolution_probe_verdict(state, a, budget)
}

/// The RECORDER-FREE half of the completeness witness (R10′).
///
/// Every axis below is written by the engine whether or not any recorder
/// exists, which is the whole point: a witness built from the recorder would
/// share its deepest dependency with the thing it checks, and the check would be
/// an identity. Two of them are the delegating legs' own turn ledgers —
/// `Damage`'s player branch delegates its life write to a companion `LifeLoss`
/// (CR 120.3a) and `Draw` delegates every card to the zone pipeline (CR 121.1),
/// so a board-resource-only axis set is BLIND on both.
#[cfg(test)]
#[derive(Debug, Default, PartialEq, Eq, Clone)]
pub(crate) struct BoardAxes {
    life: std::collections::BTreeMap<crate::types::player::PlayerId, i64>,
    poison: std::collections::BTreeMap<crate::types::player::PlayerId, i64>,
    counters: i64,
    battlefield: i64,
    cards_drawn: std::collections::BTreeMap<crate::types::player::PlayerId, i64>,
    damage_records: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, EffectScope, ModalChoice, OpponentMayScope,
        PtValue, QuantityExpr, TapStateChange, TargetFilter, TargetRef, UnlessPayModifier,
    };
    use crate::types::counter::CounterType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::proposed_event::CounterPlacement;
    use crate::types::zones::Zone;
    use std::collections::BTreeMap;

    use crate::analysis::resource::{ProbeBudget, PROBE_BUDGET};

    fn budget() -> ProbeBudget {
        ProbeBudget::for_test(PROBE_BUDGET)
    }

    /// Snapshot every axis the witness reads.
    fn board_axes(state: &GameState) -> BoardAxes {
        BoardAxes {
            life: state
                .players
                .iter()
                .map(|p| (p.id, i64::from(p.life)))
                .collect(),
            poison: state
                .players
                .iter()
                .map(|p| (p.id, i64::from(p.poison_counters)))
                .collect(),
            counters: state
                .objects
                .values()
                .map(|o| o.counters.values().map(|n| i64::from(*n)).sum::<i64>())
                .sum(),
            battlefield: state.battlefield.len() as i64,
            cards_drawn: state
                .players
                .iter()
                .map(|p| (p.id, i64::from(p.cards_drawn_this_turn)))
                .collect(),
            damage_records: state.damage_dealt_this_turn.len() as i64,
        }
    }

    fn axis_delta(before: &BoardAxes, after: &BoardAxes) -> BoardAxes {
        let map_delta = |b: &BTreeMap<PlayerId, i64>, a: &BTreeMap<PlayerId, i64>| {
            let mut out = BTreeMap::new();
            for (id, av) in a {
                let d = av - b.get(id).copied().unwrap_or(0);
                if d != 0 {
                    out.insert(*id, d);
                }
            }
            out
        };
        BoardAxes {
            life: map_delta(&before.life, &after.life),
            poison: map_delta(&before.poison, &after.poison),
            counters: after.counters - before.counters,
            battlefield: after.battlefield - before.battlefield,
            cards_drawn: map_delta(&before.cards_drawn, &after.cards_drawn),
            damage_records: after.damage_records - before.damage_records,
        }
    }

    /// The RECORDER half's prediction. `None` ⇒ the derived set contains a
    /// variant whose board effect this witness cannot account for, which
    /// `probe_resolution` turns into `Prompted`.
    ///
    /// The accounted/unaccounted partition is NOT re-derived here: it is
    /// `replacement::event_is_accounted`, the same function `probe_resolution`
    /// reads, so the resolver and the witness cannot drift.
    fn predicted_axes(events: &[ProposedEvent]) -> Option<BoardAxes> {
        let mut axes = BoardAxes::default();
        for ev in events {
            if !replacement::event_is_accounted(ev) {
                return None;
            }
            match ev {
                ProposedEvent::LifeGain {
                    player_id, amount, ..
                } => *axes.life.entry(*player_id).or_default() += i64::from(*amount),
                ProposedEvent::LifeLoss {
                    player_id, amount, ..
                } => *axes.life.entry(*player_id).or_default() -= i64::from(*amount),
                ProposedEvent::ZoneChange { from, to, .. } => {
                    axes.battlefield +=
                        i64::from(*to == Zone::Battlefield) - i64::from(*from == Zone::Battlefield);
                }
                ProposedEvent::AddCounter {
                    placement, count, ..
                } => match placement {
                    CounterPlacement::Object { .. } => axes.counters += i64::from(*count),
                    CounterPlacement::Player {
                        player_id,
                        counter_kind,
                        ..
                    } => {
                        if matches!(
                            counter_kind,
                            crate::types::player::PlayerCounterKind::Poison
                        ) {
                            *axes.poison.entry(*player_id).or_default() += i64::from(*count);
                        }
                    }
                    CounterPlacement::Energy { .. } => {}
                },
                ProposedEvent::CreateToken { count, .. } => axes.battlefield += i64::from(*count),
                // CR 120.3a: the life write is the companion `LifeLoss`'s; this
                // variant's own axis is the damage ledger.
                ProposedEvent::Damage { .. } => axes.damage_records += 1,
                // CR 121.1: the zone write is the companion `ZoneChange`'s; this
                // variant's own axis is the draw ledger.
                ProposedEvent::Draw {
                    player_id, count, ..
                } => *axes.cards_drawn.entry(*player_id).or_default() += i64::from(*count),
                other => unreachable!(
                    "accounted variant with no axis arm — the partition and this witness \
                     have drifted: {other:?}"
                ),
            }
        }
        // Drop zeroed map entries so the prediction is delta-shaped like the
        // observation.
        axes.life.retain(|_, v| *v != 0);
        axes.poison.retain(|_, v| *v != 0);
        axes.cards_drawn.retain(|_, v| *v != 0);
        Some(axes)
    }

    /// A 2p board with a P0 source creature on the battlefield, one card in each
    /// library, and a P1 creature to damage / counter.
    fn probe_board() -> GameState {
        let mut state = GameState::new_two_player(7);
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Victim".to_string(),
            Zone::Battlefield,
        );
        for owner in [PlayerId(0), PlayerId(1)] {
            create_object(
                &mut state,
                CardId(3),
                owner,
                "Deck Card".to_string(),
                Zone::Library,
            );
        }
        state
    }

    fn source_id(state: &GameState) -> ObjectId {
        *state.battlefield.front().expect("source on battlefield")
    }

    fn victim_id(state: &GameState) -> ObjectId {
        *state.battlefield.get(1).expect("victim on battlefield")
    }

    fn ability(state: &GameState, effect: Effect, targets: Vec<TargetRef>) -> ResolvedAbility {
        ResolvedAbility::new(effect, targets, source_id(state), PlayerId(0))
    }

    fn fixed(n: i32) -> QuantityExpr {
        QuantityExpr::Fixed { value: n }
    }

    fn up_to(n: i32) -> QuantityExpr {
        QuantityExpr::UpTo {
            max: Box::new(fixed(n)),
        }
    }

    fn token_effect(count: QuantityExpr) -> Effect {
        Effect::Token {
            name: "Servo".to_string(),
            power: PtValue::Fixed(1),
            toughness: PtValue::Fixed(1),
            types: vec!["Creature".to_string()],
            colors: vec![],
            keywords: vec![],
            tapped: false,
            count,
            owner: TargetFilter::Controller,
            attach_to: None,
            enters_attacking: false,
            supertypes: vec![],
            static_abilities: vec![],
            enter_with_counters: vec![],
        }
    }

    /// The six allow-listed classes, each with a board that exercises it and the
    /// name the population conjunct reports.
    fn six_allow_listed_arms(state: &GameState) -> Vec<(&'static str, ResolvedAbility)> {
        vec![
            (
                "GainLife",
                ability(
                    state,
                    Effect::GainLife {
                        amount: fixed(3),
                        player: TargetFilter::Controller,
                    },
                    vec![],
                ),
            ),
            (
                "LoseLife",
                ability(
                    state,
                    Effect::LoseLife {
                        amount: fixed(2),
                        target: Some(TargetFilter::Controller),
                    },
                    vec![],
                ),
            ),
            (
                "DealDamage",
                ability(
                    state,
                    Effect::DealDamage {
                        amount: fixed(1),
                        target: TargetFilter::Any,
                        damage_source: None,
                        excess: None,
                    },
                    vec![TargetRef::Player(PlayerId(1))],
                ),
            ),
            (
                "PutCounter",
                ability(
                    state,
                    Effect::PutCounter {
                        target: TargetFilter::Any,
                        counter_type: CounterType::Plus1Plus1,
                        count: fixed(2),
                    },
                    vec![TargetRef::Object(victim_id(state))],
                ),
            ),
            ("Token", ability(state, token_effect(fixed(1)), vec![])),
            (
                "Draw",
                ability(
                    state,
                    Effect::Draw {
                        count: fixed(1),
                        target: TargetFilter::Controller,
                    },
                    vec![],
                ),
            ),
        ]
    }

    /// CR 732.2a — a prompt already parked on the incoming board must REFUSE the probe,
    /// even though the resolution leaves the variant unchanged.
    ///
    /// The struck guard compared only `WaitingFor` DISCRIMINANTS between the probed clone
    /// and the incoming board. When the incoming board already carries a non-priority
    /// variant, the two discriminants are equal for a resolution that re-parks the SAME
    /// variant (and, in the reduced form asserted here, for one that simply leaves the
    /// standing prompt in place) — so the probe reported CHOICE-FREE while an unanswered
    /// choice sat on the board. Fail-open, in the one direction this function closes.
    ///
    /// MATCHED PAIR, which is what makes this non-vacuous:
    /// * NEGATIVE arm — the same ability, same board, `waiting_for` parked at a real
    ///   resolution-time prompt (`ReplacementChoice`, CR 616.1) ⇒ MUST be `Prompted`.
    /// * POSITIVE arm — the same ability on the same board at `Priority` ⇒ MUST still
    ///   reach `Events`. Without it, a probe that refused EVERYTHING would pass the
    ///   negative arm, and the row would prove nothing.
    ///
    /// REVERT-PROBE (run, recorded): restore the guard to the bare
    /// `discriminant(&work.waiting_for) != discriminant(&state.waiting_for)` ⇒ the
    /// NEGATIVE arm FLIPS TO FAIL (the probe returns `Events`) while the POSITIVE arm
    /// stays green — i.e. the new conjunct, not the old one, is what carries this row.
    #[test]
    fn a_prompt_standing_on_the_incoming_board_refuses_the_probe() {
        let base = probe_board();
        let parked = WaitingFor::ReplacementChoice {
            player: PlayerId(0),
            candidate_count: 2,
            candidates: Vec::new(),
        };
        assert!(
            !matches!(base.waiting_for, WaitingFor::ReplacementChoice { .. }),
            "reach-guard: the parked variant must DIFFER from the board's own starting \
             variant, or the negative arm could pass for the wrong reason"
        );

        let mut refused = 0usize;
        for (name, a) in six_allow_listed_arms(&base) {
            // POSITIVE — at priority, this arm is known to reach the recorder.
            match probe_resolution(&base, &a, &mut budget()) {
                ResolutionProbe::Events(events) => assert!(
                    !events.is_empty(),
                    "{name}: positive control must derive a non-empty event set"
                ),
                ResolutionProbe::Prompted => panic!(
                    "{name}: POSITIVE CONTROL FAILED — this arm must still be probeable \
                     from a priority board, otherwise the negative arm below is vacuous"
                ),
            }

            // NEGATIVE — same ability, same board, a prompt already standing.
            let mut stalled = base.clone();
            stalled.waiting_for = parked.clone();
            // WHAT ESTABLISHES THE SAME-DISCRIMINANT CONDITION: the REVERT-PROBE, not an
            // assertion here. Comparing `stalled.waiting_for` against `parked` at this
            // point would compare a value to itself and prove nothing, and the probe's
            // internal `work` board is not observable from outside `probe_resolution`.
            // The probe supplies it empirically instead — under the struck guard this arm
            // returns `Events` rather than `Prompted`, which can only happen when the two
            // discriminants compared EQUAL, i.e. the resolution left the standing prompt
            // in place. That is exactly the masking condition this row is about.
            assert!(
                matches!(
                    probe_resolution(&stalled, &a, &mut budget()),
                    ResolutionProbe::Prompted
                ),
                "{name}: an UNANSWERED prompt on the incoming board is a choice the \
                 certificate cannot describe — the probe must refuse it, not read the \
                 equal discriminants as `unchanged`"
            );
            refused += 1;
        }
        assert_eq!(
            refused, 6,
            "reach-guard: the row must range over all six allow-listed arms"
        );
    }

    /// R10′ — THE AXIS-COMPLETENESS WITNESS, over ALL SIX allow-listed arms.
    ///
    /// Two INDEPENDENT clone-and-resolves of the same ability on the same board:
    /// ARM 1 derives the event set through the recorder; ARM 2 reads the axes
    /// before/after and touches no recorder, no `ProposedEvent`, no
    /// `replace_event`, no `pipeline_loop`. The two arms share
    /// `resolve_ability_chain` + `GameState` — the SUBJECT under test — and
    /// share NOTHING on the CHECKING side. That is the whole point: a check that
    /// shares its deepest dependency with the thing checked is an identity.
    ///
    /// CONJUNCT 2 (the population gate) is asserted as a conjunct that FAILS the
    /// test, not promised in prose: the set of arms observed must equal all six.
    #[test]
    fn derived_event_set_accounts_for_every_board_axis_on_all_six_allow_listed_arms() {
        let state = probe_board();
        let mut observed_arms: Vec<&'static str> = Vec::new();

        for (name, a) in six_allow_listed_arms(&state) {
            // ARM 1 — the recorder.
            let events = match probe_resolution(&state, &a, &mut budget()) {
                ResolutionProbe::Events(events) => events,
                ResolutionProbe::Prompted => {
                    panic!("{name}: the probe must reach the recorder arm on this board")
                }
            };
            assert!(!events.is_empty(), "{name}: derivation must be non-empty");

            // ARM 2 — recorder-free. A separate clone, resolved directly.
            let before = board_axes(&state);
            let mut work = state.clone();
            let mut ev = Vec::new();
            let _ = effects::resolve_ability_chain(&mut work, &a, &mut ev, 0);
            let observed = axis_delta(&before, &board_axes(&work));

            let predicted = predicted_axes(&events)
                .unwrap_or_else(|| panic!("{name}: derived set contains an Unaccounted variant"));
            assert_eq!(
                predicted, observed,
                "{name}: the derived set must ACCOUNT FOR what the resolution did to the board \
                 (events {events:?})"
            );
            observed_arms.push(name);
        }

        observed_arms.sort_unstable();
        let mut expected = [
            "DealDamage",
            "Draw",
            "GainLife",
            "LoseLife",
            "PutCounter",
            "Token",
        ];
        expected.sort_unstable();
        assert_eq!(
            observed_arms, expected,
            "the witness must range over ALL SIX allow-listed arms — an unexercised arm is an \
             unmeasured population, not a passing row"
        );
    }

    /// R10′'s own discriminating control: a witness blind on a delegating leg
    /// scores CLEAN on a recorder that lost that leg's variant. Cripple the
    /// recorded set by dropping `Damage`, then `Draw`, and assert the SIX-axis
    /// witness catches both — while the four board-resource axes alone do not.
    #[test]
    fn the_two_ledger_axes_are_load_bearing_not_decorative() {
        let state = probe_board();
        let arms = six_allow_listed_arms(&state);

        for (name, dropped) in [("DealDamage", "Damage"), ("Draw", "Draw")] {
            let (_, a) = arms
                .iter()
                .find(|(n, _)| *n == name)
                .expect("arm present in the six");
            let ResolutionProbe::Events(events) = probe_resolution(&state, a, &mut budget()) else {
                panic!("{name}: reach-guard — the probe must produce events here");
            };
            let before = board_axes(&state);
            let mut work = state.clone();
            let mut ev = Vec::new();
            let _ = effects::resolve_ability_chain(&mut work, a, &mut ev, 0);
            let observed = axis_delta(&before, &board_axes(&work));

            // HEALTHY: the full six-axis witness agrees.
            assert_eq!(
                predicted_axes(&events).expect("accounted"),
                observed,
                "{name}: healthy control must be CLEAN"
            );

            // CRIPPLED: drop the delegating variant from the RECORDED set only.
            let crippled: Vec<ProposedEvent> = events
                .iter()
                .filter(|e| {
                    !matches!(
                        (dropped, e),
                        ("Damage", ProposedEvent::Damage { .. })
                            | ("Draw", ProposedEvent::Draw { .. })
                    )
                })
                .cloned()
                .collect();
            assert!(
                crippled.len() < events.len(),
                "{name}: reach-guard — the cripple must actually remove a {dropped} event \
                 (events {events:?})"
            );
            let crippled_prediction = predicted_axes(&crippled).expect("accounted");
            assert_ne!(
                crippled_prediction, observed,
                "{name}: the six-axis witness must MISMATCH once {dropped} is dropped"
            );

            // And the exact defect the two ledger axes close: with them deleted
            // (round 5's board-resource-only axis set) the same cripple scores
            // CLEAN — the blindness, reproduced in the same test.
            let blind =
                |x: &BoardAxes| (x.life.clone(), x.poison.clone(), x.counters, x.battlefield);
            assert_eq!(
                blind(&crippled_prediction),
                blind(&observed),
                "{name}: a board-resource-only axis set is BLIND on this delegating leg — this \
                 is why `cards_drawn` and `damage_records` are axes"
            );
        }
    }

    /// R11 — AN EMPTY DERIVATION IS FAIL-CLOSED.
    ///
    /// Measured: an entry whose targets are not yet announced derives ZERO
    /// events while the resolver still returns `Ok`, so the `Result` does NOT
    /// discriminate "proposes nothing" from "could not run". Without the
    /// `is_empty ⇒ Prompted` arm the caller's `any()` discharges vacuously and
    /// an entry that has made no choices yet certifies as choice-free.
    #[test]
    fn an_unannounced_target_derives_nothing_and_is_prompted() {
        let state = probe_board();
        let unannounced = ability(
            &state,
            Effect::DealDamage {
                amount: fixed(1),
                target: TargetFilter::Any,
                damage_source: None,
                excess: None,
            },
            vec![],
        );
        assert_eq!(
            probe_resolution(&state, &unannounced, &mut budget()),
            ResolutionProbe::Prompted,
            "an entry with no announced target proposes nothing, and nothing is never 'safe'"
        );
        // PAIRED POSITIVE, differing in exactly the announced target: the same
        // ability one announcement later derives {Damage, LifeLoss}. Without it
        // the negative above could pass on a board where nothing resolves at all.
        let announced = ability(
            &state,
            Effect::DealDamage {
                amount: fixed(1),
                target: TargetFilter::Any,
                damage_source: None,
                excess: None,
            },
            vec![TargetRef::Player(PlayerId(1))],
        );
        let ResolutionProbe::Events(events) = probe_resolution(&state, &announced, &mut budget())
        else {
            panic!("reach-guard: the announced twin must derive events");
        };
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ProposedEvent::Damage { .. })),
            "CR 120.3a: the announced twin derives the damage event ({events:?})"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ProposedEvent::LifeLoss { .. })),
            "CR 120.3a: damage to a player delegates a companion LifeLoss ({events:?})"
        );
    }

    /// R12 — INTRINSIC-PROMPT DETECTION, a matched pair on one board.
    ///
    /// This is the row that turns the allow-list's old *"its only prompt is
    /// `NeedsChoice`"* doc claim into a measurement: the probe OBSERVES the real
    /// resolver opening a real prompt instead of trusting a comment that was
    /// true when it was written.
    #[test]
    fn an_intrinsically_prompting_resolver_is_prompted_and_the_six_arms_are_not() {
        let state = probe_board();
        let scry = ability(
            &state,
            Effect::Scry {
                count: fixed(1),
                target: TargetFilter::Controller,
            },
            vec![],
        );
        assert_eq!(
            probe_resolution(&state, &scry, &mut budget()),
            ResolutionProbe::Prompted,
            "CR 732.2a: a resolver that parks on its own is a choice the certificate cannot \
             describe"
        );
        // The same instrument, the same board, the same axis: none of the six
        // allow-listed classes parks. Without this half the row would pass by
        // refusing everything.
        for (name, a) in six_allow_listed_arms(&state) {
            assert!(
                matches!(
                    probe_resolution(&state, &a, &mut budget()),
                    ResolutionProbe::Events(_)
                ),
                "{name} must NOT be Prompted on the same board that makes Scry Prompted"
            );
        }
    }

    /// R19a — `Unaccounted ⇒ Prompted` FIRES IN THE RESOLVER, not only in the
    /// witness. A gate that ships only inside a test is not a gate.
    ///
    /// KEYED on a board that ACTUALLY DERIVES an `Unaccounted` variant: a draw
    /// chained with a tap. `ProposedEvent::Tap` writes none of the six axes, so
    /// the certificate cannot account for what that resolution did to the board
    /// even though every other event in the set is accounted.
    ///
    /// TWO ARM-ATTRIBUTION CONJUNCTS, because `probe_resolution`'s `Prompted`
    /// arms are ordered budget → `waiting_for` discriminant → `is_empty` →
    /// `event_is_accounted`: without them an upstream arm could dominate, the
    /// row would pass for the wrong reason, and deleting the accounting arm
    /// could not flip it.
    #[test]
    fn an_unaccounted_derived_event_is_prompted_in_the_resolver() {
        let state = probe_board();
        let victim = victim_id(&state);
        let mut chained = ability(
            &state,
            Effect::Draw {
                count: fixed(1),
                target: TargetFilter::Controller,
            },
            vec![],
        );
        chained.sub_ability = Some(Box::new(ResolvedAbility::new(
            Effect::SetTapState {
                target: TargetFilter::Any,
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            },
            vec![TargetRef::Object(victim)],
            source_id(&state),
            PlayerId(0),
        )));

        // ATTRIBUTION (i)+(ii): re-derive the same run's inputs to the verdict so
        // the outcome is pinned to the accounting arm and to no other.
        let events = replacement::record_proposed_events(|| {
            let _probe = SimulationProbeGuard::enter();
            let mut work = state.clone();
            let mut ev = Vec::new();
            let _ = effects::resolve_ability_chain(&mut work, &chained, &mut ev, 0);
            // The production guard has TWO legs (`!matches!(.., Priority)` OR the
            // discriminants differ), and the attribution claim above names the whole
            // arm rather than one leg of it, so both legs are asserted.
            //
            // COMPLETENESS GUARD, NOT A LIVE FIX — stated from measurement so it is not
            // oversold. Deleting the accounting arm flips this row red WITH the leg
            // assert and WITHOUT it, so the row was never vacuous as it stood. The
            // divergence the second leg would catch (probe board at a non-priority
            // `waiting_for` whose variant still matches `state`'s) is not expressible on
            // this fixture: parking the board at `ReplacementChoice` changes what the
            // chain proposes — the sub-ability's `Tap` stops being derived — so the row
            // dies at its own reach-guard before any arm attribution is reached. This
            // assert buys future-drift coverage, not a currently-reachable bug.
            assert!(
                matches!(work.waiting_for, WaitingFor::Priority { .. }),
                "attribution (i): the non-priority leg of the waiting_for arm must NOT \
                 be what fires here"
            );
            assert_eq!(
                std::mem::discriminant(&work.waiting_for),
                std::mem::discriminant(&state.waiting_for),
                "attribution (i): the discriminant leg of the waiting_for arm must NOT \
                 be what fires here"
            );
        });
        assert!(
            !events.is_empty(),
            "attribution (ii): the is_empty arm must NOT be what fires here"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ProposedEvent::Tap { .. })),
            "reach-guard: the derived set must actually carry the Unaccounted variant ({events:?})"
        );
        assert!(
            events.iter().any(replacement::event_is_accounted),
            "reach-guard: the set must ALSO carry accounted events, so the refusal is \
             attributable to the one unaccounted member and not to an all-unknown set"
        );

        assert_eq!(
            probe_resolution(&state, &chained, &mut budget()),
            ResolutionProbe::Prompted,
            "CR 732.2a: an event whose board effect the certificate cannot account for is a \
             choice surface the certificate cannot describe"
        );

        // PAIRED POSITIVE, differing in exactly the chained tap: the same draw
        // without it is fully accounted and is NOT Prompted, so the row cannot
        // pass by refusing everything.
        let mut plain = chained.clone();
        plain.sub_ability = None;
        assert!(
            matches!(
                probe_resolution(&state, &plain, &mut budget()),
                ResolutionProbe::Events(_)
            ),
            "the same draw without the chained tap is accounted"
        );

        // The zero-payload guards are the other members of this class, pinned on
        // the partition itself: a zero-valued event writes no axis while still
        // being drawn as a candidate under a `> 0` gate.
        assert!(!replacement::event_is_accounted(
            &ProposedEvent::TokenEntry {
                entry_ref: victim,
                enter_tapped: Default::default(),
                enter_with_counters: Vec::new(),
                applied: Default::default(),
            }
        ));
        for (accounted, count) in [(false, 0u32), (true, 1u32)] {
            assert_eq!(
                replacement::event_is_accounted(&ProposedEvent::AddCounter {
                    placement: CounterPlacement::Object {
                        actor: PlayerId(0),
                        object_id: victim,
                        counter_type: CounterType::Plus1Plus1,
                    },
                    count,
                    applied: Default::default(),
                }),
                accounted,
                "CR 702.150a: the Compleated virtual candidate is gated on `count > 0`, so a \
                 zero-count placement must be honest-red rather than silently candidate-free"
            );
            assert_eq!(
                replacement::event_is_accounted(&ProposedEvent::Damage {
                    source_id: source_id(&state),
                    target: TargetRef::Player(PlayerId(1)),
                    amount: count,
                    is_combat: false,
                    applied: Default::default(),
                }),
                accounted,
                "the shield-damage virtual candidate is gated on `amount > 0`"
            );
            assert_eq!(
                replacement::event_is_accounted(&ProposedEvent::Draw {
                    player_id: PlayerId(0),
                    count,
                    applied: Default::default(),
                }),
                accounted
            );
        }
    }

    /// CR 107.1c + CR 608.2d: THE `UpTo` GUARD IS ON EVERY QUANTITY-CARRYING
    /// ARM, not only `Draw`.
    ///
    /// `game/quantity.rs` resolves `UpTo { max }` as the maximum instead of
    /// prompting, and none of the six allow-listed classes calls
    /// `QuantityExpr::peel_up_to`. So an unguarded arm reports choice-free on an
    /// ability whose resolution opens a count choice — a fail-OPEN, not a
    /// coverage gap. Both directions are asserted per arm so neither can go
    /// vacuous.
    #[test]
    fn an_up_to_count_is_may_prompt_on_every_quantity_carrying_arm() {
        let state = probe_board();
        let victim = victim_id(&state);
        let cases: Vec<(&str, Effect, Effect, Vec<TargetRef>)> = vec![
            (
                "GainLife",
                Effect::GainLife {
                    amount: up_to(3),
                    player: TargetFilter::Controller,
                },
                Effect::GainLife {
                    amount: fixed(3),
                    player: TargetFilter::Controller,
                },
                vec![],
            ),
            (
                "LoseLife",
                Effect::LoseLife {
                    amount: up_to(2),
                    target: Some(TargetFilter::Controller),
                },
                Effect::LoseLife {
                    amount: fixed(2),
                    target: Some(TargetFilter::Controller),
                },
                vec![],
            ),
            (
                "DealDamage",
                Effect::DealDamage {
                    amount: up_to(1),
                    target: TargetFilter::Any,
                    damage_source: None,
                    excess: None,
                },
                Effect::DealDamage {
                    amount: fixed(1),
                    target: TargetFilter::Any,
                    damage_source: None,
                    excess: None,
                },
                vec![TargetRef::Player(PlayerId(1))],
            ),
            (
                "PutCounter",
                Effect::PutCounter {
                    target: TargetFilter::Any,
                    counter_type: CounterType::Plus1Plus1,
                    count: up_to(2),
                },
                Effect::PutCounter {
                    target: TargetFilter::Any,
                    counter_type: CounterType::Plus1Plus1,
                    count: fixed(2),
                },
                vec![TargetRef::Object(victim)],
            ),
            (
                "Token",
                token_effect(up_to(1)),
                token_effect(fixed(1)),
                vec![],
            ),
            (
                "Draw",
                Effect::Draw {
                    count: up_to(1),
                    target: TargetFilter::Controller,
                },
                Effect::Draw {
                    count: fixed(1),
                    target: TargetFilter::Controller,
                },
                vec![],
            ),
        ];
        for (name, up_to_effect, fixed_effect, targets) in cases {
            let guarded = ability(&state, up_to_effect, targets.clone());
            assert_eq!(
                ability_resolution_choice_freedom(&state, &guarded, &mut budget()),
                ResolutionChoiceFreedom::MayPrompt,
                "{name}: an `up to N` count is a CR 608.2d resolution-time choice the probe \
                 would ANSWER rather than surface"
            );
            // MATCHED POSITIVE: the same arm at a fixed count is probe-backed,
            // so the `MayPrompt` above is attributable to the guard and not to
            // the arm being out of scope.
            let unguarded = ability(&state, fixed_effect, targets);
            assert!(
                matches!(
                    ability_resolution_choice_freedom(&state, &unguarded, &mut budget()),
                    ResolutionChoiceFreedom::FreeUnlessReplacements(_)
                ),
                "{name}: the same arm at a fixed count must be probe-backed"
            );
        }
    }

    /// The reject side stays fail-closed, and the ability-level wrapper flips.
    ///
    /// Successor to `ability_scan::resolution_choice_verdicts_are_exactly_pinned`
    /// for the halves that survive the payload change; the per-arm obligation
    /// pin it carried is superseded by the derived-event witness above, which
    /// pins the EVENTS rather than a class name and cannot go stale.
    #[test]
    fn the_reject_side_and_the_ability_level_gates_are_pinned_in_both_directions() {
        let state = probe_board();
        let rejects = [
            Effect::Proliferate,
            Effect::Populate,
            Effect::Clash,
            Effect::Explore,
            Effect::Scry {
                count: fixed(1),
                target: TargetFilter::Controller,
            },
            Effect::Sacrifice {
                target: TargetFilter::Any,
                count: fixed(1),
                min_count: 0,
            },
            Effect::DiscardCard {
                count: 1,
                target: TargetFilter::Any,
            },
        ];
        for e in rejects {
            let a = ability(&state, e.clone(), vec![]);
            assert_eq!(
                ability_resolution_choice_freedom(&state, &a, &mut budget()),
                ResolutionChoiceFreedom::MayPrompt,
                "{e:?} must be MayPrompt"
            );
        }

        // Base ⇒ probe-backed (the paired positive reach-guard); each single-field
        // mutation ⇒ MayPrompt, proving the FLIP causes the rejection and not
        // something upstream.
        let base = ability(
            &state,
            Effect::GainLife {
                amount: fixed(1),
                player: TargetFilter::Controller,
            },
            vec![],
        );
        assert!(
            matches!(
                ability_resolution_choice_freedom(&state, &base, &mut budget()),
                ResolutionChoiceFreedom::FreeUnlessReplacements(_)
            ),
            "reach-guard: the unmutated base is probe-backed"
        );

        let mut mutations: Vec<(&str, ResolvedAbility)> = Vec::new();
        let mut push = |label: &'static str, f: &dyn Fn(&mut ResolvedAbility)| {
            let mut a = base.clone();
            f(&mut a);
            mutations.push((label, a));
        };
        push("optional", &|a| a.optional = true);
        push("optional_targeting", &|a| a.optional_targeting = true);
        push("unless_pay", &|a| {
            a.unless_pay = Some(UnlessPayModifier {
                cost: AbilityCost::Tap,
                payer: TargetFilter::Controller,
            })
        });
        push("target_chooser", &|a| {
            a.target_chooser = Some(TargetFilter::Controller)
        });
        push("target_choice_timing", &|a| {
            a.target_choice_timing = TargetChoiceTiming::Resolution
        });
        push("mode_abilities", &|a| {
            a.mode_abilities = vec![AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp)]
        });
        push("repeat_until", &|a| {
            a.repeat_until = Some(RepeatContinuation::ControllerChoice)
        });
        push("modal", &|a| a.modal = Some(ModalChoice::default()));
        // The gate at the top of the classifier is
        // `*optional || *optional_targeting || optional_for.is_some()`, and only the
        // first two disjuncts had a row. A row per DISJUNCT, not per gate: with two of
        // three covered, `optional_for` could have been dropped from the condition and
        // every existing row would still pass.
        push("optional_for", &|a| {
            a.optional_for = Some(OpponentMayScope::AnyOpponent)
        });
        // CR 608.2d + CR 107.1c: an `UpTo` REPEAT COUNT is a resolution-time choice.
        // `repeat_for` used to be bound `_` as "pure quantity eval", so this class of
        // ability was certified choice-free while its resolution opens a count prompt.
        push("repeat_for_up_to", &|a| {
            a.repeat_for = Some(QuantityExpr::UpTo {
                max: Box::new(fixed(3)),
            })
        });
        for (label, a) in mutations {
            assert_eq!(
                ability_resolution_choice_freedom(&state, &a, &mut budget()),
                ResolutionChoiceFreedom::MayPrompt,
                "{label} is a resolution-time choice the probe must not swallow"
            );
        }

        // REACH-GUARD for the `repeat_for` row above, and the half that makes it
        // DISCRIMINATING rather than merely red-on-mutation: a `repeat_for` that is NOT
        // `UpTo` must still be probe-backed. Without this arm the row would pass just as
        // well against a guard that rejected EVERY `repeat_for`, which would be a
        // coverage loss dressed up as a fix — "for each" loops over a fixed or
        // state-derived count really are pure evaluation, and that is the common case.
        let mut fixed_repeat = base.clone();
        fixed_repeat.repeat_for = Some(fixed(3));
        assert!(
            matches!(
                ability_resolution_choice_freedom(&state, &fixed_repeat, &mut budget()),
                ResolutionChoiceFreedom::FreeUnlessReplacements(_)
            ),
            "a non-`UpTo` repeat count is pure evaluation and must stay probe-backed — \
             rejecting it too would trade the fail-open bug for lost certification"
        );
        // And the guard must see through a WRAPPER, or "up to N, doubled" would evade it.
        // This is the composability the shared `quantity_offers_up_to_choice` authority
        // buys; a bespoke `matches!(.., UpTo { .. })` here would pass the row above and
        // fail this one.
        let mut wrapped = base.clone();
        wrapped.repeat_for = Some(QuantityExpr::Multiply {
            inner: Box::new(QuantityExpr::UpTo {
                max: Box::new(fixed(3)),
            }),
            factor: 2,
        });
        assert_eq!(
            ability_resolution_choice_freedom(&state, &wrapped, &mut budget()),
            ResolutionChoiceFreedom::MayPrompt,
            "an `UpTo` nested under an arithmetic wrapper is still a resolution-time count choice"
        );

        // ── THE RECURSION IS RETAINED FOR GATES, which is the premise that lets the
        //    probe run once at the chain ROOT.
        //
        //    ⚠ MEASURED ASYMMETRY between the two branch sites, recorded because the
        //    obvious reading of these two rows is WRONG. Each recursion site was
        //    revert-probed independently:
        //
        //    * deleting the `else_ability` recursion FLIPS its row. The else branch is
        //      not the branch this board resolves, so the root probe never executes it
        //      and the AST walk is the ONLY thing that can see a gate there.
        //    * deleting the `sub_ability` recursion does NOT flip its row — measured,
        //      9 of 9 still pass. `resolve_ability_chain` resolves the TAKEN
        //      sub-ability, so the root probe observes that prompt directly and the
        //      upstream conjunct dominates the discriminator.
        //
        //    So the `sub_ability` row is a REGRESSION GUARD, not a proof of that
        //    recursion, and says so rather than implying coverage it does not carry.
        //    That recursion still earns its place on COST — `analysis::resource` calls
        //    `chain_offers_choice` before cloning the board, where no probe has run yet
        //    — but the soundness there rests on the probe, not on this row.
        for (label, attach, why) in [
            (
                "sub_ability",
                (&|a: &mut ResolvedAbility, branch: ResolvedAbility| {
                    a.sub_ability = Some(Box::new(branch))
                }) as &dyn Fn(&mut ResolvedAbility, ResolvedAbility),
                // REGRESSION GUARD ONLY — measured non-discriminating for the recursion:
                // the root probe resolves the taken sub-ability and sees this prompt itself.
                "the root probe also sees this one, so this row guards against regression \
                 rather than proving the recursion",
            ),
            (
                "else_ability",
                &|a, branch| a.else_ability = Some(Box::new(branch)),
                // The discriminating half: this branch is never resolved on this board.
                "the root probe never resolves this branch, so the AST recursion is the \
                 ONLY thing that can see it",
            ),
        ] {
            let mut branch = base.clone();
            branch.optional = true; // a gate that only the recursion can see
            let mut with_branch = base.clone();
            attach(&mut with_branch, branch);
            assert_eq!(
                ability_resolution_choice_freedom(&state, &with_branch, &mut budget()),
                ResolutionChoiceFreedom::MayPrompt,
                "a choice gate on `{label}` must reject the whole chain ({why})"
            );

            // NEGATIVE CONTROL for the pair above: the identical chain SHAPE with a
            // gate-free branch must still certify. Without it, a `chain_offers_choice`
            // that rejected any ability merely for HAVING a branch would pass both rows
            // and silently stop certifying every chained ability in the corpus.
            let mut clean_branch = base.clone();
            clean_branch.optional = false;
            let mut with_clean = base.clone();
            attach(&mut with_clean, clean_branch);
            assert!(
                matches!(
                    ability_resolution_choice_freedom(&state, &with_clean, &mut budget()),
                    ResolutionChoiceFreedom::FreeUnlessReplacements(_)
                ),
                "a gate-free `{label}` branch must remain probe-backed, or the recursion \
                 is rejecting chain SHAPE rather than chain CONTENT"
            );
        }
    }

    /// STRUCTURAL INVARIANT: `game/ability_scan.rs` holds NO `GameState`.
    ///
    /// The module header defines it as a pure AST walk, and that contract is
    /// exactly why the resolution-choice classifier moved out of it. Pinned at
    /// the WIDEST form — a word-bounded `GameState` token anywhere in the file —
    /// because the narrow `state: &GameState` spelling is evaded by `st:` or
    /// `&mut`. Keyed by this file, which contains many.
    #[test]
    fn ability_scan_holds_no_game_state() {
        let count_tokens = |path: &str| -> usize {
            let src = std::fs::read_to_string(path).expect("source file readable");
            src.split(|c: char| !(c.is_alphanumeric() || c == '_'))
                .filter(|tok| *tok == "GameState")
                .count()
        };
        let dir = env!("CARGO_MANIFEST_DIR");
        let scanner = format!("{dir}/src/game/ability_scan.rs");
        let here = format!("{dir}/src/game/resolution_prompt.rs");
        assert!(
            count_tokens(&here) > 0,
            "positive control: this file names GameState, so the instrument can return non-zero"
        );
        assert_eq!(
            count_tokens(&scanner),
            0,
            "ability_scan.rs is a pure AST walk and must hold no board — probing a resolution \
             needs one, which is why that classifier lives here instead"
        );
    }
}
