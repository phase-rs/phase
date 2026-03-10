use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::types::ability::{
    AbilityDefinition, ReplacementDefinition, StaticDefinition, TriggerDefinition,
};

/// Per-face ability set for multi-face cards.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FaceAbilities {
    /// Spell and activated ability definitions
    #[serde(default)]
    pub abilities: Vec<AbilityDefinition>,
    /// Trigger definitions
    #[serde(default)]
    pub triggers: Vec<TriggerDefinition>,
    /// Static ability definitions
    #[serde(default)]
    pub statics: Vec<StaticDefinition>,
    /// Replacement effect definitions
    #[serde(default)]
    pub replacements: Vec<ReplacementDefinition>,
}

/// Root structure for per-card ability JSON files.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AbilityFile {
    /// JSON Schema reference for editor autocompletion
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// Spell and activated ability definitions
    #[serde(default)]
    pub abilities: Vec<AbilityDefinition>,
    /// Trigger definitions
    #[serde(default)]
    pub triggers: Vec<TriggerDefinition>,
    /// Static ability definitions
    #[serde(default)]
    pub statics: Vec<StaticDefinition>,
    /// Replacement effect definitions
    #[serde(default)]
    pub replacements: Vec<ReplacementDefinition>,
    /// Per-face ability definitions for multi-face cards
    #[serde(default)]
    pub faces: Vec<FaceAbilities>,
}

/// Generate the JSON Schema for the AbilityFile format.
pub fn generate_schema() -> schemars::Schema {
    schemars::schema_for!(AbilityFile)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{AbilityKind, DamageAmount, Effect, TargetSpec};

    #[test]
    fn generate_ability_schema_and_write_file() {
        let schema = generate_schema();
        let json = serde_json::to_string_pretty(&schema).unwrap();

        // Write to data/abilities/schema.json
        let schema_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../data/abilities/schema.json");
        std::fs::create_dir_all(schema_path.parent().unwrap()).unwrap();
        std::fs::write(&schema_path, format!("{json}\n")).unwrap();

        // Verify the schema has key definitions
        assert!(json.contains("AbilityFile"));
        assert!(json.contains("Effect"));
        assert!(json.contains("DealDamage"));
    }

    #[test]
    fn ability_schema_snapshot() {
        let schema = generate_schema();
        insta::assert_json_snapshot!("ability_schema", schema);
    }

    #[test]
    fn lightning_bolt_deserializes() {
        let json = include_str!("../../../../data/abilities/lightning_bolt.json");
        let file: AbilityFile = serde_json::from_str(json).unwrap();
        assert_eq!(file.abilities.len(), 1);
        assert_eq!(file.abilities[0].kind, AbilityKind::Spell);
        match &file.abilities[0].effect {
            Effect::DealDamage { amount, target } => {
                assert_eq!(*amount, DamageAmount::Fixed(3));
                assert_eq!(*target, TargetSpec::Any);
            }
            other => panic!("Expected DealDamage, got {:?}", other),
        }
    }

    #[test]
    fn ability_file_without_faces_backward_compat() {
        // Existing JSON without `faces` should still deserialize
        let json = r#"{ "abilities": [] }"#;
        let file: AbilityFile = serde_json::from_str(json).unwrap();
        assert!(file.faces.is_empty());
        assert!(file.abilities.is_empty());
    }

    #[test]
    fn ability_file_with_faces_deserializes() {
        let json = r#"{
            "abilities": [],
            "faces": [
                { "abilities": [] },
                { "abilities": [] }
            ]
        }"#;
        let file: AbilityFile = serde_json::from_str(json).unwrap();
        assert_eq!(file.faces.len(), 2);
    }

    #[test]
    fn face_abilities_roundtrip() {
        let face = FaceAbilities {
            abilities: vec![],
            triggers: vec![],
            statics: vec![],
            replacements: vec![],
        };
        let json = serde_json::to_string(&face).unwrap();
        let deserialized: FaceAbilities = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.abilities.len(), 0);
    }

    #[test]
    fn card_face_scryfall_oracle_id_backward_compat() {
        use crate::types::card::CardFace;
        // CardFace without scryfall_oracle_id should still deserialize
        let face = CardFace {
            name: "Test".to_string(),
            mana_cost: crate::types::mana::ManaCost::NoCost,
            card_type: crate::types::card_type::CardType::default(),
            power: None,
            toughness: None,
            loyalty: None,
            defense: None,
            oracle_text: None,
            non_ability_text: None,
            flavor_name: None,
            keywords: vec![],
            abilities: vec![],
            triggers: vec![],
            static_abilities: vec![],
            replacements: vec![],
            svars: std::collections::HashMap::new(),
            color_override: None,
            scryfall_oracle_id: None,
        };
        let json = serde_json::to_string(&face).unwrap();
        let deserialized: CardFace = serde_json::from_str(&json).unwrap();
        assert!(deserialized.scryfall_oracle_id.is_none());
    }

    #[test]
    fn schema_includes_face_abilities_definition() {
        let schema = generate_schema();
        let json = serde_json::to_string_pretty(&schema).unwrap();
        assert!(json.contains("FaceAbilities"), "Schema should include FaceAbilities definition");
    }

    #[test]
    fn ability_json_roundtrip() {
        let json = include_str!("../../../../data/abilities/lightning_bolt.json");
        let file: AbilityFile = serde_json::from_str(json).unwrap();
        let reserialized = serde_json::to_string_pretty(&file).unwrap();
        let file2: AbilityFile = serde_json::from_str(&reserialized).unwrap();
        assert_eq!(file.abilities.len(), file2.abilities.len());
        // Compare serialized forms for structural equality
        let json1 = serde_json::to_value(&file).unwrap();
        let json2 = serde_json::to_value(&file2).unwrap();
        assert_eq!(json1, json2);
    }
}
