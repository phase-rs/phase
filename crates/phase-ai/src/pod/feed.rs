//! Shared Commander feed loader.

use std::fmt;
use std::path::Path;

use engine::database::CardDatabase;
use engine::game::PlayerDeckList;

/// One Commander deck as the feed describes it, before name resolution.
#[derive(Debug, Clone)]
pub struct FeedDeck {
    pub label: String,
    pub list: PlayerDeckList,
}

/// A feed could not be read or did not have the expected shape.
#[derive(Debug)]
pub struct FeedError {
    message: String,
}

impl FeedError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for FeedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for FeedError {}

/// Loads Commander decks using either supported feed convention.
pub fn load_commander_decks(
    db: &CardDatabase,
    cards_root: &Path,
    feed: &str,
    max: Option<usize>,
) -> Result<(Vec<FeedDeck>, Vec<String>), FeedError> {
    let feed_path = cards_root.join(feed);
    let feed_file = std::fs::File::open(&feed_path).map_err(|error| {
        FeedError::new(format!("failed to open {}: {error}", feed_path.display()))
    })?;
    let feed_json: serde_json::Value = serde_json::from_reader(feed_file).map_err(|error| {
        FeedError::new(format!("failed to parse {}: {error}", feed_path.display()))
    })?;
    let decks = feed_json["decks"]
        .as_array()
        .ok_or_else(|| FeedError::new(format!("{} missing decks array", feed_path.display())))?;

    let mut loaded = Vec::new();
    let mut skipped = Vec::new();
    for deck in decks {
        if max.is_some_and(|limit| loaded.len() == limit) {
            break;
        }
        let label = deck["name"].as_str().unwrap_or("<unnamed>").to_string();
        let commander: Vec<String> = match deck["commander"].as_array() {
            Some(names) if !names.is_empty() => names
                .iter()
                .filter_map(|name| name.as_str().map(str::to_string))
                .collect(),
            Some(_) | None => vec![label.clone()],
        };
        let Some(primary) = commander.first() else {
            skipped.push(format!("{label}: commander list is empty"));
            continue;
        };
        if db.get_face_by_name(primary).is_none() {
            skipped.push(format!("{label}: commander '{primary}' not in card db"));
            continue;
        }
        let Some(entries) = deck["main"].as_array() else {
            skipped.push(format!("{label}: main deck is missing"));
            continue;
        };
        let mut main_deck = Vec::new();
        for entry in entries {
            let Some(name) = entry["name"].as_str() else {
                continue;
            };
            if commander.iter().any(|candidate| candidate == name) {
                continue;
            }
            let count = entry["count"].as_u64().unwrap_or(0) as usize;
            main_deck.extend(std::iter::repeat_n(name.to_string(), count));
        }
        loaded.push(FeedDeck {
            label,
            list: PlayerDeckList {
                main_deck,
                commander,
                ..Default::default()
            },
        });
    }
    Ok((loaded, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_db() -> CardDatabase {
        let card = |name: &str| {
            serde_json::json!({
                "name": name,
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": ["Legendary"], "core_types": ["Creature"], "subtypes": [] },
                "power": { "type": "Fixed", "value": 1 },
                "toughness": { "type": "Fixed", "value": 1 },
                "loyalty": null, "defense": null, "oracle_text": null, "non_ability_text": null,
                "flavor_name": null, "keywords": [], "abilities": [], "triggers": [],
                "static_abilities": [], "replacements": [], "color_override": null,
                "scryfall_oracle_id": null
            })
        };
        CardDatabase::from_json_str(
            &serde_json::json!({
                "commander x": card("Commander X"),
                "commander y": card("Commander Y"),
                "card z": card("Card Z")
            })
            .to_string(),
        )
        .expect("fixture parses")
    }

    #[test]
    fn feed_loader_handles_both_conventions() {
        let dir = std::env::temp_dir().join(format!("phase_pod_feed_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create fixture directory");
        let path = dir.join("feed.json");
        std::fs::write(
            &path,
            serde_json::json!({ "decks": [
                { "name": "Precon", "commander": ["Commander X"], "main": [{"name":"Card Z","count":2}] },
                { "name": "Commander Y", "main": [{"name":"Commander Y","count":1},{"name":"Card Z","count":1}] }
            ]})
            .to_string(),
        )
        .expect("write fixture");
        let (decks, skipped) =
            load_commander_decks(&fixture_db(), &dir, "feed.json", None).expect("load feed");
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_dir(dir);
        assert!(skipped.is_empty());
        assert_eq!(decks.len(), 2);
        assert_eq!(decks[0].list.commander, vec!["Commander X".to_string()]);
        assert_eq!(decks[1].list.commander, vec!["Commander Y".to_string()]);
        assert_eq!(decks[1].list.main_deck, vec!["Card Z".to_string()]);
    }
}
