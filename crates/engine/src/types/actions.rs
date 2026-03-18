use serde::{Deserialize, Serialize};

use super::ability::TargetRef;
use super::game_state::AutoPassRequest;
use super::identifiers::{CardId, ObjectId};
use super::match_config::DeckCardCount;
use crate::game::combat::AttackTarget;

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
        attacks: Vec<(ObjectId, AttackTarget)>,
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
    /// CR 605.3a: Undo a manual mana ability activation — untap source, remove produced mana.
    /// Only valid for lands in `lands_tapped_for_mana` whose mana hasn't been spent.
    UntapLandForMana {
        object_id: ObjectId,
    },
    SelectCards {
        cards: Vec<ObjectId>,
    },
    SelectTargets {
        targets: Vec<TargetRef>,
    },
    ChooseTarget {
        target: Option<TargetRef>,
    },
    ChooseReplacement {
        index: usize,
    },
    CancelCast,
    Equip {
        equipment_id: ObjectId,
        target_id: ObjectId,
    },
    Transform {
        object_id: ObjectId,
    },
    PlayFaceDown {
        card_id: CardId,
    },
    TurnFaceUp {
        object_id: ObjectId,
    },
    SubmitSideboard {
        main: Vec<DeckCardCount>,
        sideboard: Vec<DeckCardCount>,
    },
    ChoosePlayDraw {
        play_first: bool,
    },
    ChooseOption {
        choice: String,
    },
    SelectModes {
        indices: Vec<usize>,
    },
    DecideOptionalCost {
        pay: bool,
    },
    /// CR 715.3a: Choose creature face (true) or Adventure half (false).
    ChooseAdventureFace {
        creature: bool,
    },
    /// CR 702.49a: Activate Ninjutsu from hand during declare blockers step.
    ActivateNinjutsu {
        ninjutsu_card_id: CardId,
        attacker_to_return: ObjectId,
    },
    /// CR 609.3: Accept or decline an optional effect ("You may X").
    DecideOptionalEffect {
        accept: bool,
    },
    /// CR 118.12: Pay or decline an "unless pays" cost (e.g., Mana Leak, No More Lies).
    PayUnlessCost {
        pay: bool,
    },
    /// Set auto-pass mode for the acting player (CR 117.4).
    SetAutoPass {
        mode: AutoPassRequest,
    },
    /// Cancel any active auto-pass for the acting player.
    CancelAutoPass,
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

    #[test]
    fn declare_attackers_with_attack_targets_roundtrips() {
        use crate::game::combat::AttackTarget;
        use crate::types::player::PlayerId;

        let action = GameAction::DeclareAttackers {
            attacks: vec![
                (ObjectId(1), AttackTarget::Player(PlayerId(1))),
                (ObjectId(2), AttackTarget::Planeswalker(ObjectId(99))),
            ],
        };
        let serialized = serde_json::to_string(&action).unwrap();
        let deserialized: GameAction = serde_json::from_str(&serialized).unwrap();
        assert_eq!(action, deserialized);
    }

    #[test]
    fn attack_target_serializes_as_tagged_union() {
        use crate::game::combat::AttackTarget;
        use crate::types::player::PlayerId;

        let target = AttackTarget::Player(PlayerId(1));
        let json = serde_json::to_value(&target).unwrap();
        assert_eq!(json["type"], "Player");
        assert_eq!(json["data"], 1);

        let target = AttackTarget::Planeswalker(ObjectId(42));
        let json = serde_json::to_value(&target).unwrap();
        assert_eq!(json["type"], "Planeswalker");
        assert_eq!(json["data"], 42);
    }

    #[test]
    fn declare_attackers_empty_attacks_roundtrips() {
        let action = GameAction::DeclareAttackers {
            attacks: Vec::new(),
        };
        let serialized = serde_json::to_string(&action).unwrap();
        let deserialized: GameAction = serde_json::from_str(&serialized).unwrap();
        assert_eq!(action, deserialized);
    }
}
