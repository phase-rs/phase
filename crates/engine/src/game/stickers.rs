use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

use serde::Deserialize;

use crate::database::synthesis::{
    merge_extracted_keywords, parse_oracle_with_cleave_brackets, synthesize_all,
};
use crate::game::game_object::GameObject;
use crate::types::ability::{
    ContinuousModification, FilterProp, TargetFilter, TypeFilter, TypedFilter,
};
use crate::types::card::CardFace;
use crate::types::card_type::CardType;
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::keywords::Keyword;
use crate::types::layers::{ActiveContinuousEffect, Layer};
use crate::types::mana::ManaCost;
use crate::types::player::{PlayerCounterKind, PlayerId};
use crate::types::resolved_commands::ResolvedPlayerEdit;
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

/// CR 123.5 + CR 400.2: an object keeps its stickers in the public zones and
/// loses them in the hidden ones; the single zone predicate is `Zone::is_public`.
pub fn zone_retains_stickers(zone: Zone) -> bool {
    zone.is_public()
}

pub fn object_is_stickered(obj: &GameObject) -> bool {
    !obj.stickers.is_empty()
}

pub fn object_has_sticker_kind(obj: &GameObject, kind: StickerKind) -> bool {
    obj.stickers.iter().any(|sticker| sticker.kind() == kind)
}

/// CR 123.3b: A player can't put a sticker on an object they don't own; an
/// effect that would cause them to does nothing for that part. The single
/// predicate the PutSticker resolver and `apply_selected_sticker` share.
pub(crate) fn may_put_sticker_on(object: &GameObject, player: PlayerId) -> bool {
    object.owner == player
}

/// CR 123.5 + CR 123.6c + CR 123.7: re-derive a card's name and ability
/// stickers from its printed characteristics in a public zone other than the
/// battlefield and the stack (P/T stickers ride the ordered layer pass).
pub fn rebuild_public_zone_stickers(obj: &mut GameObject) {
    obj.revert_layered_characteristics_to_base();
    apply_name_stickers_from_base(obj);
    apply_ability_stickers(obj);
}

/// CR 702.33h + CR 123.6b + CR 123.7: a sticker put on a spell modifies the
/// spell as it is. A spell carries state fixed as it was cast — its prototype
/// form (CR 718.3b) and cast-bound effects such as Cavern of Souls' "that
/// spell can't be countered" — that no printed baseline records, so the spell
/// is never reverted: only the new sticker's name words and abilities are
/// added. Its P/T sticker is applied by the ordered layer pass (CR 613.4b).
fn apply_sticker_to_spell(obj: &mut GameObject, sticker: &AppliedSticker) {
    match sticker {
        AppliedSticker::Name { text, position, .. } => {
            let mut words = current_name_words(obj);
            insert_name_sticker_words(&mut words, text, *position);
            obj.name = words.join(" ");
        }
        AppliedSticker::Ability { text, .. } => grant_ability_sticker(obj, text),
        AppliedSticker::PowerToughness { .. } | AppliedSticker::Art { .. } => {}
    }
}

/// CR 123.1 + CR 707.2 + CR 707.10 + CR 707.12: a copy of a spell or card
/// acquires the original's copiable values, and changes from stickers aren't
/// copiable values; the stickers themselves are markers on the original, not on
/// its copy. Given a copy cloned from a stickered object, remove its stickers
/// and undo exactly what each one added — its name words, its granted
/// abilities and its P/T — in reverse placement order, leaving every other
/// characteristic as cloned.
///
/// The single authority every object-cloning copy birth calls, before it
/// derives anything else from the clone: `copy_spell.rs` (copy a spell),
/// `cast_copy_of_card.rs` (cast a copy of a card), `paradigm.rs` (the exiled
/// Paradigm source), `epic.rs` (the Epic card) and `prepare.rs` (the prepare
/// copy in exile). Token copies are built fresh, with no stickers.
pub(crate) fn remove_stickers_from_copy(copy: &mut GameObject) {
    let stickers = std::mem::take(&mut copy.stickers);
    for sticker in stickers.iter().rev() {
        match sticker {
            AppliedSticker::Name { text, position, .. } => {
                let mut words = current_name_words(copy);
                remove_name_sticker_words(&mut words, text, *position);
                copy.name = words.join(" ");
            }
            AppliedSticker::Ability { text, .. } => revoke_ability_sticker(copy, text),
            AppliedSticker::PowerToughness { .. } | AppliedSticker::Art { .. } => {}
        }
    }
    // CR 613.4b: a P/T sticker's setter lives only in the layer pass, which no
    // longer reaches a copy without stickers; return the copy to its own P/T
    // baseline for the pass to build on.
    if stickers
        .iter()
        .any(|sticker| sticker.kind() == StickerKind::PowerToughness)
    {
        reset_pt_to_object_baseline(copy);
    }
}

/// CR 718.3b: an object's own P/T baseline, which a P/T sticker's layer-7b
/// setter writes over — its prototype values for a prototyped spell, else its
/// printed P/T.
pub(crate) fn reset_pt_to_object_baseline(obj: &mut GameObject) {
    let (power, toughness) = obj
        .prototype_form
        .as_ref()
        .map_or((obj.base_power, obj.base_toughness), |form| {
            (Some(form.power), Some(form.toughness))
        });
    obj.power = power;
    obj.toughness = toughness;
    obj.layer_base_power = power;
    obj.layer_base_toughness = toughness;
}

/// The inverse of [`insert_name_sticker_words`] (CR 123.6b + CR 123.6c): the
/// sticker's words sit at its recorded placement — `position` words in, or at
/// the end when the name then had fewer words — once every later name sticker
/// has been removed first (callers undo in reverse placement order). If the
/// words there are not the sticker's, the name was rewritten by something
/// other than this sticker after it was placed; nothing is removed rather than
/// guessing which occurrence of those words the sticker added.
fn remove_name_sticker_words(words: &mut Vec<String>, text: &str, position: u8) {
    let sticker_words: Vec<&str> = text.split_whitespace().collect();
    let count = sticker_words.len();
    if count == 0 || count > words.len() {
        return;
    }
    let placed_at = usize::min(position as usize, words.len() - count);
    let placed = words[placed_at..placed_at + count]
        .iter()
        .map(String::as_str)
        .eq(sticker_words.iter().copied());
    if placed {
        words.drain(placed_at..placed_at + count);
    }
}

/// The inverse of [`grant_ability_sticker`]: remove one occurrence (the last)
/// of each keyword and definition the sticker's text grants.
fn revoke_ability_sticker(obj: &mut GameObject, text: &str) {
    let parsed = parse_sticker_ability_face(text);
    for keyword in &parsed.keywords {
        // allow-raw-authority: structural edit of this object's own keyword list (removing the instance the sticker added); the helpers only answer has/has-not.
        if let Some(index) = obj.keywords.iter().rposition(|k| k == keyword) {
            obj.keywords.remove(index);
        }
    }
    for ability in &parsed.abilities {
        if let Some(index) = obj.abilities.iter().rposition(|a| a == ability) {
            Arc::make_mut(&mut obj.abilities).remove(index);
        }
    }
    for trigger in parsed.triggers {
        let entry = crate::types::ability::TriggerEntry::unmaterialized_legacy(trigger);
        remove_last_equal(&mut obj.trigger_definitions, &entry);
    }
    for replacement in &parsed.replacements {
        remove_last_equal(&mut obj.replacement_definitions, replacement);
    }
    for static_ability in &parsed.static_abilities {
        remove_last_equal(&mut obj.static_definitions, static_ability);
    }
}

fn remove_last_equal<T: Clone + PartialEq>(
    definitions: &mut crate::types::definitions::Definitions<T>,
    item: &T,
) {
    // Structural edit of the definition list, not a functioning read.
    let Some(target) = definitions.iter_all().rposition(|d| d == item) else {
        return;
    };
    let mut index = 0;
    definitions.retain(|_| {
        let keep = index != target;
        index += 1;
        keep
    });
}

/// CR 123.7 + CR 613.1f: the keywords the object's ability stickers grant. The
/// stack seed of a full layer pass resets a spell's keywords to its printed
/// ones; these are re-granted with them so a stickered spell keeps them.
pub(crate) fn ability_sticker_keywords(obj: &GameObject) -> Vec<Keyword> {
    obj.stickers
        .iter()
        .filter_map(|sticker| match sticker {
            AppliedSticker::Ability { text, .. } => Some(parse_sticker_ability_face(text).keywords),
            AppliedSticker::Name { .. }
            | AppliedSticker::PowerToughness { .. }
            | AppliedSticker::Art { .. } => None,
        })
        .flatten()
        .collect()
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

/// The one enumeration of objects carrying a sticker of `kind` in a zone that
/// retains stickers: the zone's actual population (`zone_object_ids`, so
/// phased-out permanents, phantom stack objects and Attraction/Contraption deck
/// objects are excluded) whose `zone` agrees with the container.
pub(crate) fn public_zone_sticker_carriers(
    state: &GameState,
    kind: StickerKind,
) -> impl Iterator<Item = (Zone, ObjectId)> + '_ {
    Zone::ALL
        .into_iter()
        .filter(|zone| zone_retains_stickers(*zone))
        .flat_map(move |zone| {
            crate::game::targeting::zone_object_ids(state, zone)
                .into_iter()
                .map(move |id| (zone, id))
        })
        .filter(move |(zone, id)| {
            state
                .objects
                .get(id)
                .is_some_and(|obj| obj.zone == *zone && object_has_sticker_kind(obj, kind))
        })
}

/// CR 123.8 + CR 613.4b + CR 112.1: P/T stickers set P/T in layer 7b for
/// creatures on the battlefield and creature or Vehicle cards (including
/// creature spells) in other public zones; eligibility is read after layer 4
/// (CR 613.1d). Each sticker is a setter with its own timestamp (CR 613.7k).
pub fn append_public_zone_pt_sticker_effects(
    state: &GameState,
    effects: &mut [(Layer, Vec<ActiveContinuousEffect>)],
) {
    let Some((_, bucket)) = effects.iter_mut().find(|(layer, _)| *layer == Layer::SetPT) else {
        return;
    };

    for (zone, id) in public_zone_sticker_carriers(state, StickerKind::PowerToughness) {
        let Some(obj) = state.objects.get(&id) else {
            continue;
        };
        let (type_filter, properties) = if zone == Zone::Battlefield {
            (TypeFilter::Creature, vec![FilterProp::InZone { zone }])
        } else {
            (
                TypeFilter::AnyOf(vec![
                    TypeFilter::Creature,
                    TypeFilter::Subtype("Vehicle".to_string()),
                ]),
                vec![FilterProp::InZone { zone }, FilterProp::RepresentedByCard],
            )
        };
        let affected_filter = TargetFilter::And {
            filters: vec![
                TargetFilter::SpecificObject { id },
                TargetFilter::Typed(TypedFilter::new(type_filter).properties(properties)),
            ],
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
                affected_filter: affected_filter.clone(),
                condition: None,
                mode: crate::types::statics::StaticMode::Continuous,
                characteristic_defining: false,
                granter: None,
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
                affected_filter: affected_filter.clone(),
                condition: None,
                mode: crate::types::statics::StaticMode::Continuous,
                characteristic_defining: false,
                granter: None,
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
    let Some(target) = state.objects.get(&target_id) else {
        return;
    };
    // CR 123.3b: refused before any ticket is paid (CR 123.3c: the owner pays).
    if !may_put_sticker_on(target, player) {
        return;
    }

    let ticket_cost = match &sticker {
        AppliedSticker::Ability { ticket_cost, .. }
        | AppliedSticker::PowerToughness { ticket_cost, .. } => u32::from(*ticket_cost),
        _ => 0,
    };

    if pay_ticket {
        // CR 122.1: Ticket payment removes counters through the scalar authority.
        let Some(player_entry) = state.players.iter().find(|p| p.id == player) else {
            return;
        };
        if player_entry.player_counter(&PlayerCounterKind::Ticket) < ticket_cost {
            return;
        }
        if ticket_cost > 0 {
            state
                .resolve_and_apply_player_edit(
                    player,
                    ResolvedPlayerEdit::Counter {
                        kind: PlayerCounterKind::Ticket,
                        delta: -(ticket_cost as i32),
                    },
                )
                .expect("the checked ticket cost must satisfy its resolved precondition");
        }
    }

    // CR 613.7k: a sticker receives a new timestamp each time it's put on an object.
    sticker.set_timestamp(state.next_timestamp());

    let Some(obj) = state.objects.get_mut(&target_id) else {
        return;
    };
    obj.stickers.push(sticker.clone());

    // CR 123.8 + CR 613.4b: every public-zone recipient is re-settled by the
    // ordered layer pass; off the battlefield the name/ability stickers are
    // applied first — from the printed card in a card zone, and onto the spell
    // as it is on the stack.
    match obj.zone {
        Zone::Battlefield => {}
        Zone::Stack => apply_sticker_to_spell(obj, &sticker),
        Zone::Graveyard | Zone::Exile | Zone::Command => rebuild_public_zone_stickers(obj),
        Zone::Library | Zone::Hand => {}
    }
    if zone_retains_stickers(obj.zone) {
        state.layers_dirty.mark_full();
    }
    // CR 608.2c: latch "that sticker" for the rest of this resolution.
    state.placed_sticker_this_resolution = Some(sticker.clone());

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
        insert_name_sticker_words(&mut words, text, *position);
    }

    obj.name = words.join(" ");
    if obj.name.is_empty() {
        obj.name = source_name.to_string();
    }
}

/// CR 123.6b + CR 123.6c: a name sticker's words go after `position` words of
/// the current name, or at its end when the name now has fewer words.
fn insert_name_sticker_words(words: &mut Vec<String>, text: &str, position: u8) {
    let insert_at = usize::min(position as usize, words.len());
    for (offset, word) in text.split_whitespace().enumerate() {
        words.insert(insert_at + offset, word.to_string());
    }
}

fn apply_ability_stickers(obj: &mut GameObject) {
    let texts: Vec<String> = obj
        .stickers
        .iter()
        .filter_map(|sticker| match sticker {
            AppliedSticker::Ability { text, .. } => Some(text.clone()),
            AppliedSticker::Name { .. }
            | AppliedSticker::PowerToughness { .. }
            | AppliedSticker::Art { .. } => None,
        })
        .collect();
    for text in &texts {
        grant_ability_sticker(obj, text);
    }
}

/// CR 123.7: the object gains the abilities printed on an ability sticker.
fn grant_ability_sticker(obj: &mut GameObject, text: &str) {
    let parsed = parse_sticker_ability_face(text);
    for keyword in parsed.keywords {
        obj.keywords.push(keyword);
    }
    Arc::make_mut(&mut obj.abilities).extend(parsed.abilities);
    for trigger in parsed.triggers {
        // FIXME(stickers): needs a real occurrence ref. Unlike the other
        // sites in this change, a sticker-granted trigger is NOT a printed
        // slot — `rebuild_public_zone_stickers` reverts to base and
        // re-applies, so pushing to `base_trigger_definitions` would make
        // the sticker permanent. It wants a Layer-6 grant producer key, but
        // neither `TriggerProducerOrigin::Static` (no owning static
        // definition) nor `Transient` (no continuous-effect id) describes a
        // sticker without a design decision. Left explicit rather than
        // guessed; a stickered object still fails to serialize.
        obj.trigger_definitions
            .push(crate::types::ability::TriggerEntry::unmaterialized_legacy(
                trigger,
            ));
    }
    for replacement in parsed.replacements {
        obj.replacement_definitions.push(replacement);
    }
    for static_ability in parsed.static_abilities {
        obj.static_definitions.push(static_ability);
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
