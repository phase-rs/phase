//! `Effect::ExileResolvingSpellInsteadOfGraveyard` — the "exile it instead of
//! putting it into a graveyard as it resolves" self-replacement rider applied by
//! a `WhenAPlayerCasts` trigger to the spell that caused the trigger (Rod of
//! Absorption).
//!
//! CR 614.1a + CR 608.2n: "instead" makes this a replacement effect that swaps
//! the resolving spell's normal CR 608.2n graveyard destination for exile.
//! CR 607.2b + CR 406.6: the exiled spell is recorded as "exiled with" the
//! trigger source so the source's linked ability ("cast any number of spells
//! from among cards exiled with this artifact") can refer to the accumulating
//! set.

use crate::game::targeting::extract_source_from_event;
use crate::types::ability::{
    CastingPermission, DelayedTriggerCondition, Effect, EffectError, EffectKind, ExiledSpellRider,
    PermissionGrantee, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{DelayedTrigger, GameState};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// Stamp the per-object exile-instead-of-graveyard rider on the triggering spell
/// and link it to the trigger source.
///
/// The triggering spell is still on the stack when this effect resolves (the
/// trigger resolves above it), so this does NOT move the card — it sets the
/// marker the stack-resolution router reads when the spell finishes resolving.
/// The link source is stashed on the spell and turned into a real
/// `TrackedBySource` `ExileLink` only when the
/// spell actually reaches exile, so the linked set never lists a spell that was
/// countered or otherwise removed before it would have hit the graveyard.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 603.2 + CR 608.2c: the spell that caused the `WhenAPlayerCasts` trigger
    // is the trigger event's source object.
    let spell_id = state
        .current_trigger_event
        .as_ref()
        .and_then(extract_source_from_event);

    // CR 603.7a + CR 702.170c: the "If you do, ..." consequence to apply once
    // the exile actually happens — Feather's return-to-hand or Lilah's
    // become-plotted. Carried on the typed rider; `None` for the riderless Rod
    // of Absorption form.
    let on_exile = match &ability.effect {
        Effect::ExileResolvingSpellInsteadOfGraveyard { on_exile } => on_exile.clone(),
        _ => None,
    };

    if let Some(spell_id) = spell_id {
        // CR 614.1a: only a spell still on the stack can be redirected as it
        // resolves; if it already left the stack (countered, fizzled) there is
        // nothing to replace.
        if let Some(obj) = state.objects.get_mut(&spell_id) {
            if obj.zone == Zone::Stack {
                // CR 607.2b: record the linking source so the eventual exile is
                // tracked as "exiled with [this source]". Presence of this
                // typed source is also the CR 614.1a exile-instead marker.
                obj.exile_from_stack_linked_source = Some(ability.source_id);
                if let Some(rider) = on_exile {
                    // CR 603.7a: stamp the typed rider so the stack router
                    // applies the consequence when the replacement is actually
                    // APPLIED (the spell lands in exile) — not now. Cleared on
                    // any zone exit (zones.rs), so a counter or fizzle in
                    // response makes this a no-op.
                    obj.exile_from_stack_rider = Some(rider);
                }
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ExileResolvingSpellInsteadOfGraveyard,
        source_id: ability.source_id,
        subject: None,
    });
    Ok(())
}

/// CR 603.7a + CR 702.170c: On-application hook for the exile-instead
/// consequence — called from `stack.rs::resolve_top` after the exiled-instead
/// replacement has actually been APPLIED (the resolving spell landed in the
/// exile zone) and the spell carried the `exile_from_stack_rider` marker.
///
/// CR 603.7a: a consequence created "as the result of a replacement effect
/// being applied" exists only once the replacement applies, so this must NOT
/// run when the trigger resolves (a spell countered or fizzled in response
/// never reaches this hook — the marker is cleared on its zone exit). Dispatch:
/// Feather arms a delayed return; Lilah grants the plotted permission.
pub fn apply_exile_rider(
    state: &mut GameState,
    exiled_id: ObjectId,
    controller: PlayerId,
    source_id: ObjectId,
    rider: ExiledSpellRider,
    events: &mut Vec<GameEvent>,
) {
    match rider {
        ExiledSpellRider::ReturnTo {
            destination,
            timing,
        } => arm_return_to(state, exiled_id, controller, source_id, destination, timing),
        // CR 702.170c: the card is now in exile, so it may become plotted. Route
        // through the single grant-permission authority so `turn_plotted` is
        // stamped and the `BecomesPlotted` event fires exactly as for the
        // Aven Interrupter-class "exile ... it becomes plotted" grant.
        ExiledSpellRider::BecomePlotted => {
            grant_plotted(state, exiled_id, controller, source_id, events)
        }
    }
}

/// CR 702.170c: grant the exiled card the Plotted casting permission bound to
/// its owner and emit `BecomesPlotted` (CR 702.170d makes it castable without
/// paying its mana cost on a later turn). Delegates to
/// `grant_permission::resolve`, the single authority for casting-permission
/// grants — `turn_plotted` is stamped from `state.turn_number` there.
fn grant_plotted(
    state: &mut GameState,
    exiled_id: ObjectId,
    controller: PlayerId,
    source_id: ObjectId,
    events: &mut Vec<GameEvent>,
) {
    let ability = ResolvedAbility::new(
        Effect::GrantCastingPermission {
            // CR 702.170a: `turn_plotted` is a placeholder here — the grant
            // resolver stamps the concrete `state.turn_number`.
            permission: CastingPermission::Plotted { turn_plotted: 0 },
            // CR 608.2c: the exiled card is pre-bound in `targets` below;
            // `ParentTarget` reads it (never a player-chosen target slot).
            target: TargetFilter::ParentTarget,
            // CR 702.170d: a plotted card's *owner* may later cast it.
            grantee: PermissionGrantee::ObjectOwner,
        },
        vec![TargetRef::Object(exiled_id)],
        source_id,
        controller,
    );
    // The grant resolver is infallible for a bound object target; a missing
    // object (already left exile) yields an empty grant, which is correct.
    let _ = crate::game::effects::grant_permission::resolve(state, &ability, events);
}

/// CR 603.7a + CR 603.7b: arm Feather's one-shot delayed return. Pushes a
/// `DelayedTrigger` keyed on `timing` (Feather's `AtNextPhase { End }` fires at
/// the beginning of the next end step, whoever's turn it is) whose body returns
/// the concrete exiled card to `destination`. `origin: Some(Zone::Exile)` is
/// the CR 603.7c residency guard: if the card left exile before the trigger
/// fires it is a new object and is not returned.
fn arm_return_to(
    state: &mut GameState,
    exiled_id: ObjectId,
    controller: PlayerId,
    source_id: ObjectId,
    destination: Zone,
    timing: DelayedTriggerCondition,
) {
    let ability = ResolvedAbility::new(
        Effect::ChangeZone {
            // CR 603.7c: only return the card if it is still in exile.
            origin: Some(Zone::Exile),
            destination,
            // CR 603.7c + CR 115.1: the exiled card is pre-bound in `targets`
            // below — it is never a player-chosen target. `ParentTarget` is a
            // context ref (no target slot is surfaced when the delayed trigger
            // fires; `resolved_targets` reads the pre-bound `targets`),
            // mirroring the Flickerwisp-class delayed return pattern. A
            // `SpecificObject` filter would surface a mandatory target slot
            // whose exiled candidate is not a legal target, dropping the
            // trigger as target-unresolved.
            target: TargetFilter::ParentTarget,
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
        vec![TargetRef::Object(exiled_id)],
        // CR 603.7e: the source of the delayed trigger is the source of the
        // triggered ability whose replacement created it (Feather).
        source_id,
        // CR 603.7e: the delayed trigger's controller is the player who
        // controlled the triggered ability as it resolved. The `controller`
        // passed in is the resolving SPELL's controller (`entry.controller` at
        // the stack.rs call site), which equals the CR 603.7e ability
        // controller only for the current "whenever you cast" carrier class
        // (spell controller == trigger controller by the trigger condition).
        // A future opponent-cast carrier of this rider would need the stamping
        // ability's controller carried on the marker instead.
        controller,
    );

    state.delayed_triggers.push(DelayedTrigger {
        // CR 603.7b: when the one-shot return fires (Feather: "at the
        // beginning of the next end step").
        condition: timing,
        ability,
        controller,
        source_id,
        // CR 603.7b: one-shot — removed after it fires.
        one_shot: true,
    });
}
