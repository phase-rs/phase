use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::ability::TargetRef;
use super::identifiers::ObjectId;
use super::player::PlayerId;
use super::zones::Zone;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ReplacementId {
    pub source: ObjectId,
    pub index: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProposedEvent {
    ZoneChange {
        object_id: ObjectId,
        from: Zone,
        to: Zone,
        cause: Option<ObjectId>,
        applied: HashSet<ReplacementId>,
    },
    Damage {
        source_id: ObjectId,
        target: TargetRef,
        amount: u32,
        is_combat: bool,
        applied: HashSet<ReplacementId>,
    },
    Draw {
        player_id: PlayerId,
        count: u32,
        applied: HashSet<ReplacementId>,
    },
    LifeGain {
        player_id: PlayerId,
        amount: u32,
        applied: HashSet<ReplacementId>,
    },
    LifeLoss {
        player_id: PlayerId,
        amount: u32,
        applied: HashSet<ReplacementId>,
    },
    AddCounter {
        object_id: ObjectId,
        counter_type: String,
        count: u32,
        applied: HashSet<ReplacementId>,
    },
    RemoveCounter {
        object_id: ObjectId,
        counter_type: String,
        count: u32,
        applied: HashSet<ReplacementId>,
    },
    CreateToken {
        owner: PlayerId,
        name: String,
        applied: HashSet<ReplacementId>,
    },
    Discard {
        player_id: PlayerId,
        object_id: ObjectId,
        applied: HashSet<ReplacementId>,
    },
    Tap {
        object_id: ObjectId,
        applied: HashSet<ReplacementId>,
    },
    Untap {
        object_id: ObjectId,
        applied: HashSet<ReplacementId>,
    },
    Destroy {
        object_id: ObjectId,
        source: Option<ObjectId>,
        applied: HashSet<ReplacementId>,
    },
    Sacrifice {
        object_id: ObjectId,
        player_id: PlayerId,
        applied: HashSet<ReplacementId>,
    },
}

impl ProposedEvent {
    pub fn applied_set(&self) -> &HashSet<ReplacementId> {
        match self {
            ProposedEvent::ZoneChange { applied, .. }
            | ProposedEvent::Damage { applied, .. }
            | ProposedEvent::Draw { applied, .. }
            | ProposedEvent::LifeGain { applied, .. }
            | ProposedEvent::LifeLoss { applied, .. }
            | ProposedEvent::AddCounter { applied, .. }
            | ProposedEvent::RemoveCounter { applied, .. }
            | ProposedEvent::CreateToken { applied, .. }
            | ProposedEvent::Discard { applied, .. }
            | ProposedEvent::Tap { applied, .. }
            | ProposedEvent::Untap { applied, .. }
            | ProposedEvent::Destroy { applied, .. }
            | ProposedEvent::Sacrifice { applied, .. } => applied,
        }
    }

    pub fn applied_set_mut(&mut self) -> &mut HashSet<ReplacementId> {
        match self {
            ProposedEvent::ZoneChange { applied, .. }
            | ProposedEvent::Damage { applied, .. }
            | ProposedEvent::Draw { applied, .. }
            | ProposedEvent::LifeGain { applied, .. }
            | ProposedEvent::LifeLoss { applied, .. }
            | ProposedEvent::AddCounter { applied, .. }
            | ProposedEvent::RemoveCounter { applied, .. }
            | ProposedEvent::CreateToken { applied, .. }
            | ProposedEvent::Discard { applied, .. }
            | ProposedEvent::Tap { applied, .. }
            | ProposedEvent::Untap { applied, .. }
            | ProposedEvent::Destroy { applied, .. }
            | ProposedEvent::Sacrifice { applied, .. } => applied,
        }
    }

    pub fn already_applied(&self, id: &ReplacementId) -> bool {
        self.applied_set().contains(id)
    }

    pub fn mark_applied(&mut self, id: ReplacementId) {
        self.applied_set_mut().insert(id);
    }

    pub fn affected_player(&self, state: &crate::types::game_state::GameState) -> PlayerId {
        match self {
            ProposedEvent::ZoneChange { object_id, .. }
            | ProposedEvent::Tap { object_id, .. }
            | ProposedEvent::Untap { object_id, .. }
            | ProposedEvent::Destroy { object_id, .. }
            | ProposedEvent::AddCounter { object_id, .. }
            | ProposedEvent::RemoveCounter { object_id, .. } => {
                state
                    .objects
                    .get(object_id)
                    .map(|o| o.controller)
                    .unwrap_or(PlayerId(0))
            }
            ProposedEvent::Damage { target, .. } => match target {
                TargetRef::Player(pid) => *pid,
                TargetRef::Object(oid) => {
                    state
                        .objects
                        .get(oid)
                        .map(|o| o.controller)
                        .unwrap_or(PlayerId(0))
                }
            },
            ProposedEvent::Draw { player_id, .. }
            | ProposedEvent::LifeGain { player_id, .. }
            | ProposedEvent::LifeLoss { player_id, .. }
            | ProposedEvent::Discard { player_id, .. }
            | ProposedEvent::Sacrifice { player_id, .. } => *player_id,
            ProposedEvent::CreateToken { owner, .. } => *owner,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proposed_event_has_13_variants() {
        // Verify all 13 variants compile
        let events: Vec<ProposedEvent> = vec![
            ProposedEvent::ZoneChange {
                object_id: ObjectId(1),
                from: Zone::Battlefield,
                to: Zone::Graveyard,
                cause: None,
                applied: HashSet::new(),
            },
            ProposedEvent::Damage {
                source_id: ObjectId(1),
                target: TargetRef::Player(PlayerId(0)),
                amount: 3,
                is_combat: false,
                applied: HashSet::new(),
            },
            ProposedEvent::Draw {
                player_id: PlayerId(0),
                count: 1,
                applied: HashSet::new(),
            },
            ProposedEvent::LifeGain {
                player_id: PlayerId(0),
                amount: 3,
                applied: HashSet::new(),
            },
            ProposedEvent::LifeLoss {
                player_id: PlayerId(0),
                amount: 3,
                applied: HashSet::new(),
            },
            ProposedEvent::AddCounter {
                object_id: ObjectId(1),
                counter_type: "+1/+1".to_string(),
                count: 1,
                applied: HashSet::new(),
            },
            ProposedEvent::RemoveCounter {
                object_id: ObjectId(1),
                counter_type: "+1/+1".to_string(),
                count: 1,
                applied: HashSet::new(),
            },
            ProposedEvent::CreateToken {
                owner: PlayerId(0),
                name: "Soldier".to_string(),
                applied: HashSet::new(),
            },
            ProposedEvent::Discard {
                player_id: PlayerId(0),
                object_id: ObjectId(2),
                applied: HashSet::new(),
            },
            ProposedEvent::Tap {
                object_id: ObjectId(1),
                applied: HashSet::new(),
            },
            ProposedEvent::Untap {
                object_id: ObjectId(1),
                applied: HashSet::new(),
            },
            ProposedEvent::Destroy {
                object_id: ObjectId(1),
                source: None,
                applied: HashSet::new(),
            },
            ProposedEvent::Sacrifice {
                object_id: ObjectId(1),
                player_id: PlayerId(0),
                applied: HashSet::new(),
            },
        ];
        assert_eq!(events.len(), 13);
    }

    #[test]
    fn replacement_id_equality_and_hash() {
        let id1 = ReplacementId {
            source: ObjectId(1),
            index: 0,
        };
        let id2 = ReplacementId {
            source: ObjectId(1),
            index: 0,
        };
        let id3 = ReplacementId {
            source: ObjectId(1),
            index: 1,
        };
        assert_eq!(id1, id2);
        assert_ne!(id1, id3);

        let mut set = HashSet::new();
        set.insert(id1);
        assert!(set.contains(&id2));
        assert!(!set.contains(&id3));
    }

    #[test]
    fn mark_applied_and_already_applied() {
        let mut event = ProposedEvent::Draw {
            player_id: PlayerId(0),
            count: 1,
            applied: HashSet::new(),
        };
        let rid = ReplacementId {
            source: ObjectId(5),
            index: 0,
        };
        assert!(!event.already_applied(&rid));
        event.mark_applied(rid);
        assert!(event.already_applied(&rid));
    }
}
