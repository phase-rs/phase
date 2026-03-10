use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::types::ability::{
    AbilityDefinition, ReplacementDefinition, StaticDefinition, TriggerDefinition,
};

/// Root structure for per-card ability JSON files.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AbilityFile {
    /// JSON Schema reference for editor autocompletion
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// Spell and activated ability definitions
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
