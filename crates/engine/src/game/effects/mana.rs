use crate::types::ability::{effect_variant_name, Effect, EffectError, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::mana::{ManaColor, ManaType, ManaUnit};

/// Mana effect: adds mana to the controller's mana pool.
///
/// Reads from `Effect::Mana { produced: Vec<ManaColor> }`.
/// Each ManaColor in the produced list generates one mana unit.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let colors = match &ability.effect {
        Effect::Mana { produced } => produced,
        _ => return Err(EffectError::MissingParam("Produced".to_string())),
    };

    let player = state
        .players
        .iter_mut()
        .find(|p| p.id == ability.controller)
        .ok_or(EffectError::PlayerNotFound)?;

    for color in colors {
        let mana_type = mana_color_to_type(color);
        let unit = ManaUnit {
            color: mana_type,
            source_id: ability.source_id,
            snow: false,
            restrictions: Vec::new(),
        };
        player.mana_pool.add(unit);

        events.push(GameEvent::ManaAdded {
            player_id: ability.controller,
            mana_type,
            source_id: ability.source_id,
        });
    }

    events.push(GameEvent::EffectResolved {
        api_type: effect_variant_name(&ability.effect).to_string(),
        source_id: ability.source_id,
    });

    Ok(())
}

/// Convert a ManaColor to the runtime ManaType.
fn mana_color_to_type(color: &ManaColor) -> ManaType {
    match color {
        ManaColor::White => ManaType::White,
        ManaColor::Blue => ManaType::Blue,
        ManaColor::Black => ManaType::Black,
        ManaColor::Red => ManaType::Red,
        ManaColor::Green => ManaType::Green,
        ManaColor::Colorless => ManaType::Colorless,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;

    fn make_mana_ability(colors: Vec<ManaColor>) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Mana { produced: colors },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn produce_single_red_mana() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(vec![ManaColor::Red]),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 1);
        assert_eq!(state.players[0].mana_pool.total(), 1);
    }

    #[test]
    fn produce_multiple_of_same_color() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(vec![ManaColor::Green, ManaColor::Green, ManaColor::Green]),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 3);
    }

    #[test]
    fn produce_colorless() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(vec![ManaColor::Colorless, ManaColor::Colorless]),
            &mut events,
        )
        .unwrap();

        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            2
        );
    }

    #[test]
    fn produce_multi_color_fixed() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(vec![ManaColor::White, ManaColor::Blue]),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::White), 1);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 1);
        assert_eq!(state.players[0].mana_pool.total(), 2);
    }

    #[test]
    fn emits_mana_added_per_unit() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(vec![ManaColor::Red, ManaColor::Red]),
            &mut events,
        )
        .unwrap();

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

        resolve(
            &mut state,
            &make_mana_ability(vec![ManaColor::Green]),
            &mut events,
        )
        .unwrap();

        assert!(events.iter().any(
            |e| matches!(e, GameEvent::EffectResolved { api_type, .. } if api_type == "Mana")
        ));
    }

    #[test]
    fn empty_produced_adds_no_mana() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(&mut state, &make_mana_ability(vec![]), &mut events).unwrap();

        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    #[test]
    fn mana_units_track_source() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_mana_ability(vec![ManaColor::Red]),
            &mut events,
        )
        .unwrap();

        let unit = &state.players[0].mana_pool.mana[0];
        assert_eq!(unit.source_id, ObjectId(100));
    }
}
