use std::collections::HashMap;
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::parser::{parse_card_file, ParseError};
use crate::types::card::{CardFace, CardLayout, CardRules};

pub struct CardDatabase {
    cards: HashMap<String, CardRules>,
    face_index: HashMap<String, CardFace>,
    errors: Vec<(PathBuf, String)>,
}

impl CardDatabase {
    pub fn load(root: &Path) -> Result<Self, ParseError> {
        let mut cards = HashMap::new();
        let mut face_index = HashMap::new();
        let mut errors = Vec::new();

        for entry in WalkDir::new(root)
            .into_iter()
            .filter_entry(|e| {
                e.depth() == 0
                    || !e.file_name().to_str().is_some_and(|s| s.starts_with('.'))
            })
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("txt") {
                continue;
            }

            let content = match std::fs::read_to_string(path) {
                Ok(c) => c,
                Err(e) => {
                    errors.push((path.to_path_buf(), e.to_string()));
                    continue;
                }
            };

            match parse_card_file(&content) {
                Ok(card_rules) => {
                    // Index each face
                    for face in layout_faces(&card_rules.layout) {
                        face_index.insert(face.name.to_lowercase(), face.clone());
                    }
                    // Index the card by primary name
                    let name_key = card_rules.name().to_lowercase();
                    cards.insert(name_key, card_rules);
                }
                Err(e) => {
                    errors.push((path.to_path_buf(), e.to_string()));
                }
            }
        }

        Ok(Self { cards, face_index, errors })
    }

    pub fn get_by_name(&self, name: &str) -> Option<&CardRules> {
        self.cards.get(&name.to_lowercase())
    }

    pub fn get_face_by_name(&self, name: &str) -> Option<&CardFace> {
        self.face_index.get(&name.to_lowercase())
    }

    pub fn card_count(&self) -> usize {
        self.cards.len()
    }

    pub fn errors(&self) -> &[(PathBuf, String)] {
        &self.errors
    }
}

fn layout_faces(layout: &CardLayout) -> Vec<&CardFace> {
    match layout {
        CardLayout::Single(face) => vec![face],
        CardLayout::Split(a, b)
        | CardLayout::Flip(a, b)
        | CardLayout::Transform(a, b)
        | CardLayout::Meld(a, b)
        | CardLayout::Adventure(a, b)
        | CardLayout::Modal(a, b)
        | CardLayout::Omen(a, b) => vec![a, b],
        CardLayout::Specialize(base, variants) => {
            let mut faces = vec![base];
            faces.extend(variants);
            faces
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn create_card_file(dir: &Path, name: &str, content: &str) {
        let file_path = dir.join(format!("{}.txt", name));
        fs::write(file_path, content).unwrap();
    }

    fn lightning_bolt_content() -> &'static str {
        "Name:Lightning Bolt\nManaCost:R\nTypes:Instant\nA:SP$ DealDamage | Cost$ R | NumDmg$ 3\nOracle:Lightning Bolt deals 3 damage to any target."
    }

    fn grizzly_bears_content() -> &'static str {
        "Name:Grizzly Bears\nManaCost:1 G\nTypes:Creature Bear\nPT:2/2\nOracle:No flavor text."
    }

    fn bonecrusher_giant_content() -> &'static str {
        "Name:Bonecrusher Giant\nManaCost:2 R\nTypes:Creature Giant\nPT:4/3\nOracle:Trample\nALTERNATE\nName:Stomp\nManaCost:1 R\nTypes:Instant Adventure\nA:SP$ DealDamage | Cost$ 1 R | NumDmg$ 2\nAlternateMode:Adventure\nOracle:Deal 2 damage to any target."
    }

    fn fire_ice_content() -> &'static str {
        "Name:Fire\nManaCost:1 R\nTypes:Instant\nA:SP$ DealDamage | Cost$ 1 R | NumDmg$ 2\nOracle:Fire deals 2 damage.\nALTERNATE\nName:Ice\nManaCost:1 U\nTypes:Instant\nA:SP$ Tap | Cost$ 1 U\nAlternateMode:Split\nOracle:Tap target permanent."
    }

    #[test]
    fn load_directory_with_multiple_cards() {
        let tmp = tempfile::tempdir().unwrap();
        create_card_file(tmp.path(), "lightning_bolt", lightning_bolt_content());
        create_card_file(tmp.path(), "grizzly_bears", grizzly_bears_content());
        create_card_file(tmp.path(), "bonecrusher_giant", bonecrusher_giant_content());

        let db = CardDatabase::load(tmp.path()).unwrap();
        assert_eq!(db.card_count(), 3);
    }

    #[test]
    fn case_insensitive_name_lookup() {
        let tmp = tempfile::tempdir().unwrap();
        create_card_file(tmp.path(), "lightning_bolt", lightning_bolt_content());

        let db = CardDatabase::load(tmp.path()).unwrap();

        assert!(db.get_by_name("Lightning Bolt").is_some());
        assert!(db.get_by_name("lightning bolt").is_some());
        assert!(db.get_by_name("LIGHTNING BOLT").is_some());
        assert!(db.get_by_name("Nonexistent Card").is_none());
    }

    #[test]
    fn face_level_lookup_adventure() {
        let tmp = tempfile::tempdir().unwrap();
        create_card_file(tmp.path(), "bonecrusher_giant", bonecrusher_giant_content());

        let db = CardDatabase::load(tmp.path()).unwrap();

        // Main face lookup
        let main = db.get_face_by_name("Bonecrusher Giant").unwrap();
        assert_eq!(main.name, "Bonecrusher Giant");

        // Adventure face lookup
        let adv = db.get_face_by_name("Stomp").unwrap();
        assert_eq!(adv.name, "Stomp");

        // Case insensitive
        assert!(db.get_face_by_name("stomp").is_some());
        assert!(db.get_face_by_name("STOMP").is_some());
    }

    #[test]
    fn face_level_lookup_split() {
        let tmp = tempfile::tempdir().unwrap();
        create_card_file(tmp.path(), "fire_ice", fire_ice_content());

        let db = CardDatabase::load(tmp.path()).unwrap();

        let fire = db.get_face_by_name("Fire").unwrap();
        assert_eq!(fire.name, "Fire");

        let ice = db.get_face_by_name("Ice").unwrap();
        assert_eq!(ice.name, "Ice");
    }

    #[test]
    fn parse_errors_dont_prevent_other_cards() {
        let tmp = tempfile::tempdir().unwrap();
        create_card_file(tmp.path(), "lightning_bolt", lightning_bolt_content());
        create_card_file(tmp.path(), "malformed", "This is not a valid card file at all");

        let db = CardDatabase::load(tmp.path()).unwrap();

        // Lightning Bolt should still load
        assert!(db.get_by_name("Lightning Bolt").is_some());
        // One error from malformed file
        assert_eq!(db.errors().len(), 1);
        // Only one card loaded successfully
        assert_eq!(db.card_count(), 1);
    }

    #[test]
    fn empty_directory_returns_empty_database() {
        let tmp = tempfile::tempdir().unwrap();
        let db = CardDatabase::load(tmp.path()).unwrap();

        assert_eq!(db.card_count(), 0);
        assert!(db.errors().is_empty());
    }

    #[test]
    fn recursive_directory_loading() {
        let tmp = tempfile::tempdir().unwrap();
        let subdir = tmp.path().join("subfolder");
        fs::create_dir(&subdir).unwrap();

        create_card_file(tmp.path(), "lightning_bolt", lightning_bolt_content());
        create_card_file(&subdir, "grizzly_bears", grizzly_bears_content());

        let db = CardDatabase::load(tmp.path()).unwrap();
        assert_eq!(db.card_count(), 2);
        assert!(db.get_by_name("Lightning Bolt").is_some());
        assert!(db.get_by_name("Grizzly Bears").is_some());
    }

    #[test]
    fn skips_non_txt_files() {
        let tmp = tempfile::tempdir().unwrap();
        create_card_file(tmp.path(), "lightning_bolt", lightning_bolt_content());
        fs::write(tmp.path().join("readme.md"), "# Not a card").unwrap();
        fs::write(tmp.path().join("data.json"), "{}").unwrap();

        let db = CardDatabase::load(tmp.path()).unwrap();
        assert_eq!(db.card_count(), 1);
    }

    #[test]
    fn skips_dotfiles_and_dot_directories() {
        let tmp = tempfile::tempdir().unwrap();
        create_card_file(tmp.path(), "lightning_bolt", lightning_bolt_content());

        // Create a dotfile
        fs::write(tmp.path().join(".hidden.txt"), lightning_bolt_content()).unwrap();

        // Create a dot directory with a card
        let dotdir = tmp.path().join(".git");
        fs::create_dir(&dotdir).unwrap();
        create_card_file(&dotdir, "hidden_card", grizzly_bears_content());

        let db = CardDatabase::load(tmp.path()).unwrap();
        assert_eq!(db.card_count(), 1);
    }

    #[test]
    #[ignore]
    fn load_real_forge_cards() {
        let forge_path = Path::new("../forge/forge-gui/res/cardsfolder/");
        if !forge_path.exists() {
            eprintln!("Forge card directory not found at {:?}, skipping", forge_path);
            return;
        }

        let db = CardDatabase::load(forge_path).unwrap();
        eprintln!("Loaded {} cards with {} errors", db.card_count(), db.errors().len());

        // Print first few errors for debugging
        for (path, err) in db.errors().iter().take(5) {
            eprintln!("  Error in {:?}: {}", path, err);
        }
    }
}
