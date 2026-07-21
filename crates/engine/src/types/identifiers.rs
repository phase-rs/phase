use serde::{Deserialize, Serialize};

use super::game_state::LKISnapshot;
use super::zones::Zone;
use crate::game::game_object::GameObject;
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CardId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ObjectId(pub u64);

/// Monotonic identity for one logical simultaneous zone-change action.
///
/// This remains distinct from an [`ObjectId`]: a logical group can contain
/// several object incarnations, and a nested batch must never inherit its
/// parent's trigger-observation authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LogicalZoneChangeGroupId(pub u64);

/// Unique identifier for a set of objects tracked across delayed trigger boundaries.
/// CR 603.7: Delayed triggers reference the specific objects from the originating effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TrackedSetId(pub u64);

/// Sentinel `incarnation` bound to a pre-migration `crew_activated_this_turn`
/// record that serialized as a bare `ObjectId` (no incarnation was stored).
///
/// CR 400.7: a pre-migration record cannot prove which incarnation crewed, so it
/// is bound to a value that can never collide with a real current incarnation.
pub const LEGACY_INCARNATION: u64 = u64::MAX;

/// CR 400.7: an object that changes zones becomes a new object. This pair is the
/// exact cross-incarnation identity of one object: the stable storage `ObjectId`
/// plus the monotonic `incarnation` epoch (see `GameObject::incarnation`).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[serde(from = "ObjectIncarnationRefCompat")]
pub struct ObjectIncarnationRef {
    pub object_id: ObjectId,
    pub incarnation: u64,
}

impl ObjectIncarnationRef {
    /// Construct a reference from an explicit id + incarnation.
    pub fn of(object_id: ObjectId, incarnation: u64) -> Self {
        Self {
            object_id,
            incarnation,
        }
    }

    /// Convenience: capture the current incarnation of a live object.
    pub fn from_object(obj: &GameObject) -> Self {
        Self {
            object_id: obj.id,
            incarnation: obj.incarnation,
        }
    }
}

/// Private serde shim mirroring `PhaseStopCompat` (`types/phase.rs`): new writes
/// emit the full `{ object_id, incarnation }` pair; legacy saves stored a bare
/// `ObjectId` number. The two arms are shape-disjoint (map vs. number), so
/// serde's untagged matching selects by shape regardless of declaration order.
#[derive(Deserialize)]
#[serde(untagged)]
enum ObjectIncarnationRefCompat {
    Full {
        object_id: ObjectId,
        incarnation: u64,
    },
    Legacy(ObjectId),
}

impl From<ObjectIncarnationRefCompat> for ObjectIncarnationRef {
    fn from(c: ObjectIncarnationRefCompat) -> Self {
        match c {
            ObjectIncarnationRefCompat::Full {
                object_id,
                incarnation,
            } => Self {
                object_id,
                incarnation,
            },
            // CR 400.7: a pre-migration record cannot prove its incarnation; bind
            // the sentinel so it never matches a current object's reference.
            ObjectIncarnationRefCompat::Legacy(object_id) => Self {
                object_id,
                incarnation: LEGACY_INCARNATION,
            },
        }
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

    #[test]
    fn card_id_and_object_id_are_distinct_types() {
        let card_id = CardId(1);
        let object_id = ObjectId(1);
        // They have the same inner value but are different types.
        // This test verifies they exist as separate newtypes.
        assert_eq!(card_id.0, object_id.0);
        // The following would not compile (different types):
        // let _: CardId = object_id;
    }

    #[test]
    fn card_id_serializes_as_number() {
        let id = CardId(42);
        let json = serde_json::to_value(id).unwrap();
        assert_eq!(json, 42);
    }

    #[test]
    fn object_id_serializes_as_number() {
        let id = ObjectId(99);
        let json = serde_json::to_value(id).unwrap();
        assert_eq!(json, 99);
    }

    #[test]
    fn identifiers_are_hashable() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(CardId(1));
        set.insert(CardId(2));
        set.insert(CardId(1));
        assert_eq!(set.len(), 2);
    }

    // T-serde: a pre-migration `crew_activated_this_turn` entry serialized as a
    // bare `ObjectId` number; it must still load, bound to the sentinel.
    #[test]
    fn object_incarnation_ref_legacy_bare_id_deserializes_to_sentinel() {
        let r: ObjectIncarnationRef = serde_json::from_str("7").unwrap();
        assert_eq!(r, ObjectIncarnationRef::of(ObjectId(7), LEGACY_INCARNATION));
    }

    // T-serde: a new write emits the full `{ object_id, incarnation }` pair and
    // round-trips. Fails if the shape-disjoint compat arms are mis-declared such
    // that the struct form is swallowed by the scalar arm.
    #[test]
    fn object_incarnation_ref_full_pair_roundtrips() {
        let r = ObjectIncarnationRef::of(ObjectId(3), 5);
        let json = serde_json::to_string(&r).unwrap();
        assert!(
            json.contains("incarnation"),
            "new writes emit the full pair"
        );
        let back: ObjectIncarnationRef = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    // T-serde: a whole legacy `HashSet<ObjectId>` (array of bare numbers) loads as
    // a `HashSet<ObjectIncarnationRef>` with every entry bound to the sentinel.
    #[test]
    fn legacy_crew_set_id_array_loads() {
        use std::collections::HashSet;
        let set: HashSet<ObjectIncarnationRef> = serde_json::from_str("[1,2]").unwrap();
        assert!(set.contains(&ObjectIncarnationRef::of(ObjectId(1), LEGACY_INCARNATION)));
        assert!(set.contains(&ObjectIncarnationRef::of(ObjectId(2), LEGACY_INCARNATION)));
    }
}
