use crate::parser::ability::parse_ability;
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::mana::ManaType;
use crate::types::player::PlayerId;

use super::engine::EngineError;
use super::mana_payment;

/// Check if an ability string represents a mana ability (MTG Rule 605).
/// Mana abilities produce mana and resolve immediately without using the stack.
/// In Forge format, these have api_type "Mana".
pub fn is_mana_ability(ability_text: &str) -> bool {
    let Ok(def) = parse_ability(ability_text) else {
        return false;
    };
    def.api_type() == "Mana"
}

/// Map a Forge "Produced$" color code to ManaType.
fn produced_to_mana_type(produced: &str) -> ManaType {
    match produced {
        "W" => ManaType::White,
        "U" => ManaType::Blue,
        "B" => ManaType::Black,
        "R" => ManaType::Red,
        "G" => ManaType::Green,
        _ => ManaType::Colorless,
    }
}

/// Resolve a mana ability immediately without using the stack.
/// Taps the source if the cost includes "T", then produces the mana indicated
/// by the "Produced$" parameter.
pub fn resolve_mana_ability(
    state: &mut GameState,
    source_id: ObjectId,
    player: PlayerId,
    ability_text: &str,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    let def = parse_ability(ability_text)
        .map_err(|e| EngineError::InvalidAction(format!("Failed to parse ability: {}", e)))?;

    // Pay tap cost if present
    let has_tap_cost = matches!(def.cost, Some(crate::types::ability::AbilityCost::Tap));

    if has_tap_cost {
        let obj = state
            .objects
            .get(&source_id)
            .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
        if obj.tapped {
            return Err(EngineError::ActionNotAllowed(
                "Cannot activate tap ability: permanent is tapped".to_string(),
            ));
        }
        let obj = state.objects.get_mut(&source_id).unwrap();
        obj.tapped = true;
        events.push(GameEvent::PermanentTapped {
            object_id: source_id,
        });
    }

    // Determine produced mana color from the typed Effect
    let compat_params = def.params();
    let produced = compat_params
        .get("Produced")
        .cloned()
        .unwrap_or_else(|| "C".to_string());

    // Handle "Combo" (multi-color choice) -- produce first listed color for now
    let color_str = if produced.starts_with("Combo ") {
        produced
            .strip_prefix("Combo ")
            .and_then(|rest| rest.split_whitespace().next())
            .unwrap_or("C")
    } else {
        &produced
    };

    let mana_type = produced_to_mana_type(color_str);

    // Handle Amount$ parameter (default 1)
    let amount: u32 = compat_params
        .get("Amount")
        .and_then(|a| a.parse().ok())
        .unwrap_or(1);

    for _ in 0..amount {
        mana_payment::produce_mana(state, source_id, mana_type, player, events);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::identifiers::CardId;
    use crate::types::mana::ManaType;
    use crate::types::zones::Zone;

    #[test]
    fn mana_api_type_detected_as_mana_ability() {
        assert!(is_mana_ability(
            "AB$ Mana | Cost$ T | Produced$ G | SpellDescription$ Add {G}."
        ));
    }

    #[test]
    fn non_mana_api_type_not_detected() {
        assert!(!is_mana_ability(
            "AB$ DealDamage | Cost$ T | NumDmg$ 1 | ValidTgts$ Any"
        ));
    }

    #[test]
    fn draw_ability_is_not_mana_ability() {
        assert!(!is_mana_ability("AB$ Draw | Cost$ T | NumCards$ 1"));
    }

    #[test]
    fn resolve_mana_ability_produces_mana_and_taps() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Llanowar Elves".to_string(),
            Zone::Battlefield,
        );

        let mut events = Vec::new();
        resolve_mana_ability(
            &mut state,
            obj_id,
            PlayerId(0),
            "AB$ Mana | Cost$ T | Produced$ G | SpellDescription$ Add {G}.",
            &mut events,
        )
        .unwrap();

        // Object should be tapped
        assert!(state.objects.get(&obj_id).unwrap().tapped);

        // Player 0 should have 1 green mana
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 1);

        // Should have PermanentTapped + ManaAdded events
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::PermanentTapped { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::ManaAdded { .. })));
    }

    #[test]
    fn resolve_mana_ability_fails_if_already_tapped() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Llanowar Elves".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&obj_id).unwrap().tapped = true;

        let mut events = Vec::new();
        let result = resolve_mana_ability(
            &mut state,
            obj_id,
            PlayerId(0),
            "AB$ Mana | Cost$ T | Produced$ G",
            &mut events,
        );

        assert!(result.is_err());
    }

    #[test]
    fn resolve_mana_ability_colorless_produced() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Sol Ring".to_string(),
            Zone::Battlefield,
        );

        let mut events = Vec::new();
        resolve_mana_ability(
            &mut state,
            obj_id,
            PlayerId(0),
            "AB$ Mana | Cost$ T | Produced$ C | Amount$ 2",
            &mut events,
        )
        .unwrap();

        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            2
        );
    }
}
