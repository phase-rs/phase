use crate::game::game_object::ObjectIncarnationRefExt;
use crate::game::quantity::{quantity_ref_object_scope, resolve_quantity_with_targets};
use crate::game::replacement::EventModifiers;
use crate::game::targeting::{
    resolve_event_context_target, triggering_spell, triggering_spell_identity, TriggeringSpell,
};
use crate::types::ability::{
    expiry_from_duration, host_gate_enforceable, quantity_ref_recipient_read,
    replacement_install_is_refused, AbilityDefinition, CastManaObjectScope, DamageTargetFilter,
    DamageTargetPlayerScope, Effect, EffectError, EffectKind, ObjectScope, PlayerFilter,
    QuantityExpr, QuantityRef, RecipientRead, ReplacementCondition, ReplacementDefinition,
    ReplacementDurationExpiry, ResolvedAbility, RestrictionExpiry, SourceExclusion, TargetFilter,
    TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::replacements::ReplacementEvent;
use crate::types::zones::Zone;

fn replacement_with_ability_expiry(
    replacement: &ReplacementDefinition,
    ability: &ResolvedAbility,
) -> Result<ReplacementDefinition, EffectError> {
    let mut replacement = replacement.clone();
    // CR 611.2a: the refusal itself, read from the SAME authority the parse-time
    // honesty net reads. Asking it here rather than re-deriving both axes is what
    // makes "neither list exists twice" true of the code and not just of the
    // comment: a duration or a form the net demotes and a duration or form this
    // seam installs can never drift apart, because there is only one answer.
    if replacement_install_is_refused(&replacement, ability.duration.as_ref()) {
        return Err(unenforceable_lifetime(ability));
    }
    if replacement.expiry.is_none() {
        match expiry_from_duration(ability.duration.as_ref()) {
            ReplacementDurationExpiry::Unstated => {
                replacement = replacement.with_resolution_shield_expiry();
            }
            ReplacementDurationExpiry::Explicit(expiry) => replacement.expiry = Some(expiry),
            // CR 611.2a: the one class whose stamp needs the resolving
            // ability's controller, named here rather than inside the shared
            // classification.
            ReplacementDurationExpiry::ExplicitControllerNextTurn => {
                replacement.expiry = Some(RestrictionExpiry::UntilPlayerNextTurn {
                    player: ability.controller,
                });
            }
            // CR 611.2a: `GateControlled` promises that a runtime applicability
            // gate enforces the host-bound duration — and that gate exists only
            // for the bare untap-prevention rider
            // (`stamp_for_as_long_as_controlled_gate`). Any other replacement
            // shape would install with no lifetime the engine can end
            // CORRECTLY: a floating or player-bound install sits in
            // `pending_damage_replacements`, whose prunes key on `expiry` only,
            // and outlives its printed window; an object install lands
            // live-only (`install_to_base` is false for this shape) and is
            // wiped at the next CR 613.1 layer reseed, far too early. Either
            // way the printed duration is dropped. Fail closed exactly like
            // `Unsupported` until a typed host lifetime exists for those
            // shapes. `host_gate_enforceable` answers the form axis for both
            // this guard and the stamp; the stamp consumes
            // `expiry_from_duration` for the duration axis, so neither list
            // exists twice.
            // Not refused above, so the form CAN carry the gate. There is no
            // stamp to write: the lifetime IS the gate, installed by
            // `stamp_for_as_long_as_controlled_gate` further down.
            ReplacementDurationExpiry::GateControlled => {}
            // CR 611.2a: do not install a replacement whose stated duration the
            // engine cannot enforce. In particular, never replace it with the
            // end-of-turn fallback, which would shorten the printed window.
            // Unreachable: `replacement_install_is_refused` returns true for
            // this class unconditionally. Kept as its own arm rather than folded
            // into the one above so the match stays wildcard-free and a duration
            // newly routed here cannot silently inherit the gate's meaning.
            ReplacementDurationExpiry::Unsupported => {
                return Err(unenforceable_lifetime(ability));
            }
        }
    }
    // CR 514.2 + CR 615.3: a SHIELD installed by a resolving spell or ability with
    // no stated duration falls back to the engine's turn window —
    // see `ReplacementDefinition::with_resolution_shield_expiry` (an engine
    // default, not a CR rule). Gated on `shield_kind.is_shield()` so
    // runtime-installed NON-shield riders that are legitimately durable keep
    // `expiry: None`: the CR 611.2b `ControllerControlsSource` lock (ended by its
    // own gate) and the CR 702.84a `UntilHostLeavesPlay` rider (ended by the
    // battlefield-exit prune).
    //
    // CR 604.2: printed static shields never reach this seam — they are seeded
    // into `base_replacement_definitions` by `printed_cards.rs` — so this cannot
    // make a durable printed shield turn-bound.
    //
    // DEFENCE IN DEPTH: no corpus card reaches this stamp today. Exactly one
    // `AddTargetReplacement` shield node exists in the card corpus (Impulsive
    // Maneuvers) and the parser already stamps it `EndOfTurn`. This guard exists
    // so that removing cleanup's `shield_kind` blanket cannot make a future
    // unstamped runtime shield immortal.
    // CR 109.4 + CR 614.1a: Anchor the installing player onto the replacement so
    // global pending damage replacements (pushed under the sentinel `ObjectId(0)`,
    // which has no controller in `state.objects`) can resolve a controller-relative
    // `damage_source_filter` ("a source you control"). Without this anchor,
    // `ControllerRef::You` never matches because the sentinel source has no
    // controller, so the boost silently never fires (I Call for Slaughter, Rankle
    // and Torbran, Taii Wakeen's +X boost). Guarded on `is_none` so a replacement
    // that already specified a controller is never clobbered.
    if replacement.source_controller.is_none() {
        replacement.source_controller = Some(ability.controller);
    }
    stamp_for_as_long_as_controlled_gate(&mut replacement, ability);
    freeze_damage_modification_x(&mut replacement, ability);
    freeze_parent_copy_target(&mut replacement, ability);
    Ok(replacement)
}

/// CR 611.2a: the refusal, as a hard resolution error rather than a successful
/// no-op.
///
/// Reaching this is a PARSER defect, not a game state: the post-lowering
/// honesty net demotes every refused shape to `Effect::Unimplemented` before a
/// card can claim support for it.
///
/// What the error buys, precisely: in the test harness a refused install now
/// FAILS instead of passing, which is what the `expect_err` pins hold
/// (`tap_untap::tests::host_duration_on_non_untap_replacement_fails_closed`,
/// `tests::stated_unrepresentable_duration_does_not_install_a_shield`). In
/// production it aborts the remainder of the resolution chain — `stack.rs`
/// discards the `Result` (`let _ = effects::resolve_ability_chain(..)`) and no
/// consumer logs an `EffectError`, so nothing becomes visible to a player. It is
/// therefore a REGRESSION PIN plus a fail-closed stop, not a diagnostic; the
/// visibility half is the parser net's job.
fn unenforceable_lifetime(ability: &ResolvedAbility) -> EffectError {
    EffectError::InvalidParam(format!(
        "AddTargetReplacement: no enforceable lifetime for duration {:?} on this replacement form \
         (CR 611.2a); the parser must lower this line to Effect::Unimplemented instead",
        ability.duration
    ))
}

/// CR 400.7 + CR 614.1c: bind an entry rider to its recipient spell occurrence,
/// not to the installer's source or controller. The resolving carrier's pin
/// supplies both the concrete selector and the installed identity; a later cast
/// with the same storage id cannot inherit it, even after a control change.
///
/// CR 400.7: installation needs the original spell still live on the stack; a
/// departed occurrence is a new object the rider can never name.
/// Missing, departed, or mismatched authority rejects the install rather than
/// storing a raw-id fallback or a permanently stale rider. Definitions without
/// the placeholder retain their unchanged installation path.
fn bind_replacement_to_trigger_source(
    mut replacement: ReplacementDefinition,
    state: &GameState,
) -> Option<ReplacementDefinition> {
    let Some(valid_card) = replacement.valid_card.as_mut() else {
        return Some(replacement);
    };
    if !target_filter_contains_placeholder(valid_card) {
        return Some(replacement);
    }
    let pin = triggering_spell_identity(state)?;
    if !matches!(triggering_spell(state), Some(TriggeringSpell::OnStack(_))) {
        return None;
    }
    concretize_triggering_spell_placeholder(valid_card, pin.object_id);
    replacement.triggering_spell = Some(pin);
    Some(replacement)
}

fn target_filter_contains_placeholder(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::SpecificObject { id } => {
            *id == crate::types::identifiers::TRIGGERING_SPELL_PLACEHOLDER
        }
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(target_filter_contains_placeholder)
        }
        TargetFilter::Not { filter } => target_filter_contains_placeholder(filter),
        _ => false,
    }
}

fn concretize_triggering_spell_placeholder(filter: &mut TargetFilter, bound: ObjectId) {
    match filter {
        TargetFilter::SpecificObject { id }
            if *id == crate::types::identifiers::TRIGGERING_SPELL_PLACEHOLDER =>
        {
            *id = bound;
        }
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            for f in filters.iter_mut() {
                concretize_triggering_spell_placeholder(f, bound);
            }
        }
        TargetFilter::Not { filter } => concretize_triggering_spell_placeholder(filter, bound),
        _ => {}
    }
}

// CR 614.12a + CR 707.2: If the resolving spell chose the object to copy, bind
// that object into the delayed enter-as-copy replacement when the shield is
// created so the later entry event does not ask for a new copy source.
fn freeze_parent_copy_target(replacement: &mut ReplacementDefinition, ability: &ResolvedAbility) {
    let Some(copy_source) = ability.targets.iter().find_map(|target| match target {
        TargetRef::Object(id) => Some(*id),
        TargetRef::Player(_) => None,
    }) else {
        return;
    };
    if let Some(execute) = replacement.execute.as_mut() {
        concretize_parent_copy_target(execute, copy_source);
    }
}

fn concretize_parent_copy_target(
    def: &mut AbilityDefinition,
    copy_source: crate::types::identifiers::ObjectId,
) {
    // CR 614.12a + CR 707.2: a Mystic Reflection-style replacement chooses the
    // copied object when the spell resolves, before the later battlefield-entry
    // replacement applies. Freeze that parent target into the installed shield
    // so the later enter event does not prompt for a new copy source.
    if let Effect::BecomeCopy { target, .. } = def.effect.as_mut() {
        if matches!(target, TargetFilter::ParentTarget) {
            *target = TargetFilter::SpecificObject { id: copy_source };
        }
    }
    if let Some(sub) = def.sub_ability.as_mut() {
        concretize_parent_copy_target(sub, copy_source);
    }
    if let Some(else_ability) = def.else_ability.as_mut() {
        concretize_parent_copy_target(else_ability, copy_source);
    }
    for mode in def.mode_abilities.iter_mut() {
        concretize_parent_copy_target(mode, copy_source);
    }
}

/// CR 611.2b: Translate a "for as long as you control ~" duration on the
/// installing ability into a `ControllerControlsSource` applicability gate for a
/// broad untap-prevention rider (Spider-Woman, Secret Agent: "That creature
/// can't become untapped for as long as you control ~.").
///
/// The clause shell peels "for as long as you control ~" onto the ability frame
/// as `Duration::WhileControllingHost`. For a replacement installed on a
/// DIFFERENT object (the chosen creature) that mapping is insufficient on its
/// own — an object-installed replacement whose only lifetime statement sits on
/// the ability frame has no pruner of its own, and the control reading must
/// end on a control SWAP of the originating source, not just when it leaves
/// play. Stamping the gate with the originating source (`ability.source_id`,
/// e.g. Spider-Woman) and its controller (`ability.controller`) re-checks
/// "you still control [the source]" on every untap, matching the Master Thief
/// example.
///
/// Tightly scoped: only a bare untap-prevention rider (event `Untap`, no
/// `execute`, no pre-existing condition) under a `GateControlled` duration is
/// translated, so unrelated `AddTargetReplacement` installs are untouched.
///
/// The three host wordings are NOT interchangeable here, and this stamp now
/// matches only one of them. `ReplacementCondition::ControllerControlsSource`
/// ends on a control change; that is the printed end of
/// `Duration::WhileControllingHost` and of nothing else. The presence reading
/// (`Duration::WhileHostOnBattlefield`) and the event deadline
/// (`Duration::UntilHostLeavesPlay`) both survive a control change while their
/// source stays on the battlefield, so gating them here would END THEM EARLY —
/// a window shorter than printed, which CR 611.2a forbids exactly as much as a
/// longer one.
///
/// Those two therefore classify as `Unsupported` in `expiry_from_duration` and
/// never reach this stamp. That is a refusal, not a silent drop: the parser's
/// post-lowering honesty net demotes such a line to `Effect::Unimplemented`
/// (`parser::oracle::demote_unenforceable_replacement_lifetimes`), and the
/// install seam raises a hard `EffectError` if one arrives anyway. Reachability
/// is measured, not assumed: all three wordings DO lower to this replacement
/// path from ordinary Oracle text (`can't become untapped for as long as ~
/// remains on the battlefield` / `until ~ leaves the battlefield`), so the
/// corpus being free of them today is an accident of printing, not a guard.
///
/// The duration axis is consumed from `expiry_from_duration` — the same
/// authority the install seam's `GateControlled` arm answers to — and the form
/// axis from `host_gate_enforceable`, shared with that arm's fail-closed
/// check. Neither the duration set nor the form predicate exists twice, so a
/// future duration routed to `GateControlled` reaches this stamp and the
/// fail-closed guard in the same breath.
fn stamp_for_as_long_as_controlled_gate(
    replacement: &mut ReplacementDefinition,
    ability: &ResolvedAbility,
) {
    if host_gate_enforceable(replacement)
        && matches!(
            expiry_from_duration(ability.duration.as_ref()),
            ReplacementDurationExpiry::GateControlled
        )
    {
        replacement.condition = Some(ReplacementCondition::ControllerControlsSource {
            source: ability.source_id,
            controller: ability.controller,
        });
    }
}

/// CR 107.3a + CR 601.2b: Freeze the announced value of X into a "deals that
/// much damage plus X" replacement at activation time. The parser emits the
/// bare-"plus x" form (no "where X is" binding) as
/// `DamageModification::Plus { value: QuantityExpr::Fixed { value: 0 } }`
/// placeholder; here the announced X (held on the activating ability as
/// `chosen_x`) replaces the placeholder so the replacement applies the
/// locked-in value for the rest of the turn (Taii Wakeen's second ability). The
/// `Fixed { value: 0 }` guard ensures a genuine literal "plus 0" (no X) or a
/// where-bound dynamic offset (`Ref`, e.g. Hawkeye) is never clobbered. (CR
/// 107.3a: an activated ability's X equals its announced value while on the
/// stack and beyond.)
fn freeze_damage_modification_x(
    replacement: &mut ReplacementDefinition,
    ability: &ResolvedAbility,
) {
    if let (Some(crate::types::ability::DamageModification::Plus { value }), Some(chosen_x)) =
        (replacement.damage_modification.as_mut(), ability.chosen_x)
    {
        if matches!(
            value,
            crate::types::ability::QuantityExpr::Fixed { value: 0 }
        ) {
            *value = crate::types::ability::QuantityExpr::Fixed {
                value: chosen_x as i32,
            };
        }
    }
}

/// CR 614.1c: preserve reference leaves that read the entering permanent.
/// Entry extraction supplies `entering`, not a layer `recipient`: only object
/// scopes and quantities that retain the enclosing context can use that fallback.
/// CR 608.2h: event-scoped reads must be fixed before trigger context is cleared.
fn entry_counter_reference_is_deferred(qty: &QuantityRef) -> bool {
    match qty {
        // Payment facts belong to the entering spell. Preserved limitations:
        // CostXPaid still reads source/trigger context, not ctx.entering;
        // SharedCardTypes needs ctx.spell, which entry extraction lacks.
        QuantityRef::CostXPaid
        | QuantityRef::KickerCount
        | QuantityRef::AdditionalCostPaymentCount
        | QuantityRef::AdditionalCostPaymentCountFor { .. }
        | QuantityRef::ConvokedCreatureCount
        | QuantityRef::SharedCardTypes { .. } => true,
        QuantityRef::ManaSpentToCast { scope, .. } => match scope {
            CastManaObjectScope::SelfObject => true,
            CastManaObjectScope::TriggeringSpell | CastManaObjectScope::AbilityTarget => false,
        },
        QuantityRef::Power { .. }
        | QuantityRef::BasePower { .. }
        | QuantityRef::Intensity { .. }
        | QuantityRef::Toughness { .. }
        | QuantityRef::ObjectManaValue { .. }
        | QuantityRef::ObjectColorCount { .. }
        | QuantityRef::ObjectNameWordCount { .. }
        | QuantityRef::ObjectTypelineComponentCount { .. }
        | QuantityRef::ManaSymbolsInManaCost { .. }
        | QuantityRef::CountersOn { .. }
        | QuantityRef::NameStickerLetterCount { .. } => match quantity_ref_recipient_read(qty) {
            RecipientRead::Characteristics => true,
            RecipientRead::Unread | RecipientRead::PopulationIdentity => false,
        },
        QuantityRef::PlayerCount { filter } => match filter {
            // The threshold preserves ctx.entering, unlike object filter thresholds.
            // Keep the census live when its threshold needs the entrant. The
            // freezer still snapshots non-Recipient ObjectScope leaves within
            // that threshold; they are independent of its candidate binding.
            // All other threshold leaves stay live conservatively, including
            // ScopedPlayer reads and filters with ScopedPlayer controllers.
            // Resolution-only reads without an ObjectScope remain unsupported
            // inside a deferred threshold.
            PlayerFilter::PlayerAttribute { value, .. } => {
                value.any_ref(&mut entry_counter_reference_is_deferred)
            }
            PlayerFilter::Controller
            | PlayerFilter::Opponent
            | PlayerFilter::DefendingPlayer
            | PlayerFilter::OpponentLostLife
            | PlayerFilter::OpponentGainedLife
            | PlayerFilter::HasLostTheGame
            | PlayerFilter::OpponentAttacked { .. }
            | PlayerFilter::OpponentAttackingEnchantedPlayer
            | PlayerFilter::All
            | PlayerFilter::AllExcept { .. }
            | PlayerFilter::HighestSpeed
            | PlayerFilter::ZoneChangedThisWay
            | PlayerFilter::PerformedActionThisWay { .. }
            | PlayerFilter::OwnersOfCardsExiledBySource
            | PlayerFilter::TriggeringPlayer
            | PlayerFilter::OpponentOtherThanTriggering
            | PlayerFilter::OpponentOfTriggeringPlayer
            | PlayerFilter::OpponentOfTriggeringPlayerNotAttacked
            | PlayerFilter::VotedFor { .. }
            | PlayerFilter::ParentObjectTargetController
            | PlayerFilter::ChosenPlayer { .. }
            | PlayerFilter::ParentObjectTargetOwner
            | PlayerFilter::TargetPlayer
            | PlayerFilter::GrantingObjectCaster
            | PlayerFilter::OpponentDealtDamage { .. }
            | PlayerFilter::ControlsCount { .. }
            | PlayerFilter::TrackedSetPossessor { .. }
            | PlayerFilter::AnyOf { .. } => false,
        },
        // CR 611.2d + CR 608.2h: these player/population reads are fixed now.
        // FilterContext receives only ctx.recipient (absent at install AND entry);
        // its nested thresholds start fresh contexts without ctx.entering.
        // Even Characteristics-graded filter/player reads do not gain an entering
        // binding; PopulationIdentity ("other", "attached to it") is source-relative.
        QuantityRef::HandSize { .. }
        | QuantityRef::LifeTotal { .. }
        | QuantityRef::GraveyardSize { .. }
        | QuantityRef::StartingLifeTotal { .. }
        | QuantityRef::LifeLostThisTurn { .. }
        | QuantityRef::PartySize { .. }
        | QuantityRef::Speed { .. }
        | QuantityRef::LifeGainedThisTurn { .. }
        | QuantityRef::CardsDrawnThisTurn { .. }
        | QuantityRef::CardsDrawnLastTurn { .. }
        | QuantityRef::LandsPlayedThisTurn { .. }
        | QuantityRef::PlayerChosenNumber { .. }
        | QuantityRef::LoyaltyAbilitiesActivatedThisTurn { .. }
        | QuantityRef::CardsDiscardedThisTurn { .. }
        | QuantityRef::TokensCreatedThisTurn { .. }
        | QuantityRef::PlayerActionsThisTurn { .. }
        | QuantityRef::SacrificedThisTurn { .. }
        | QuantityRef::BattlefieldEntriesThisTurn { .. }
        | QuantityRef::ObjectCount { .. }
        | QuantityRef::ObjectCountDistinct { .. }
        | QuantityRef::ObjectCountBySharedQuality { .. }
        | QuantityRef::CountersOnObjects { .. }
        | QuantityRef::EnteredThisTurn { .. }
        | QuantityRef::DistinctCounterKindsAmong { .. }
        | QuantityRef::FilteredTrackedSetSize { .. }
        | QuantityRef::ZoneCardCount { .. }
        | QuantityRef::AttackedThisTurn { .. }
        | QuantityRef::CounterAddedThisTurn { .. }
        | QuantityRef::DamageDealtThisTurn { .. }
        | QuantityRef::PropertyAggregate(..)
        | QuantityRef::DistinctCardTypes { .. }
        | QuantityRef::DistinctSubtypes { .. }
        | QuantityRef::DistinctColorsAmong { .. } => false,
        // CR 608.2h: remaining resolution-owned source/controller/event reads
        // are fixed now, as are non-recipient object reads classified above.
        QuantityRef::ControlledByEachPlayer { .. }
        | QuantityRef::ZoneChangeCountThisTurn { .. }
        | QuantityRef::ZoneChangeAggregateThisTurn { .. }
        | QuantityRef::SpellsCastThisTurn { .. }
        | QuantityRef::SpellsCastBeforeTriggeringSpell { .. }
        | QuantityRef::SpellsCastThisGame { .. }
        | QuantityRef::TargetObjectManaValue { .. }
        | QuantityRef::EventContextPlayerCount { .. }
        | QuantityRef::AttachmentsOnLeavingObject { .. }
        | QuantityRef::LifeAboveStarting
        | QuantityRef::TriggeringDiscoverValue
        | QuantityRef::TriggeringScryLookCount
        | QuantityRef::TriggeringScryBottomCount
        | QuantityRef::PlayerCounter { .. }
        | QuantityRef::TargetControllerCounter { .. }
        | QuantityRef::Variable { .. }
        | QuantityRef::SelfManaValue
        | QuantityRef::TargetZoneCardCount { .. }
        | QuantityRef::Devotion { .. }
        | QuantityRef::CardsExiledBySource
        | QuantityRef::ExiledCardPower { .. }
        | QuantityRef::BasicLandTypeCount { .. }
        | QuantityRef::TrackedSetSize
        | QuantityRef::ExiledFromHandThisResolution
        | QuantityRef::PreviousEffectAmount { .. }
        | QuantityRef::PreviousEffectCount
        | QuantityRef::UnspentMana { .. }
        | QuantityRef::EventContextAmount
        | QuantityRef::EventContextSourceCostX
        | QuantityRef::EventContextSourceModesChosen
        | QuantityRef::CrimesCommittedThisTurn
        | QuantityRef::BendTypesThisTurn
        | QuantityRef::TurnsTaken
        | QuantityRef::ChosenNumber
        | QuantityRef::NotedNumber
        | QuantityRef::DescendedThisTurn
        | QuantityRef::SpellsCastLastTurn
        | QuantityRef::OpponentSpellsCastSinceLastTurn { .. }
        | QuantityRef::DungeonsCompleted
        | QuantityRef::TimesCostPaidThisResolution
        | QuantityRef::ColorsInCommandersColorIdentity
        | QuantityRef::CommanderCastFromCommandZoneCount
        | QuantityRef::CommanderManaValue { .. }
        | QuantityRef::VoteCount { .. } => false,
    }
}

/// CR 611.2d + CR 608.2h: snapshot each resolution-owned reference while its
/// ability/source/event context is available; keep entry-time leaves live.
/// Arithmetic remains the shared quantity resolver's responsibility.
fn freeze_entry_counter_expression(
    expr: &mut QuantityExpr,
    state: &GameState,
    ability: &ResolvedAbility,
    freeze_reference: fn(&QuantityRef) -> bool,
) {
    match expr {
        QuantityExpr::Fixed { .. } => {}
        QuantityExpr::Ref { qty } => {
            if freeze_reference(qty) {
                *expr = QuantityExpr::Fixed {
                    value: resolve_quantity_with_targets(state, expr, ability),
                };
            } else if let QuantityRef::PlayerCount {
                filter: PlayerFilter::PlayerAttribute { value, .. },
            } = qty
            {
                // CR 608.2h + CR 614.1c: preserve entry/candidate bindings while
                // fixing ability-bound object reads before trigger context clears.
                freeze_entry_counter_expression(value, state, ability, |reference| {
                    quantity_ref_object_scope(reference)
                        .is_some_and(|scope| scope != ObjectScope::Recipient)
                });
            }
        }
        QuantityExpr::DivideRounded { inner, .. }
        | QuantityExpr::Offset { inner, .. }
        | QuantityExpr::ClampMin { inner, .. }
        | QuantityExpr::Multiply { inner, .. } => {
            freeze_entry_counter_expression(inner, state, ability, freeze_reference);
        }
        QuantityExpr::Sum { exprs } | QuantityExpr::Max { exprs } => {
            for expr in exprs {
                freeze_entry_counter_expression(expr, state, ability, freeze_reference);
            }
        }
        QuantityExpr::UpTo { max } => {
            freeze_entry_counter_expression(max, state, ability, freeze_reference);
        }
        QuantityExpr::Power { exponent, .. } => {
            freeze_entry_counter_expression(exponent, state, ability, freeze_reference);
        }
        QuantityExpr::Difference { left, right } => {
            freeze_entry_counter_expression(left, state, ability, freeze_reference);
            freeze_entry_counter_expression(right, state, ability, freeze_reference);
        }
    }
}

/// CR 611.2d + CR 608.2h: a resolving ability determines the variable in its
/// continuous effect once, on resolution, rather than when the creature enters.
/// CR 109.5 + CR 113.7a: use the resolving ability's controller and source context,
/// including its carried source information if the source has left.
fn freeze_entry_counter_quantities(
    replacement: &mut ReplacementDefinition,
    state: &GameState,
    ability: &ResolvedAbility,
) {
    if replacement.event != ReplacementEvent::ChangeZone
        || replacement.destination_zone != Some(Zone::Battlefield)
    {
        return;
    }
    let mut current = replacement.execute.as_deref_mut();
    while let Some(execute) = current {
        // CR 614.1c: only the event-modifier prefix changes how the object enters.
        // Choice branches and post-entry instructions retain their own timing.
        if !EventModifiers::is_event_modifier_effect(&execute.effect) {
            break;
        }
        if let Effect::PutCounter { count, .. } = execute.effect.as_mut() {
            freeze_entry_counter_expression(count, state, ability, |qty| {
                !entry_counter_reference_is_deferred(qty)
            });
        }
        current = execute.sub_ability.as_deref_mut();
    }
}

fn replacement_targets(
    state: &GameState,
    ability: &ResolvedAbility,
    target: &TargetFilter,
) -> Vec<TargetRef> {
    if matches!(target, TargetFilter::Any) {
        if let Some(context) = &ability.context.forwarded_result_context {
            // CR 608.2c + CR 400.7: a forward-result reanimation rider binds to
            // the newly moved object, not the spell's original declared target.
            // `Some([])` is a completed empty result and must not fall back to
            // stale targets; object pins prevent a later incarnation from
            // receiving the rider.
            return context
                .targets
                .iter()
                .filter(|target| match target {
                    TargetRef::Object(id) => context.object_pin_is_current(*id, state),
                    TargetRef::Player(_) => true,
                })
                .cloned()
                .collect();
        }
        return ability.targets.clone();
    }

    // CR 201.5 + CR 400.7: SelfRef names the ability's source as the object
    // the carried replacement affects (Unearth's "If it would leave the
    // battlefield, exile it instead", `valid_card: SelfRef`), so it installs on
    // the source only while `self_ref_binding` can still find it. A replacement
    // that affects no object (Crafty Cutpurse's token controller redirect) is
    // not hosted on its source: it uses the floating `TargetFilter::None` arm
    // in `resolve` (CR 113.7a).
    if matches!(target, TargetFilter::SelfRef) {
        return ability
            .self_ref_binding(state)
            .map(TargetRef::Object)
            .into_iter()
            .collect();
    }

    // CR 614.1a + CR 601.2c + CR 400.7: a rider bound to an earlier declared
    // target slot ("If that creature would die this turn, exile it instead" after
    // an intervening Equipment target — Fiery Annihilation) installs on that
    // slot's object only while it is still the announced object on the live
    // board. An illegal-at-resolution slot (CR 608.2b), a missing pin, or a
    // same-id new object installs nothing.
    if let TargetFilter::ParentTargetSlot { index } = target {
        return crate::game::targeting::declared_slot_referent(state, ability, *index)
            .filter(|referent| referent.pin.is_current(state))
            .map(|referent| vec![TargetRef::Object(referent.id)])
            .unwrap_or_default();
    }

    resolve_event_context_target(state, target, ability.source_id)
        .into_iter()
        .collect()
}

/// CR 614.1a + CR 514.2: Push a replacement effect onto the parent
/// ability's target object or player at resolution time. Used by riders like
/// "If that creature would die this turn, exile it instead." attached to
/// damage-dealing spells/abilities. The carried `ReplacementDefinition`
/// is appended to each targeted object's `replacement_definitions`, or to
/// GameState pending damage replacements for player-scoped damage effects.
///
/// Multiple targets each receive their own copy of the replacement —
/// `valid_card: SelfRef` inside the carried definition naturally binds
/// to the carrying object, so each instance fires only for its host.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::AddTargetReplacement {
        replacement,
        target,
    } = &ability.effect
    else {
        return Err(EffectError::MissingParam(
            "AddTargetReplacement replacement".to_string(),
        ));
    };

    let mut attached = 0usize;

    // CR 614.1a: `TargetFilter::None` is the "no per-target binding" signal —
    // the carried replacement is self-contained (its own source/target filters
    // already constrain when it fires) and is pushed directly to the global
    // pending_damage_replacements through `install_floating_damage_replacement`,
    // the install authority for floating replacements. Used by triggered
    // creation of turn-bound damage-modification replacements (Rankle and
    // Torbran's "If a source would deal damage to a player or battle this
    // turn..."; I Call for Slaughter's "If a source you control would deal
    // damage this turn, it deals that much damage plus 1 instead."), of
    // turn-bound token-creation replacements (Kaya, Geist Hunter's −2; Crafty
    // Cutpurse's "each token that would be created under an opponent's control
    // this turn is created under your control instead", whose "your" is the
    // `ability.controller` latched below) and of floating entry-modifier riders
    // (Communal Brewing's "that creature enters with X additional +1/+1
    // counters on it").
    if matches!(target, TargetFilter::None) {
        let replacement = replacement_with_ability_expiry(replacement, ability)?;
        let Some(mut replacement) = bind_replacement_to_trigger_source(replacement, state) else {
            return Ok(());
        };
        freeze_entry_counter_quantities(&mut replacement, state, ability);
        // CR 113.7a + CR 608.2h: a floating rider may still refer to its source
        // ("this enchantment"); a battlefield source anchors host-relative gates
        // and deferred quantities (via `replacement_source_host`).
        // Resolution-owned counts, including source reads, were frozen above
        // with the resolving ability's source/LKI context (CR 611.2d). Deferred
        // entering-object reads stay live. A source not on the battlefield at
        // resolution still leaves the host as the sentinel for gates and any
        // deferred source read.
        super::install_floating_damage_replacement(
            state,
            replacement,
            ability.controller,
            ability.source_id,
            &[Zone::Battlefield],
        );
        attached += 1;
    } else {
        for resolved_target in replacement_targets(state, ability, target) {
            match resolved_target {
                TargetRef::Object(obj_id) => {
                    let mut replacement = replacement_with_ability_expiry(replacement, ability)?;
                    replacement.fix_legacy_parse_time_consumed_flag();
                    // CR 611.2b: A "for as long as you control [source]" gated
                    // replacement is a continuous effect that must survive every
                    // layer reset (evaluate_layers rebuilds live
                    // replacement_definitions from base — layers.rs). The base
                    // store is otherwise the printed baseline (CR 613.1,
                    // game_object.rs); this is a deliberate, prune-bounded
                    // exception: the three lapse prunes (control swap, source
                    // leave-play, host leave-play) remove this def on every
                    // CR 611.2b lapse, so base never accumulates a stale runtime
                    // rider. printed_cards.rs is the only intrinsic base-write
                    // precedent, and this is the only additive-runtime base push in
                    // the tree, so the exception is documented here.
                    //
                    // Everything that is NOT one of these three classes now goes
                    // through `GameObject::install_resolution_replacement` instead
                    // (CR 611.2a) — the typed-provenance authority that survives the
                    // CR 613.1 reset via the carried set rather than via a base copy.
                    // The trio deliberately stays OUTSIDE it: a def that is both
                    // base-resident and `Resolution`-stamped would be reseeded from
                    // base AND carried from live, i.e. applied twice.
                    // A turn-bound die-exile rider must also survive a layer
                    // reset: a damaged creature can gain/lose characteristics
                    // or enter combat before it dies. Cleanup prunes this
                    // narrowly scoped base copy at end of turn.
                    // A host-lifetime rider (CR 702.84a "if it would leave the
                    // battlefield, exile it instead", stamped
                    // `UntilHostLeavesPlay`) is the same class: it must survive
                    // every CR 613.1 reseed so the redirect still fires after the
                    // returned permanent gains/loses characteristics, and its
                    // base+live copies are pruned together the instant the host
                    // leaves the battlefield (`prune_controller_controls_source_on_leave`,
                    // CR 400.7) so it never revives on a same-ObjectId re-entry.
                    //
                    // Acknowledged out-of-scope edges (NOT fixed here): (1) Cleave
                    // re-baselining only touches spells on the stack (casting.rs)
                    // and structurally cannot hit a battlefield host — non-issue.
                    // (2) Turning the LOCKED HOST face-down
                    // (morph.rs apply_face_down_creature_characteristics clears
                    // base+live replacement defs, CR 708.2a) would end the lock
                    // early — an under-prune, strictly safer than a revival; rare
                    // corner, out of scope.
                    let durable_die_exile =
                        crate::game::printed_cards::is_runtime_target_die_exile_replacement(
                            &replacement,
                        );
                    let host_lifetime =
                        crate::game::printed_cards::is_runtime_host_lifetime_replacement(
                            &replacement,
                        );
                    let install_to_base = durable_die_exile
                        || host_lifetime
                        || matches!(
                            replacement.condition,
                            Some(ReplacementCondition::ControllerControlsSource { .. })
                        );
                    if let Some(obj) = state.objects.get_mut(&obj_id) {
                        if install_to_base {
                            // CR 611.2b / CR 702.84a: the three hand-audited durable
                            // classes stay BASE-resident and are reseeded into live
                            // by every CR 613.1 pass. They must NOT be stamped
                            // `Resolution`: that would place them in base AND in the
                            // carried set, and apply them twice.
                            std::sync::Arc::make_mut(&mut obj.base_replacement_definitions)
                                .push(replacement.clone());
                            obj.replacement_definitions.push(replacement);
                        } else {
                            // CR 611.2a: everything else is a resolution-created
                            // continuous effect with a stated window. The authority
                            // stamps it `Resolution` when it carries an enforceable
                            // expiry, and fails closed (live-only, today's behavior)
                            // when it does not.
                            obj.install_resolution_replacement(replacement);
                        }
                        attached += 1;
                    }
                }
                TargetRef::Player(player) => {
                    let mut replacement = replacement_with_ability_expiry(replacement, ability)?;
                    if matches!(
                        replacement.event,
                        crate::types::replacements::ReplacementEvent::DamageDone
                    ) && replacement.damage_target_filter.is_none()
                    {
                        replacement.damage_target_filter =
                            Some(DamageTargetFilter::PlayerOrPermanentsControlledBy {
                                player: DamageTargetPlayerScope::Specific(player),
                                permanent_type: None,
                                // CR 109.1: no "other" article in this class —
                                // the granted shield covers every permanent the
                                // targeted player controls.
                                source_scope: SourceExclusion::Include,
                            });
                    }
                    state.pending_damage_replacements.push(replacement);
                    attached += 1;
                }
            }
        }
    }

    if attached > 0 {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::AddTargetReplacement,
            source_id: ability.source_id,
            subject: None,
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::replacement::{replace_event, ReplacementResult};
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, DamageModification, DamageTargetPlayerScope, Duration,
        ForwardedResultContext, ReplacementDefinition, RestrictionExpiry, SourceExclusion,
        TargetFilter, TypeFilter, TypedFilter,
    };
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::proposed_event::ProposedEvent;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::zones::Zone;

    fn damage_to(target: TargetRef, amount: u32) -> ProposedEvent {
        ProposedEvent::Damage {
            source_id: ObjectId(99),
            target,
            amount,
            is_combat: false,
            applied: Default::default(),
        }
    }

    /// CR 514.2 + CR 615.3: a shield-carrying replacement installed by a resolving
    /// ability that stated NO representable window gets the engine's turn window at
    /// this seam, so `turns::execute_cleanup` — which reads `expiry` alone — can
    /// still end it. The `EndOfTurn` value is an engine default, NOT a CR rule; see
    /// `ReplacementDefinition::with_resolution_shield_expiry`.
    ///
    /// DEFENCE IN DEPTH: no corpus card reaches this stamp today — exactly one
    /// `AddTargetReplacement` shield node exists (Impulsive Maneuvers) and the
    /// parser already stamps it `EndOfTurn`. This guard exists so that removing
    /// cleanup's `shield_kind` blanket cannot make a future unstamped runtime
    /// shield immortal.
    #[test]
    fn unstated_duration_shield_install_gets_engine_turn_window() {
        use crate::types::ability::{Effect, PreventionAmount, ShieldKind};

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let target = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        // `prevention_shield` is the ONE builder that deliberately stamps no
        // lifetime (it is shared with the printed static lowering), so the `None`
        // reaching the install seam is genuine and not a builder artifact.
        let shield = ReplacementDefinition::new(ReplacementEvent::DamageDone)
            .prevention_shield(PreventionAmount::All)
            .valid_card(TargetFilter::SelfRef);
        assert_eq!(
            shield.expiry, None,
            "fixture must reach the seam with an unset expiry"
        );

        let ability = ResolvedAbility::new(
            Effect::AddTargetReplacement {
                replacement: Box::new(shield),
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(target)],
            source,
            PlayerId(0),
        );
        assert_eq!(
            ability.duration, None,
            "fixture must reach the seam with both duration carriers unset"
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Positive reach-guard: the definition actually landed on the target.
        let obj = state.objects.get(&target).unwrap();
        assert_eq!(obj.replacement_definitions.len(), 1);
        assert_eq!(
            obj.replacement_definitions[0].shield_kind,
            ShieldKind::Prevention {
                amount: PreventionAmount::All
            }
        );
        assert_eq!(
            obj.replacement_definitions[0].expiry,
            Some(RestrictionExpiry::EndOfTurn),
            "CR 514.2: an unstated-window resolution shield takes the engine turn default"
        );

        // Negative sibling: a NON-shield rider installed the same way keeps
        // `expiry: None` — the gate is `shield_kind.is_shield()`, not "stamp
        // everything". CR 611.2b / CR 702.84a riders are legitimately durable.
        let rider = ReplacementDefinition::new(ReplacementEvent::Moved)
            .valid_card(TargetFilter::SelfRef)
            .destination_zone(Zone::Exile);
        let rider_ability = ResolvedAbility::new(
            Effect::AddTargetReplacement {
                replacement: Box::new(rider),
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(target)],
            source,
            PlayerId(0),
        );
        resolve(&mut state, &rider_ability, &mut events).unwrap();

        let obj = state.objects.get(&target).unwrap();
        let installed_rider = obj
            .replacement_definitions
            .as_slice()
            .iter()
            .find(|r| r.event == ReplacementEvent::Moved)
            .expect("non-shield rider must be installed");
        assert!(
            installed_rider.shield_kind.is_none(),
            "reach-guard: the negative sibling must genuinely be a non-shield"
        );
        assert_eq!(
            installed_rider.expiry, None,
            "a non-shield rider must not acquire a turn window at this seam"
        );
    }

    #[test]
    fn forwarded_result_context_binds_an_any_replacement_to_the_moved_object() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Reanimator".to_string(),
            Zone::Battlefield,
        );
        let reanimated = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Reanimated Creature".to_string(),
            Zone::Battlefield,
        );
        let rider = ReplacementDefinition::new(ReplacementEvent::Moved)
            .valid_card(TargetFilter::SelfRef)
            .destination_zone(Zone::Exile)
            .expiry(RestrictionExpiry::UntilHostLeavesPlay);
        let mut ability = ResolvedAbility::new(
            Effect::AddTargetReplacement {
                replacement: Box::new(rider),
                target: TargetFilter::Any,
            },
            vec![],
            source,
            PlayerId(0),
        );
        ability.context.forwarded_result_context = Some(Box::new(
            ForwardedResultContext::from_object_ids(&state, &[reanimated]),
        ));

        resolve(&mut state, &ability, &mut Vec::new()).unwrap();

        assert!(
            state.objects[&reanimated]
                .replacement_definitions
                .as_slice()
                .iter()
                .any(|replacement| replacement.event == ReplacementEvent::Moved),
            "the rider must bind to the forwarded reanimated object"
        );
    }

    #[test]
    fn empty_forwarded_result_context_does_not_fall_back_to_declared_targets() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Reanimator".to_string(),
            Zone::Battlefield,
        );
        let declared_target = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Declared Target".to_string(),
            Zone::Battlefield,
        );
        let rider = ReplacementDefinition::new(ReplacementEvent::Moved)
            .valid_card(TargetFilter::SelfRef)
            .destination_zone(Zone::Exile);
        let mut ability = ResolvedAbility::new(
            Effect::AddTargetReplacement {
                replacement: Box::new(rider),
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(declared_target)],
            source,
            PlayerId(0),
        );
        ability.context.forwarded_result_context = Some(Box::new(
            ForwardedResultContext::from_object_ids(&state, &[]),
        ));

        resolve(&mut state, &ability, &mut Vec::new()).unwrap();

        assert!(state.objects[&declared_target]
            .replacement_definitions
            .is_empty());
    }

    #[test]
    fn stale_forwarded_result_context_does_not_bind_a_replacement() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Reanimator".to_string(),
            Zone::Battlefield,
        );
        let moved = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Returned Creature".to_string(),
            Zone::Battlefield,
        );
        let context = ForwardedResultContext::from_object_ids(&state, &[moved]);
        state.objects.get_mut(&moved).unwrap().incarnation += 1;
        let rider = ReplacementDefinition::new(ReplacementEvent::Moved)
            .valid_card(TargetFilter::SelfRef)
            .destination_zone(Zone::Exile);
        let mut ability = ResolvedAbility::new(
            Effect::AddTargetReplacement {
                replacement: Box::new(rider),
                target: TargetFilter::Any,
            },
            vec![],
            source,
            PlayerId(0),
        );
        ability.context.forwarded_result_context = Some(Box::new(context));

        resolve(&mut state, &ability, &mut Vec::new()).unwrap();

        assert!(state.objects[&moved].replacement_definitions.is_empty());
    }

    #[test]
    fn any_replacement_without_forwarded_context_uses_declared_targets() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let target = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Target".to_string(),
            Zone::Battlefield,
        );
        let rider = ReplacementDefinition::new(ReplacementEvent::Moved)
            .valid_card(TargetFilter::SelfRef)
            .destination_zone(Zone::Exile);
        let ability = ResolvedAbility::new(
            Effect::AddTargetReplacement {
                replacement: Box::new(rider),
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(target)],
            source,
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut Vec::new()).unwrap();

        assert!(state.objects[&target]
            .replacement_definitions
            .as_slice()
            .iter()
            .any(|replacement| replacement.event == ReplacementEvent::Moved));
    }

    /// CR 611.2a: a stated duration this seam cannot represent must not be
    /// shortened to the end-of-turn fallback — and the refusal must be LOUD.
    ///
    /// The `expect_err` is the second half of the pin: an `unwrap()` here would
    /// pass again the moment the seam goes back to a successful no-op, which is
    /// exactly how the previous revision reported an unenforceable lifetime as
    /// applied. Sibling of
    /// `game::effects::tap_untap::tests::host_duration_on_non_untap_replacement_fails_closed`,
    /// which covers the host-bound wordings on all three call sites.
    #[test]
    fn stated_unrepresentable_duration_does_not_install_a_shield() {
        use crate::types::ability::{Effect, PreventionAmount};

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let target = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let shield = ReplacementDefinition::new(ReplacementEvent::DamageDone)
            .prevention_shield(PreventionAmount::All)
            .valid_card(TargetFilter::SelfRef);
        let mut ability = ResolvedAbility::new(
            Effect::AddTargetReplacement {
                replacement: Box::new(shield),
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(target)],
            source,
            PlayerId(0),
        );
        ability.duration = Some(Duration::UntilEndOfNextTurnOf {
            player: crate::types::ability::PlayerScope::Controller,
        });

        resolve(&mut state, &ability, &mut Vec::new()).expect_err(
            "CR 611.2a: an unrepresentable stated duration must FAIL the resolution, \
             not succeed with no effect",
        );

        assert!(
            state.objects[&target].replacement_definitions.is_empty(),
            "CR 611.2a: a stated next-turn duration must not be shortened to EndOfTurn"
        );
    }

    #[test]
    fn die_exile_rider_with_legacy_is_consumed_applies_exile_redirect() {
        use crate::types::ability::{AbilityKind, Effect, TargetFilter};
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let target = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&target)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Creature);

        let mut repl = ReplacementDefinition::new(ReplacementEvent::Moved)
            .valid_card(TargetFilter::SelfRef)
            .destination_zone(Zone::Graveyard)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChangeZone {
                    origin: Some(Zone::Battlefield),
                    destination: Zone::Exile,
                    target: TargetFilter::SelfRef,
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: None,
                    enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                    conditional_enter_with_counters: vec![],
                    face_down_profile: None,
                    enters_modified_if: None,
                },
            ));
        repl.is_consumed = true;
        repl.expiry = Some(RestrictionExpiry::EndOfTurn);
        repl.fix_legacy_parse_time_consumed_flag();

        let ability = ResolvedAbility::new(
            Effect::AddTargetReplacement {
                replacement: Box::new(repl),
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(target)],
            ObjectId(0),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let proposed = crate::types::proposed_event::ProposedEvent::zone_change(
            target,
            Zone::Battlefield,
            Zone::Graveyard,
            None,
        );
        let result = crate::game::replacement::replace_event(&mut state, proposed, &mut events);
        match result {
            crate::game::replacement::ReplacementResult::Execute(
                crate::types::proposed_event::ProposedEvent::ZoneChange { to, .. },
            ) => assert_eq!(to, Zone::Exile),
            other => panic!("expected exile redirect, got {other:?}"),
        }
        assert!(
            state.objects.get(&target).unwrap().replacement_definitions[0].is_consumed,
            "one-shot rider must consume after applying"
        );
    }

    /// CR 611.2b + CR 611.2a + CR 613.1 (issue #8485, matrix row 21): the C4
    /// restructure — the three hand-audited DURABLE classes keep the base push and
    /// must NOT be stamped `Resolution` (that would put them in base AND in the
    /// carried set, applying them twice); everything else goes through
    /// `GameObject::install_resolution_replacement` instead.
    ///
    /// MULTI-AUTHORITY HOSTILE FIXTURE: the SAME host carries both arms at once — a
    /// `ControllerControlsSource`-gated durable rider and an ordinary end-of-turn
    /// rider — so one object exercises both sides of the restructure and a
    /// mis-routed arm cannot hide behind the other.
    #[test]
    fn controller_gated_rider_is_not_carried_twice_across_a_layer_pass() {
        use crate::types::ability::Effect;

        let mut state = GameState::new_two_player(42);
        let host = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Locked Permanent".to_string(),
            Zone::Battlefield,
        );
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Master Thief".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&host)
            .unwrap()
            .base_characteristics_initialized = true;

        // ARM 1: the CR 611.2b durable class — a bare untap-prevention rider with a
        // `WhileControllingHost` window, which `stamp_for_as_long_as_controlled_gate`
        // turns into a `ControllerControlsSource` applicability gate.
        let mut durable = ResolvedAbility::new(
            Effect::AddTargetReplacement {
                replacement: Box::new(ReplacementDefinition::new(ReplacementEvent::Untap)),
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(host)],
            source,
            PlayerId(0),
        );
        durable.duration = Some(Duration::WhileControllingHost);
        resolve(&mut state, &durable, &mut Vec::new()).unwrap();

        // ARM 2: an ordinary end-of-turn rider on the SAME host.
        let mut eot = ReplacementDefinition::new(ReplacementEvent::Moved)
            .valid_card(TargetFilter::SelfRef)
            .destination_zone(Zone::Graveyard);
        eot.expiry = Some(RestrictionExpiry::EndOfTurn);
        let transient = ResolvedAbility::new(
            Effect::AddTargetReplacement {
                replacement: Box::new(eot),
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(host)],
            source,
            PlayerId(0),
        );
        resolve(&mut state, &transient, &mut Vec::new()).unwrap();

        {
            let obj = &state.objects[&host];
            assert_eq!(
                obj.base_replacement_definitions.len(),
                1,
                "only the durable arm is base-written"
            );
            assert_eq!(obj.replacement_definitions.len(), 2);
            let gated = obj
                .replacement_definitions
                .iter_all()
                .find(|d| d.event == ReplacementEvent::Untap)
                .expect("the gated rider is installed");
            assert!(
                !gated.is_resolution_installed(),
                "CR 611.2b: a base-resident durable rider must stay `Characteristic`"
            );
            let carried = obj
                .replacement_definitions
                .iter_all()
                .find(|d| d.event == ReplacementEvent::Moved)
                .expect("the EOT rider is installed");
            assert!(
                carried.is_resolution_installed(),
                "CR 611.2a: an ordinary turn-bound rider IS resolution-created"
            );
        }

        state.layers_dirty.mark_full();
        crate::game::layers::evaluate_layers(&mut state);

        let obj = &state.objects[&host];
        assert_eq!(
            obj.replacement_definitions
                .iter_all()
                .filter(|d| d.event == ReplacementEvent::Untap)
                .count(),
            1,
            "the durable rider must be reseeded from base EXACTLY once, not \
             reseeded AND carried"
        );
        assert_eq!(
            obj.replacement_definitions
                .iter_all()
                .filter(|d| d.event == ReplacementEvent::Moved)
                .count(),
            1,
            "the carried EOT rider survives the CR 613.1 reset"
        );
    }

    #[test]
    fn pushes_eot_replacement_onto_target_object() {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        let mut repl = ReplacementDefinition::new(ReplacementEvent::Moved)
            .valid_card(TargetFilter::SelfRef)
            .destination_zone(Zone::Graveyard);
        repl.expiry = Some(RestrictionExpiry::EndOfTurn);

        let ability = ResolvedAbility::new(
            Effect::AddTargetReplacement {
                replacement: Box::new(repl),
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(id)],
            ObjectId(0),
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&id).unwrap();
        assert_eq!(obj.replacement_definitions.iter_all().count(), 1);
        assert_eq!(
            obj.replacement_definitions[0].expiry,
            Some(RestrictionExpiry::EndOfTurn)
        );
        // CR 611.2b gate-scoping: a transient (end-of-turn) rider WITHOUT a
        // `ControllerControlsSource` condition must stay live-only — it must NOT
        // be mirrored into the printed-baseline base store (CR 613.1). Only the
        // duration-bound can't-untap class gets the durable base-push.
        assert!(
            obj.base_replacement_definitions.is_empty(),
            "non-ControllerControlsSource rider must not be pushed to base"
        );
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::AddTargetReplacement,
                ..
            }
        )));
    }

    #[test]
    fn global_enter_as_copy_replacement_freezes_parent_target_copy_source() {
        let mut state = GameState::new_two_player(42);
        let copy_source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Chosen Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&copy_source)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Creature);

        let mut replacement = ReplacementDefinition::new(ReplacementEvent::Moved)
            .valid_card(TargetFilter::Or {
                filters: vec![
                    TargetFilter::Typed(TypedFilter::creature()),
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Planeswalker)),
                ],
            })
            .destination_zone(Zone::Battlefield)
            .execute(AbilityDefinition::new(
                crate::types::ability::AbilityKind::Spell,
                Effect::BecomeCopy {
                    target: TargetFilter::ParentTarget,
                    recipient: crate::types::ability::CopyRecipient::Source,
                    duration: None,
                    mana_value_limit: None,
                    additional_modifications: Vec::new(),
                },
            ));
        replacement.consume_on_apply = true;
        replacement.expiry = Some(RestrictionExpiry::EndOfTurn);

        let ability = ResolvedAbility::new(
            Effect::AddTargetReplacement {
                replacement: Box::new(replacement),
                target: TargetFilter::None,
            },
            vec![TargetRef::Object(copy_source)],
            ObjectId(0),
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let installed = state
            .pending_damage_replacements
            .last()
            .expect("global replacement shield must be installed");
        let execute = installed.execute.as_ref().expect("copy execute");
        let Effect::BecomeCopy { target, .. } = &*execute.effect else {
            panic!("expected BecomeCopy execute, got {:?}", execute.effect);
        };
        assert_eq!(
            *target,
            TargetFilter::SpecificObject { id: copy_source },
            "the chosen creature must be captured before the later entry event"
        );
    }

    #[test]
    fn pushes_damage_replacement_for_triggering_player() {
        let mut state = GameState::new_two_player(42);
        state.current_trigger_event = Some(GameEvent::DamageDealt {
            source_id: ObjectId(7),
            target: TargetRef::Player(PlayerId(1)),
            amount: 3,
            is_combat: true,
            excess: 0,
            source_incarnation: None,
        });

        let replacement = ReplacementDefinition::new(ReplacementEvent::DamageDone)
            .damage_modification(DamageModification::Double);
        let mut ability = ResolvedAbility::new(
            Effect::AddTargetReplacement {
                replacement: Box::new(replacement),
                target: TargetFilter::TriggeringPlayer,
            },
            Vec::new(),
            ObjectId(7),
            PlayerId(0),
        );
        ability.duration = Some(Duration::UntilNextTurnOf {
            player: crate::types::ability::PlayerScope::Controller,
        });

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.pending_damage_replacements.len(), 1);
        let pending = &state.pending_damage_replacements[0];
        assert_eq!(
            pending.damage_target_filter,
            Some(DamageTargetFilter::PlayerOrPermanentsControlledBy {
                player: DamageTargetPlayerScope::Specific(PlayerId(1)),
                permanent_type: None,
                source_scope: SourceExclusion::Include,
            })
        );
        assert_eq!(
            pending.expiry,
            Some(RestrictionExpiry::UntilPlayerNextTurn {
                player: PlayerId(0)
            })
        );

        let proposed = damage_to(TargetRef::Player(PlayerId(1)), 2);
        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::Execute(ProposedEvent::Damage { amount, .. }) = result else {
            panic!("expected modified damage event, got {result:?}");
        };
        assert_eq!(amount, 4);

        let permanent = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Permanent".to_string(),
            Zone::Battlefield,
        );
        let proposed = damage_to(TargetRef::Object(permanent), 3);
        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::Execute(ProposedEvent::Damage { amount, .. }) = result else {
            panic!("expected modified permanent damage event, got {result:?}");
        };
        assert_eq!(amount, 6);
    }

    #[test]
    fn pending_damage_replacement_expires_on_controllers_next_turn() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.current_trigger_event = Some(GameEvent::DamageDealt {
            source_id: ObjectId(7),
            target: TargetRef::Player(PlayerId(1)),
            amount: 3,
            is_combat: true,
            excess: 0,
            source_incarnation: None,
        });

        let replacement = ReplacementDefinition::new(ReplacementEvent::DamageDone)
            .damage_modification(DamageModification::Double);
        let mut ability = ResolvedAbility::new(
            Effect::AddTargetReplacement {
                replacement: Box::new(replacement),
                target: TargetFilter::TriggeringPlayer,
            },
            Vec::new(),
            ObjectId(7),
            PlayerId(0),
        );
        ability.duration = Some(Duration::UntilNextTurnOf {
            player: crate::types::ability::PlayerScope::Controller,
        });

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert_eq!(state.pending_damage_replacements.len(), 1);

        crate::game::turns::execute_untap(&mut state, &mut events);
        assert!(state.pending_damage_replacements.is_empty());

        let proposed = damage_to(TargetRef::Player(PlayerId(1)), 2);
        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::Execute(ProposedEvent::Damage { amount, .. }) = result else {
            panic!("expected unmodified damage event, got {result:?}");
        };
        assert_eq!(amount, 2);
    }

    #[test]
    fn target_filter_none_pushes_global_replacement_without_inference() {
        // CR 614.1a: `TargetFilter::None` is the no-binding mode used by
        // self-contained turn-bound damage-modification replacements
        // (Rankle and Torbran, I Call for Slaughter). The resolver must
        // push the carried replacement directly to
        // `pending_damage_replacements` WITHOUT inferring a
        // `damage_target_filter` from a player target — the carried
        // replacement's own source/target/scope filters are the source
        // of truth.
        let mut state = GameState::new_two_player(42);
        let replacement = ReplacementDefinition::new(ReplacementEvent::DamageDone)
            .damage_modification(DamageModification::Plus {
                value: crate::types::ability::QuantityExpr::Fixed { value: 1 },
            })
            .damage_source_filter(TargetFilter::Typed(
                crate::types::ability::TypedFilter::default()
                    .controller(crate::types::ability::ControllerRef::You),
            ));
        let mut ability = ResolvedAbility::new(
            Effect::AddTargetReplacement {
                replacement: Box::new(replacement),
                target: TargetFilter::None,
            },
            Vec::new(),
            ObjectId(7),
            PlayerId(0),
        );
        ability.duration = Some(Duration::UntilEndOfTurn);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.pending_damage_replacements.len(), 1);
        let pending = &state.pending_damage_replacements[0];
        // Critical: damage_target_filter must remain None — no per-target
        // inference (which would scope to a specific player).
        assert_eq!(pending.damage_target_filter, None);
        assert_eq!(pending.expiry, Some(RestrictionExpiry::EndOfTurn));
    }

    /// CR 109.4 + CR 614.1a: discriminating runtime test for the
    /// controller-anchor fix. A global "If a source you control would deal
    /// damage this turn, it deals that much damage plus 1 instead." replacement
    /// (`damage_source_filter = controller You`) is pushed under the sentinel
    /// `ObjectId(0)`. The boost MUST fire for damage from a source controlled by
    /// the installing player, and MUST NOT fire for damage from an opponent's
    /// source.
    ///
    /// The boosted-amount assertion (`amount, 3`) flips if the anchor read at
    /// `replacement.rs` is reverted: without it, `from_source(state, ObjectId(0))`
    /// yields `source_controller = None`, `ControllerRef::You` never matches, and
    /// the replacement is skipped (amount stays 2).
    #[test]
    fn global_source_you_control_boost_fires_for_own_source_only() {
        use crate::types::ability::{ControllerRef, TypedFilter};

        let mut state = GameState::new_two_player(42);
        // A source we control, and a source the opponent controls.
        let my_source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "My Bear".to_string(),
            Zone::Battlefield,
        );
        let their_source = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Their Bear".to_string(),
            Zone::Battlefield,
        );
        let victim = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Victim".to_string(),
            Zone::Battlefield,
        );

        let replacement = ReplacementDefinition::new(ReplacementEvent::DamageDone)
            .damage_modification(DamageModification::Plus {
                value: crate::types::ability::QuantityExpr::Fixed { value: 1 },
            })
            .damage_source_filter(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            ));
        let mut ability = ResolvedAbility::new(
            Effect::AddTargetReplacement {
                replacement: Box::new(replacement),
                target: TargetFilter::None,
            },
            Vec::new(),
            // Installing ability controlled by PlayerId(0) — the anchor source.
            ObjectId(7),
            PlayerId(0),
        );
        ability.duration = Some(Duration::UntilEndOfTurn);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert_eq!(state.pending_damage_replacements.len(), 1);
        assert_eq!(
            state.pending_damage_replacements[0].source_controller,
            Some(PlayerId(0)),
            "install chokepoint must stamp the activating ability's controller"
        );

        // Positive: damage from OUR source is boosted 2 -> 3.
        let proposed = ProposedEvent::Damage {
            source_id: my_source,
            target: TargetRef::Object(victim),
            amount: 2,
            is_combat: false,
            applied: Default::default(),
        };
        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::Execute(ProposedEvent::Damage { amount, .. }) = result else {
            panic!("expected modified damage event, got {result:?}");
        };
        assert_eq!(
            amount, 3,
            "a source we control must deal damage plus 1 (anchor read at the match site)"
        );

        // Negative: damage from the OPPONENT's source is unchanged.
        let proposed = ProposedEvent::Damage {
            source_id: their_source,
            target: TargetRef::Object(victim),
            amount: 2,
            is_combat: false,
            applied: Default::default(),
        };
        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::Execute(ProposedEvent::Damage { amount, .. }) = result else {
            panic!("expected unmodified damage event, got {result:?}");
        };
        assert_eq!(
            amount, 2,
            "an opponent's source must not be boosted by 'a source you control'"
        );
    }

    // Crafty Cutpurse end-to-end: a floating CreateToken replacement with
    // `token_owner_scope: Opponent` and `token_owner_redirect: You` redirects
    // opponent-created tokens to the installing ability's controller.
    // Covers CR 111.2 (token controller redirection — "the token enters the
    // battlefield under that player's control") + CR 614.1a (replacement
    // ordering: redirect applies before the token materializes).
    #[test]
    fn crafty_cutpurse_floating_redirect_redirects_opponent_tokens_to_controller() {
        use crate::types::ability::ControllerRef;
        use crate::types::proposed_event::TokenSpec;
        use std::collections::HashSet;

        let mut state = GameState::new_two_player(42);
        let cutpurse_id = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Crafty Cutpurse".to_string(),
            Zone::Battlefield,
        );

        // Build the replacement that the parsed trigger would install.
        let mut repl = ReplacementDefinition::new(ReplacementEvent::CreateToken)
            .token_owner_scope(ControllerRef::Opponent)
            .token_owner_redirect(ControllerRef::You);
        repl.expiry = Some(RestrictionExpiry::EndOfTurn);

        let install_ability = ResolvedAbility::new(
            Effect::AddTargetReplacement {
                replacement: Box::new(repl),
                target: TargetFilter::None,
            },
            Vec::new(),
            cutpurse_id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &install_ability, &mut events).unwrap();

        // Sanity: CR 113.7a — the replacement floats (not on Cutpurse), latched
        // to the installing controller, marked EOT-expiring.
        assert!(state.objects[&cutpurse_id]
            .replacement_definitions
            .iter_all()
            .next()
            .is_none());
        let installed = &state.pending_damage_replacements;
        assert_eq!(installed.len(), 1);
        assert_eq!(
            installed[0].token_owner_scope,
            Some(ControllerRef::Opponent)
        );
        assert_eq!(installed[0].token_owner_redirect, Some(ControllerRef::You));
        assert_eq!(installed[0].source_controller, Some(PlayerId(0)));
        assert_eq!(installed[0].expiry, Some(RestrictionExpiry::EndOfTurn));

        // Opponent (PlayerId(1)) proposes creating a Treasure token under their control.
        let token_spec = TokenSpec {
            characteristics: crate::types::proposed_event::TokenCharacteristics {
                display_name: "Treasure".to_string(),
                power: None,
                toughness: None,
                loyalty: None,
                core_types: vec![crate::types::card_type::CoreType::Artifact],
                subtypes: vec!["Treasure".to_string()],
                supertypes: Vec::new(),
                colors: Vec::new(),
                keywords: Vec::new(),
            },
            script_name: "Treasure".to_string(),
            static_abilities: Vec::new(),
            enter_with_counters: Vec::new(),
            tapped: false,
            enters_attacking: false,
            attack_request: crate::types::proposed_event::TokenAttackRequest::Unspecified,
            sacrifice_at: None,
            source_id: ObjectId(50),
            controller: PlayerId(1),
            attach_to: crate::types::proposed_event::TokenHostRequest::NotRequested,
        };
        let proposed = ProposedEvent::CreateToken {
            owner: PlayerId(1),
            spec: Box::new(token_spec),
            copy: None,
            enter_tapped: crate::types::proposed_event::EtbTapState::Unspecified,
            count: 1,
            appended: Vec::new(),
            applied: HashSet::new(),
        };

        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::Execute(ProposedEvent::CreateToken {
            owner, ref spec, ..
        }) = result
        else {
            panic!("expected modified CreateToken event, got {result:?}");
        };
        assert_eq!(
            owner,
            PlayerId(0),
            "Crafty Cutpurse should redirect opponent's token to its controller"
        );
        // CR 111.2: `spec.controller` is consumed by the apply path
        // (combat::enter_attacking defending-player resolution, ETB-counter
        // accounting) and must move with the redirected owner — otherwise an
        // enters-attacking Goblin Rabblemaster token would compute its
        // defender against the original effect controller (the opponent) and
        // end up attacking its new controller.
        assert_eq!(
            spec.controller,
            PlayerId(0),
            "spec.controller must follow the redirected owner under CR 111.2"
        );
    }

    // Crafty Cutpurse + Goblin Rabblemaster class: an opponent creates a token
    // *that's tapped and attacking*. The redirect rewires owner to Cutpurse's
    // controller; `spec.controller` must follow so the apply path's
    // `enter_attacking` lookup picks a defending player from the redirected
    // controller's opponents — not from the original effect's controller.
    #[test]
    fn crafty_cutpurse_redirects_spec_controller_for_enters_attacking_token() {
        use crate::types::ability::ControllerRef;
        use crate::types::proposed_event::TokenSpec;
        use std::collections::HashSet;

        let mut state = GameState::new_two_player(42);
        let cutpurse_id = create_object(
            &mut state,
            CardId(12),
            PlayerId(0),
            "Crafty Cutpurse".to_string(),
            Zone::Battlefield,
        );

        let mut repl = ReplacementDefinition::new(ReplacementEvent::CreateToken)
            .token_owner_scope(ControllerRef::Opponent)
            .token_owner_redirect(ControllerRef::You);
        repl.expiry = Some(RestrictionExpiry::EndOfTurn);

        let install_ability = ResolvedAbility::new(
            Effect::AddTargetReplacement {
                replacement: Box::new(repl),
                target: TargetFilter::None,
            },
            Vec::new(),
            cutpurse_id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &install_ability, &mut events).unwrap();

        // Opponent's Rabblemaster-style "create a 1/1 Goblin that's tapped
        // and attacking" — `enters_attacking: true`, `spec.controller: P1`.
        let token_spec = TokenSpec {
            characteristics: crate::types::proposed_event::TokenCharacteristics {
                display_name: "Goblin".to_string(),
                power: Some(1),
                toughness: Some(1),
                loyalty: None,
                core_types: vec![crate::types::card_type::CoreType::Creature],
                subtypes: vec!["Goblin".to_string()],
                supertypes: Vec::new(),
                colors: vec![crate::types::mana::ManaColor::Red],
                keywords: Vec::new(),
            },
            script_name: "Goblin".to_string(),
            static_abilities: Vec::new(),
            enter_with_counters: Vec::new(),
            tapped: true,
            enters_attacking: true,
            attack_request: crate::types::proposed_event::TokenAttackRequest::Unspecified,
            sacrifice_at: None,
            source_id: ObjectId(70),
            controller: PlayerId(1),
            attach_to: crate::types::proposed_event::TokenHostRequest::NotRequested,
        };
        let proposed = ProposedEvent::CreateToken {
            owner: PlayerId(1),
            spec: Box::new(token_spec),
            copy: None,
            enter_tapped: crate::types::proposed_event::EtbTapState::Unspecified,
            count: 1,
            appended: Vec::new(),
            applied: HashSet::new(),
        };

        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::Execute(ProposedEvent::CreateToken {
            owner, ref spec, ..
        }) = result
        else {
            panic!("expected modified CreateToken event, got {result:?}");
        };
        assert_eq!(owner, PlayerId(0));
        assert_eq!(
            spec.controller,
            PlayerId(0),
            "redirected enters-attacking token must carry the new controller \
             so enter_attacking picks the correct defender"
        );
    }

    // Symmetry guard: tokens already created under our control are untouched.
    // Without the `token_owner_scope: Opponent` filter the redirect would also
    // fire on our own tokens — but `find_applicable_replacements` skips the
    // entry when the proposed owner does not match the scope, so this is the
    // existing matcher's job; here we just make sure that's still true.
    #[test]
    fn crafty_cutpurse_does_not_redirect_own_tokens() {
        use crate::types::ability::ControllerRef;
        use crate::types::proposed_event::TokenSpec;
        use std::collections::HashSet;

        let mut state = GameState::new_two_player(42);
        let cutpurse_id = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Crafty Cutpurse".to_string(),
            Zone::Battlefield,
        );

        let mut repl = ReplacementDefinition::new(ReplacementEvent::CreateToken)
            .token_owner_scope(ControllerRef::Opponent)
            .token_owner_redirect(ControllerRef::You);
        repl.expiry = Some(RestrictionExpiry::EndOfTurn);

        let install_ability = ResolvedAbility::new(
            Effect::AddTargetReplacement {
                replacement: Box::new(repl),
                target: TargetFilter::None,
            },
            Vec::new(),
            cutpurse_id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &install_ability, &mut events).unwrap();
        assert_eq!(
            state.pending_damage_replacements.len(),
            1,
            "reach guard: the floating redirect is installed"
        );

        // Our own token creation — must not be intercepted.
        let token_spec = TokenSpec {
            characteristics: crate::types::proposed_event::TokenCharacteristics {
                display_name: "Saproling".to_string(),
                power: Some(1),
                toughness: Some(1),
                loyalty: None,
                core_types: vec![crate::types::card_type::CoreType::Creature],
                subtypes: vec!["Saproling".to_string()],
                supertypes: Vec::new(),
                colors: vec![crate::types::mana::ManaColor::Green],
                keywords: Vec::new(),
            },
            script_name: "Saproling".to_string(),
            static_abilities: Vec::new(),
            enter_with_counters: Vec::new(),
            tapped: false,
            enters_attacking: false,
            attack_request: crate::types::proposed_event::TokenAttackRequest::Unspecified,
            sacrifice_at: None,
            source_id: ObjectId(60),
            controller: PlayerId(0),
            attach_to: crate::types::proposed_event::TokenHostRequest::NotRequested,
        };
        let proposed = ProposedEvent::CreateToken {
            owner: PlayerId(0),
            spec: Box::new(token_spec),
            copy: None,
            enter_tapped: crate::types::proposed_event::EtbTapState::Unspecified,
            count: 1,
            appended: Vec::new(),
            applied: HashSet::new(),
        };

        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::Execute(ProposedEvent::CreateToken { owner, .. }) = result else {
            panic!("expected unmodified CreateToken event, got {result:?}");
        };
        assert_eq!(
            owner,
            PlayerId(0),
            "our own token creation must not be redirected by our own Crafty Cutpurse"
        );
    }

    /// Fixture for the floating entry-counter rider tests (issue #9991): a source
    /// `S` on the battlefield controlled by P1 that is neither a Dog nor a Wolf,
    /// P1's Dog / Dog Wolf / Wolf, P0's Wolf, and P1's creature spell `E` on the
    /// stack. Returns `(state, S, E)`.
    fn floating_entry_counter_fixture() -> (GameState, ObjectId, ObjectId) {
        use crate::types::card_type::CoreType;

        fn creature(
            state: &mut GameState,
            card: u64,
            owner: PlayerId,
            name: &str,
            zone: Zone,
            subtypes: &[&str],
        ) -> ObjectId {
            let id = create_object(state, CardId(card), owner, name.to_string(), zone);
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types
                .subtypes
                .extend(subtypes.iter().map(|s| s.to_string()));
            id
        }

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Rider Source".to_string(),
            Zone::Battlefield,
        );
        creature(
            &mut state,
            2,
            PlayerId(1),
            "P1 Dog",
            Zone::Battlefield,
            &["Dog"],
        );
        creature(
            &mut state,
            3,
            PlayerId(1),
            "P1 Dog Wolf",
            Zone::Battlefield,
            &["Dog", "Wolf"],
        );
        creature(
            &mut state,
            4,
            PlayerId(1),
            "P1 Wolf",
            Zone::Battlefield,
            &["Wolf"],
        );
        creature(
            &mut state,
            5,
            PlayerId(0),
            "P0 Wolf",
            Zone::Battlefield,
            &["Wolf"],
        );
        let entering = creature(
            &mut state,
            6,
            PlayerId(1),
            "Entering Human",
            Zone::Stack,
            &["Human"],
        );
        (state, source, entering)
    }

    /// Resolve a floating cast-enters-with rider shaped like Torgal, A Fine
    /// Hound's payload ("that creature enters with an additional +1/+1 counter on
    /// it for each Dog and/or Wolf you control"; the first-Human-each-turn trigger
    /// gate is omitted) the way a resolving trigger installs it:
    /// `AddTargetReplacement` with `TargetFilter::None`, a `PutCounter` whose count
    /// reads the installing controller's board, and a `valid_card` bound to the
    /// triggering spell.
    fn install_dog_wolf_entry_rider(state: &mut GameState, source: ObjectId, entering: ObjectId) {
        use crate::types::ability::{
            AbilityKind, ControllerRef, QuantityExpr, QuantityRef, TypedFilter,
        };
        use crate::types::counter::CounterType;
        use crate::types::game_state::{CastingVariant, StackEntry, StackEntryKind};
        use crate::types::identifiers::ObjectIncarnationRef;

        let subtype_you = |subtype: &str| {
            TargetFilter::Typed(
                TypedFilter::creature()
                    .subtype(subtype.to_string())
                    .controller(ControllerRef::You),
            )
        };
        let mut replacement = ReplacementDefinition::new(ReplacementEvent::ChangeZone)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::PutCounter {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount {
                            filter: TargetFilter::Or {
                                filters: vec![subtype_you("Dog"), subtype_you("Wolf")],
                            },
                        },
                    },
                    target: TargetFilter::SelfRef,
                },
            ))
            .valid_card(TargetFilter::SpecificObject {
                id: crate::types::identifiers::TRIGGERING_SPELL_PLACEHOLDER,
            })
            .destination_zone(Zone::Battlefield);
        replacement.consume_on_apply = true;

        // Model the live recipient and the pin carried by its resolving cast trigger.
        state.current_trigger_event = Some(GameEvent::SpellCast {
            card_id: CardId(6),
            controller: PlayerId(1),
            object_id: entering,
            cast_mana_value: None,
        });
        let mut ability = ResolvedAbility::new(
            Effect::AddTargetReplacement {
                replacement: Box::new(replacement),
                target: TargetFilter::None,
            },
            Vec::new(),
            source,
            PlayerId(1),
        );
        ability.context.triggering_spell =
            Some(ObjectIncarnationRef::from_object(&state.objects[&entering]));
        state.stack.push_back(StackEntry {
            id: entering,
            source_id: entering,
            controller: PlayerId(1),
            kind: StackEntryKind::Spell {
                card_id: CardId(6),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });
        let trigger_id = ObjectId(state.next_object_id);
        state.next_object_id += 1;
        state.resolving_stack_entry = Some(StackEntry {
            id: trigger_id,
            source_id: source,
            controller: PlayerId(1),
            kind: StackEntryKind::TriggeredAbility {
                source_id: source,
                ability: Box::new(ability.clone()),
                condition: None,
                trigger_event: state.current_trigger_event.clone(),
                description: None,
                source_name: String::new(),
                subject_match_count: None,
                die_result: None,
                provenance: None,
            },
        });
        let mut events = Vec::new();
        resolve(state, &ability, &mut events).unwrap();
        state.resolving_stack_entry = None;
        // Entry replacement runs after the resolving permanent spell is popped.
        state.stack.pop_back();
    }

    /// Drive the entering spell's stack-to-battlefield event through the
    /// replacement pipeline and return the counters it will enter with.
    fn entry_counters(
        state: &mut GameState,
        entering: ObjectId,
    ) -> Vec<(crate::types::counter::CounterType, u32)> {
        let mut events = Vec::new();
        let proposed = ProposedEvent::zone_change(entering, Zone::Stack, Zone::Battlefield, None);
        let result = replace_event(state, proposed, &mut events);
        let ReplacementResult::Execute(ProposedEvent::ZoneChange {
            enter_with_counters,
            ..
        }) = result
        else {
            panic!("expected the entering ZoneChange to execute, got {result:?}");
        };
        enter_with_counters
    }

    /// CR 611.2d: the rider fixes its count at install from the trigger controller's
    /// board: P1's Dog, Dog Wolf (counted once), and Wolf give 3, not P0's 1.
    /// The provenance assertions separately check the source anchor and controller
    /// latch. Entry-time controller threading matters only for deferred leaves
    /// that need that controller; no current corpus cast rider has such a leaf.
    #[test]
    fn floating_entry_counter_rider_counts_installing_controllers_board() {
        use crate::types::counter::CounterType;

        let (mut state, source, entering) = floating_entry_counter_fixture();
        install_dog_wolf_entry_rider(&mut state, source, entering);

        // Reach guard: exactly one rider, anchored to its installer.
        assert_eq!(state.pending_damage_replacements.len(), 1);
        let rider = &state.pending_damage_replacements[0];
        assert_eq!(rider.source_object, Some(source));
        assert_eq!(rider.source_controller, Some(PlayerId(1)));
        assert!(!rider.is_consumed);

        assert_eq!(
            entry_counters(&mut state, entering),
            vec![(CounterType::Plus1Plus1, 3)],
            "'you' is the installing controller (P1: Dog, Dog Wolf, Wolf), not PlayerId(0)"
        );
        assert!(
            state.pending_damage_replacements[0].is_consumed,
            "the one-shot rider must be consumed once it applies"
        );
    }

    /// CR 611.2d: a source control change after install cannot change the count
    /// already fixed from the trigger controller's board (P1's 3, not P0's 1).
    /// This guards install-time freezing, not the entry-time controller latch;
    /// only deferred leaves can exercise that latch, and no current corpus cast
    /// rider has a controller-dependent deferred leaf.
    #[test]
    fn floating_entry_counter_rider_you_is_latched_when_source_changes_control() {
        use crate::types::counter::CounterType;

        let (mut state, source, entering) = floating_entry_counter_fixture();
        install_dog_wolf_entry_rider(&mut state, source, entering);
        assert_eq!(
            state.pending_damage_replacements[0].source_object,
            Some(source),
            "reach guard: the rider is anchored, so the source is the live object below"
        );
        state.objects.get_mut(&source).unwrap().controller = PlayerId(0);

        assert_eq!(
            entry_counters(&mut state, entering),
            vec![(CounterType::Plus1Plus1, 3)]
        );
    }

    /// CR 611.2d: source departure after install cannot change the count already
    /// fixed from the trigger controller's board. P0 owns the source, but the
    /// trigger controller P1 supplied the frozen count of 3, not P0's 1.
    /// This does not discriminate entry-time controller threading, which matters
    /// only for deferred leaves; no current corpus cast rider has a deferred
    /// leaf that needs that controller.
    #[test]
    fn floating_entry_counter_rider_survives_source_leaving_battlefield() {
        use crate::types::counter::CounterType;

        let (mut state, source, entering) = floating_entry_counter_fixture();
        state.objects.get_mut(&source).unwrap().owner = PlayerId(0);
        install_dog_wolf_entry_rider(&mut state, source, entering);
        assert_eq!(
            state.pending_damage_replacements[0].source_object,
            Some(source),
            "reach guard: the rider was anchored before its source left"
        );
        let mut events = Vec::new();
        crate::game::zones::move_to_zone(&mut state, source, Zone::Graveyard, &mut events);
        assert_eq!(state.objects.get(&source).unwrap().zone, Zone::Graveyard);

        assert_eq!(
            entry_counters(&mut state, entering),
            vec![(CounterType::Plus1Plus1, 3)]
        );
    }
}
