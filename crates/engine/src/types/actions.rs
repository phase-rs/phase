use serde::{Deserialize, Serialize};

use super::ability::TargetRef;
use super::identifiers::{CardId, ObjectId};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum GameAction {
    PassPriority,
    PlayLand {
        card_id: CardId,
    },
    CastSpell {
        card_id: CardId,
        targets: Vec<ObjectId>,
    },
    ActivateAbility {
        source_id: ObjectId,
        ability_index: usize,
    },
    DeclareAttackers {
        attacker_ids: Vec<ObjectId>,
    },
    DeclareBlockers {
        assignments: Vec<(ObjectId, ObjectId)>,
    },
    MulliganDecision {
        keep: bool,
    },
    TapLandForMana {
        object_id: ObjectId,
    },
    SelectCards {
        cards: Vec<ObjectId>,
    },
    SelectTargets {
        targets: Vec<TargetRef>,
    },
    ChooseReplacement {
        index: usize,
    },
    CancelCast,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pass_priority_serializes_as_tagged_union() {
        let action = GameAction::PassPriority;
        let json = serde_json::to_value(&action).unwrap();
        assert_eq!(json["type"], "PassPriority");
        assert!(json.get("data").is_none());
    }

    #[test]
    fn play_land_serializes_with_data() {
        let action = GameAction::PlayLand {
            card_id: CardId(42),
        };
        let json = serde_json::to_value(&action).unwrap();
        assert_eq!(json["type"], "PlayLand");
        assert_eq!(json["data"]["card_id"], 42);
    }

    #[test]
    fn cast_spell_serializes_with_targets() {
        let action = GameAction::CastSpell {
            card_id: CardId(1),
            targets: vec![ObjectId(10), ObjectId(20)],
        };
        let json = serde_json::to_value(&action).unwrap();
        assert_eq!(json["type"], "CastSpell");
        assert_eq!(json["data"]["targets"], serde_json::json!([10, 20]));
    }

    #[test]
    fn mulligan_decision_roundtrips() {
        let action = GameAction::MulliganDecision { keep: true };
        let serialized = serde_json::to_string(&action).unwrap();
        let deserialized: GameAction = serde_json::from_str(&serialized).unwrap();
        assert_eq!(action, deserialized);
    }

    #[test]
    fn deserialize_from_tagged_json() {
        let json = r#"{"type":"PassPriority"}"#;
        let action: GameAction = serde_json::from_str(json).unwrap();
        assert_eq!(action, GameAction::PassPriority);
    }
}
