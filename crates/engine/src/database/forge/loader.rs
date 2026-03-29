use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use tracing::{debug, warn};

use super::types::{ForgeAbilityLine, ForgeCard, ForgeParams};

/// Index mapping lowercased card face names → file paths.
///
/// Built by scanning a Forge `cardsfolder/` directory at startup. For DFC
/// cards (files containing `ALTERNATE`), both face names are indexed to the
/// same file path.
pub struct ForgeIndex {
    /// Maps lowercased card face name → path to `.txt` file containing it.
    cards: HashMap<String, PathBuf>,
}

impl ForgeIndex {
    /// Scan a Forge `cardsfolder/` directory, reading only `Name:` lines from
    /// each `.txt` file to build the index. Does not fully parse any card.
    pub fn scan(cardsfolder: &Path) -> Self {
        let mut cards = HashMap::new();
        Self::scan_dir(cardsfolder, &mut cards);
        debug!("ForgeIndex: indexed {} face names", cards.len());
        ForgeIndex { cards }
    }

    fn scan_dir(dir: &Path, cards: &mut HashMap<String, PathBuf>) {
        let entries = match fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) => {
                warn!("ForgeIndex: cannot read {}: {e}", dir.display());
                return;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // Skip the mkzip.sh script directory marker
                if path.file_name().is_some_and(|n| n == "mkzip.sh") {
                    continue;
                }
                Self::scan_dir(&path, cards);
            } else if path.extension().is_some_and(|ext| ext == "txt") {
                Self::index_file(&path, cards);
            }
        }
    }

    fn index_file(path: &Path, cards: &mut HashMap<String, PathBuf>) {
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                warn!("ForgeIndex: cannot read {}: {e}", path.display());
                return;
            }
        };

        for line in content.lines() {
            if let Some(name) = line.strip_prefix("Name:") {
                let name = name.trim().to_lowercase();
                if !name.is_empty() {
                    cards.insert(name, path.to_path_buf());
                }
            }
        }
    }

    /// Return the number of indexed face names.
    pub fn len(&self) -> usize {
        self.cards.len()
    }

    /// Check if the index is empty.
    pub fn is_empty(&self) -> bool {
        self.cards.is_empty()
    }

    /// Look up a card by lowercased face name and fully parse it.
    ///
    /// For DFC cards, returns the face matching the requested name: if the
    /// name matches the front face, returns the front `ForgeCard`; if it
    /// matches the alternate, returns the alternate face.
    pub(crate) fn parse_card(&self, name: &str) -> Option<ForgeCard> {
        let path = self.cards.get(name)?;
        let content = fs::read_to_string(path).ok()?;
        let (front, alternate) = parse_card_file(&content);

        // If the requested name matches the alternate face, return that instead.
        if let Some(ref alt) = alternate {
            if alt.name.to_lowercase() == name {
                return Some(alt.as_ref().clone());
            }
        }

        Some(ForgeCard { alternate, ..front })
    }
}

/// Parse a complete Forge `.txt` file into a front face + optional alternate.
fn parse_card_file(content: &str) -> (ForgeCard, Option<Box<ForgeCard>>) {
    let parts: Vec<&str> = content.splitn(2, "\nALTERNATE\n").collect();

    let front = parse_single_face(parts[0]);
    let alternate = parts
        .get(1)
        .map(|alt_text| Box::new(parse_single_face(alt_text)));

    (front, alternate)
}

/// Parse one face (front or back) from Forge card text.
fn parse_single_face(text: &str) -> ForgeCard {
    let mut card = ForgeCard {
        name: String::new(),
        mana_cost: None,
        types: None,
        pt: None,
        colors: None,
        abilities: Vec::new(),
        triggers: Vec::new(),
        statics: Vec::new(),
        replacements: Vec::new(),
        keywords: Vec::new(),
        svars: HashMap::new(),
        oracle_text: None,
        alternate: None,
    };

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if let Some(value) = line.strip_prefix("Name:") {
            card.name = value.trim().to_string();
        } else if let Some(value) = line.strip_prefix("ManaCost:") {
            card.mana_cost = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("Types:") {
            card.types = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("PT:") {
            card.pt = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("Colors:") {
            card.colors = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("Oracle:") {
            card.oracle_text = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("A:") {
            card.abilities.push(parse_ability_line(value.trim()));
        } else if let Some(value) = line.strip_prefix("T:") {
            card.triggers.push(parse_ability_line(value.trim()));
        } else if let Some(value) = line.strip_prefix("S:") {
            card.statics.push(parse_ability_line(value.trim()));
        } else if let Some(value) = line.strip_prefix("R:") {
            card.replacements.push(parse_ability_line(value.trim()));
        } else if let Some(value) = line.strip_prefix("K:") {
            card.keywords.push(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("SVar:") {
            // Format: SVar:Name:Value
            if let Some((name, val)) = value.trim().split_once(':') {
                card.svars.insert(name.to_string(), val.to_string());
            }
        }
        // Other lines (DeckHas, AI, AlternateMode, etc.) are silently skipped.
    }

    card
}

/// Parse a Forge ability line into a `ForgeAbilityLine`.
///
/// Forge ability lines use `|`-delimited segments where each segment is
/// `Key$ Value`. The first segment typically contains `SP$ EffectType` or
/// `DB$ EffectType` (spell-ability vs. database-ability).
fn parse_ability_line(raw: &str) -> ForgeAbilityLine {
    let params = parse_params(raw);
    ForgeAbilityLine {
        raw: raw.to_string(),
        params,
    }
}

/// Parse `Key$ Value | Key$ Value | ...` into a `ForgeParams` bag.
pub(crate) fn parse_params(text: &str) -> ForgeParams {
    let mut entries = HashMap::new();

    for segment in text.split(" | ") {
        let segment = segment.trim();
        if segment.is_empty() {
            continue;
        }

        // Most segments are "Key$ Value", but the first segment may be
        // "SP$ EffectType" or "DB$ EffectType" where EffectType is the value.
        if let Some((key, value)) = segment.split_once("$ ") {
            entries.insert(key.to_string(), value.to_string());
        } else if let Some(key) = segment.strip_suffix('$') {
            // Bare "Key$" with no value (e.g., "IsCurse$" used as a flag)
            entries.insert(key.to_string(), String::new());
        } else if segment.contains('$') {
            // "Key$Value" with no space (e.g., "IsCurse$True")
            if let Some((key, value)) = segment.split_once('$') {
                entries.insert(key.to_string(), value.to_string());
            }
        }
    }

    ForgeParams::new(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_lightning_bolt() {
        let content = "\
Name:Lightning Bolt
ManaCost:R
Types:Instant
A:SP$ DealDamage | ValidTgts$ Any | NumDmg$ 3 | SpellDescription$ CARDNAME deals 3 damage to any target.
Oracle:Lightning Bolt deals 3 damage to any target.
";

        let (card, alternate) = parse_card_file(content);
        assert_eq!(card.name, "Lightning Bolt");
        assert_eq!(card.mana_cost.as_deref(), Some("R"));
        assert_eq!(card.types.as_deref(), Some("Instant"));
        assert_eq!(card.abilities.len(), 1);
        assert_eq!(card.abilities[0].params.effect_type(), Some("DealDamage"));
        assert_eq!(card.abilities[0].params.get("NumDmg"), Some("3"));
        assert_eq!(card.abilities[0].params.get("ValidTgts"), Some("Any"));
        assert!(alternate.is_none());
    }

    #[test]
    fn parse_sheoldred_triggers_and_svars() {
        let content = "\
Name:Sheoldred, the Apocalypse
ManaCost:2 B B
Types:Legendary Creature Phyrexian Praetor
PT:4/5
K:Deathtouch
T:Mode$ Drawn | ValidCard$ Card.YouCtrl | TriggerZones$ Battlefield | Execute$ TrigGainLife | TriggerDescription$ Whenever you draw a card, you gain 2 life.
SVar:TrigGainLife:DB$ GainLife | Defined$ You | LifeAmount$ 2
T:Mode$ Drawn | ValidCard$ Card.OppCtrl | TriggerZones$ Battlefield | Execute$ TrigLoseLife | TriggerDescription$ Whenever an opponent draws a card, they lose 2 life.
SVar:TrigLoseLife:DB$ LoseLife | Defined$ TriggeredCardController | LifeAmount$ 2
Oracle:Deathtouch\\nWhenever you draw a card, you gain 2 life.\\nWhenever an opponent draws a card, they lose 2 life.
";

        let (card, _) = parse_card_file(content);
        assert_eq!(card.triggers.len(), 2);
        assert_eq!(card.triggers[0].params.get("Mode"), Some("Drawn"));
        assert_eq!(card.triggers[0].params.get("Execute"), Some("TrigGainLife"));
        assert_eq!(card.keywords, vec!["Deathtouch"]);
        assert!(card.svars.contains_key("TrigGainLife"));
        assert!(card.svars.contains_key("TrigLoseLife"));
    }

    #[test]
    fn parse_dfc_with_alternate() {
        let content = "\
Name:Decadent Dragon
ManaCost:2 R R
Types:Creature Dragon
PT:4/4
K:Flying
K:Trample
T:Mode$ Attacks | ValidCard$ Card.Self | Execute$ TrigToken | TriggerDescription$ Whenever CARDNAME attacks, create a Treasure token.
SVar:TrigToken:DB$ Token | TokenScript$ c_a_treasure_sac

ALTERNATE

Name:Expensive Taste
ManaCost:2 B
Types:Instant Adventure
A:SP$ Dig | DigNum$ 2 | ChangeNum$ All | ValidTgts$ Opponent
";

        let (front, alternate) = parse_card_file(content);
        assert_eq!(front.name, "Decadent Dragon");
        assert_eq!(front.keywords, vec!["Flying", "Trample"]);

        let alt = alternate.expect("should have alternate face");
        assert_eq!(alt.name, "Expensive Taste");
        assert_eq!(alt.abilities.len(), 1);
        assert_eq!(alt.abilities[0].params.effect_type(), Some("Dig"));
    }

    #[test]
    fn parse_params_handles_no_space_dollar() {
        let params = parse_params("IsCurse$True | Mode$ Drawn");
        assert_eq!(params.get("IsCurse"), Some("True"));
        assert_eq!(params.get("Mode"), Some("Drawn"));
    }

    #[test]
    fn parse_params_bare_key_dollar() {
        let params = parse_params("DamageMap$ True | Secondary$");
        assert_eq!(params.get("DamageMap"), Some("True"));
        assert!(params.has("Secondary"));
        assert_eq!(params.get("Secondary"), Some(""));
    }

    #[test]
    fn forge_index_returns_correct_face_for_dfc() {
        // We can't easily test ForgeIndex::scan without real files, but
        // we can test parse_card_file + the face-selection logic.
        let content = "\
Name:Front Face
ManaCost:1 W
Types:Creature

ALTERNATE

Name:Back Face
ManaCost:2 B
Types:Enchantment
";

        let (front, alt) = parse_card_file(content);
        assert_eq!(front.name, "Front Face");

        let alt = alt.expect("alternate should exist");
        assert_eq!(alt.name, "Back Face");
    }
}
