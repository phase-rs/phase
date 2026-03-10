use std::collections::HashMap;

use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, StackEntry, StackEntryKind, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::phase::Phase;
use crate::types::player::PlayerId;

use super::engine::EngineError;
use super::stack;

use crate::types::ability::ResolvedAbility;

/// Check whether a planeswalker's loyalty ability can be activated.
pub fn can_activate_loyalty(
    state: &GameState,
    planeswalker_id: ObjectId,
    player: PlayerId,
) -> bool {
    let obj = match state.objects.get(&planeswalker_id) {
        Some(o) => o,
        None => return false,
    };

    // Must be a planeswalker on the battlefield controlled by player
    if !obj.card_types.core_types.contains(&CoreType::Planeswalker) {
        return false;
    }
    if obj.zone != crate::types::zones::Zone::Battlefield {
        return false;
    }
    if obj.controller != player {
        return false;
    }

    // Once per turn
    if obj.loyalty_activated_this_turn {
        return false;
    }

    // Sorcery speed: main phase, empty stack, active player
    if !matches!(state.phase, Phase::PreCombatMain | Phase::PostCombatMain) {
        return false;
    }
    if !state.stack.is_empty() {
        return false;
    }
    if state.active_player != player {
        return false;
    }

    true
}

/// Activate a planeswalker loyalty ability.
///
/// Parses the loyalty cost from the ability text (e.g. "+1", "-3", "0"),
/// adjusts loyalty counters, marks activated this turn, and pushes
/// the ability onto the stack.
pub fn handle_activate_loyalty(
    state: &mut GameState,
    player: PlayerId,
    pw_id: ObjectId,
    ability_index: usize,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if !can_activate_loyalty(state, pw_id, player) {
        return Err(EngineError::ActionNotAllowed(
            "Cannot activate loyalty ability".to_string(),
        ));
    }

    let obj = state
        .objects
        .get(&pw_id)
        .ok_or_else(|| EngineError::InvalidAction("Planeswalker not found".to_string()))?;

    if ability_index >= obj.abilities.len() {
        return Err(EngineError::InvalidAction(
            "Invalid ability index".to_string(),
        ));
    }

    let ability_text = &obj.abilities[ability_index];
    let loyalty_cost = parse_loyalty_cost(ability_text);
    let current_loyalty = obj.loyalty.unwrap_or(0) as i32;

    // For minus abilities, must have enough loyalty
    if loyalty_cost < 0 && current_loyalty + loyalty_cost < 0 {
        return Err(EngineError::ActionNotAllowed(
            "Not enough loyalty to activate ability".to_string(),
        ));
    }

    // Parse the ability to get a ResolvedAbility for the stack
    let resolved = parse_pw_ability(ability_text, pw_id, player);

    // Adjust loyalty
    let new_loyalty = (current_loyalty + loyalty_cost).max(0) as u32;
    let obj = state.objects.get_mut(&pw_id).unwrap();
    obj.loyalty = Some(new_loyalty);
    obj.loyalty_activated_this_turn = true;

    // Emit counter events
    if loyalty_cost > 0 {
        events.push(GameEvent::CounterAdded {
            object_id: pw_id,
            counter_type: "Loyalty".to_string(),
            count: loyalty_cost as u32,
        });
    } else if loyalty_cost < 0 {
        events.push(GameEvent::CounterRemoved {
            object_id: pw_id,
            counter_type: "Loyalty".to_string(),
            count: (-loyalty_cost) as u32,
        });
    }

    // Push ability onto the stack
    let entry_id = ObjectId(state.next_object_id);
    state.next_object_id += 1;

    stack::push_to_stack(
        state,
        StackEntry {
            id: entry_id,
            source_id: pw_id,
            controller: player,
            kind: StackEntryKind::ActivatedAbility {
                source_id: pw_id,
                ability: resolved,
            },
        },
        events,
    );

    events.push(GameEvent::AbilityActivated { source_id: pw_id });
    state.priority_pass_count = 0;

    Ok(WaitingFor::Priority { player })
}

/// Parse the loyalty cost from an ability string.
///
/// Looks for patterns like:
/// - `AB$ ... | PW_Cost$ +1 | ...` -> +1
/// - `AB$ ... | PW_Cost$ -3 | ...` -> -3
/// - `AB$ ... | PW_Cost$ 0 | ...`  -> 0
///
/// Falls back to 0 if no PW_Cost found.
fn parse_loyalty_cost(ability_text: &str) -> i32 {
    for part in ability_text.split('|') {
        let trimmed = part.trim();
        if let Some(cost_str) = trimmed.strip_prefix("PW_Cost$") {
            let cost_str = cost_str.trim();
            return cost_str.parse::<i32>().unwrap_or(0);
        }
    }
    0
}

/// Parse a planeswalker ability text into a ResolvedAbility.
fn parse_pw_ability(
    ability_text: &str,
    source_id: ObjectId,
    controller: PlayerId,
) -> ResolvedAbility {
    // Try to parse via the standard parser; fall back to a minimal ability
    match crate::parser::ability::parse_ability(ability_text) {
        Ok(def) => ResolvedAbility {
            api_type: def.api_type().to_string(),
            params: def.params(),
            targets: Vec::new(),
            source_id,
            controller,
            sub_ability: None,
            svars: HashMap::new(),
        },
        Err(_) => ResolvedAbility {
            api_type: String::new(),
            params: HashMap::new(),
            targets: Vec::new(),
            source_id,
            controller,
            sub_ability: None,
            svars: HashMap::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;
    use crate::types::zones::Zone;

    fn setup() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        state
    }

    fn create_planeswalker(
        state: &mut GameState,
        owner: PlayerId,
        name: &str,
        loyalty: u32,
        abilities: Vec<String>,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Planeswalker);
        obj.loyalty = Some(loyalty);
        obj.abilities = abilities;
        obj.entered_battlefield_turn = Some(state.turn_number);
        id
    }

    #[test]
    fn activate_plus_loyalty_adds_counter() {
        let mut state = setup();
        let pw = create_planeswalker(
            &mut state,
            PlayerId(0),
            "Jace",
            3,
            vec!["AB$ Draw | PW_Cost$ +1 | NumCards$ 1".to_string()],
        );

        let mut events = Vec::new();
        let result = handle_activate_loyalty(&mut state, PlayerId(0), pw, 0, &mut events);

        assert!(result.is_ok());
        assert_eq!(state.objects[&pw].loyalty, Some(4)); // 3 + 1
        assert!(state.objects[&pw].loyalty_activated_this_turn);
        assert!(!state.stack.is_empty()); // ability on stack
    }

    #[test]
    fn activate_minus_loyalty_removes_counters() {
        let mut state = setup();
        let pw = create_planeswalker(
            &mut state,
            PlayerId(0),
            "Liliana",
            5,
            vec!["AB$ Destroy | PW_Cost$ -3 | ValidTgts$ Creature".to_string()],
        );

        let mut events = Vec::new();
        let result = handle_activate_loyalty(&mut state, PlayerId(0), pw, 0, &mut events);

        assert!(result.is_ok());
        assert_eq!(state.objects[&pw].loyalty, Some(2)); // 5 - 3
    }

    #[test]
    fn second_activation_same_turn_rejected() {
        let mut state = setup();
        let pw = create_planeswalker(
            &mut state,
            PlayerId(0),
            "Jace",
            3,
            vec!["AB$ Draw | PW_Cost$ +1 | NumCards$ 1".to_string()],
        );

        let mut events = Vec::new();
        // First activation succeeds
        handle_activate_loyalty(&mut state, PlayerId(0), pw, 0, &mut events).unwrap();
        // Clear stack so sorcery speed check passes
        state.stack.clear();

        // Second activation fails
        let result = handle_activate_loyalty(&mut state, PlayerId(0), pw, 0, &mut events);
        assert!(result.is_err());
    }

    #[test]
    fn loyalty_activation_resets_at_turn_start() {
        let mut state = setup();
        let pw = create_planeswalker(
            &mut state,
            PlayerId(0),
            "Jace",
            3,
            vec!["AB$ Draw | PW_Cost$ +1 | NumCards$ 1".to_string()],
        );

        // Activate loyalty
        let mut events = Vec::new();
        handle_activate_loyalty(&mut state, PlayerId(0), pw, 0, &mut events).unwrap();
        assert!(state.objects[&pw].loyalty_activated_this_turn);

        // Simulate turn start reset (what turns.rs start_next_turn should do)
        crate::game::turns::start_next_turn(&mut state, &mut events);

        // After turn starts, the flag should be reset for active player's permanents
        // Player 0's pw should reset when player 0's turn starts again
        // After one start_next_turn, active player is PlayerId(1), so p0's pw won't reset yet
        // After another start_next_turn, active player is PlayerId(0), so p0's pw resets
        crate::game::turns::start_next_turn(&mut state, &mut events);
        // Now active is p0, turn start should have reset loyalty_activated_this_turn
        // But we need to implement the reset in start_next_turn!
        // This test will FAIL until we add the reset logic.
        assert!(!state.objects[&pw].loyalty_activated_this_turn);
    }

    #[test]
    fn loyalty_activation_requires_sorcery_speed() {
        let mut state = setup();
        let pw = create_planeswalker(
            &mut state,
            PlayerId(0),
            "Jace",
            3,
            vec!["AB$ Draw | PW_Cost$ +1 | NumCards$ 1".to_string()],
        );

        // Not main phase
        state.phase = Phase::DeclareAttackers;
        let mut events = Vec::new();
        let result = handle_activate_loyalty(&mut state, PlayerId(0), pw, 0, &mut events);
        assert!(result.is_err());

        // Not active player
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(1);
        let result = handle_activate_loyalty(&mut state, PlayerId(0), pw, 0, &mut events);
        assert!(result.is_err());

        // Stack not empty
        state.active_player = PlayerId(0);
        state.stack.push(crate::types::game_state::StackEntry {
            id: ObjectId(99),
            source_id: ObjectId(99),
            controller: PlayerId(1),
            kind: StackEntryKind::Spell {
                card_id: CardId(99),
                ability: ResolvedAbility {
                    api_type: String::new(),
                    params: HashMap::new(),
                    targets: vec![],
                    source_id: ObjectId(99),
                    controller: PlayerId(1),
                    sub_ability: None,
                    svars: HashMap::new(),
                },
            },
        });
        let result = handle_activate_loyalty(&mut state, PlayerId(0), pw, 0, &mut events);
        assert!(result.is_err());
    }

    #[test]
    fn minus_ability_insufficient_loyalty_rejected() {
        let mut state = setup();
        let pw = create_planeswalker(
            &mut state,
            PlayerId(0),
            "Liliana",
            2,
            vec!["AB$ Destroy | PW_Cost$ -3 | ValidTgts$ Creature".to_string()],
        );

        let mut events = Vec::new();
        let result = handle_activate_loyalty(&mut state, PlayerId(0), pw, 0, &mut events);
        assert!(result.is_err());
    }

    #[test]
    fn parse_loyalty_cost_extracts_values() {
        assert_eq!(
            parse_loyalty_cost("AB$ Draw | PW_Cost$ +1 | NumCards$ 1"),
            1
        );
        assert_eq!(
            parse_loyalty_cost("AB$ Destroy | PW_Cost$ -3 | ValidTgts$ Creature"),
            -3
        );
        assert_eq!(parse_loyalty_cost("AB$ Mill | PW_Cost$ 0 | NumCards$ 3"), 0);
        assert_eq!(parse_loyalty_cost("AB$ Draw | NumCards$ 1"), 0); // no PW_Cost
    }
}
