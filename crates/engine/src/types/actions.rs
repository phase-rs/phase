use serde::{Deserialize, Serialize};

use super::ability::TargetRef;
use super::game_state::AutoPassRequest;
use super::identifiers::{CardId, ObjectId};
use super::match_config::DeckCardCount;
use crate::game::combat::AttackTarget;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, strum::IntoStaticStr)]
#[serde(tag = "type", content = "data")]
pub enum GameAction {
    PassPriority,
    PlayLand {
        object_id: ObjectId,
        card_id: CardId,
    },
    CastSpell {
        object_id: ObjectId,
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
        object_id: ObjectId,
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
    /// Choose normal cast (false) or Warp cast (true) from hand.
    ChooseWarpCost {
        use_warp: bool,
    },
    /// CR 702.49: Activate a Ninjutsu-family keyword from hand or command zone during combat.
    ActivateNinjutsu {
        ninjutsu_card_id: CardId,
        /// The creature to return — unblocked attacker (Ninjutsu/Sneak) or tapped creature (WebSlinging).
        creature_to_return: ObjectId,
    },
    /// CR 609.3: Accept or decline an optional effect ("You may X").
    DecideOptionalEffect {
        accept: bool,
    },
    /// CR 118.12: Pay or decline an "unless pays" cost (e.g., Mana Leak, No More Lies).
    PayUnlessCost {
        pay: bool,
    },
    /// CR 701.54a: Choose a creature to be the ring-bearer.
    ChooseRingBearer {
        target: ObjectId,
    },
    /// CR 702.51a: Tap creature/artifact for convoke or waterbend mana.
    /// CR 302.6: Summoning sickness does not apply (convoke doesn't use the tap ability mechanism).
    TapForConvoke {
        object_id: ObjectId,
        mana_type: super::mana::ManaType,
    },
    /// CR 702.180a/b: Harmonize — optionally tap a creature to reduce casting cost by its power.
    /// None = skip (decline the cost reduction).
    HarmonizeTap {
        creature_id: Option<ObjectId>,
    },
    /// CR 702.139a: Declare a companion during pre-game reveal (or decline).
    DeclareCompanion {
        /// Index into the eligible_companions list, or None to decline.
        card_index: Option<usize>,
    },
    /// CR 702.139a: Pay {3} to put companion into hand (special action, see rule 116.2g).
    CompanionToHand,
    /// CR 701.57a: Choose to cast discovered card or put it to hand.
    DiscoverChoice {
        /// true = cast without paying mana cost, false = put to hand
        cast: bool,
    },
    /// CR 401.4: Choose top or bottom of library.
    ChooseTopOrBottom {
        top: bool,
    },
    /// CR 704.5j: Choose which legendary permanent to keep.
    ChooseLegend {
        keep: ObjectId,
    },
    /// Set auto-pass mode for the acting player (CR 117.4).
    SetAutoPass {
        mode: AutoPassRequest,
    },
    /// Cancel any active auto-pass for the acting player.
    CancelAutoPass,
    /// CR 510.1c/d: Assign damage from an attacker to its blockers (and optionally
    /// the defending player/PW with trample, plus PW controller with trample-over-PW).
    AssignCombatDamage {
        assignments: Vec<(ObjectId, u32)>,
        trample_damage: u32,
        /// CR 702.19c: Damage to PW controller when trample-over-PW spills past loyalty.
        #[serde(default)]
        controller_damage: u32,
    },
    /// CR 601.2d: Distribute N among targets at casting time.
    DistributeAmong {
        distribution: Vec<(TargetRef, u32)>,
    },
    /// CR 115.7: Choose new target(s) for a spell or ability on the stack.
    RetargetSpell {
        new_targets: Vec<TargetRef>,
    },
    /// CR 701.48a: Learn — choose to rummage (discard a card, draw a card) or skip.
    LearnDecision {
        choice: LearnOption,
    },
}

/// CR 701.48a: Learn choice — rummage a specific card, or skip entirely.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum LearnOption {
    /// Discard the specified card, then draw one.
    Rummage { card_id: ObjectId },
    /// Decline to learn (skip).
    Skip,
}

impl GameAction {
    /// Returns the enum variant name as a static string (e.g., `"CastSpell"`, `"PassPriority"`).
    /// Useful for structured logging without the full `Debug` representation.
    pub fn variant_name(&self) -> &'static str {
        self.into()
    }

    /// CR 605.3a: Whether this action is a mana ability activation.
    ///
    /// Mana abilities are excluded from `legal_actions()` because they do not
    /// represent meaningful priority decisions — the frontend derives land
    /// tappability from game state directly. The engine's `apply()` validates
    /// mana actions independently, so they bypass the server's pre-validation.
    pub fn is_mana_ability(&self) -> bool {
        matches!(
            self,
            GameAction::TapLandForMana { .. } | GameAction::UntapLandForMana { .. }
        )
    }
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
            object_id: ObjectId(99),
            card_id: CardId(42),
        };
        let json = serde_json::to_value(&action).unwrap();
        assert_eq!(json["type"], "PlayLand");
        assert_eq!(json["data"]["card_id"], 42);
        assert_eq!(json["data"]["object_id"], 99);
    }

    #[test]
    fn cast_spell_serializes_with_targets() {
        let action = GameAction::CastSpell {
            object_id: ObjectId(5),
            card_id: CardId(1),
            targets: vec![ObjectId(10), ObjectId(20)],
        };
        let json = serde_json::to_value(&action).unwrap();
        assert_eq!(json["type"], "CastSpell");
        assert_eq!(json["data"]["object_id"], 5);
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
