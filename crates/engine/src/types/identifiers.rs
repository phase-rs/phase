pub use phase_ast::types::identifiers::*;

use serde::{Deserialize, Serialize};

use super::game_state::{LKISnapshot, ZoneChangeRecord};
use super::zones::Zone;
use crate::game::game_object::GameObject;

/// Monotonic identity for one logical simultaneous zone-change action.
///
/// This remains distinct from an [`ObjectId`]: a logical group can contain
/// several object incarnations, and a nested batch must never inherit its
/// parent's trigger-observation authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LogicalZoneChangeGroupId(pub u64);

/// Monotonic identity for one operation-owned discard result frame. This is
/// distinct from an object id: one discard instruction may be replaced or
/// paused, while the frame remains the sole provenance authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DiscardFrameId(pub u64);

/// Private durable origin for a delayed-trigger installation.
///
/// This belongs to engine scheduling state, never to a public `GameEvent`.
/// [`DelayedInstallIdentity::LegacyDelayed`] represents an older persisted
/// record that cannot be matched unambiguously to a durable install command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct DelayedTriggerOrigin {
    pub(crate) token: DelayedTriggerToken,
    pub(crate) instance: DelayedTriggerInstanceId,
    pub(crate) source_id: ObjectId,
    /// Present only when the trigger was installed by a paid offer's immediate
    /// direct synchronous tail; legacy and ordinary triggers stay ownerless.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) offer_id: Option<ResolutionCastOfferId>,
}

/// Durable identity carried by a CR 603.7 delayed-trigger installation.
///
/// A legacy installation remains a delayed trigger for rules scheduling, but
/// lacks the command-backed root required for prospective receipt tracking.
/// A fresh installation always carries the full root and can be transported to
/// the corresponding [`TriggerFiring`] without re-deriving it from a source.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum DelayedInstallIdentity {
    #[default]
    LegacyDelayed,
    ReceiptEligible(DelayedTriggerOrigin),
}

impl DelayedInstallIdentity {
    pub(crate) fn origin(self) -> Option<DelayedTriggerOrigin> {
        match self {
            Self::LegacyDelayed => None,
            Self::ReceiptEligible(origin) => Some(origin),
        }
    }

    pub(crate) fn firing(self) -> TriggerFiring {
        match self {
            Self::LegacyDelayed => TriggerFiring::LegacyDelayed,
            Self::ReceiptEligible(origin) => TriggerFiring::ReceiptEligible(origin),
        }
    }
}

/// Private classification of a triggered-ability firing.
///
/// CR 603.7: a delayed ability remains distinct from an ordinary triggered
/// ability even when an older persisted delayed record has no reconstructible
/// installation receipt. `UnknownLegacy` is intentionally fail-closed and is
/// never inferred from an omitted historical discriminator.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum TriggerFiring {
    Ordinary,
    LegacyDelayed,
    ReceiptEligible(DelayedTriggerOrigin),
    #[default]
    UnknownLegacy,
}

impl TriggerFiring {
    pub(crate) fn is_delayed(self) -> bool {
        matches!(self, Self::LegacyDelayed | Self::ReceiptEligible(_))
    }
}

/// CR 608.2h: an identity reference paired with the public zone the object was
/// expected to be in. Phase 2B strict resolution consumes `expected_zone` to
/// decide current-info vs. last-known-information.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct ObjectIdentityBinding {
    pub reference: ObjectIncarnationRef,
    pub expected_zone: Zone,
}

impl ObjectIdentityBinding {
    pub fn new(reference: ObjectIncarnationRef, expected_zone: Zone) -> Self {
        Self {
            reference,
            expected_zone,
        }
    }
}

/// CR 400.7 + CR 603.4 + CR 603.6a: the identity of the object whose zone change
/// fired the trigger whose intervening-`if` is being evaluated — the referent of
/// "another" in a trigger-anaphoric filter.
///
/// Deliberately NOT a bare [`ObjectId`]. CR 400.7: "an object that moves from one
/// zone to another becomes a new object with no memory of, or relation to, its
/// previous existence", but the engine REUSES the storage id across that move and
/// records the discontinuity as an `incarnation` bump
/// (`GameObject::bump_incarnation`). An exclusion keyed on the id alone therefore
/// also excludes a *different* object that happens to occupy the same slot: blink
/// the original entrant and re-play it before the CR 603.4 resolution recheck, and
/// the new incarnation — which is "another creature you control" under CR 400.7 —
/// is silently dropped from the reference population.
///
/// Deliberately NOT [`ObjectIncarnationRef`] either: that type asserts an exact,
/// always-known incarnation, and the zone-change events this is bound from prove
/// one only for battlefield entries (`ZoneChangeRecord::entered_incarnation`).
/// `incarnation: None` is the honest spelling of "this event does not prove an
/// incarnation", and it falls back to storage identity — the same
/// `is_none_or` fallback the sibling `entered_incarnation` consumers in
/// `game/filter.rs` and `game/triggers.rs` already use for legacy and synthesized
/// records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TriggeringObjectRef {
    pub object_id: ObjectId,
    /// CR 400.7: the incarnation the triggering event proves for `object_id`, or
    /// `None` when the event record stamps none.
    pub incarnation: Option<u64>,
}

impl TriggeringObjectRef {
    /// Bind from a zone-change event's subject id plus the incarnation its record
    /// proves. `ZoneChangeRecord::entered_incarnation` is captured AFTER the
    /// battlefield-entry bump, so it names the entrant that actually fired the
    /// trigger rather than its pre-move self; it is `None` for every
    /// non-battlefield destination, which degrades to storage identity.
    pub fn from_zone_change(object_id: ObjectId, entered_incarnation: Option<u64>) -> Self {
        Self {
            object_id,
            incarnation: entered_incarnation,
        }
    }

    /// CR 400.7: true when `object` IS this triggering object — same storage id
    /// AND, when the event proved one, the same incarnation. A re-entered object
    /// at the same id answers `false`, which is what makes "another" honest.
    pub fn is_object(self, object: &GameObject) -> bool {
        object.id == self.object_id
            && self
                .incarnation
                .is_none_or(|incarnation| object.incarnation == incarnation)
    }

    /// CR 400.7: record-side counterpart of [`Self::is_object`] for the last-known-information
    /// path, which evaluates a `ZoneChangeRecord` projection rather than a live
    /// object. The record's own `entered_incarnation` is the same authority this
    /// reference is bound from, so the two agree exactly when the record describes
    /// THIS triggering event; an unproven incarnation on either side degrades to
    /// storage identity.
    pub fn describes_record(self, record: &ZoneChangeRecord) -> bool {
        record.object_id == self.object_id
            && match (self.incarnation, record.entered_incarnation) {
                (Some(bound), Some(recorded)) => bound == recorded,
                _ => true,
            }
    }
}

/// CR 608.2h / CR 113.7a: a binding plus the last-known-information snapshot used
/// when the object is no longer in its expected public zone. Defined in Phase 1;
/// consumed by Phase 2B strict resolution.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct ObjectProvenance {
    pub binding: ObjectIdentityBinding,
    pub lki: Option<LKISnapshot>,
}

impl ObjectProvenance {
    pub fn new(binding: ObjectIdentityBinding, lki: Option<LKISnapshot>) -> Self {
        Self { binding, lki }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A battlefield-entry record for `object_id` stamped with `entered_incarnation`,
    /// the shape `matches_zone_change_event_object_filter` binds a triggering object
    /// from.
    fn entry_record(object_id: ObjectId, entered_incarnation: Option<u64>) -> ZoneChangeRecord {
        ZoneChangeRecord {
            entered_incarnation,
            ..ZoneChangeRecord::test_minimal(object_id, Some(Zone::Hand), Zone::Battlefield)
        }
    }

    /// CR 400.7: the whole reason `TriggeringObjectRef` is not a bare `ObjectId`.
    /// A record that proves an incarnation distinguishes the entrant from a LATER
    /// object at the same storage id, so the "another" exclusion admits the
    /// re-entered object. An id-keyed comparison cannot tell these two apart — it
    /// answers `true` for both rows below.
    #[test]
    fn a_proven_incarnation_separates_the_entrant_from_a_reentry_at_the_same_id() {
        let entrant = TriggeringObjectRef::from_zone_change(ObjectId(7), Some(3));

        assert!(
            entrant.describes_record(&entry_record(ObjectId(7), Some(3))),
            "the entry record this reference was bound from IS the triggering object"
        );
        assert!(
            !entrant.describes_record(&entry_record(ObjectId(7), Some(4))),
            "CR 400.7: a later entry at the same id is a different object"
        );
        // A different storage id is never the triggering object, proven or not.
        assert!(!entrant.describes_record(&entry_record(ObjectId(8), Some(3))));
    }

    /// An event that proves no incarnation must keep the pre-existing, purely
    /// storage-keyed behavior rather than silently ceasing to exclude anything.
    #[test]
    fn an_unproven_incarnation_degrades_to_storage_identity() {
        let entrant = TriggeringObjectRef::from_zone_change(ObjectId(7), None);
        assert_eq!(entrant.incarnation, None);

        assert!(
            entrant.describes_record(&entry_record(ObjectId(7), Some(9))),
            "with nothing proven, the id alone decides — the historical contract"
        );
        assert!(!entrant.describes_record(&entry_record(ObjectId(8), Some(9))));
    }
}
