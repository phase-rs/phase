use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::types::ability::{ReplacementDefinition, StaticDefinition, TriggerDefinition};
use crate::types::card_type::CardType;
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::keywords::Keyword;
use crate::types::mana::{ManaColor, ManaCost};
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// Stored back-face data for double-faced cards (DFCs).
/// Populated when a Transform-layout card enters the game.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackFaceData {
    pub name: String,
    pub power: Option<i32>,
    pub toughness: Option<i32>,
    pub card_types: CardType,
    pub keywords: Vec<Keyword>,
    pub abilities: Vec<String>,
    pub color: Vec<ManaColor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CounterType {
    Plus1Plus1,
    Minus1Minus1,
    Loyalty,
    Generic(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameObject {
    pub id: ObjectId,
    pub card_id: CardId,
    pub owner: PlayerId,
    pub controller: PlayerId,
    pub zone: Zone,

    // Battlefield state
    pub tapped: bool,
    pub face_down: bool,
    pub flipped: bool,
    pub transformed: bool,

    // Combat
    pub damage_marked: u32,
    pub dealt_deathtouch_damage: bool,

    // Attachments
    pub attached_to: Option<ObjectId>,
    pub attachments: Vec<ObjectId>,

    // Counters
    pub counters: HashMap<CounterType, u32>,

    // Characteristics
    pub name: String,
    pub power: Option<i32>,
    pub toughness: Option<i32>,
    pub loyalty: Option<u32>,
    pub card_types: CardType,
    pub mana_cost: ManaCost,
    pub keywords: Vec<Keyword>,
    pub abilities: Vec<String>,
    pub trigger_definitions: Vec<TriggerDefinition>,
    pub replacement_definitions: Vec<ReplacementDefinition>,
    pub static_definitions: Vec<StaticDefinition>,
    pub svars: HashMap<String, String>,
    pub color: Vec<ManaColor>,

    // Back face data for double-faced cards (DFCs)
    pub back_face: Option<BackFaceData>,

    // Base characteristics (for layer system)
    pub base_power: Option<i32>,
    pub base_toughness: Option<i32>,
    pub base_keywords: Vec<Keyword>,
    pub base_color: Vec<ManaColor>,

    // Timestamp for layer ordering
    pub timestamp: u64,

    // Summoning sickness
    pub entered_battlefield_turn: Option<u32>,

    // Coverage flag (computed for serialization, not persisted)
    #[serde(skip_deserializing, default)]
    pub has_unimplemented_mechanics: bool,

    // Derived field: true when this creature can't attack/block due to summoning sickness.
    // Computed before serialization, not persisted.
    #[serde(skip_deserializing, default)]
    pub has_summoning_sickness: bool,

    // Planeswalker: whether a loyalty ability has been activated this turn
    #[serde(skip_deserializing, default)]
    pub loyalty_activated_this_turn: bool,
}

impl GameObject {
    pub fn new(id: ObjectId, card_id: CardId, owner: PlayerId, name: String, zone: Zone) -> Self {
        GameObject {
            id,
            card_id,
            owner,
            controller: owner,
            zone,
            tapped: false,
            face_down: false,
            flipped: false,
            transformed: false,
            damage_marked: 0,
            dealt_deathtouch_damage: false,
            attached_to: None,
            attachments: Vec::new(),
            counters: HashMap::new(),
            name,
            power: None,
            toughness: None,
            loyalty: None,
            card_types: CardType::default(),
            mana_cost: ManaCost::default(),
            keywords: Vec::new(),
            abilities: Vec::new(),
            trigger_definitions: Vec::new(),
            replacement_definitions: Vec::new(),
            static_definitions: Vec::new(),
            svars: HashMap::new(),
            color: Vec::new(),
            back_face: None,
            base_power: None,
            base_toughness: None,
            base_keywords: Vec::new(),
            base_color: Vec::new(),
            timestamp: 0,
            entered_battlefield_turn: None,
            has_unimplemented_mechanics: false,
            has_summoning_sickness: false,
            loyalty_activated_this_turn: false,
        }
    }

    /// Check if this object has a specific keyword, using discriminant-based matching.
    pub fn has_keyword(&self, keyword: &Keyword) -> bool {
        super::keywords::has_keyword(self, keyword)
    }

    /// Check if this object uses any mechanics the engine cannot handle.
    pub fn has_unimplemented_mechanics(&self) -> bool {
        super::coverage::has_unimplemented_mechanics(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn game_object_has_all_rules_relevant_fields() {
        let obj = GameObject::new(
            ObjectId(1),
            CardId(100),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Hand,
        );

        assert_eq!(obj.id, ObjectId(1));
        assert_eq!(obj.card_id, CardId(100));
        assert_eq!(obj.owner, PlayerId(0));
        assert_eq!(obj.controller, PlayerId(0));
        assert_eq!(obj.zone, Zone::Hand);
        assert!(!obj.tapped);
        assert!(!obj.face_down);
        assert!(!obj.flipped);
        assert!(!obj.transformed);
        assert_eq!(obj.damage_marked, 0);
        assert!(!obj.dealt_deathtouch_damage);
        assert!(obj.attached_to.is_none());
        assert!(obj.attachments.is_empty());
        assert!(obj.counters.is_empty());
        assert_eq!(obj.name, "Lightning Bolt");
        assert!(obj.power.is_none());
        assert!(obj.toughness.is_none());
        assert!(obj.loyalty.is_none());
        assert!(obj.keywords.is_empty());
        assert!(obj.abilities.is_empty());
        assert!(obj.color.is_empty());
        assert!(obj.entered_battlefield_turn.is_none());
    }

    #[test]
    fn counter_type_covers_required_variants() {
        let counters = [
            CounterType::Plus1Plus1,
            CounterType::Minus1Minus1,
            CounterType::Loyalty,
            CounterType::Generic("charge".to_string()),
        ];
        assert_eq!(counters.len(), 4);
    }

    #[test]
    fn game_object_serializes_and_roundtrips() {
        let obj = GameObject::new(
            ObjectId(1),
            CardId(100),
            PlayerId(0),
            "Test Card".to_string(),
            Zone::Battlefield,
        );
        let json = serde_json::to_string(&obj).unwrap();
        let deserialized: GameObject = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, "Test Card");
        assert_eq!(deserialized.id, ObjectId(1));
    }

    #[test]
    fn controller_defaults_to_owner() {
        let obj = GameObject::new(
            ObjectId(1),
            CardId(1),
            PlayerId(1),
            "Card".to_string(),
            Zone::Hand,
        );
        assert_eq!(obj.controller, obj.owner);
    }
}
