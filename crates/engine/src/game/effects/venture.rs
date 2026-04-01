use crate::game::dungeon::{
    self, available_dungeons, dungeon_sentinel_id, is_bottommost, next_rooms, room_name, DungeonId,
    VentureSource,
};
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::player::PlayerId;

/// CR 701.49: Venture into the dungeon.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::VentureIntoDungeon,
        source_id: ability.source_id,
    });
    resolve_venture_for_player(state, ability.controller, VentureSource::Normal, events)
}

/// CR 701.49d: Venture into a specific dungeon (e.g., "venture into the Undercity").
pub fn resolve_venture_into(
    state: &mut GameState,
    ability: &ResolvedAbility,
    dungeon: DungeonId,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::VentureInto,
        source_id: ability.source_id,
    });
    resolve_venture_for_player(
        state,
        ability.controller,
        VentureSource::Specific(dungeon),
        events,
    )
}

/// CR 725.2: Take the initiative.
///
/// CR 725.5: Re-taking initiative when you already have it still triggers
/// the venture into Undercity — the set is idempotent but the venture always fires.
pub fn resolve_take_initiative(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let player = ability.controller;

    // CR 725.3: Only one player can have the initiative at a time.
    state.initiative = Some(player);

    events.push(GameEvent::InitiativeTaken { player_id: player });
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::TakeTheInitiative,
        source_id: ability.source_id,
    });

    // CR 725.2: "Whenever a player takes the initiative, that player ventures into Undercity."
    resolve_venture_for_player(
        state,
        player,
        VentureSource::Specific(DungeonId::Undercity),
        events,
    )
}

/// Core venture logic implementing CR 701.49a-c.
///
/// Three cases:
/// 1. No active dungeon → choose a dungeon (or auto-select if only one option)
/// 2. Active dungeon, not at bottommost → advance to next room
/// 3. Active dungeon, at bottommost → complete dungeon, then choose a new one (CR 701.49c)
fn resolve_venture_for_player(
    state: &mut GameState,
    player: PlayerId,
    source: VentureSource,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let progress = state.dungeon_progress.entry(player).or_default();

    match progress.current_dungeon {
        None => {
            // CR 701.49a: No active dungeon — choose one from outside the game.
            enter_new_dungeon(state, player, source, events)
        }
        Some(dungeon_id) => {
            let current_room = progress.current_room;

            if is_bottommost(dungeon_id, current_room) {
                // CR 701.49c: At bottommost room — complete dungeon, then choose new one.
                // The dungeon is removed from the game (completing it), then a new dungeon
                // is chosen and the marker placed on the topmost room.
                complete_dungeon(state, player, dungeon_id, events);
                enter_new_dungeon(state, player, source, events)
            } else {
                // CR 701.49b: Advance to the next room.
                let next = next_rooms(dungeon_id, current_room);
                advance_to_room(state, player, dungeon_id, next, events)
            }
        }
    }
}

/// CR 701.49a: Choose a dungeon and enter its topmost room.
fn enter_new_dungeon(
    state: &mut GameState,
    player: PlayerId,
    source: VentureSource,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let options = available_dungeons(source);

    if options.len() == 1 {
        // Auto-select: only one dungeon available (e.g., initiative → Undercity).
        let dungeon = options[0];
        start_dungeon_and_enter(state, player, dungeon, events);
        Ok(())
    } else {
        // Multiple options — ask the player to choose.
        state.waiting_for = WaitingFor::ChooseDungeon { player, options };
        Ok(())
    }
}

/// Start a dungeon, enter room 0 (topmost), emit RoomEntered, and queue the room trigger.
/// CR 309.4a: Marker placed on topmost room.
/// CR 309.4c: Room ability triggers on entry.
fn start_dungeon_and_enter(
    state: &mut GameState,
    player: PlayerId,
    dungeon: DungeonId,
    events: &mut Vec<GameEvent>,
) {
    let progress = state.dungeon_progress.entry(player).or_default();
    progress.current_dungeon = Some(dungeon);
    progress.current_room = 0;

    events.push(GameEvent::RoomEntered {
        player_id: player,
        dungeon,
        room_index: 0,
        room_name: room_name(dungeon, 0).to_string(),
    });

    queue_room_trigger(state, player, dungeon, 0);
}

/// CR 309.7: Complete a dungeon — remove from game, record completion.
fn complete_dungeon(
    state: &mut GameState,
    player: PlayerId,
    dungeon: DungeonId,
    events: &mut Vec<GameEvent>,
) {
    let progress = state.dungeon_progress.entry(player).or_default();
    progress.current_dungeon = None;
    progress.current_room = 0;
    progress.completed.insert(dungeon);

    events.push(GameEvent::DungeonCompleted {
        player_id: player,
        dungeon,
    });
}

/// CR 701.49b / CR 309.5a: Advance to the next room.
/// If there's one option, auto-advance. If multiple, present choice.
fn advance_to_room(
    state: &mut GameState,
    player: PlayerId,
    dungeon: DungeonId,
    next: &[u8],
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    if next.len() == 1 {
        // Single path — auto-advance.
        let room = next[0];
        let progress = state.dungeon_progress.entry(player).or_default();
        progress.current_room = room;

        events.push(GameEvent::RoomEntered {
            player_id: player,
            dungeon,
            room_index: room,
            room_name: room_name(dungeon, room).to_string(),
        });

        // CR 309.4c: Queue room trigger.
        queue_room_trigger(state, player, dungeon, room);

        Ok(())
    } else {
        // Branch point — ask the player to choose.
        let option_names: Vec<String> = next
            .iter()
            .map(|&r| room_name(dungeon, r).to_string())
            .collect();

        state.waiting_for = WaitingFor::ChooseDungeonRoom {
            player,
            dungeon,
            options: next.to_vec(),
            option_names,
        };

        Ok(())
    }
}

/// CR 309.4c: Queue a room's triggered ability onto the pending trigger list.
/// Room abilities are triggered abilities ("When you move your venture marker
/// into this room, [effect].") — they go on the stack.
///
/// Uses a synthetic ObjectId (dungeon sentinel) so the SBA (CR 704.5t) can
/// identify pending room abilities when checking dungeon completion.
fn queue_room_trigger(state: &mut GameState, player: PlayerId, dungeon: DungeonId, room: u8) {
    let source_id = dungeon_sentinel_id(player);
    let name = room_name(dungeon, room);

    // Room effects are currently unimplemented — the trigger is queued
    // but resolves as a no-op. Individual room effects will be wired
    // as Effect variants are mapped to each room.
    let room_ability = ResolvedAbility::new(
        Effect::VentureIntoDungeon, // Placeholder — room effects TBD
        vec![],
        source_id,
        player,
    );

    // Push as a pending trigger so it goes on the stack properly.
    state.pending_trigger = Some(crate::game::triggers::PendingTrigger {
        source_id,
        controller: player,
        condition: None,
        ability: room_ability,
        timestamp: 0,
        target_constraints: Vec::new(),
        trigger_event: Some(GameEvent::RoomEntered {
            player_id: player,
            dungeon,
            room_index: room,
            room_name: name.to_string(),
        }),
        modal: None,
        mode_abilities: vec![],
        description: Some(format!("{}: {name}", dungeon::get_definition(dungeon).name)),
    });
}

/// Called by the engine handler when the player chooses a dungeon.
/// Delegates to `start_dungeon_and_enter` which handles state + events + room trigger.
pub fn handle_choose_dungeon(
    state: &mut GameState,
    player: PlayerId,
    dungeon: DungeonId,
    events: &mut Vec<GameEvent>,
) {
    start_dungeon_and_enter(state, player, dungeon, events);
}

/// Called by the engine handler when the player chooses a room at a branch point.
pub fn handle_choose_room(
    state: &mut GameState,
    player: PlayerId,
    dungeon: DungeonId,
    room_index: u8,
    events: &mut Vec<GameEvent>,
) {
    let progress = state.dungeon_progress.entry(player).or_default();
    progress.current_room = room_index;

    events.push(GameEvent::RoomEntered {
        player_id: player,
        dungeon,
        room_index,
        room_name: room_name(dungeon, room_index).to_string(),
    });

    // CR 309.4c: Queue room trigger.
    queue_room_trigger(state, player, dungeon, room_index);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::ResolvedAbility;
    use crate::types::identifiers::ObjectId;

    fn make_venture_ability(player: PlayerId) -> ResolvedAbility {
        ResolvedAbility::new(Effect::VentureIntoDungeon, vec![], ObjectId(99), player)
    }

    #[test]
    fn venture_with_no_dungeon_offers_afr_trio() {
        let mut state = GameState::new_two_player(42);
        let ability = make_venture_ability(PlayerId(0));
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Should present choice of 3 AFR dungeons.
        match &state.waiting_for {
            WaitingFor::ChooseDungeon { player, options } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(options.len(), 3);
                assert!(!options.contains(&DungeonId::Undercity));
            }
            other => panic!("Expected ChooseDungeon, got {other:?}"),
        }
    }

    #[test]
    fn venture_into_undercity_auto_selects() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::VentureInto {
                dungeon: DungeonId::Undercity,
            },
            vec![],
            ObjectId(99),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_venture_into(&mut state, &ability, DungeonId::Undercity, &mut events).unwrap();

        // Undercity is the only option → auto-selected, marker on room 0.
        let progress = &state.dungeon_progress[&PlayerId(0)];
        assert_eq!(progress.current_dungeon, Some(DungeonId::Undercity));
        assert_eq!(progress.current_room, 0);
    }

    #[test]
    fn venture_advances_through_linear_path() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);

        // Set up: already in Dungeon of the Mad Mage, room 0 (Yawning Portal).
        // Room 0 → [1] (single path), should auto-advance.
        let progress = state.dungeon_progress.entry(player).or_default();
        progress.current_dungeon = Some(DungeonId::DungeonOfTheMadMage);
        progress.current_room = 0;

        let ability = make_venture_ability(player);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Should auto-advance to room 1 (Dungeon Level).
        let progress = &state.dungeon_progress[&player];
        assert_eq!(progress.current_room, 1);
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::RoomEntered { room_index: 1, .. })));
    }

    #[test]
    fn venture_at_branch_presents_choice() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);

        // Set up: in Lost Mine, room 0 (Cave Entrance → [1, 2]).
        let progress = state.dungeon_progress.entry(player).or_default();
        progress.current_dungeon = Some(DungeonId::LostMineOfPhandelver);
        progress.current_room = 0;

        let ability = make_venture_ability(player);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::ChooseDungeonRoom {
                player: p,
                dungeon,
                options,
                option_names,
            } => {
                assert_eq!(*p, player);
                assert_eq!(*dungeon, DungeonId::LostMineOfPhandelver);
                assert_eq!(options, &[1, 2]);
                assert_eq!(option_names, &["Goblin Lair", "Mine Tunnels"]);
            }
            other => panic!("Expected ChooseDungeonRoom, got {other:?}"),
        }
    }

    #[test]
    fn venture_at_bottommost_completes_and_offers_new_dungeon() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);

        // Set up: in Lost Mine, room 6 (Temple of Dumathoin — bottommost).
        let progress = state.dungeon_progress.entry(player).or_default();
        progress.current_dungeon = Some(DungeonId::LostMineOfPhandelver);
        progress.current_room = 6;

        let ability = make_venture_ability(player);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Should have completed Lost Mine.
        let progress = &state.dungeon_progress[&player];
        assert!(progress
            .completed
            .contains(&DungeonId::LostMineOfPhandelver));

        // Should have emitted DungeonCompleted.
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::DungeonCompleted {
                dungeon: DungeonId::LostMineOfPhandelver,
                ..
            }
        )));

        // Should now offer choice for a new dungeon (normal venture → AFR trio).
        match &state.waiting_for {
            WaitingFor::ChooseDungeon { options, .. } => {
                assert_eq!(options.len(), 3);
            }
            other => panic!("Expected ChooseDungeon after completion, got {other:?}"),
        }
    }

    #[test]
    fn take_initiative_sets_designation_and_ventures() {
        let mut state = GameState::new_two_player(42);
        let ability =
            ResolvedAbility::new(Effect::TakeTheInitiative, vec![], ObjectId(99), PlayerId(0));
        let mut events = Vec::new();

        resolve_take_initiative(&mut state, &ability, &mut events).unwrap();

        // Initiative should be set.
        assert_eq!(state.initiative, Some(PlayerId(0)));

        // Should have ventured into Undercity (auto-selected).
        let progress = &state.dungeon_progress[&PlayerId(0)];
        assert_eq!(progress.current_dungeon, Some(DungeonId::Undercity));
        assert_eq!(progress.current_room, 0);

        // Should have emitted InitiativeTaken.
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::InitiativeTaken { .. })));
    }

    #[test]
    fn take_initiative_retake_still_ventures() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);

        // Already has initiative and is in Undercity at room 0.
        state.initiative = Some(player);
        let progress = state.dungeon_progress.entry(player).or_default();
        progress.current_dungeon = Some(DungeonId::Undercity);
        progress.current_room = 0;

        let ability = ResolvedAbility::new(Effect::TakeTheInitiative, vec![], ObjectId(99), player);
        let mut events = Vec::new();

        resolve_take_initiative(&mut state, &ability, &mut events).unwrap();

        // CR 725.5: Re-taking initiative still triggers venture.
        // Undercity room 0 → [1, 2] (branch), so should present choice.
        match &state.waiting_for {
            WaitingFor::ChooseDungeonRoom { dungeon, .. } => {
                assert_eq!(*dungeon, DungeonId::Undercity);
            }
            other => panic!("Expected ChooseDungeonRoom on retake, got {other:?}"),
        }
    }
}
