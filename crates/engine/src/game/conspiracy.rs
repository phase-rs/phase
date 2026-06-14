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
//! put into the command zone face down; any time its controller has priority
//! they may turn it face up ([`turn_hidden_agenda_face_up`]). While face down it
//! is not yet functioning, so its abilities don't apply until it is revealed.
//!
//! This is the runtime sibling of `game::archenemy` (schemes) and
//! `game::planechase` (planes/phenomena) — the other command-zone card types.
//! Draft-time abilities (CR 905.2, "as you draft") are out of scope: there is no
//! draft engine.

use crate::game::game_object::GameObject;
use crate::types::card_type::CoreType;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// CR 905: True when the object is a conspiracy card.
pub fn is_conspiracy(obj: &GameObject) -> bool {
    obj.card_types.core_types.contains(&CoreType::Conspiracy)
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
    obj.zone = Zone::Command;
    // CR 905.4a: hidden-agenda conspiracies start face down; others face up.
    obj.face_down = hidden_agenda;
    // CR 905.5: the owner of a conspiracy is its controller.
    obj.controller = obj.owner;

    if !state.command_zone.contains(&id) {
        state.command_zone.push_back(id);
    }

    // CR 611.2: a newly functioning command-zone static source changes the set
    // of continuous-effect generators, so the cached layer state must be rebuilt.
    crate::game::layers::mark_layers_full(state);
}

/// CR 905.4a + CR 702.106: Turn a face-down hidden-agenda conspiracy face up.
///
/// A player may do this any time they have priority. No-op (returns `false`)
/// unless `id` is a face-down conspiracy that `player` owns in the command zone
/// (CR 404.2 / CR 905.5: a conspiracy's owner is its controller). On success the
/// conspiracy turns face up and begins functioning, so layers are marked dirty
/// to gather its now-active command-zone statics (CR 611.2).
pub fn turn_hidden_agenda_face_up(state: &mut GameState, id: ObjectId, player: PlayerId) -> bool {
    let Some(obj) = state.objects.get_mut(&id) else {
        return false;
    };
    if !(obj.zone == Zone::Command && obj.face_down && obj.owner == player && is_conspiracy(obj)) {
        return false;
    }
    obj.face_down = false;
    crate::game::layers::mark_layers_full(state);
    true
}
