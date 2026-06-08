//! Dash (CR 702.109) — alternative-cost runtime riders.
//!
//! CR 702.109a: "Dash [cost]" means "You may cast this card by paying [cost]
//! rather than its mana cost," and — if the dash cost was paid — the permanent
//! it becomes has **haste**, and you **return it to its owner's hand at the
//! beginning of the next end step**.
//!
//! The alternative cost itself is wired into the casting pipeline as
//! `CastingVariant::Dash` (offered like Evoke, substituted like Warp). This
//! module owns the *resolution riders*: when a dash-cast spell resolves into a
//! permanent, [`install_dash_riders`] grants haste and schedules the end-step
//! return to hand.
//!
//! The riders are granted directly at resolution (Suspend/Warp-style) rather
//! than synthesized as tag-gated abilities: granting them only to a permanent
//! whose dash cost was paid makes their *presence* the gate, with no dependence
//! on `cast_variant_paid` surviving zone changes.

use crate::types::ability::{
    ContinuousModification, DelayedTriggerCondition, Duration, Effect, ResolvedAbility,
    TargetFilter,
};
use crate::types::game_state::{DelayedTrigger, GameState};
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// CR 702.109a: Install Dash's resolution riders on the permanent a dash-cast
/// spell just became. Called from the stack resolution path when
/// `casting_variant == CastingVariant::Dash`.
///
/// 1. Haste — a continuous keyword grant scoped to this permanent.
/// 2. "Return it to its owner's hand at the beginning of the next end step" — a
///    one-shot delayed trigger (mirroring Warp's end-step delayed trigger).
pub(crate) fn install_dash_riders(
    state: &mut GameState,
    object_id: ObjectId,
    controller: PlayerId,
) {
    // CR 702.109a: the permanent has haste. A transient continuous keyword grant
    // scoped to this object (Layer 6), present while it is on the battlefield —
    // Dash returns it to hand at the next end step.
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

    // CR 702.109a: "Return the permanent this spell becomes to its owner's hand
    // at the beginning of the next end step." A one-shot delayed trigger; the
    // `origin: Battlefield` zone-change silently no-ops if the permanent has
    // already left the battlefield (CR 400.7, `process_one_zone_move`).
    let return_to_hand = ResolvedAbility::new(
        return_to_owner_hand_effect(),
        Vec::new(),
        object_id,
        controller,
    );
    state.delayed_triggers.push(DelayedTrigger {
        condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
        ability: return_to_hand,
        controller,
        source_id: object_id,
        one_shot: true,
    });
}

/// CR 702.109a + CR 400.3: "Return it to its owner's hand." A battlefield → hand
/// zone change targeting this permanent (`SelfRef`); a card always moves to its
/// owner's hand, so `enters_under` / `owner_library` stay at their defaults.
fn return_to_owner_hand_effect() -> Effect {
    Effect::ChangeZone {
        origin: Some(Zone::Battlefield),
        destination: Zone::Hand,
        target: TargetFilter::SelfRef,
        owner_library: false,
        enter_transformed: false,
        enters_under: None,
        enter_tapped: false,
        enters_attacking: false,
        up_to: false,
        enter_with_counters: vec![],
        face_down_profile: None,
    }
}
