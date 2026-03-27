use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::types::ability::{
    AbilityDefinition, AdditionalCost, BasicLandType, CastingPermission, CastingRestriction,
    ChosenAttribute, ChosenSubtypeKind, ModalChoice, NinjutsuVariant, ReplacementDefinition,
    SolveCondition, SpellCastingOption, StaticDefinition, TriggerDefinition,
};
use crate::types::card::PrintedCardRef;
use crate::types::card_type::{CardType, CoreType};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::keywords::Keyword;
use crate::types::mana::{ManaColor, ManaCost};
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

// Note: HashMap still used for counters field

/// Stored back-face data for double-faced cards (DFCs).
/// Populated when a Transform-layout card enters the game.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackFaceData {
    pub name: String,
    pub power: Option<i32>,
    pub toughness: Option<i32>,
    pub loyalty: Option<u32>,
    pub card_types: CardType,
    pub mana_cost: ManaCost,
    pub keywords: Vec<Keyword>,
    pub abilities: Vec<AbilityDefinition>,
    pub trigger_definitions: Vec<TriggerDefinition>,
    pub replacement_definitions: Vec<ReplacementDefinition>,
    pub static_definitions: Vec<StaticDefinition>,
    pub color: Vec<ManaColor>,
    pub printed_ref: Option<PrintedCardRef>,
    pub modal: Option<ModalChoice>,
    pub additional_cost: Option<AdditionalCost>,
    pub strive_cost: Option<ManaCost>,
    pub casting_restrictions: Vec<CastingRestriction>,
    pub casting_options: Vec<SpellCastingOption>,
}

/// Counter types serialize as flat strings so they can be used as JSON map keys
/// in `HashMap<CounterType, u32>`. Without this, `Generic("quest")` would serialize
/// as `{"Generic":"quest"}` which serde_json rejects as a map key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, schemars::JsonSchema)]
pub enum CounterType {
    Plus1Plus1,
    Minus1Minus1,
    Loyalty,
    /// CR 122.1g: When a permanent with a stun counter would become untapped during its
    /// controller's untap step, one stun counter is removed instead of untapping.
    Stun,
    /// CR 714.1: Lore counters track Saga chapter progression.
    Lore,
    Generic(String),
}

impl CounterType {
    pub fn as_str(&self) -> &str {
        match self {
            CounterType::Plus1Plus1 => "P1P1",
            CounterType::Minus1Minus1 => "M1M1",
            CounterType::Loyalty => "loyalty",
            CounterType::Stun => "stun",
            CounterType::Lore => "lore",
            CounterType::Generic(s) => s.as_str(),
        }
    }
}

impl serde::Serialize for CounterType {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for CounterType {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(parse_counter_type(&s))
    }
}

pub fn parse_counter_type(text: &str) -> CounterType {
    match text.trim().trim_end_matches(" counter").trim() {
        "P1P1" | "+1/+1" | "plus1plus1" => CounterType::Plus1Plus1,
        "M1M1" | "-1/-1" | "minus1minus1" => CounterType::Minus1Minus1,
        "LOYALTY" | "loyalty" => CounterType::Loyalty,
        "stun" => CounterType::Stun,
        "lore" | "LORE" => CounterType::Lore,
        other => CounterType::Generic(other.to_string()),
    }
}

/// CR 719.3b: Tracks the solve state of a Case enchantment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseState {
    pub is_solved: bool,
    pub solve_condition: SolveCondition,
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
    pub abilities: Vec<AbilityDefinition>,
    pub trigger_definitions: Vec<TriggerDefinition>,
    pub replacement_definitions: Vec<ReplacementDefinition>,
    pub static_definitions: Vec<StaticDefinition>,
    pub color: Vec<ManaColor>,
    pub printed_ref: Option<PrintedCardRef>,

    // Back face data for double-faced cards (DFCs)
    pub back_face: Option<BackFaceData>,

    // Base characteristics (for layer system)
    pub base_power: Option<i32>,
    pub base_toughness: Option<i32>,
    pub base_card_types: CardType,
    pub base_keywords: Vec<Keyword>,
    pub base_abilities: Vec<AbilityDefinition>,
    pub base_trigger_definitions: Vec<TriggerDefinition>,
    pub base_replacement_definitions: Vec<ReplacementDefinition>,
    pub base_static_definitions: Vec<StaticDefinition>,
    pub base_color: Vec<ManaColor>,

    // Timestamp for layer ordering
    pub timestamp: u64,

    // Summoning sickness
    pub entered_battlefield_turn: Option<u32>,

    /// CR 702.49: Which ninjutsu-family variant was paid to put this permanent onto the
    /// battlefield, and on which turn. Used by trigger conditions and ability conditions
    /// that check "if its sneak/ninjutsu cost was paid this turn."
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ninjutsu_variant_paid: Option<(NinjutsuVariant, u32)>,

    // Coverage: lists unimplemented mechanics (computed for serialization, not persisted)
    #[serde(skip_deserializing, default, skip_serializing_if = "Vec::is_empty")]
    pub unimplemented_mechanics: Vec<String>,

    // Derived field: true when this creature can't attack/block due to summoning sickness.
    // Computed before serialization, not persisted.
    #[serde(skip_deserializing, default)]
    pub has_summoning_sickness: bool,

    // Derived field: devotion count for cards that reference devotion.
    // Computed before serialization based on DevotionColors in static params.
    #[serde(skip_deserializing, default, skip_serializing_if = "Option::is_none")]
    pub devotion: Option<u32>,

    // Derived field: true when this permanent has an activatable mana ability.
    // Computed before serialization, not persisted.
    #[serde(skip_deserializing, default)]
    pub has_mana_ability: bool,

    // Derived field: ability index of the first mana ability, for frontend dispatch.
    // Computed before serialization, not persisted.
    #[serde(skip_deserializing, default, skip_serializing_if = "Option::is_none")]
    pub mana_ability_index: Option<usize>,

    // Derived field: currently available colored mana options for this object.
    // Computed before serialization from mana abilities + activation constraints.
    #[serde(skip_deserializing, default, skip_serializing_if = "Vec::is_empty")]
    pub available_mana_colors: Vec<ManaColor>,

    // Planeswalker: whether a loyalty ability has been activated this turn
    #[serde(skip_deserializing, default)]
    pub loyalty_activated_this_turn: bool,

    // Commander: whether this object is a commander card
    #[serde(default)]
    pub is_commander: bool,

    /// CR 903.8: Commander tax — pre-computed {2} per previous cast from command zone.
    /// Display-only: computed by `derive_display_state()`.
    #[serde(skip_deserializing, default, skip_serializing_if = "Option::is_none")]
    pub commander_tax: Option<u32>,

    /// CR 114.5: Whether this object is an emblem (immune to removal, persists in command zone)
    #[serde(default)]
    pub is_emblem: bool,

    /// CR 111.1: Whether this object is a token (not a card).
    #[serde(default)]
    pub is_token: bool,

    /// Modal spell metadata ("Choose one —", etc.). Copied from CardFace at load time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modal: Option<ModalChoice>,

    /// Additional casting cost. Copied from CardFace at load time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_cost: Option<AdditionalCost>,

    /// CR 207.2c + CR 601.2f: Strive per-target surcharge cost. Copied from CardFace at load time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strive_cost: Option<ManaCost>,

    /// Spell-casting restrictions. Copied from CardFace at load time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub casting_restrictions: Vec<CastingRestriction>,

    /// Spell-casting options. Copied from CardFace at load time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub casting_options: Vec<SpellCastingOption>,

    /// CR 715.3d: Runtime casting permissions (e.g., Adventure creature castable from exile).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub casting_permissions: Vec<CastingPermission>,

    /// Choices made as this permanent entered (e.g., "choose a color").
    /// Persists for the object's lifetime on the battlefield.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chosen_attributes: Vec<ChosenAttribute>,

    /// CR 701.15c: Which players have goaded this creature. A goaded creature must attack
    /// each combat if able and must attack a player other than the goading player(s) if able.
    /// Multiple players can goad the same creature, creating additional combat requirements.
    #[serde(default, skip_serializing_if = "std::collections::HashSet::is_empty")]
    pub goaded_by: std::collections::HashSet<PlayerId>,

    /// CR 701.60a: Whether this creature is currently suspected.
    /// The designation is the source of truth; menace and CantBlock are derived
    /// via `base_keywords`/`base_static_definitions` (Option C architecture).
    #[serde(default)]
    pub is_suspected: bool,

    /// CR 701.37b: Monstrous designation. Stays until the permanent leaves the battlefield.
    /// Not an ability or copiable value — purely a marker for monstrosity and related abilities.
    #[serde(default)]
    pub monstrous: bool,

    /// CR 613 + CR 510.1: This creature assigns combat damage equal to its toughness
    /// rather than its power. Set by continuous effects during layer evaluation.
    #[serde(default)]
    pub assigns_damage_from_toughness: bool,

    /// CR 719.3b: Case enchantment solve state. Present only on Case permanents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub case_state: Option<CaseState>,

    /// CR 716.3: Class enchantment level. Present only on Class permanents.
    /// Class level is NOT a counter (CR 716) — proliferate/counter manipulation must not interact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class_level: Option<u8>,

    /// CR 400.7d: Transient field tracking the zone a spell was cast from.
    /// Set when a spell resolves to a permanent; consumed by ETB trigger processing
    /// to evaluate conditions like "if you cast it from your hand".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cast_from_zone: Option<Zone>,
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
            color: Vec::new(),
            printed_ref: None,
            back_face: None,
            base_power: None,
            base_toughness: None,
            base_card_types: CardType::default(),
            base_keywords: Vec::new(),
            base_abilities: Vec::new(),
            base_trigger_definitions: Vec::new(),
            base_replacement_definitions: Vec::new(),
            base_static_definitions: Vec::new(),
            base_color: Vec::new(),
            timestamp: 0,
            entered_battlefield_turn: None,
            ninjutsu_variant_paid: None,
            unimplemented_mechanics: Vec::new(),
            has_summoning_sickness: false,
            has_mana_ability: false,
            mana_ability_index: None,
            devotion: None,
            available_mana_colors: Vec::new(),
            loyalty_activated_this_turn: false,
            is_commander: false,
            commander_tax: None,
            is_emblem: false,
            is_token: false,
            modal: None,
            additional_cost: None,
            strive_cost: None,
            casting_restrictions: Vec::new(),
            casting_options: Vec::new(),
            casting_permissions: Vec::new(),
            chosen_attributes: Vec::new(),
            goaded_by: std::collections::HashSet::new(),
            is_suspected: false,
            monstrous: false,
            assigns_damage_from_toughness: false,
            case_state: None,
            class_level: None,
            cast_from_zone: None,
        }
    }

    /// Check if this object has a specific keyword, using discriminant-based matching.
    pub fn has_keyword(&self, keyword: &Keyword) -> bool {
        super::keywords::has_keyword(self, keyword)
    }

    /// Check if this object uses any mechanics the engine cannot handle.
    pub fn has_unimplemented_mechanics(&self) -> bool {
        !super::coverage::unimplemented_mechanics(self).is_empty()
    }

    /// Look up a stored choice by category.
    pub fn chosen_color(&self) -> Option<ManaColor> {
        self.chosen_attributes.iter().find_map(|a| match a {
            ChosenAttribute::Color(c) => Some(*c),
            _ => None,
        })
    }

    /// Look up a stored basic land type choice.
    pub fn chosen_basic_land_type(&self) -> Option<BasicLandType> {
        self.chosen_attributes.iter().find_map(|a| match a {
            ChosenAttribute::BasicLandType(t) => Some(*t),
            _ => None,
        })
    }

    /// Look up a stored creature type choice.
    pub fn chosen_creature_type(&self) -> Option<&str> {
        self.chosen_attributes.iter().find_map(|a| match a {
            ChosenAttribute::CreatureType(s) => Some(s.as_str()),
            _ => None,
        })
    }

    /// Look up a stored chosen number (e.g., Talion's "choose a number").
    pub fn chosen_number(&self) -> Option<u8> {
        self.chosen_attributes.iter().find_map(|a| match a {
            ChosenAttribute::Number(n) => Some(*n),
            _ => None,
        })
    }

    /// CR 714.1: Returns the final chapter number for a Saga, or None if not a Saga.
    /// Derived at runtime from the maximum threshold in the trigger definitions' counter filters.
    pub fn final_chapter_number(&self) -> Option<u32> {
        if !self.card_types.subtypes.iter().any(|s| s == "Saga") {
            return None;
        }
        self.trigger_definitions
            .iter()
            .filter_map(|t| t.counter_filter.as_ref().and_then(|f| f.threshold))
            .max()
    }

    /// CR 702.51a: Whether this object can be tapped for convoke/waterbend mana.
    /// Requires: on battlefield, untapped, creature or artifact, controlled by `player`.
    pub fn is_convoke_eligible(&self, player: PlayerId) -> bool {
        self.controller == player
            && self.zone == Zone::Battlefield
            && !self.tapped
            && (self.card_types.core_types.contains(&CoreType::Creature)
                || self.card_types.core_types.contains(&CoreType::Artifact))
    }

    /// Get the chosen subtype as a string, unified across creature types and basic land types.
    /// Used by the layer system's `AddChosenSubtype` modification.
    pub fn chosen_subtype_str(&self, kind: &ChosenSubtypeKind) -> Option<String> {
        match kind {
            ChosenSubtypeKind::CreatureType => self.chosen_creature_type().map(|s| s.to_string()),
            ChosenSubtypeKind::BasicLandType => self
                .chosen_basic_land_type()
                .map(|t| t.as_subtype_str().to_string()),
        }
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
    fn chosen_color_returns_stored_color() {
        let mut obj = GameObject::new(
            ObjectId(1),
            CardId(100),
            PlayerId(0),
            "Test Land".to_string(),
            Zone::Battlefield,
        );
        assert!(obj.chosen_color().is_none());
        obj.chosen_attributes
            .push(ChosenAttribute::Color(ManaColor::Red));
        assert_eq!(obj.chosen_color(), Some(ManaColor::Red));
    }

    #[test]
    fn chosen_basic_land_type_returns_stored_type() {
        let mut obj = GameObject::new(
            ObjectId(1),
            CardId(100),
            PlayerId(0),
            "Test Land".to_string(),
            Zone::Battlefield,
        );
        obj.chosen_attributes
            .push(ChosenAttribute::BasicLandType(BasicLandType::Forest));
        assert_eq!(obj.chosen_basic_land_type(), Some(BasicLandType::Forest));
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

    #[test]
    fn parse_counter_type_lore() {
        assert_eq!(parse_counter_type("lore"), CounterType::Lore);
        assert_eq!(parse_counter_type("LORE"), CounterType::Lore);
        assert_eq!(parse_counter_type("lore counter"), CounterType::Lore);
    }

    #[test]
    fn final_chapter_number_returns_max() {
        use crate::types::ability::{CounterTriggerFilter, TriggerDefinition};
        use crate::types::triggers::TriggerMode;

        let mut obj = GameObject::new(
            ObjectId(1),
            CardId(1),
            PlayerId(0),
            "The Eldest Reborn".to_string(),
            Zone::Battlefield,
        );
        obj.card_types.subtypes.push("Saga".to_string());
        obj.trigger_definitions = vec![
            TriggerDefinition::new(TriggerMode::CounterAdded).counter_filter(
                CounterTriggerFilter {
                    counter_type: CounterType::Lore,
                    threshold: Some(1),
                },
            ),
            TriggerDefinition::new(TriggerMode::CounterAdded).counter_filter(
                CounterTriggerFilter {
                    counter_type: CounterType::Lore,
                    threshold: Some(2),
                },
            ),
            TriggerDefinition::new(TriggerMode::CounterAdded).counter_filter(
                CounterTriggerFilter {
                    counter_type: CounterType::Lore,
                    threshold: Some(3),
                },
            ),
        ];
        assert_eq!(obj.final_chapter_number(), Some(3));
    }

    #[test]
    fn final_chapter_number_non_saga() {
        let obj = GameObject::new(
            ObjectId(1),
            CardId(1),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Hand,
        );
        assert_eq!(obj.final_chapter_number(), None);
    }
}
