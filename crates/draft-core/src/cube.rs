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
        let name = name.trim();
        if count == 0 || name.is_empty() {
            errors.push(CubeImportError::InvalidLine { line: idx + 1 });
            continue;
        }

        entries.push(CubeListEntry {
            name: name.to_string(),
            count,
        });
    }

    if errors.is_empty() {
        Ok(entries)
    } else {
        Err(errors)
    }
}

pub fn cube_cards_from_entries(
    entries: &[CubeListEntry],
    db: &CardDatabase,
) -> Result<Vec<DraftCardInstance>, Vec<CubeImportError>> {
    let mut cards = Vec::new();
    let mut errors = Vec::new();

    for entry in entries {
        let Some(face) = db.get_face_by_name(&entry.name) else {
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
