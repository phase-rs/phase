use crate::types::ability::{Effect, EffectError, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::mana::{ManaType, ManaUnit};

/// Mana effect: adds mana to the controller's mana pool.
///
/// Reads from `Effect::Mana { produced, params }`:
///   - `produced` — mana color(s) to produce (e.g., "R", "W U", "C", "Combo R G")
///   - `params["Amount"]` — number of each mana unit to add (default 1)
///
/// Color codes: W=White, U=Blue, B=Black, R=Red, G=Green, C=Colorless.
/// "Combo X Y" means choose one from the listed colors (treated as first option
/// for AI/auto-resolution until player choice is implemented).
/// "Any" means any color (defaults to Colorless for auto-resolution).
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (produced, amount) = match &ability.effect {
        Effect::Mana {
            produced,
            params: effect_params,
        } => {
            let amt = effect_params
                .get("Amount")
                .map(|v| v.parse().unwrap_or(1))
                .unwrap_or(1u32);
            (produced.as_str(), amt)
        }
        _ => {
            let p = ability
                .params
                .get("Produced")
                .ok_or_else(|| EffectError::MissingParam("Produced".to_string()))?
                .as_str();
            let amt = ability
                .params
                .get("Amount")
                .map(|v| v.parse().unwrap_or(1))
                .unwrap_or(1u32);
            (p, amt)
        }
    };

    let colors = parse_produced(produced);

    let player = state
        .players
        .iter_mut()
        .find(|p| p.id == ability.controller)
        .ok_or(EffectError::PlayerNotFound)?;

    for color in &colors {
        for _ in 0..amount {
            let unit = ManaUnit {
                color: *color,
                source_id: ability.source_id,
                snow: false,
                restrictions: Vec::new(),
            };
            player.mana_pool.add(unit);

            events.push(GameEvent::ManaAdded {
                player_id: ability.controller,
                mana_type: *color,
                source_id: ability.source_id,
            });
        }
    }

    events.push(GameEvent::EffectResolved {
        api_type: ability.api_type().to_string(),
        source_id: ability.source_id,
    });

    Ok(())
}

/// Parse Forge's `Produced` param into a list of `ManaType` values.
///
/// Formats:
///   - Single color: "R" → [Red]
///   - Multiple fixed: "W U" → [White, Blue]
///   - Combo (choose one): "Combo R G" → [Red] (first option for auto-resolution)
///   - Any: "Any" → [Colorless]
///   - Colorless: "C" → [Colorless]
fn parse_produced(produced: &str) -> Vec<ManaType> {
    let trimmed = produced.trim();

    // Handle "Combo X Y Z" — choose one from listed colors
    if let Some(rest) = trimmed.strip_prefix("Combo ") {
        // For auto-resolution, produce the first listed color
        if let Some(first) = rest.split_whitespace().next() {
            if let Some(color) = parse_color_code(first) {
                return vec![color];
            }
        }
        return vec![ManaType::Colorless];
    }

    // Handle "Any" — any single color
    if trimmed.eq_ignore_ascii_case("Any") {
        return vec![ManaType::Colorless];
    }

    // Handle space-separated color codes: "W U B R G"
    trimmed
        .split_whitespace()
        .filter_map(parse_color_code)
        .collect()
}

/// Convert a single Forge color code to ManaType.
fn parse_color_code(code: &str) -> Option<ManaType> {
    match code {
        "W" => Some(ManaType::White),
        "U" => Some(ManaType::Blue),
        "B" => Some(ManaType::Black),
        "R" => Some(ManaType::Red),
        "G" => Some(ManaType::Green),
        "C" => Some(ManaType::Colorless),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;
    use std::collections::HashMap;

    fn make_mana_ability(produced: &str, amount: Option<u32>) -> ResolvedAbility {
        let mut params = HashMap::from([("Produced".to_string(), produced.to_string())]);
        if let Some(amt) = amount {
            params.insert("Amount".to_string(), amt.to_string());
        }
        ResolvedAbility::from_raw("Mana", params, vec![], ObjectId(100), PlayerId(0))
    }

    #[test]
    fn produce_single_red_mana() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(&mut state, &make_mana_ability("R", None), &mut events).unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 1);
        assert_eq!(state.players[0].mana_pool.total(), 1);
    }

    #[test]
    fn produce_multiple_amount() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(&mut state, &make_mana_ability("G", Some(3)), &mut events).unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 3);
    }

    #[test]
    fn produce_colorless() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(&mut state, &make_mana_ability("C", Some(2)), &mut events).unwrap();

        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            2
        );
    }

    #[test]
    fn produce_combo_uses_first_color() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability("Combo R G", None),
            &mut events,
        )
        .unwrap();

        // Combo auto-resolves to first listed color
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 1);
        assert_eq!(state.players[0].mana_pool.total(), 1);
    }

    #[test]
    fn produce_any_defaults_to_colorless() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(&mut state, &make_mana_ability("Any", None), &mut events).unwrap();

        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            1
        );
    }

    #[test]
    fn produce_multi_color_fixed() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(&mut state, &make_mana_ability("W U", None), &mut events).unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::White), 1);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 1);
        assert_eq!(state.players[0].mana_pool.total(), 2);
    }

    #[test]
    fn emits_mana_added_per_unit() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(&mut state, &make_mana_ability("R", Some(2)), &mut events).unwrap();

        let mana_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, GameEvent::ManaAdded { .. }))
            .collect();
        assert_eq!(mana_events.len(), 2);
    }

    #[test]
    fn emits_effect_resolved() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(&mut state, &make_mana_ability("G", None), &mut events).unwrap();

        assert!(events.iter().any(
            |e| matches!(e, GameEvent::EffectResolved { api_type, .. } if api_type == "Mana")
        ));
    }

    #[test]
    fn missing_produced_param_errors() {
        let mut state = GameState::new_two_player(42);
        let ability =
            ResolvedAbility::from_raw("Mana", HashMap::new(), vec![], ObjectId(100), PlayerId(0));
        let mut events = Vec::new();

        let result = resolve(&mut state, &ability, &mut events);
        assert!(matches!(result, Err(EffectError::MissingParam(_))));
    }

    #[test]
    fn mana_units_track_source() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(&mut state, &make_mana_ability("R", None), &mut events).unwrap();

        let unit = &state.players[0].mana_pool.mana[0];
        assert_eq!(unit.source_id, ObjectId(100));
    }

    #[test]
    fn parse_produced_single() {
        assert_eq!(parse_produced("R"), vec![ManaType::Red]);
        assert_eq!(parse_produced("W"), vec![ManaType::White]);
        assert_eq!(parse_produced("C"), vec![ManaType::Colorless]);
    }

    #[test]
    fn parse_produced_multi() {
        assert_eq!(
            parse_produced("W U B R G"),
            vec![
                ManaType::White,
                ManaType::Blue,
                ManaType::Black,
                ManaType::Red,
                ManaType::Green,
            ]
        );
    }

    #[test]
    fn parse_produced_combo() {
        assert_eq!(parse_produced("Combo R G"), vec![ManaType::Red]);
        assert_eq!(parse_produced("Combo W U B"), vec![ManaType::White]);
    }

    #[test]
    fn parse_produced_any() {
        assert_eq!(parse_produced("Any"), vec![ManaType::Colorless]);
    }
}
