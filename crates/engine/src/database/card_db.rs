use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;

use super::legality::{normalize_legalities, CardLegalities, LegalityFormat, LegalityStatus};
use super::mtgjson::Ruling;
use crate::types::card::{CardFace, CardRules, LayoutKind, PrintedCardRef};

use std::io::BufReader;

pub struct CardDatabase {
    pub(crate) cards: HashMap<String, CardRules>,
    pub(crate) face_index: HashMap<String, CardFace>,
    pub(crate) oracle_id_index: HashMap<String, Vec<String>>,
    /// Maps oracle_id → runtime LayoutKind for multi-face cards.
    /// Populated only from the export path (the MTGJSON path uses `cards` directly).
    /// Enables `rehydrate_game_from_card_db` to determine the correct layout kind
    /// when `get_by_name` returns None (export path doesn't build `CardRules`).
    pub(crate) layout_index: HashMap<String, LayoutKind>,
    pub(crate) legalities: HashMap<String, CardLegalities>,
    /// Maps face key (lowercased card name) → set codes the card was printed in.
    /// Populated only via the export path (MTGJSON `printings` field).
    /// Used by the coverage dashboard to group cards by set.
    pub(crate) printings_index: HashMap<String, Vec<String>>,
    /// Maps face key (lowercased card name) → official WotC rulings.
    /// Populated only via the export path. Only front faces of multi-face
    /// cards carry rulings; back-face lookups return the empty slice.
    pub(crate) rulings_index: HashMap<String, Vec<Ruling>>,
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
        let mut oracle_id_index: HashMap<String, Vec<String>> = HashMap::new();
        let mut layout_index: HashMap<String, LayoutKind> = HashMap::new();
        let mut legalities = HashMap::new();
        let mut printings_index: HashMap<String, Vec<String>> = HashMap::new();
        let mut rulings_index: HashMap<String, Vec<Ruling>> = HashMap::new();

        for (_name, entry) in entries {
            let key = entry.face.name.to_lowercase();
            if let Some(oracle_id) = entry.face.scryfall_oracle_id.clone() {
                oracle_id_index
                    .entry(oracle_id.clone())
                    .or_default()
                    .push(key.clone());
                if let Some(layout_kind) = entry.layout.as_deref().and_then(map_layout_str) {
                    layout_index.entry(oracle_id).or_insert(layout_kind);
                }
            }
            face_index.insert(key.clone(), entry.face);

            if !entry.printings.is_empty() {
                printings_index.insert(key.clone(), entry.printings);
            }

            if !entry.rulings.is_empty() {
                rulings_index.insert(key.clone(), entry.rulings);
            }

            let normalized = normalize_legalities(&entry.legalities);
            if !normalized.is_empty() {
                legalities.insert(key, normalized);
            }
        }

        Self {
            cards: HashMap::new(),
            face_index,
            oracle_id_index,
            layout_index,
            legalities,
            printings_index,
            rulings_index,
            errors: Vec::new(),
        }
    }

    pub fn get_by_name(&self, name: &str) -> Option<&CardRules> {
        self.cards.get(&name.to_lowercase())
    }

    pub fn get_face_by_name(&self, name: &str) -> Option<&CardFace> {
        self.face_index.get(&name.to_lowercase())
    }

    pub fn get_face_by_printed_ref(&self, printed_ref: &PrintedCardRef) -> Option<&CardFace> {
        self.oracle_id_index
            .get(&printed_ref.oracle_id)?
            .iter()
            .filter_map(|name| self.face_index.get(name))
            .find(|face| face.name == printed_ref.face_name)
    }

    pub fn get_other_face_by_printed_ref(&self, printed_ref: &PrintedCardRef) -> Option<&CardFace> {
        let mut other_faces = self
            .oracle_id_index
            .get(&printed_ref.oracle_id)?
            .iter()
            .filter_map(|name| self.face_index.get(name))
            .filter(|face| face.name != printed_ref.face_name);
        let other = other_faces.next()?;
        if other_faces.next().is_some() {
            return None;
        }
        Some(other)
    }

    pub fn get_legalities(&self, name: &str) -> Option<&CardLegalities> {
        self.legalities.get(&name.to_lowercase())
    }

    pub fn legality_status(&self, name: &str, format: LegalityFormat) -> Option<LegalityStatus> {
        self.get_legalities(name)
            .and_then(|m| m.get(&format).copied())
    }

    /// Returns the set codes a card has been printed in (e.g. `["M11", "LEA"]`),
    /// or `None` if the card was loaded via a path that doesn't record printings.
    pub fn printings_for(&self, name: &str) -> Option<&[String]> {
        self.printings_index
            .get(&name.to_lowercase())
            .map(Vec::as_slice)
    }

    /// Returns the official WotC rulings for a card. Returns an empty slice
    /// when the card has no recorded rulings, when the card was loaded via a
    /// path that doesn't record rulings, or when looking up a back-face name
    /// (rulings are attached to the front face only).
    pub fn rulings_for(&self, name: &str) -> &[Ruling] {
        self.rulings_index
            .get(&name.to_lowercase())
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn card_count(&self) -> usize {
        self.cards.len().max(self.face_index.len())
    }

    /// Returns the runtime layout kind for a face identified by oracle_id.
    /// Used by `rehydrate_game_from_card_db` to determine the correct layout
    /// discriminant when `get_by_name` returns None (export loading path).
    pub fn get_layout_kind(&self, oracle_id: &str) -> Option<LayoutKind> {
        self.layout_index.get(oracle_id).copied()
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

    /// Returns all card names (title-cased as stored in face data), sorted.
    pub fn card_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .face_index
            .values()
            .map(|face| face.name.clone())
            .collect();
        names.sort();
        names
    }
}

#[derive(Debug, Clone, Deserialize)]
struct CardExportEntry {
    #[serde(flatten)]
    face: CardFace,
    #[serde(default)]
    legalities: HashMap<String, String>,
    /// MTGJSON layout string for multi-face cards (e.g. "modal_dfc", "transform").
    #[serde(default)]
    layout: Option<String>,
    /// Set codes the card has been printed in (from MTGJSON `printings`).
    #[serde(default)]
    printings: Vec<String>,
    /// Official WotC rulings; populated on the front face only for multi-face cards.
    #[serde(default)]
    rulings: Vec<Ruling>,
}

/// Convert MTGJSON layout string to runtime `LayoutKind`.
/// Returns `None` for single-face layouts since they don't need a layout discriminant.
fn map_layout_str(s: &str) -> Option<LayoutKind> {
    match s {
        "modal_dfc" => Some(LayoutKind::Modal),
        "transform" => Some(LayoutKind::Transform),
        "adventure" => Some(LayoutKind::Adventure),
        "meld" => Some(LayoutKind::Meld),
        "split" => Some(LayoutKind::Split),
        "flip" => Some(LayoutKind::Flip),
        "omen" => Some(LayoutKind::Omen),
        // CR 702.xxx: Prepare (Strixhaven) — Adventure-family frame. Assign
        // when WotC publishes SOS CR update.
        "prepare" => Some(LayoutKind::Prepare),
        _ => None,
    }
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
            color_identity: vec![],
            scryfall_oracle_id: None,
            modal: None,
            additional_cost: None,
            strive_cost: None,
            casting_restrictions: vec![],
            casting_options: vec![],
            solve_condition: None,
            parse_warnings: vec![],
            brawl_commander: false,
            metadata: Default::default(),
            rarities: Default::default(),
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
                "mana_cost": { "type": "NoCost" },
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
