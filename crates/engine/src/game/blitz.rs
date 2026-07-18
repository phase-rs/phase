//! Blitz (CR 702.152) — alternative-cost runtime riders.
//!
//! CR 702.152a: "Blitz [cost]" means "You may cast this card by paying [cost]
//! rather than its mana cost," and — if the blitz cost was paid — the permanent
//! it becomes gains **haste** and "When this permanent is put into a graveyard
//! from the battlefield, draw a card," and you **sacrifice it at the beginning
//! of the next end step."
//!
//! The alternative cost itself is wired into the casting pipeline as
//! `CastingVariant::Blitz` (offered like Evoke, substituted like Warp). This
//! module owns the *resolution riders*: when a blitz-cast spell resolves into a
//! permanent, [`install_blitz_riders`] grants the three abilities directly to
//! that permanent.
//!
//! Why grant at resolution rather than synthesize tag-gated abilities onto the
//! face (the Evoke/Impending pattern): the dies-draw is a *leaves-the-battlefield*
//! trigger, and `cast_variant_paid` is cleared by the new-object reset
//! (`game_object.rs`) as the permanent moves to the graveyard — so a trigger
//! gated on that tag would never fire on death. Granting the rider directly (its
//! *presence* on the permanent is the gate, like Suspend's runtime haste and
//! granted-Evoke's runtime trigger) sidesteps that entirely: the trigger is part
//! of the dying object's last-known information and fires normally (CR 603.10a).

use crate::game::game_object::GameObject;
use crate::types::ability::{
    AbilityDefinition, AbilityKind, ContinuousModification, DelayedTriggerCondition, Duration,
    Effect, QuantityExpr, ResolvedAbility, TargetFilter, TriggerDefinition,
};
use crate::types::game_state::{DelayedTrigger, GameState};
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;

/// CR 702.152a: Install Blitz's resolution riders on the permanent a blitz-cast
/// spell just became. Called from the stack resolution path when
/// `casting_variant == CastingVariant::Blitz`.
///
/// 1. Haste — a continuous keyword grant scoped to this permanent (CR 702.152a).
/// 2. "When this dies, draw a card" — granted into the permanent's durable
///    `base_trigger_definitions` so it survives the layer rebuild and fires via
///    last-known-information when the permanent leaves the battlefield.
/// 3. "Sacrifice at the beginning of the next end step" — a one-shot delayed
///    trigger (CR 702.152a), mirroring Warp's end-step delayed trigger.
pub(crate) fn install_blitz_riders(
    state: &mut GameState,
    object_id: ObjectId,
    controller: PlayerId,
) {
    // CR 702.152a: the permanent gains haste. A transient continuous effect
    // scoped to this object (Layer 6 keyword grant) — present for as long as the
    // permanent is on the battlefield, which Blitz ends at the next end step.
    state.add_transient_continuous_effect(
        object_id,
        controller,
        Duration::Permanent,
        TargetFilter::SpecificObject { id: object_id },
        vec![ContinuousModification::AddKeyword {
            keyword: Keyword::Haste,
        }],
        None,
    );

    // CR 702.152a: "When this permanent is put into a graveyard from the
    // battlefield, draw a card." Granted onto the live permanent; pushed to the
    // durable base so the layer rebuild does not wipe it before it is collected.
    if let Some(obj) = state.objects.get_mut(&object_id) {
        grant_dies_draw_trigger(obj);
    }

    // CR 702.152a: "Sacrifice the permanent at the beginning of the next end step."
    let sacrifice =
        ResolvedAbility::new(sacrifice_self_effect(), Vec::new(), object_id, controller);
    state.delayed_triggers.push(DelayedTrigger {
        condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
        ability: sacrifice,
        controller,
        source_id: object_id,
        one_shot: true,
    });
}

/// CR 702.152a: Grant the dies-draw trigger to a live permanent, idempotently.
/// Mirrors `ensure_evoke_etb_sac_trigger`: push to the durable
/// `base_trigger_definitions` (the source the layer system rebuilds from) and
/// refresh the live copy so the trigger is collectable this same resolution.
fn grant_dies_draw_trigger(obj: &mut GameObject) {
    if obj.base_trigger_definitions.iter().any(is_blitz_dies_draw) {
        if !obj
            .trigger_definitions
            .iter_all()
            .any(|entry| is_blitz_dies_draw(entry.definition()))
        {
            obj.trigger_definitions.push(build_dies_draw_trigger());
        }
        return;
    }
    let trigger = build_dies_draw_trigger();
    std::sync::Arc::make_mut(&mut obj.base_trigger_definitions).push(trigger.clone());
    obj.materialize_base_trigger_definitions();
}

/// Exact description of the Blitz-granted dies-draw trigger. Used both as the
/// trigger's runtime description and as the unique idempotency marker (see
/// [`is_blitz_dies_draw`]), so the marker and the granted trigger cannot drift.
const BLITZ_DIES_DRAW_DESC: &str =
    "Blitz (CR 702.152a): When this permanent is put into a graveyard from the battlefield, draw a card.";

/// Idempotency guard: matches ONLY the Blitz-granted dies-draw trigger, by its
/// unique description marker. A purely structural match (`ChangesZone` +
/// `Battlefield→Graveyard` + `SelfRef` + `Draw`) is exactly the shape of a
/// PRINTED "when this dies, draw a card" trigger, so it would false-positive and
/// suppress Blitz's distinct, additive grant (CR 702.152a grants a second,
/// separate dies-draw). Mirrors how Evoke discriminates on its unique
/// `CastVariantPaid` tag rather than on shape.
fn is_blitz_dies_draw(trigger: &TriggerDefinition) -> bool {
    trigger.description.as_deref() == Some(BLITZ_DIES_DRAW_DESC)
}

/// CR 702.152a: "When this permanent is put into a graveyard from the
/// battlefield, draw a card." No intervening-if condition — the trigger is only
/// ever granted to a permanent whose blitz cost was paid, so its presence is the
/// gate (CR 603.10a: it fires from the permanent's last-known information).
fn build_dies_draw_trigger() -> TriggerDefinition {
    let draw = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    );
    TriggerDefinition::new(TriggerMode::ChangesZone)
        .origin(Zone::Battlefield)
        .destination(Zone::Graveyard)
        .valid_card(TargetFilter::SelfRef)
        .execute(draw)
        .description(BLITZ_DIES_DRAW_DESC.to_string())
}

/// CR 702.152a + CR 701.21a: "Sacrifice this permanent." Used by the delayed
/// end-step trigger.
fn sacrifice_self_effect() -> Effect {
    Effect::Sacrifice {
        target: TargetFilter::SelfRef,
        count: QuantityExpr::Fixed { value: 1 },
        min_count: 0,
    }
}
