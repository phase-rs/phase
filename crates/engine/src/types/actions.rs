use serde::{Deserialize, Serialize};

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
}
