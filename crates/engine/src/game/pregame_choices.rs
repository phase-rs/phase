//! CR 103.2c + CR 903.4b + CR 607.2p: Pre-game linked-CDA color choices.
//!
//! Some commanders carry a static ability that causes their controller to
//! choose a color before the game begins, feeding a linked characteristic-
//! defining ability that sets the commander's color to that choice (Clara
//! Oswald's "Impossible Girl", The Prismatic Piper, Faceless One). Per CR 903.4b
//! the choice is revealed as the commander is placed into the command zone —
//! after the companion reveal (CR 103.2b) and before mulligans (CR 103.5). This
//! module mirrors [`super::companion`]'s pre-game reveal flow: it scans for
//! commanders needing a choice, emits the [`WaitingFor::PregameChooseColor`]
//! prompt in seat order, seeds the persistent
//! [`GameState::pregame_chosen_colors`] map on submission, then advances to the
//! next choice or to mulligans.
//!
//! The "if ~ is your commander" gate (CR 604.3a(5) keeps it OFF the CDA, which
//! is unconditional) is enforced structurally here: only `is_commander` objects
//! are ever scanned, so a copy of the card in the 99 never prompts.

use crate::types::ability::ContinuousModification;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::mana::ManaColor;
use crate::types::player::PlayerId;

/// CR 607.2p: Does this commander carry a printed color CDA whose choice is made
/// before the game begins (a `SetPregameChosenColor` modification on one of its
/// base static abilities)? Reads `base_static_definitions` (the printed statics)
/// so the check holds regardless of the commander's current zone.
fn commander_has_pregame_color_cda(state: &GameState, obj_id: ObjectId) -> bool {
    let Some(obj) = state.objects.get(&obj_id) else {
        return false;
    };
    if !obj.is_commander {
        return false;
    }
    obj.base_static_definitions.iter().any(|def| {
        def.modifications
            .iter()
            .any(|m| matches!(m, ContinuousModification::SetPregameChosenColor))
    })
}

/// CR 607.2p: A commander needs a pre-game color choice iff it has the linked
/// color CDA and hasn't been seeded yet.
fn commander_needs_pregame_color(state: &GameState, obj_id: ObjectId) -> bool {
    !state.pregame_chosen_colors.contains_key(&obj_id)
        && commander_has_pregame_color_cda(state, obj_id)
}

/// CR 903.4b: Find the first commander owned by `player` still needing a pre-game
/// color choice, in ascending `ObjectId` order for determinism.
fn check_pregame_color_choice(state: &GameState, player: PlayerId) -> Option<WaitingFor> {
    let mut commander_ids: Vec<ObjectId> = state
        .objects
        .values()
        .filter(|obj| obj.is_commander && obj.owner == player)
        .map(|obj| obj.id)
        .collect();
    commander_ids.sort_unstable();

    commander_ids
        .into_iter()
        .find(|&id| commander_needs_pregame_color(state, id))
        .map(|commander_id| WaitingFor::PregameChooseColor {
            player,
            commander_id,
        })
}

/// CR 103.2c: Check pre-game color choices for all players in seat order.
/// Returns the first prompt found, or `None` when every commander is seeded.
pub fn check_all_pregame_color_choices(state: &GameState) -> Option<WaitingFor> {
    for &player_id in &state.seat_order {
        if let Some(wf) = check_pregame_color_choice(state, player_id) {
            return Some(wf);
        }
    }
    None
}

/// CR 103.2c + CR 903.4b: The shared pre-game funnel entry — begin the pre-game
/// color choices if any commander needs one, otherwise start mulligans. Called
/// both from `engine::start_game` (no-companion branch) and from
/// `companion::advance_companion_reveal` (after the last companion reveal).
pub fn begin_pregame_color_or_mulligan(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> WaitingFor {
    match check_all_pregame_color_choices(state) {
        Some(wf) => wf,
        None => super::mulligan::start_mulligan(state, events),
    }
}

/// CR 903.4b + CR 105.3: Record the pre-game color and advance the pre-game
/// flow. Writes `pregame_chosen_colors[commander_id]`, emits
/// [`GameEvent::PregameColorChosen`], marks layers dirty (the color CDA now
/// applies), then advances to the next commander needing a choice or to
/// mulligans.
pub fn handle_choose_pregame_color(
    state: &mut GameState,
    color: ManaColor,
    events: &mut Vec<GameEvent>,
) -> WaitingFor {
    let (player, commander_id) = match &state.waiting_for {
        WaitingFor::PregameChooseColor {
            player,
            commander_id,
        } => (*player, *commander_id),
        // Defensive: not in the expected pre-game state — fall through to the
        // mulligan step rather than seeding an unrelated object.
        _ => return super::mulligan::start_mulligan(state, events),
    };

    state
        .pregame_chosen_colors
        .insert(commander_id, vec![color]);
    // CR 604.3: the linked CDA reads this at layer-evaluation time — recompute.
    state.layers_dirty.mark_full();
    events.push(GameEvent::PregameColorChosen {
        player,
        commander_id,
        color,
    });

    begin_pregame_color_or_mulligan(state, events)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{StaticDefinition, TargetFilter};
    use crate::types::card_type::CoreType;
    use crate::types::format::FormatConfig;
    use crate::types::identifiers::CardId;
    use crate::types::zones::Zone;

    fn pregame_color_cda() -> StaticDefinition {
        StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(vec![ContinuousModification::SetPregameChosenColor])
            .active_zones(vec![
                Zone::Library,
                Zone::Hand,
                Zone::Battlefield,
                Zone::Graveyard,
                Zone::Stack,
                Zone::Exile,
                Zone::Command,
            ])
            .cda()
    }

    fn commander_game() -> GameState {
        GameState::new(FormatConfig::commander(), 2, 42)
    }

    /// Place a Clara-like commander (carrying the printed pre-game color CDA) in
    /// the command zone. `is_commander` gates whether the orchestrator prompts.
    fn place_clara(state: &mut GameState, owner: PlayerId, is_commander: bool) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            "Clara Oswald".to_string(),
            Zone::Command,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.is_commander = is_commander;
        obj.base_static_definitions = Arc::new(vec![pregame_color_cda()]);
        id
    }

    #[test]
    fn detects_pregame_color_cda_modification() {
        let def = pregame_color_cda();
        assert!(def
            .modifications
            .iter()
            .any(|m| matches!(m, ContinuousModification::SetPregameChosenColor)));
        assert!(def.characteristic_defining);
    }

    #[test]
    fn orchestrator_prompts_seeds_and_advances() {
        // CR 103.2c + CR 903.4b: a commander with the linked color CDA is
        // prompted; the choice is seeded, an event is emitted, and the flow
        // advances past the pre-game color step.
        let mut state = commander_game();
        let clara = place_clara(&mut state, PlayerId(0), true);

        let wf = check_all_pregame_color_choices(&state).expect("Clara must be prompted");
        assert!(matches!(
            wf,
            WaitingFor::PregameChooseColor { commander_id, .. } if commander_id == clara
        ));

        state.waiting_for = wf;
        let mut events = Vec::new();
        let next = handle_choose_pregame_color(&mut state, ManaColor::Blue, &mut events);

        assert_eq!(
            state.pregame_chosen_colors.get(&clara),
            Some(&vec![ManaColor::Blue])
        );
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::PregameColorChosen {
                color: ManaColor::Blue,
                commander_id,
                ..
            } if *commander_id == clara
        )));
        assert!(
            !matches!(next, WaitingFor::PregameChooseColor { .. }),
            "flow must advance past the pre-game color step, got {next:?}"
        );
        assert!(
            check_all_pregame_color_choices(&state).is_none(),
            "no further pre-game color choices remain once seeded"
        );
    }

    #[test]
    fn non_commander_clara_is_not_prompted() {
        // CR 604.3a(5): the CDA is unconditional; the "if ~ is your commander"
        // gate is enforced by scanning only is_commander objects. A Clara sitting
        // in the 99 (not this player's commander) is never prompted.
        let mut state = commander_game();
        place_clara(&mut state, PlayerId(0), false);
        assert!(check_all_pregame_color_choices(&state).is_none());
    }
}
