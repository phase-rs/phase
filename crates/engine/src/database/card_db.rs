use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;

use crate::types::card::{CardFace, CardRules};

use std::io::BufReader;

pub struct CardDatabase {
    pub(crate) cards: HashMap<String, CardRules>,
    pub(crate) face_index: HashMap<String, CardFace>,
    pub(crate) errors: Vec<(PathBuf, String)>,
}

impl CardDatabase {
    /// Build from MTGJSON atomic cards, running the Oracle text parser.
    /// Used by tests and the oracle_gen binary for library-level access.
    pub fn from_mtgjson(mtgjson_path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        super::oracle_loader::load_from_mtgjson(mtgjson_path)
    }

    /// Load from a pre-processed card-data export (HashMap<String, CardFace> as JSON).
    pub fn from_export(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let file = std::fs::File::open(path)?;
        let reader = BufReader::new(file);
        let faces: HashMap<String, CardFace> = serde_json::from_reader(reader)?;
        Ok(Self::from_face_map(faces))
    }

    /// Load from a JSON string containing a HashMap<String, CardFace>.
    /// Used by the WASM bridge to receive card data from the frontend.
    pub fn from_json_str(json: &str) -> Result<Self, serde_json::Error> {
        let faces: HashMap<String, CardFace> = serde_json::from_str(json)?;
        Ok(Self::from_face_map(faces))
    }

    fn from_face_map(faces: HashMap<String, CardFace>) -> Self {
        let mut face_index = HashMap::with_capacity(faces.len());
        for (name, face) in faces {
            face_index.insert(name.to_lowercase(), face);
        }

        Self {
            cards: HashMap::new(),
            face_index,
            errors: Vec::new(),
        }
    }

    pub fn get_by_name(&self, name: &str) -> Option<&CardRules> {
        self.cards.get(&name.to_lowercase())
    }

    pub fn get_face_by_name(&self, name: &str) -> Option<&CardFace> {
        self.face_index.get(&name.to_lowercase())
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
