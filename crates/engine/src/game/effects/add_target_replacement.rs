use crate::game::targeting::{extract_source_from_event, resolve_event_context_target};
use crate::types::ability::{
    AbilityDefinition, DamageTargetFilter, DamageTargetPlayerScope, Duration, Effect, EffectError,
    EffectKind, ReplacementCondition, ReplacementDefinition, ResolvedAbility, RestrictionExpiry,
    SourceExclusion, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::replacements::ReplacementEvent;

/// Whether a duration supplies a replacement expiry at the installation seam.
///
/// `Unstated` is deliberately distinct from `Unsupported`: only a truly absent
/// duration may use the engine's end-of-turn fallback. A stated duration that
/// this replacement lifecycle cannot enforce must fail closed rather than be
/// shortened to a different window (CR 611.2a).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReplacementDurationExpiry {
    Unstated,
    Explicit(RestrictionExpiry),
    /// CR 611.2a: the CONTROLLER's own next turn. The only class whose stamp
    /// needs the resolving ability's controller, so it is left unresolved here
    /// and named by each install seam. Keeping it out of `Explicit` is what
    /// makes the classification controller-free, which in turn lets the
    /// parse-time honesty net consult this same authority
    /// (`parser::oracle::demote_unenforceable_replacement_lifetimes`) with no
    /// second duration list.
    ExplicitControllerNextTurn,
    /// The duration is enforced by a separate applicability gate rather than
    /// an expiry prune — the CONTROL gate on the bare untap-prevention rider
    /// (`stamp_for_as_long_as_controlled_gate`). That gate is
    /// `ReplacementCondition::ControllerControlsSource`, so this class carries
    /// exactly one duration: `Duration::WhileControllingHost`. An install whose
    /// replacement cannot carry the gate fails closed in
    /// `replacement_with_ability_expiry`.
    GateControlled,
    Unsupported,
}

/// CR 611.2a: map a parser-side `Duration` onto the engine's replacement-side
/// lifecycle without conflating an absent duration with an unrepresentable one.
pub(crate) fn expiry_from_duration(duration: Option<&Duration>) -> ReplacementDurationExpiry {
    match duration {
        None => ReplacementDurationExpiry::Unstated,
        Some(Duration::UntilEndOfTurn) => {
            ReplacementDurationExpiry::Explicit(RestrictionExpiry::EndOfTurn)
        }
        Some(Duration::UntilEndOfCombat) => {
            ReplacementDurationExpiry::Explicit(RestrictionExpiry::EndOfCombat)
        }
        Some(Duration::UntilNextTurnOf {
            player: crate::types::ability::PlayerScope::Controller,
        }) => ReplacementDurationExpiry::ExplicitControllerNextTurn,
        // `UntilEndOfNextTurnOf` needs replacement-side arming, while non-controller
        // turn/step scopes need a resolution-time player binding. Neither is present
        // at this seam, so applying an `EndOfTurn` default would be rules-incorrect.
        Some(Duration::UntilNextTurnOf { .. })
        | Some(Duration::UntilEndOfNextTurnOf { .. })
        | Some(Duration::UntilNextStepOf { .. }) => ReplacementDurationExpiry::Unsupported,
        // CR 611.2b: the CONTROL reading, and only it. The gate this class
        // promises is `ReplacementCondition::ControllerControlsSource`, which
        // re-reads the source each layer pass, so it observes a control change
        // and a battlefield exit alike — and both genuinely end "for as long as
        // you control ~", the second because a permanent that has left the
        // battlefield is no longer controlled. CR 611.2b's own example is this
        // duration class (Master Thief, "gain control of target artifact for as
        // long as you control this creature"); it illustrates the duration
        // failing to START, not the two ways it ends, so the ends above are read
        // off the wording, not quoted from the rule.
        Some(Duration::WhileControllingHost) => ReplacementDurationExpiry::GateControlled,
        // CR 611.2a: the two NON-control host readings have no enforceable
        // lifetime at this seam, so they fail closed.
        //
        // They must not take the control gate: both survive a control change
        // while their source stays on the battlefield, and
        // `ControllerControlsSource` would end them there — a window SHORTER
        // than the printed one, which CR 611.2a forbids just as much as a
        // longer one.
        //
        // Nor is `RestrictionExpiry::UntilHostLeavesPlay` the answer for the
        // event deadline, despite the shared name: the `Duration` means "when
        // the SOURCE object leaves the battlefield", while the
        // `RestrictionExpiry` is pruned when the object HOSTING the definition
        // leaves (`layers.rs`, the host-left prune, which keys on the departed
        // id). For a rider installed on a TARGET those are different objects —
        // Old Fat Spider Can't See Me chapter II binds to the Saga while
        // hosting its shield on the targeted creature, so the identity mapping
        // would strand an immortal shield when the Saga leaves first. And
        // `WhileHostOnBattlefield` additionally ends on a phase-out
        // (CR 702.26f), which no `RestrictionExpiry` stamp expresses at all.
        //
        // Failing closed here is not a silent drop: the parse-time honesty net
        // demotes the whole line to `Effect::Unimplemented` before it can be
        // reported as supported, and the install seam returns a hard error if
        // one ever reaches it anyway.
        Some(Duration::UntilHostLeavesPlay) | Some(Duration::WhileHostOnBattlefield) => {
            ReplacementDurationExpiry::Unsupported
        }
        // CR 611.2b conditional windows are gated by
        // `stamp_for_as_long_as_controlled_gate` / `ReplacementCondition`, not by
        // an expiry stamp.
        Some(Duration::ForAsLongAs { .. })
        | Some(Duration::UntilSourceExilesAnotherCard)
        | Some(Duration::UntilOpponentBecomesMonarch)
        | Some(Duration::Permanent) => ReplacementDurationExpiry::Unsupported,
    }
}

/// CR 611.2a: does the install seam REFUSE this replacement under this stated
/// duration? The single authority for that question, and deliberately shared by
/// its two consumers:
///
/// * `replacement_with_ability_expiry`, at resolution, which turns a refusal
///   into a hard `EffectError` rather than a successful no-op; and
/// * `parser::oracle::demote_unenforceable_replacement_lifetimes`, the
///   post-lowering honesty net, which demotes a refused line to
///   `Effect::Unimplemented` so no card ever REPORTS the shape as supported.
///
/// Both axes are read from their own single owner — the duration axis from
/// `expiry_from_duration`, the form axis from `host_gate_enforceable` — so
/// neither list exists twice and a newly routed duration reaches both consumers
/// in the same breath.
///
/// The `expiry.is_some()` short-circuit mirrors the install seam exactly: a
/// replacement that already carries a parser-stamped expiry does not take its
/// lifetime from the ability's duration at all, so the duration cannot refuse it.
pub(crate) fn replacement_install_is_refused(
    replacement: &ReplacementDefinition,
    duration: Option<&Duration>,
) -> bool {
    if replacement.expiry.is_some() {
        return false;
    }
    match expiry_from_duration(duration) {
        ReplacementDurationExpiry::Unsupported => true,
        ReplacementDurationExpiry::GateControlled => !host_gate_enforceable(replacement),
        ReplacementDurationExpiry::Unstated
        | ReplacementDurationExpiry::Explicit(_)
        | ReplacementDurationExpiry::ExplicitControllerNextTurn => false,
    }
}

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

/// CR 603.2 + CR 603.3b + CR 117.3b: Concretize
/// `TRIGGERING_SPELL_PLACEHOLDER` — the parse-time sentinel
/// `parse_whenever_you_cast_enters_with_trigger` embeds inside a floating
/// (`TargetFilter::None`) replacement's `valid_card` — to the SPECIFIC spell
/// object referenced by the currently-resolving triggered ability's own
/// originating event (Runadi, Behemoth Caller and the Wildgrowth Archaic
/// cousin family — issue #6492 review).
///
/// Without this, a bare type/mana-value filter would let a DIFFERENT
/// qualifying creature — cast by the active player during the CR 117.3b
/// priority window between this trigger resolving and the originally-cast
/// spell resolving — consume the one-shot install first, leaving the intended
/// entrant uncountered. `state.current_trigger_event` is exactly this
/// ability's own trigger event (set by `push_resolving_trigger_context` for
/// the duration of its resolution — see `game/triggers.rs`), so
/// `extract_source_from_event` yields the specific cast spell's `ObjectId`.
///
/// If the event carries no extractable source, this fails CLOSED — matching
/// no object via the `ObjectId(0)` sentinel (never a real permanent) — rather
/// than silently widening back to the bare filter, which would reopen the
/// exact bug this binding exists to close.
///
/// No-op for every other floating-replacement install (Kaya's until-EOT token
/// doubler, Rankle and Torbran's damage-modification shields): none of them
/// ever embed the placeholder, so the walk finds nothing to replace.
fn bind_replacement_to_trigger_source(replacement: &mut ReplacementDefinition, state: &GameState) {
    let Some(valid_card) = replacement.valid_card.as_mut() else {
        return;
    };
    if !target_filter_contains_placeholder(valid_card) {
        return;
    }
    let bound = state
        .current_trigger_event
        .as_ref()
        .and_then(extract_source_from_event)
        .unwrap_or(ObjectId(0));
    concretize_triggering_spell_placeholder(valid_card, bound);
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

/// Whether `stamp_for_as_long_as_controlled_gate` can enforce a host-bound
/// duration on this replacement's FORM: the bare untap-prevention rider (event
/// `Untap`, no `execute`, no pre-existing condition). Shared by the stamp and
/// by `replacement_with_ability_expiry`'s `GateControlled` fail-closed arm, so
/// a shape the stamp skips can never be installed as if it were gated. The
/// duration axis is deliberately NOT part of this predicate — it lives in
/// `expiry_from_duration`, which both consumers also share.
fn host_gate_enforceable(replacement: &ReplacementDefinition) -> bool {
    replacement.event == ReplacementEvent::Untap
        && replacement.execute.is_none()
        && replacement.condition.is_none()
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

    // CR 201.5: SelfRef resolves to the ability's source object — text that
    // refers to the object it's on by name (or "~") means that particular
    // object. Used by self-installing replacements (Crafty Cutpurse: "When ~
    // enters, [until end of turn] each token that would be created under an
    // opponent's control is created under your control instead.") so the
    // trigger anchors the replacement on its own source without needing to
    // consult the target pipeline.
    if matches!(target, TargetFilter::SelfRef) {
        return vec![TargetRef::Object(ability.source_id)];
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
    // pending_damage_replacements. Used by triggered creation of turn-bound
    // damage-modification replacements (Rankle and Torbran's "If a source
    // would deal damage to a player or battle this turn..."; I Call for
    // Slaughter's "If a source you control would deal damage this turn,
    // it deals that much damage plus 1 instead.").
    if matches!(target, TargetFilter::None) {
        let mut replacement = replacement_with_ability_expiry(replacement, ability)?;
        bind_replacement_to_trigger_source(&mut replacement, state);
        state.pending_damage_replacements.push(replacement);
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
                    // precedent; there is no additive-runtime base-push
                    // precedent, so this exception is documented here.
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
                            std::sync::Arc::make_mut(&mut obj.base_replacement_definitions)
                                .push(replacement.clone());
                        }
                        obj.replacement_definitions.push(replacement);
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
                    recipient: TargetFilter::SelfRef,
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

    // Crafty Cutpurse end-to-end: a self-installed CreateToken replacement
    // with `token_owner_scope: Opponent` and `token_owner_redirect: You`
    // redirects opponent-created tokens to the source's controller.
    // Covers CR 111.2 (token controller redirection — "the token enters the
    // battlefield under that player's control") + CR 614.1a (replacement
    // ordering: redirect applies before the token materializes).
    #[test]
    fn crafty_cutpurse_self_install_redirects_opponent_tokens_to_controller() {
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
                target: TargetFilter::SelfRef,
            },
            Vec::new(),
            cutpurse_id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &install_ability, &mut events).unwrap();

        // Sanity: replacement landed on Cutpurse, marked EOT-expiring.
        let installed = &state.objects[&cutpurse_id].replacement_definitions;
        assert_eq!(installed.iter_all().count(), 1);
        assert_eq!(
            installed[0].token_owner_scope,
            Some(ControllerRef::Opponent)
        );
        assert_eq!(installed[0].token_owner_redirect, Some(ControllerRef::You));
        assert_eq!(installed[0].expiry, Some(RestrictionExpiry::EndOfTurn));

        // Opponent (PlayerId(1)) proposes creating a Treasure token under their control.
        let token_spec = TokenSpec {
            characteristics: crate::types::proposed_event::TokenCharacteristics {
                display_name: "Treasure".to_string(),
                power: None,
                toughness: None,
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
                target: TargetFilter::SelfRef,
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
                target: TargetFilter::SelfRef,
            },
            Vec::new(),
            cutpurse_id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &install_ability, &mut events).unwrap();

        // Our own token creation — must not be intercepted.
        let token_spec = TokenSpec {
            characteristics: crate::types::proposed_event::TokenCharacteristics {
                display_name: "Saproling".to_string(),
                power: Some(1),
                toughness: Some(1),
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
}
