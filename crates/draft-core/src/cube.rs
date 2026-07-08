use rand::seq::SliceRandom;
use serde::Serialize;

use engine::database::CardDatabase;
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::mana::ManaColor;

use crate::pack_source::PackSource;
use crate::types::{DeckAddableCards, DraftCardInstance, DraftConfig, DraftError, DraftPack};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CubeListEntry {
    pub name: String,
    pub count: u32,
    /// Optional Scryfall oracle id, carried by CubeCobra-fetched lists as a
    /// trailing `[oracle-id]` annotation. Used as a resolution fallback when the
    /// source's cached name no longer matches the printed name (e.g. a cube
    /// snapshotted under a set's pre-reveal placeholder names). Manual paste
    /// lists omit it.
    pub oracle_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize)]
pub enum CubeImportError {
    #[error("line {line}: expected '<count> <card name>'")]
    InvalidLine { line: usize },
    #[error("card not found: {name}")]
    UnknownCard { name: String },
}

pub fn parse_cube_list(text: &str) -> Result<Vec<CubeListEntry>, Vec<CubeImportError>> {
    let mut entries = Vec::new();
    let mut errors = Vec::new();

    for (idx, raw_line) in text.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }

        let Some((count_text, name)) = line.split_once(char::is_whitespace) else {
            errors.push(CubeImportError::InvalidLine { line: idx + 1 });
            continue;
        };
        let Ok(count) = count_text.parse::<u32>() else {
            errors.push(CubeImportError::InvalidLine { line: idx + 1 });
            continue;
        };
        let (name, oracle_id) = split_oracle_id(name.trim());
        if count == 0 || name.is_empty() {
            errors.push(CubeImportError::InvalidLine { line: idx + 1 });
            continue;
        }

        entries.push(CubeListEntry {
            name: name.to_string(),
            count,
            oracle_id,
        });
    }

    if errors.is_empty() {
        Ok(entries)
    } else {
        Err(errors)
    }
}

/// Split an optional trailing `[oracle-id]` annotation off a cube line's name.
/// CubeCobra-fetched lists append each card's Scryfall oracle id so imports stay
/// resilient when the source's cached name drifts from the printed name; manual
/// paste lists omit it. Only a bracketed UUID-shaped token is treated as an id,
/// so card names that happen to end in brackets are left intact.
fn split_oracle_id(name: &str) -> (&str, Option<String>) {
    if let Some(without_close) = name.strip_suffix(']') {
        if let Some((before, candidate)) = without_close.rsplit_once('[') {
            if is_oracle_id(candidate) {
                return (before.trim_end(), Some(candidate.to_string()));
            }
        }
    }
    (name, None)
}

/// A Scryfall oracle id is a UUID: ASCII hex digits and hyphens, with at least
/// one hyphen. This guard distinguishes ids from any incidental `[...]` suffix
/// in a real card name.
fn is_oracle_id(candidate: &str) -> bool {
    candidate.contains('-') && candidate.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

pub fn cube_cards_from_entries(
    entries: &[CubeListEntry],
    db: &CardDatabase,
) -> Result<Vec<DraftCardInstance>, Vec<CubeImportError>> {
    let mut cards = Vec::new();
    let mut errors = Vec::new();

    for entry in entries {
        // Name first (stable for the vast majority and works for manual paste
        // lists), oracle id as a fallback for names the source cached wrong.
        let face = db.get_face_by_name(&entry.name).or_else(|| {
            entry
                .oracle_id
                .as_deref()
                .and_then(|oracle_id| db.get_face_by_oracle_id(oracle_id))
        });
        let Some(face) = face else {
            errors.push(CubeImportError::UnknownCard {
                name: entry.name.clone(),
            });
            continue;
        };

        for copy in 0..entry.count {
            cards.push(card_instance_from_face(face, cards.len(), copy));
        }
    }

    if errors.is_empty() {
        Ok(cards)
    } else {
        Err(errors)
    }
}

pub fn resolve_addable_cards(
    addable_cards: &DeckAddableCards,
    db: &CardDatabase,
) -> Result<DeckAddableCards, Vec<CubeImportError>> {
    let mut resolved = addable_cards.clone();
    let mut custom = Vec::with_capacity(addable_cards.custom.len());
    let mut errors = Vec::new();

    for name in &addable_cards.custom {
        match db.get_face_by_name(name) {
            Some(face) => custom.push(face.name.clone()),
            None => errors.push(CubeImportError::UnknownCard { name: name.clone() }),
        }
    }

    custom.sort();
    custom.dedup();
    resolved.custom = custom;

    if errors.is_empty() {
        Ok(resolved)
    } else {
        Err(errors)
    }
}

fn card_instance_from_face(face: &CardFace, index: usize, copy: u32) -> DraftCardInstance {
    DraftCardInstance {
        instance_id: format!("cube-source-{index}-{copy}"),
        name: face.name.clone(),
        set_code: "CUBE".to_string(),
        collector_number: format!("{}", index + 1),
        rarity: "cube".to_string(),
        colors: face.color_identity.iter().map(mana_color_letter).collect(),
        cmc: face.mana_cost.mana_value().min(u32::from(u8::MAX)) as u8,
        type_line: type_line(face),
    }
}

fn mana_color_letter(color: &ManaColor) -> String {
    match color {
        ManaColor::White => "W",
        ManaColor::Blue => "U",
        ManaColor::Black => "B",
        ManaColor::Red => "R",
        ManaColor::Green => "G",
    }
    .to_string()
}

fn type_line(face: &CardFace) -> String {
    let core = face
        .card_type
        .core_types
        .iter()
        .map(core_type_name)
        .collect::<Vec<_>>()
        .join(" ");
    if face.card_type.subtypes.is_empty() {
        core
    } else {
        format!("{} — {}", core, face.card_type.subtypes.join(" "))
    }
}

fn core_type_name(core_type: &CoreType) -> &'static str {
    match core_type {
        CoreType::Artifact => "Artifact",
        CoreType::Battle => "Battle",
        CoreType::Creature => "Creature",
        CoreType::Dungeon => "Dungeon",
        CoreType::Enchantment => "Enchantment",
        CoreType::Instant => "Instant",
        CoreType::Kindred => "Kindred",
        CoreType::Land => "Land",
        CoreType::Plane => "Plane",
        CoreType::Phenomenon => "Phenomenon",
        CoreType::Scheme => "Scheme",
        CoreType::Conspiracy => "Conspiracy",
        CoreType::Planeswalker => "Planeswalker",
        CoreType::Sorcery => "Sorcery",
        CoreType::Tribal => "Tribal",
    }
}

pub struct CubePackSource {
    cards: Vec<DraftCardInstance>,
}

impl CubePackSource {
    pub fn new(cards: Vec<DraftCardInstance>) -> Self {
        Self { cards }
    }
}

impl PackSource for CubePackSource {
    fn generate_pack(
        &self,
        _rng: &mut dyn rand::RngCore,
        _seat: u8,
        _pack_number: u8,
    ) -> DraftPack {
        DraftPack(Vec::new())
    }

    fn generate_packs(
        &self,
        rng: &mut dyn rand::RngCore,
        config: &DraftConfig,
        seat_count: u8,
    ) -> Result<Vec<Vec<DraftPack>>, DraftError> {
        let required =
            seat_count as usize * config.pack_count as usize * config.cards_per_pack as usize;
        if self.cards.len() < required {
            return Err(DraftError::InsufficientCards {
                available: self.cards.len(),
                required,
            });
        }

        let mut cards = self.cards.clone();
        cards.shuffle(rng);

        let mut packs = vec![Vec::with_capacity(config.pack_count as usize); seat_count as usize];
        let mut cursor = 0;
        for pack_number in 0..config.pack_count {
            for seat in 0..seat_count {
                let mut pack_cards = Vec::with_capacity(config.cards_per_pack as usize);
                for card_index in 0..config.cards_per_pack {
                    let mut card = cards[cursor].clone();
                    card.instance_id = format!("cube-{seat}-{pack_number}-{card_index}");
                    card.collector_number = format!("{}", cursor + 1);
                    pack_cards.push(card);
                    cursor += 1;
                }
                packs[seat as usize].push(DraftPack(pack_cards));
            }
        }

        Ok(packs)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    use super::*;
    use crate::types::{
        DeckAddableCards, DraftKind, DraftSource, PodPolicy, SpectatorVisibility, TournamentFormat,
    };

    #[test]
    fn parses_counted_cube_list() {
        let entries = parse_cube_list("1 Lightning Bolt\n2 Island\n").unwrap();
        assert_eq!(entries[0].name, "Lightning Bolt");
        assert_eq!(entries[0].count, 1);
        assert_eq!(entries[1].name, "Island");
        assert_eq!(entries[1].count, 2);
    }

    #[test]
    fn cube_pack_source_deals_without_replacement() {
        let cards: Vec<DraftCardInstance> = (0..8)
            .map(|i| DraftCardInstance {
                instance_id: format!("source-{i}"),
                name: format!("Card {i}"),
                set_code: "CUBE".to_string(),
                collector_number: format!("{i}"),
                rarity: "cube".to_string(),
                colors: Vec::new(),
                cmc: 0,
                type_line: String::new(),
            })
            .collect();
        let source = CubePackSource::new(cards);
        let config = DraftConfig {
            source: DraftSource::Cube {
                id: "cube".to_string(),
                name: "Cube".to_string(),
            },
            set_code: "cube".to_string(),
            kind: DraftKind::Quick,
            pod_size: 2,
            cards_per_pack: 2,
            pack_count: 2,
            min_deck_size: 4,
            addable_cards: DeckAddableCards::standard_basics(),
            rng_seed: 1,
            tournament_format: TournamentFormat::Swiss,
            pod_policy: PodPolicy::Competitive,
            spectator_visibility: SpectatorVisibility::Public,
        };
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        let packs = source.generate_packs(&mut rng, &config, 2).unwrap();
        let names: Vec<String> = packs
            .iter()
            .flat_map(|seat| seat.iter())
            .flat_map(|pack| pack.0.iter())
            .map(|card| card.name.clone())
            .collect();
        let unique: HashSet<String> = names.iter().cloned().collect();
        assert_eq!(names.len(), 8);
        assert_eq!(unique.len(), 8);
    }

    #[test]
    fn parses_oracle_id_annotation() {
        let entries = parse_cube_list(
            "1 Spider-Woman, Stunning Savior [be2b9c6d-4ecb-49ec-b276-4aa93c5dfc00]\n\
             2 Island\n\
             1 Weird Card [not-a-uuid!]\n",
        )
        .unwrap();
        // Annotated line: id stripped from the name, captured separately.
        assert_eq!(entries[0].name, "Spider-Woman, Stunning Savior");
        assert_eq!(
            entries[0].oracle_id.as_deref(),
            Some("be2b9c6d-4ecb-49ec-b276-4aa93c5dfc00")
        );
        // Plain manual line: no id.
        assert_eq!(entries[1].name, "Island");
        assert_eq!(entries[1].oracle_id, None);
        // A name that merely ends in brackets (non-UUID) is left intact.
        assert_eq!(entries[2].name, "Weird Card [not-a-uuid!]");
        assert_eq!(entries[2].oracle_id, None);
    }

    #[test]
    fn cube_cards_resolve_by_oracle_id_when_name_is_stale() {
        // A card present under its printed name, carrying a known oracle id.
        let mut map = serde_json::Map::new();
        map.insert(
            "spider-woman, stunning savior".to_string(),
            serde_json::json!({
                "name": "Spider-Woman, Stunning Savior",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": ["Legendary"], "core_types": ["Creature"], "subtypes": ["Spider", "Hero"] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": "be2b9c6d-4ecb-49ec-b276-4aa93c5dfc00", "legalities": {}
            }),
        );
        let db = CardDatabase::from_json_str(&serde_json::to_string(&map).unwrap()).unwrap();

        // The import names the card by a stale pre-reveal placeholder that no
        // longer exists, but carries the correct oracle id.
        let entries = vec![CubeListEntry {
            name: "Makdee and Itla, Skysnarers".to_string(),
            count: 1,
            oracle_id: Some("be2b9c6d-4ecb-49ec-b276-4aa93c5dfc00".to_string()),
        }];
        let cards = cube_cards_from_entries(&entries, &db).unwrap();
        assert_eq!(cards.len(), 1);
        // Resolved to the real printed card via the oracle-id fallback.
        assert_eq!(cards[0].name, "Spider-Woman, Stunning Savior");
    }

    #[test]
    fn cube_cards_unknown_name_without_oracle_id_still_errors() {
        let db = CardDatabase::from_json_str("{}").unwrap();
        let entries = vec![CubeListEntry {
            name: "Not A Card".to_string(),
            count: 1,
            oracle_id: None,
        }];
        let errors = cube_cards_from_entries(&entries, &db).unwrap_err();
        assert!(matches!(
            &errors[0],
            CubeImportError::UnknownCard { name } if name == "Not A Card"
        ));
    }

    #[test]
    fn resolve_addable_cards_reports_unknown_custom_card() {
        let db = CardDatabase::from_json_str("{}").unwrap();
        let addable = DeckAddableCards {
            policy: crate::types::DeckAddableCardPolicy::CustomOnly,
            custom: vec!["Not A Card".to_string()],
        };
        let errors = resolve_addable_cards(&addable, &db).unwrap_err();
        assert!(matches!(
            &errors[0],
            CubeImportError::UnknownCard { name } if name == "Not A Card"
        ));
    }
}
