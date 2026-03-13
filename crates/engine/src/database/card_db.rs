use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;

use super::legality::{normalize_legalities, CardLegalities, LegalityFormat, LegalityStatus};
use crate::types::card::{CardFace, CardRules};

use std::io::BufReader;

pub struct CardDatabase {
    pub(crate) cards: HashMap<String, CardRules>,
    pub(crate) face_index: HashMap<String, CardFace>,
    pub(crate) legalities: HashMap<String, CardLegalities>,
    pub(crate) errors: Vec<(PathBuf, String)>,
}

impl CardDatabase {
    /// Build from MTGJSON atomic cards, running the Oracle text parser.
    /// Used by tests and the oracle_gen binary for library-level access.
    pub fn from_mtgjson(mtgjson_path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        super::oracle_loader::load_from_mtgjson(mtgjson_path)
    }

    /// Load from a pre-processed card-data export.
    pub fn from_export(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let file = std::fs::File::open(path)?;
        let reader = BufReader::new(file);
        let entries: HashMap<String, CardExportEntry> = serde_json::from_reader(reader)?;
        Ok(Self::from_export_entries(entries))
    }

    /// Load from a card-data export JSON string.
    /// Used by the WASM bridge to receive card data from the frontend.
    pub fn from_json_str(json: &str) -> Result<Self, serde_json::Error> {
        let entries: HashMap<String, CardExportEntry> = serde_json::from_str(json)?;
        Ok(Self::from_export_entries(entries))
    }

    fn from_export_entries(entries: HashMap<String, CardExportEntry>) -> Self {
        let mut face_index = HashMap::with_capacity(entries.len());
        let mut legalities = HashMap::new();

        for (_name, entry) in entries {
            let key = entry.face.name.to_lowercase();
            face_index.insert(key.clone(), entry.face);

            let normalized = normalize_legalities(&entry.legalities);
            if !normalized.is_empty() {
                legalities.insert(key, normalized);
            }
        }

        Self {
            cards: HashMap::new(),
            face_index,
            legalities,
            errors: Vec::new(),
        }
    }

    pub fn get_by_name(&self, name: &str) -> Option<&CardRules> {
        self.cards.get(&name.to_lowercase())
    }

    pub fn get_face_by_name(&self, name: &str) -> Option<&CardFace> {
        self.face_index.get(&name.to_lowercase())
    }

    pub fn get_legalities(&self, name: &str) -> Option<&CardLegalities> {
        self.legalities.get(&name.to_lowercase())
    }

    pub fn legality_status(&self, name: &str, format: LegalityFormat) -> Option<LegalityStatus> {
        self.get_legalities(name)
            .and_then(|m| m.get(&format).copied())
    }

    pub fn card_count(&self) -> usize {
        self.cards.len().max(self.face_index.len())
    }

    pub fn errors(&self) -> &[(PathBuf, String)] {
        &self.errors
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &CardRules)> {
        self.cards.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn face_iter(&self) -> impl Iterator<Item = (&str, &CardFace)> {
        self.face_index.iter().map(|(k, v)| (k.as_str(), v))
    }
}

#[derive(Debug, Clone, Deserialize)]
struct CardExportEntry {
    #[serde(flatten)]
    face: CardFace,
    #[serde(default)]
    legalities: HashMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{
        AbilityDefinition, ReplacementDefinition, StaticDefinition, TriggerDefinition,
    };
    use crate::types::card_type::CardType;
    use crate::types::keywords::Keyword;
    use crate::types::mana::ManaCost;

    fn test_face(name: &str) -> CardFace {
        CardFace {
            name: name.to_string(),
            mana_cost: ManaCost::NoCost,
            card_type: CardType::default(),
            power: None,
            toughness: None,
            loyalty: None,
            defense: None,
            oracle_text: None,
            non_ability_text: None,
            flavor_name: None,
            keywords: Vec::<Keyword>::new(),
            abilities: Vec::<AbilityDefinition>::new(),
            triggers: Vec::<TriggerDefinition>::new(),
            static_abilities: Vec::<StaticDefinition>::new(),
            replacements: Vec::<ReplacementDefinition>::new(),
            color_override: None,
            scryfall_oracle_id: None,
            modal: None,
        }
    }

    #[test]
    fn from_json_str_parses_legacy_face_map_without_legalities() {
        let mut map = HashMap::new();
        map.insert("test card".to_string(), test_face("Test Card"));
        let json = serde_json::to_string(&map).unwrap();

        let db = CardDatabase::from_json_str(&json).unwrap();
        assert!(db.get_face_by_name("Test Card").is_some());
        assert!(db.get_legalities("Test Card").is_none());
    }

    #[test]
    fn from_json_str_parses_extended_export_with_legalities() {
        let mut map = serde_json::Map::new();
        map.insert(
            "test card".to_string(),
            serde_json::json!({
                "name": "Test Card",
                "mana_cost": "NoCost",
                "card_type": { "supertypes": [], "core_types": [], "subtypes": [] },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": {
                    "standard": "Legal",
                    "commander": "not_legal"
                }
            }),
        );

        let json = serde_json::Value::Object(map).to_string();
        let db = CardDatabase::from_json_str(&json).unwrap();

        assert_eq!(
            db.legality_status("Test Card", LegalityFormat::Standard),
            Some(LegalityStatus::Legal)
        );
        assert_eq!(
            db.legality_status("Test Card", LegalityFormat::Commander),
            Some(LegalityStatus::NotLegal)
        );
    }
}
