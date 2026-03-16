use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct CardId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct ObjectId(pub u64);

/// Unique identifier for a set of objects tracked across delayed trigger boundaries.
/// CR 603.7: Delayed triggers reference the specific objects from the originating effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct TrackedSetId(pub u64);

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
}
