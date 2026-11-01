//! CR 905: Conspiracy Draft — conspiracy cards in the command zone, including
//! hidden agenda (CR 905.4a + CR 702.106).
//!
//! Conspiracy cards (the `CoreType::Conspiracy` card type) are nontraditional
//! cards that exist only in the command zone. At the start of the game, before
//! decks are shuffled, each player may put any number of conspiracy cards from
//! their sideboard into the command zone (CR 905.4); their owner is their
//! controller (CR 905.5). Conspiracies are not permanents and can't be cast —
//! they apply their abilities from the command zone for the rest of the game.
//!
//! A face-up conspiracy's continuous static abilities function from the command
//! zone the same way a plane's or scheme's do: the database build path
//! (`synthesize_conspiracy`) stamps `Zone::Command` onto the static/trigger
//! definitions (CR 113.6b), and the layer/trigger gathers admit a face-up
//! conspiracy as a command-zone ability source via
//! [`functions_from_command_zone`] — the same seam that admits command-zone
//! emblems (CR 114.3). The per-static zone-of-function gate in
//! `functioning_abilities` (which already passes any command-zone static whose
//! `active_zones` lists `Zone::Command`) then decides whether each individual
//! ability applies.
//!
//! Hidden agenda (CR 905.4a + CR 702.106): a conspiracy with hidden agenda is
//! put into the command zone face down, and its owner secretly chooses a card
//! name ([`pending_agenda_choice`] / [`handle_secret_agenda_names`]); any time
//! its controller has priority they may turn it face up
//! ([`turn_hidden_agenda_face_up`]). While face down it has no characteristics
//! (CR 315.5b), so its abilities don't apply until it is revealed.
//!
//! This is the runtime sibling of `game::archenemy` (schemes) and
//! `game::planechase` (planes/phenomena) — the other command-zone card types.
//! Draft-time abilities (CR 905.2, "as you draft") are out of scope: there is no
//! draft engine.

use std::sync::Arc;

use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::combinator::value;
use nom::{IResult, Parser};

use crate::game::filter::card_names_match;
use crate::game::game_object::GameObject;
use crate::game::players;
use crate::types::ability::ChosenAttribute;
use crate::types::card::CardFace;
use crate::types::card_type::{CardType, CoreType};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::mana::ManaCost;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// CR 905: True when the object is a conspiracy card.
pub fn is_conspiracy(obj: &GameObject) -> bool {
    obj.is_conspiracy || obj.card_types.core_types.contains(&CoreType::Conspiracy)
}

/// CR 702.106a / CR 702.106f: How many card names an agenda line has its owner
/// secretly choose.
fn agenda_name_count_line(line: &str) -> IResult<&str, u8> {
    alt((
        value(1, tag("hidden agenda")),
        value(2, tag("double agenda")),
    ))
    .parse(line)
}

/// CR 702.106a / CR 702.106f: The number of card names this conspiracy's agenda
/// has its owner secretly choose — 1 for hidden agenda, 2 for double agenda, and
/// 0 for a card with neither.
pub fn agenda_name_count(face: &CardFace) -> u8 {
    if !face.card_type.core_types.contains(&CoreType::Conspiracy) {
        return 0;
    }
    face.oracle_text
        .as_deref()
        .into_iter()
        .flat_map(str::lines)
        .find_map(|line| {
            agenda_name_count_line(&line.to_lowercase())
                .ok()
                .map(|(_, count)| count)
        })
        .unwrap_or(0)
}

/// CR 905.4a / CR 702.106a / CR 702.106f: Returns true if the card has hidden agenda or double agenda.
pub fn card_has_hidden_agenda(face: &CardFace) -> bool {
    agenda_name_count(face) > 0
}

/// CR 315.5b: Face-down conspiracy cards have no characteristics. Blank all live
/// rules characteristics, leaving only the internal identity that reveal and
/// eligibility read: `is_conspiracy`, `agenda_name_count`, the secret chosen
/// names, and the `back_face` snapshot.
pub fn apply_face_down_conspiracy_characteristics(obj: &mut GameObject) {
    obj.face_down = true;
    obj.name = String::new();
    obj.base_name = String::new();
    obj.card_types = CardType::default();
    obj.base_card_types = CardType::default();
    obj.mana_cost = ManaCost::NoCost;
    obj.base_mana_cost = ManaCost::NoCost;
    obj.keywords = Vec::new();
    obj.base_keywords = Vec::new();
    obj.abilities = Arc::new(Vec::new());
    obj.base_abilities = Arc::new(Vec::new());
    obj.trigger_definitions = Default::default();
    obj.base_trigger_definitions = Arc::new(Vec::new());
    obj.replacement_definitions = Default::default();
    obj.base_replacement_definitions = Arc::new(Vec::new());
    obj.static_definitions = Default::default();
    obj.base_static_definitions = Arc::new(Vec::new());
    obj.color = Vec::new();
    obj.base_color = Vec::new();
    obj.power = None;
    obj.toughness = None;
    obj.base_power = None;
    obj.base_toughness = None;
    obj.printed_ref = None;
    obj.base_printed_ref = None;
}

/// CR 702.106a / CR 702.106f: Validate and record a player's secret choice for a
/// face-down agenda conspiracy they control.
///
/// Accepts exactly the number of names the agenda requires, each a real card
/// name (CR 201.4) and, for double agenda, "two different cards". On success the
/// names are stored privately in `obj.secret_chosen_names`, where they stay until
/// the conspiracy is turned face up.
pub fn record_secret_agenda_choices(
    state: &mut GameState,
    id: ObjectId,
    player: PlayerId,
    names: Vec<String>,
) -> Result<(), String> {
    let Some(obj) = state.objects.get(&id) else {
        return Err("Object not found".to_string());
    };
    if !is_face_down_conspiracy_candidate(obj, player) {
        return Err("You don't control this face-down conspiracy".to_string());
    }
    if names.len() != usize::from(obj.agenda_name_count) {
        return Err(format!(
            "This agenda requires exactly {} card name(s)",
            obj.agenda_name_count
        ));
    }
    if let Some(name) = names.iter().find(|name| {
        !state
            .all_card_names
            .iter()
            .any(|known| card_names_match(known, name))
    }) {
        return Err(format!("Invalid card name '{name}'"));
    }
    if let [first, second] = names.as_slice() {
        if card_names_match(first, second) {
            return Err("Double agenda requires two different card names".to_string());
        }
    }
    if let Some(obj) = state.objects.get_mut(&id) {
        obj.secret_chosen_names = names;
    }
    Ok(())
}

/// CR 702.106a: The next agenda prompt the pregame procedure still owes, in seat
/// order, or `None` once every face-down conspiracy has its secret names.
pub fn pending_agenda_choice(state: &GameState) -> Option<WaitingFor> {
    state.seat_order.iter().find_map(|&player| {
        state.command_zone.iter().find_map(|&object_id| {
            let obj = state.objects.get(&object_id)?;
            (obj.owner == player
                && players::is_alive(state, player)
                && is_face_down_conspiracy_candidate(obj, player)
                && obj.agenda_name_count > 0
                && obj.secret_chosen_names.is_empty())
            .then_some(WaitingFor::SecretAgendaChoice {
                player,
                object_id,
                name_count: obj.agenda_name_count,
            })
        })
    })
}

/// CR 702.106a: Answer a [`WaitingFor::SecretAgendaChoice`], then continue the
/// pregame procedure. No event is emitted: the choice is secret until the
/// conspiracy is turned face up (CR 702.106c).
pub fn handle_secret_agenda_names(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    names: Vec<String>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, String> {
    record_secret_agenda_choices(state, object_id, player, names)?;
    Ok(next_pregame_decision(state, events))
}

/// CR 702.106a: The next agenda prompt, or the companion and mulligan steps once
/// every agenda is named. Authority for leaving the agenda step of pregame.
pub fn next_pregame_decision(state: &mut GameState, events: &mut Vec<GameEvent>) -> WaitingFor {
    pending_agenda_choice(state)
        .unwrap_or_else(|| super::companion::companion_reveals_or_mulligan(state, events))
}

/// CR 905.4 + CR 113.6b: True when this object is a conspiracy that is currently
/// functioning from the command zone — i.e. a conspiracy that is face up in the
/// command zone.
///
/// This is the command-zone ability-source gate for conspiracies, mirroring the
/// `is_emblem` gate command-zone emblems use (CR 114.3). The layer gather
/// (`layers::for_each_static_effect_source`), its candidate index
/// (`static_source_index`), and the command-zone trigger scan admit a conspiracy
/// as a source iff this returns `true`. A face-down (hidden agenda, CR 905.4a)
/// conspiracy does not yet function and so is excluded until it is turned face
/// up.
pub fn functions_from_command_zone(obj: &GameObject) -> bool {
    obj.zone == Zone::Command && !obj.face_down && is_conspiracy(obj)
}

/// CR 905.4 / CR 905.5: The face-up conspiracies a player owns in the command
/// zone. Face-down hidden-agenda conspiracies are excluded — they aren't yet
/// functioning (CR 905.4a) — as are conspiracies owned by other players.
///
/// CR 404.2: the command zone is a non-battlefield zone, so this player-scoped
/// query filters by `obj.owner`. For conspiracies this is also the controller —
/// CR 905.5 makes a conspiracy's owner its controller.
pub fn conspiracies_in_command_zone(state: &GameState, player: PlayerId) -> Vec<ObjectId> {
    state
        .command_zone
        .iter()
        .copied()
        .filter(|&id| {
            state
                .objects
                .get(&id)
                .is_some_and(|obj| functions_from_command_zone(obj) && obj.owner == player)
        })
        .collect()
}

/// CR 905.4 / CR 905.4a / CR 905.5: Begin the game with a conspiracy in the
/// command zone.
///
/// The object is placed in the command zone with its owner as its controller
/// (CR 905.5). A conspiracy with hidden agenda enters face down (CR 905.4a +
/// CR 702.106); every other conspiracy enters face up (CR 905.4). Layers are
/// marked dirty so a face-up conspiracy's command-zone continuous statics are
/// gathered on the next layer pass (the static-source index keys on the
/// command-zone source set, which this changes).
///
/// No-op if `id` is not a known object or is not a conspiracy card. Idempotent
/// with respect to the command zone: the id is appended only if not already
/// present.
pub fn start_with_conspiracy(state: &mut GameState, id: ObjectId, hidden_agenda: bool) {
    let Some(obj) = state.objects.get_mut(&id) else {
        return;
    };
    // CR 905.4: only conspiracy cards begin the game in the command zone this way.
    if !is_conspiracy(obj) {
        return;
    }
    // allow-raw-zone: pregame conspiracy setup begins from outside the game, not a zone move (CR 400.11 + CR 905.4).
    obj.zone = Zone::Command;
    // CR 905.4a: hidden-agenda conspiracies start face down; others face up.
    obj.face_down = hidden_agenda;
    // CR 905.5: the owner of a conspiracy is its controller.
    obj.controller = obj.owner;
    obj.is_conspiracy = true;

    // CR 315.5b: the printed face is kept only as the reveal snapshot.
    if hidden_agenda {
        if obj.back_face.is_none() {
            obj.back_face = Some(crate::game::printed_cards::snapshot_object_face(obj));
        }
        apply_face_down_conspiracy_characteristics(obj);
    }

    if !state.command_zone.contains(&id) {
        // allow-raw-zone: pregame conspiracy setup begins from outside the game, not a zone move (CR 400.11 + CR 905.4).
        state.command_zone.push_back(id);
    }

    // CR 611.2: a newly functioning command-zone static source changes the set
    // of continuous-effect generators, so the cached layer state must be rebuilt.
    crate::game::layers::mark_layers_full(state);
}

/// CR 702.106c / CR 116.2j / CR 905.4a: True if `obj` is an eligible face-down
/// hidden-agenda conspiracy that `player` can turn face up from the command zone.
pub fn is_face_down_conspiracy_candidate(obj: &GameObject, player: PlayerId) -> bool {
    obj.zone == Zone::Command
        && obj.face_down
        && (obj.owner == player || obj.controller == player)
        && is_conspiracy(obj)
}

/// CR 905.4a + CR 702.106: Turn a face-down hidden-agenda conspiracy face up.
///
/// A player may do this any time they have priority. No-op (returns `false`)
/// unless `id` is a face-down conspiracy that `player` owns in the command zone
/// (CR 404.2 / CR 905.5: a conspiracy's owner is its controller). On success the
/// conspiracy turns face up and begins functioning, so layers are marked dirty
/// to gather its now-active command-zone statics (CR 611.2).
pub fn turn_hidden_agenda_face_up(
    state: &mut GameState,
    id: ObjectId,
    player: PlayerId,
    events: &mut Vec<GameEvent>,
) -> bool {
    let Some(obj) = state.objects.get_mut(&id) else {
        return false;
    };
    if !is_face_down_conspiracy_candidate(obj, player) {
        return false;
    }
    obj.face_down = false;
    if let Some(back_face) = obj.back_face.take() {
        crate::game::printed_cards::apply_back_face_to_object(obj, back_face);
    }
    // CR 702.106c + CR 702.106d: turning it face up reveals the chosen name(s),
    // which the linked "the chosen name" abilities then read.
    for name in &obj.secret_chosen_names {
        obj.chosen_attributes
            .push(ChosenAttribute::CardName(name.clone()));
        events.push(GameEvent::CardNamed {
            player_id: player,
            source_id: Some(id),
            name: name.clone(),
        });
    }
    crate::game::layers::mark_layers_full(state);
    true
}
