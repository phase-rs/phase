use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, StackEntry, StackEntryKind, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::phase::Phase;
use crate::types::player::PlayerId;

use super::engine::EngineError;
use super::game_object::CounterType;
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

    let ability_def = &obj.abilities[ability_index];
    let loyalty_cost = parse_loyalty_cost(ability_def);
    let current_loyalty = obj.loyalty.unwrap_or(0) as i32;

    // For minus abilities, must have enough loyalty
    if loyalty_cost < 0 && current_loyalty + loyalty_cost < 0 {
        return Err(EngineError::ActionNotAllowed(
            "Not enough loyalty to activate ability".to_string(),
        ));
    }

    // Build a ResolvedAbility for the stack from the typed definition
    let resolved = build_pw_resolved(ability_def, pw_id, player);

    // Adjust loyalty
    // CR 306.5d: Loyalty abilities add/remove loyalty counters as a cost.
    // Sync both obj.loyalty (display) and obj.counters[Loyalty] (used by HasCounters condition).
    let new_loyalty = (current_loyalty + loyalty_cost).max(0) as u32;
    let obj = state.objects.get_mut(&pw_id).unwrap();
    obj.loyalty = Some(new_loyalty);
    obj.counters.insert(CounterType::Loyalty, new_loyalty);
    obj.loyalty_activated_this_turn = true;

    // Emit counter events
    if loyalty_cost > 0 {
        events.push(GameEvent::CounterAdded {
            object_id: pw_id,
            counter_type: crate::game::game_object::CounterType::Loyalty,
            count: loyalty_cost as u32,
        });
    } else if loyalty_cost < 0 {
        events.push(GameEvent::CounterRemoved {
            object_id: pw_id,
            counter_type: crate::game::game_object::CounterType::Loyalty,
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
    state.priority_passes.clear();
    state.priority_pass_count = 0;

    Ok(WaitingFor::Priority { player })
}

/// Extract the loyalty cost from a typed ability definition.
///
/// Uses `AbilityCost::Loyalty` (set by the JSON loader). Falls back to 0
/// if not present.
fn parse_loyalty_cost(ability_def: &crate::types::ability::AbilityDefinition) -> i32 {
    if let Some(crate::types::ability::AbilityCost::Loyalty { amount }) = &ability_def.cost {
        return *amount;
    }
    0
}

/// Build a ResolvedAbility from a typed AbilityDefinition for the stack.
fn build_pw_resolved(
    ability_def: &crate::types::ability::AbilityDefinition,
    source_id: ObjectId,
    controller: PlayerId,
) -> ResolvedAbility {
    ResolvedAbility::new(
        ability_def.effect.clone(),
        Vec::new(),
        source_id,
        controller,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter,
        TypedFilter,
    };
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

    /// Create a loyalty ability with the given cost and effect.
    fn make_loyalty_ability(loyalty_amount: i32, effect: Effect) -> AbilityDefinition {
        AbilityDefinition::new(AbilityKind::Activated, effect).cost(AbilityCost::Loyalty {
            amount: loyalty_amount,
        })
    }

    fn create_planeswalker(
        state: &mut GameState,
        owner: PlayerId,
        name: &str,
        loyalty: u32,
        abilities: Vec<crate::types::ability::AbilityDefinition>,
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
            vec![make_loyalty_ability(
                1,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                },
            )],
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
            vec![make_loyalty_ability(
                -3,
                Effect::Destroy {
                    target: TargetFilter::Typed(TypedFilter::creature()),
                    cant_regenerate: false,
                },
            )],
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
            vec![make_loyalty_ability(
                1,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                },
            )],
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
            vec![make_loyalty_ability(
                1,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                },
            )],
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
            vec![make_loyalty_ability(
                1,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                },
            )],
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
                ability: ResolvedAbility::new(
                    crate::types::ability::Effect::Unimplemented {
                        name: "Dummy".to_string(),
                        description: None,
                    },
                    vec![],
                    ObjectId(99),
                    PlayerId(1),
                ),
                cast_as_adventure: false,
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
            vec![make_loyalty_ability(
                -3,
                Effect::Destroy {
                    target: TargetFilter::Typed(TypedFilter::creature()),
                    cant_regenerate: false,
                },
            )],
        );

        let mut events = Vec::new();
        let result = handle_activate_loyalty(&mut state, PlayerId(0), pw, 0, &mut events);
        assert!(result.is_err());
    }

    #[test]
    fn parse_loyalty_cost_prefers_typed_ability_cost() {
        use crate::types::ability::{AbilityCost, AbilityKind, Effect};
        // When AbilityCost::Loyalty is set, it should be used
        let ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
            },
        )
        .cost(AbilityCost::Loyalty { amount: -3 });
        assert_eq!(parse_loyalty_cost(&ability), -3);
    }

    #[test]
    fn parse_loyalty_cost_defaults_to_zero_without_loyalty_cost() {
        use crate::types::ability::{AbilityKind, Effect};
        // When no AbilityCost::Loyalty, fall back to 0
        let ability = crate::types::ability::AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
            },
        );
        assert_eq!(parse_loyalty_cost(&ability), 0);
    }

    #[test]
    fn parse_loyalty_cost_extracts_values() {
        assert_eq!(
            parse_loyalty_cost(&make_loyalty_ability(
                1,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 }
                }
            )),
            1
        );
        assert_eq!(
            parse_loyalty_cost(&make_loyalty_ability(
                -3,
                Effect::Destroy {
                    target: crate::types::ability::TargetFilter::Any,
                    cant_regenerate: false,
                }
            )),
            -3
        );
        assert_eq!(
            parse_loyalty_cost(&make_loyalty_ability(
                0,
                Effect::Mill {
                    count: crate::types::ability::QuantityExpr::Fixed { value: 3 },
                    target: crate::types::ability::TargetFilter::Any,
                }
            )),
            0
        );
        // No loyalty cost
        let no_cost = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
            },
        );
        assert_eq!(parse_loyalty_cost(&no_cost), 0);
    }
}
