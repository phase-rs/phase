use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CardId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ObjectId(pub u64);

/// CR 603.2 + CR 603.3b + CR 117.3b: parse-time placeholder for "the specific
/// spell object that will cause this trigger to fire", embedded inside a
/// `TargetFilter::SpecificObject` leaf of a floating (`TargetFilter::None`)
/// replacement's `valid_card` tree by `parse_whenever_you_cast_enters_with_trigger`.
/// `Effect::AddTargetReplacement` resolves it from the resolving carrier's
/// captured `ObjectIncarnationRef`, requiring that exact spell to remain live
/// on the stack. The same pin supplies the concrete selector id and the
/// installed `ReplacementDefinition::triggering_spell`; missing, departed, or
/// mismatched authority installs nothing. CR 400.7: later casts sharing the
/// storage id cannot inherit this rider. Never a real object's id (the
/// allocator starts well below `u64::MAX`), so no dedicated filter variant is
/// needed.
pub const TRIGGERING_SPELL_PLACEHOLDER: ObjectId = ObjectId(u64::MAX);

/// CR 500.8 + CR 500.9 + CR 500.10: monotonic identity of one scheduled extra
/// phase or step, minted when an effect schedules it. Two scheduled units can
/// share an anchor and a phase; this is what tells them apart. `0` is never
/// minted: an entry written without an id (seeded, or saved before ids
/// existed) reads the default and names no minted unit.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct ExtraPhaseId(pub u64);

/// Unique identifier for a set of objects tracked across delayed trigger boundaries.
/// CR 603.7: Delayed triggers reference the specific objects from the originating effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TrackedSetId(pub u64);

/// CR 702.50a: Names one rest-of-game Epic effect by its position in
/// `GameState::epic_effects`, which only grows, so a position is a stable identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EpicEffectId(pub u64);

/// CR 615.5 + CR 614.6: names one runtime continuation of a resolution-installed
/// replacement or prevention shield by its position in
/// `GameState::replacement_continuations`, which only grows, so a position is a
/// stable identity for the rest of the game.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReplacementContinuationId(pub u64);

/// CR 603.7: Monotonic identity of one installed delayed triggered ability.
///
/// This is deliberately distinct from its source object: multiple delayed
/// triggers may be created by the same source, and a source can change zones
/// before its trigger fires.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct DelayedTriggerToken(pub u64);

/// CR 603.7: Monotonic identity for one durable delayed-trigger installation.
///
/// This remains separate from [`DelayedTriggerToken`]: the token identifies the
/// installation receipt while this value identifies the specific installed
/// occurrence. Keeping both prevents a legacy record from being rebound to a
/// later trigger merely because its source object is the same.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct DelayedTriggerInstanceId(pub u64);

/// Producer-issued identity for one paid cast offer made while an ability is
/// resolving. It is distinct from the card and delayed-trigger identifiers so
/// two otherwise equivalent offers cannot share cleanup authority.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct ResolutionCastOfferId(pub u64);

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
