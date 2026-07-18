use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

use serde::Deserialize;

use crate::database::synthesis::{
    merge_extracted_keywords, parse_oracle_with_cleave_brackets, synthesize_all,
};
use crate::game::game_object::GameObject;
use crate::types::ability::{ContinuousModification, TargetFilter};
use crate::types::card::CardFace;
use crate::types::card_type::CardType;
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::keywords::Keyword;
use crate::types::layers::{ActiveContinuousEffect, Layer};
use crate::types::mana::ManaCost;
use crate::types::player::{PlayerCounterKind, PlayerId};
use crate::types::stickers::{AppliedSticker, StickerKind, StickerLocator};
use crate::types::zones::Zone;

#[derive(Debug, Clone, Deserialize)]
struct StickerSheetSpec {
    name: String,
    names: Vec<String>,
    abilities: Vec<AbilityStickerSpec>,
    power_toughness: Vec<PowerToughnessStickerSpec>,
}

#[derive(Debug, Clone, Deserialize)]
struct AbilityStickerSpec {
    cost: u8,
    text: String,
}

#[derive(Debug, Clone, Deserialize)]
struct PowerToughnessStickerSpec {
    cost: u8,
    power: i32,
    toughness: i32,
}

#[derive(Debug, Clone)]
pub struct StickerCandidate {
    pub sticker: AppliedSticker,
    pub pay_ticket: bool,
    pub description: String,
}

static STICKER_SHEETS: OnceLock<Vec<StickerSheetSpec>> = OnceLock::new();

fn sticker_sheets() -> &'static [StickerSheetSpec] {
    STICKER_SHEETS.get_or_init(|| {
        serde_json::from_str(include_str!("stickers_data.json"))
            .expect("sticker sheet registry must deserialize")
    })
}

fn canonical_sheet_name(name: &str) -> Option<String> {
    sticker_sheets()
        .iter()
        .find(|sheet| sheet.name.eq_ignore_ascii_case(name))
        .map(|sheet| sheet.name.clone())
}

pub fn normalize_selected_sheets(names: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();
    for name in names {
        let Some(canonical) = canonical_sheet_name(name) else {
            continue;
        };
        let lower = canonical.to_ascii_lowercase();
        if seen.insert(lower) {
            normalized.push(canonical);
        }
    }
    normalized.truncate(3);
    normalized
}

pub fn set_player_sticker_sheets(state: &mut GameState, player: PlayerId, names: &[String]) {
    if let Some(entry) = state.players.iter_mut().find(|p| p.id == player) {
        entry.sticker_sheets = normalize_selected_sheets(names);
    }
}

pub fn zone_retains_stickers(zone: Zone) -> bool {
    matches!(
        zone,
        Zone::Battlefield | Zone::Graveyard | Zone::Exile | Zone::Stack | Zone::Command
    )
}

pub fn object_is_stickered(obj: &GameObject) -> bool {
    !obj.stickers.is_empty()
}

pub fn object_has_sticker_kind(obj: &GameObject, kind: StickerKind) -> bool {
    obj.stickers.iter().any(|sticker| sticker.kind() == kind)
}

pub fn rebuild_public_zone_stickers(obj: &mut GameObject) {
    obj.revert_layered_characteristics_to_base();
    apply_name_stickers_from_base(obj);
    apply_ability_stickers(obj);
    apply_power_toughness_stickers(obj);
}

pub fn apply_battlefield_name_and_ability_stickers(
    state: &mut GameState,
    battlefield: &[ObjectId],
) -> bool {
    let mut has_ability_sticker = false;
    for &id in battlefield {
        let Some(obj) = state.objects.get_mut(&id) else {
            continue;
        };
        has_ability_sticker |= obj
            .stickers
            .iter()
            .any(|sticker| matches!(sticker, AppliedSticker::Ability { .. }));
        apply_name_stickers_from_current(obj);
        apply_ability_stickers(obj);
    }
    has_ability_sticker
}

pub fn append_battlefield_pt_sticker_effects(
    state: &GameState,
    effects: &mut [(Layer, Vec<ActiveContinuousEffect>)],
) {
    let Some((_, bucket)) = effects.iter_mut().find(|(layer, _)| *layer == Layer::SetPT) else {
        return;
    };

    for id in state.battlefield_phased_in_ids() {
        let Some(obj) = state.objects.get(&id) else {
            continue;
        };
        for sticker in &obj.stickers {
            let AppliedSticker::PowerToughness {
                power,
                toughness,
                timestamp,
                ..
            } = sticker
            else {
                continue;
            };

            bucket.push(ActiveContinuousEffect {
                source_id: id,
                controller: obj.controller,
                def_index: None,
                transient_id: None,
                trigger_producer_origin: None,
                expanded_trigger_provider: None,
                mod_index: 0,
                layer: Layer::SetPT,
                timestamp: *timestamp,
                modification: ContinuousModification::SetPower { value: *power },
                affected_filter: TargetFilter::SpecificObject { id },
                condition: None,
                mode: crate::types::statics::StaticMode::Continuous,
                characteristic_defining: false,
            });
            bucket.push(ActiveContinuousEffect {
                source_id: id,
                controller: obj.controller,
                def_index: None,
                transient_id: None,
                trigger_producer_origin: None,
                expanded_trigger_provider: None,
                mod_index: 1,
                layer: Layer::SetPT,
                timestamp: *timestamp,
                modification: ContinuousModification::SetToughness { value: *toughness },
                affected_filter: TargetFilter::SpecificObject { id },
                condition: None,
                mode: crate::types::statics::StaticMode::Continuous,
                characteristic_defining: false,
            });
        }
    }
}

pub fn available_sticker_candidates(
    state: &GameState,
    owner: PlayerId,
    kind: Option<StickerKind>,
    max_ticket_cost: Option<u32>,
    without_paying: bool,
) -> Vec<StickerCandidate> {
    let player = state.players.iter().find(|p| p.id == owner);
    let Some(player) = player else {
        return Vec::new();
    };
    let tickets = player.player_counter(&PlayerCounterKind::Ticket);
    let used = used_stickers_for_owner(state, owner);
    let mut candidates = Vec::new();

    for sheet_name in &player.sticker_sheets {
        let Some(sheet) = sticker_sheets()
            .iter()
            .find(|candidate| candidate.name.eq_ignore_ascii_case(sheet_name))
        else {
            continue;
        };

        for (index, text) in sheet.names.iter().enumerate() {
            if kind.is_some_and(|required| required != StickerKind::Name) {
                continue;
            }
            let locator = StickerLocator {
                sheet: sheet.name.clone(),
                index: index as u8,
            };
            if used.contains(&locator) {
                continue;
            }
            candidates.push(StickerCandidate {
                sticker: AppliedSticker::Name {
                    locator,
                    text: text.clone(),
                    position: 0,
                    timestamp: 0,
                },
                pay_ticket: false,
                description: format!("Name — {text}"),
            });
        }

        for art_index in 0..3u8 {
            if kind.is_some_and(|required| required != StickerKind::Art) {
                continue;
            }
            let locator = StickerLocator {
                sheet: sheet.name.clone(),
                index: 3 + art_index,
            };
            if used.contains(&locator) {
                continue;
            }
            candidates.push(StickerCandidate {
                sticker: AppliedSticker::Art {
                    locator,
                    label: format!("{} art {}", sheet.name, art_index + 1),
                    timestamp: 0,
                },
                pay_ticket: false,
                description: format!("Art — {} #{}", sheet.name, art_index + 1),
            });
        }

        for (offset, ability) in sheet.abilities.iter().enumerate() {
            if kind.is_some_and(|required| required != StickerKind::Ability) {
                continue;
            }
            if max_ticket_cost.is_some_and(|max| u32::from(ability.cost) > max) {
                continue;
            }
            if !without_paying && tickets < u32::from(ability.cost) {
                continue;
            }
            let locator = StickerLocator {
                sheet: sheet.name.clone(),
                index: 6 + offset as u8,
            };
            if used.contains(&locator) {
                continue;
            }
            candidates.push(StickerCandidate {
                sticker: AppliedSticker::Ability {
                    locator,
                    ticket_cost: ability.cost,
                    text: ability.text.clone(),
                    timestamp: 0,
                },
                pay_ticket: !without_paying && ability.cost > 0,
                description: format!(
                    "Ability — {} ({})",
                    ability.text,
                    ticket_cost_label(ability.cost)
                ),
            });
        }

        for (offset, pt) in sheet.power_toughness.iter().enumerate() {
            if kind.is_some_and(|required| required != StickerKind::PowerToughness) {
                continue;
            }
            if max_ticket_cost.is_some_and(|max| u32::from(pt.cost) > max) {
                continue;
            }
            if !without_paying && tickets < u32::from(pt.cost) {
                continue;
            }
            let locator = StickerLocator {
                sheet: sheet.name.clone(),
                index: 8 + offset as u8,
            };
            if used.contains(&locator) {
                continue;
            }
            candidates.push(StickerCandidate {
                sticker: AppliedSticker::PowerToughness {
                    locator,
                    ticket_cost: pt.cost,
                    power: pt.power,
                    toughness: pt.toughness,
                    timestamp: 0,
                },
                pay_ticket: !without_paying && pt.cost > 0,
                description: format!(
                    "P/T — {}/{} ({})",
                    pt.power,
                    pt.toughness,
                    ticket_cost_label(pt.cost)
                ),
            });
        }
    }

    candidates
}

pub fn name_sticker_position_choices(
    target: &GameObject,
    sticker: &AppliedSticker,
) -> Vec<StickerCandidate> {
    let AppliedSticker::Name {
        locator,
        text,
        timestamp,
        ..
    } = sticker
    else {
        return Vec::new();
    };
    let word_count = current_name_words(target).len();
    (0..=word_count)
        .map(|position| StickerCandidate {
            sticker: AppliedSticker::Name {
                locator: locator.clone(),
                text: text.clone(),
                position: position as u8,
                timestamp: *timestamp,
            },
            pay_ticket: false,
            description: format!("Name — {text} at position {}", position + 1),
        })
        .collect()
}

pub fn apply_selected_sticker(
    state: &mut GameState,
    player: PlayerId,
    target_id: ObjectId,
    mut sticker: AppliedSticker,
    pay_ticket: bool,
    events: &mut Vec<GameEvent>,
) {
    let Some(_) = state.objects.get(&target_id) else {
        return;
    };

    let ticket_cost = match &sticker {
        AppliedSticker::Ability { ticket_cost, .. }
        | AppliedSticker::PowerToughness { ticket_cost, .. } => u32::from(*ticket_cost),
        _ => 0,
    };

    if pay_ticket {
        let Some(player_entry) = state.players.iter_mut().find(|p| p.id == player) else {
            return;
        };
        if player_entry.player_counter(&PlayerCounterKind::Ticket) < ticket_cost {
            return;
        }
        player_entry.remove_player_counters(&PlayerCounterKind::Ticket, ticket_cost);
    }

    let timestamp = state.next_timestamp();
    match &mut sticker {
        AppliedSticker::Name {
            timestamp: slot, ..
        }
        | AppliedSticker::Ability {
            timestamp: slot, ..
        }
        | AppliedSticker::PowerToughness {
            timestamp: slot, ..
        }
        | AppliedSticker::Art {
            timestamp: slot, ..
        } => *slot = timestamp,
    }

    let Some(obj) = state.objects.get_mut(&target_id) else {
        return;
    };
    obj.stickers.push(sticker.clone());

    if obj.zone == Zone::Battlefield {
        state.layers_dirty.mark_full();
    } else if zone_retains_stickers(obj.zone) {
        rebuild_public_zone_stickers(obj);
    }

    events.push(GameEvent::StickerPlaced {
        player_id: player,
        object_id: target_id,
        kind: sticker.kind(),
    });
}

fn used_stickers_for_owner(state: &GameState, owner: PlayerId) -> HashSet<StickerLocator> {
    state
        .objects
        .values()
        .filter(|obj| obj.owner == owner)
        .flat_map(|obj| obj.stickers.iter().map(|sticker| sticker.locator().clone()))
        .collect()
}

fn current_name_words(obj: &GameObject) -> Vec<String> {
    if obj.name.trim().is_empty() {
        return Vec::new();
    }
    obj.name.split_whitespace().map(str::to_string).collect()
}

fn apply_name_stickers_from_current(obj: &mut GameObject) {
    let source_name = obj.name.clone();
    apply_name_stickers(obj, &source_name);
}

fn apply_name_stickers_from_base(obj: &mut GameObject) {
    let source_name = obj.base_name.clone();
    apply_name_stickers(obj, &source_name);
}

fn apply_name_stickers(obj: &mut GameObject, source_name: &str) {
    let mut words: Vec<String> = if source_name.trim().is_empty() {
        Vec::new()
    } else {
        source_name.split_whitespace().map(str::to_string).collect()
    };

    for sticker in &obj.stickers {
        let AppliedSticker::Name { text, position, .. } = sticker else {
            continue;
        };
        let insert_at = usize::min(*position as usize, words.len());
        let sticker_words: Vec<String> = text.split_whitespace().map(str::to_string).collect();
        for (offset, word) in sticker_words.into_iter().enumerate() {
            words.insert(insert_at + offset, word);
        }
    }

    obj.name = words.join(" ");
    if obj.name.is_empty() {
        obj.name = source_name.to_string();
    }
}

fn apply_ability_stickers(obj: &mut GameObject) {
    for sticker in &obj.stickers {
        let AppliedSticker::Ability { text, .. } = sticker else {
            continue;
        };
        let parsed = parse_sticker_ability_face(text);
        for keyword in parsed.keywords {
            obj.keywords.push(keyword);
        }
        Arc::make_mut(&mut obj.abilities).extend(parsed.abilities);
        for trigger in parsed.triggers {
            obj.trigger_definitions.push(trigger);
        }
        for replacement in parsed.replacements {
            obj.replacement_definitions.push(replacement);
        }
        for static_ability in parsed.static_abilities {
            obj.static_definitions.push(static_ability);
        }
    }
}

fn apply_power_toughness_stickers(obj: &mut GameObject) {
    for sticker in &obj.stickers {
        let AppliedSticker::PowerToughness {
            power, toughness, ..
        } = sticker
        else {
            continue;
        };
        obj.power = Some(*power);
        obj.toughness = Some(*toughness);
    }
}

fn parse_sticker_ability_face(oracle_text: &str) -> CardFace {
    let mut source = GameObject::new(
        ObjectId(0),
        CardId(0),
        PlayerId(0),
        "Sticker".to_string(),
        Zone::Battlefield,
    );
    source.card_types = CardType::default();
    source.mana_cost = ManaCost::default();

    let mut keyword_names: Vec<String> = oracle_text
        .split(',')
        .map(|part| part.trim().to_lowercase())
        .filter(|text| {
            let keyword: Keyword = text.parse().unwrap_or(Keyword::Unknown(String::new()));
            !matches!(keyword, Keyword::Unknown(_))
        })
        .collect();
    keyword_names.sort();
    keyword_names.dedup();

    let type_strings: Vec<String> = source
        .card_types
        .core_types
        .iter()
        .map(|entry| entry.to_string())
        .collect();
    let subtype_strings = source.card_types.subtypes.clone();
    let (parsed, cleave_variant) = parse_oracle_with_cleave_brackets(
        oracle_text,
        &source.name,
        &keyword_names,
        &type_strings,
        &subtype_strings,
    );

    let mut keywords: Vec<Keyword> = keyword_names
        .iter()
        .filter_map(|name| {
            let keyword: Keyword = name.parse().ok()?;
            if matches!(keyword, Keyword::Unknown(_)) {
                None
            } else {
                Some(keyword)
            }
        })
        .collect();
    merge_extracted_keywords(&mut keywords, parsed.extracted_keywords);

    let mut face = CardFace {
        name: source.name,
        power: None,
        toughness: None,
        card_type: source.card_types,
        mana_cost: source.mana_cost,
        oracle_text: Some(oracle_text.to_string()),
        keywords,
        abilities: parsed.abilities,
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        replacements: parsed.replacements,
        cleave_variant,
        modal: parsed.modal,
        additional_cost: parsed.additional_cost,
        casting_restrictions: parsed.casting_restrictions,
        casting_options: parsed.casting_options,
        solve_condition: parsed.solve_condition,
        strive_cost: parsed.strive_cost,
        ..Default::default()
    };
    synthesize_all(&mut face);
    face
}

fn ticket_cost_label(cost: u8) -> String {
    "{TK}".repeat(cost as usize)
}
