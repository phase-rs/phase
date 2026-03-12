use crate::types::ability::{EffectKind, Effect, EffectError, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// Cleanup effect: clears transient card state after complex ability chains.
///
/// Reads typed bool fields from Effect::Cleanup:
///   - `clear_remembered` — clear remembered objects
///   - `clear_chosen_player` — clear chosen player
///   - `clear_chosen_color` — clear chosen color(s)
///   - `clear_chosen_type` — clear chosen type(s)
///   - `clear_chosen_card` — clear chosen card(s)
///   - `clear_imprinted` — clear imprinted cards
///   - `clear_triggers` — clear delayed triggers
///   - `clear_coin_flips` — clear coin flip results
///
/// Currently all are no-ops until transient state tracking is added to GameState.
pub fn resolve(
    _state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // Read typed fields for future implementation
    if let Effect::Cleanup {
        clear_remembered: _,
        clear_chosen_player: _,
        clear_chosen_color: _,
        clear_chosen_type: _,
        clear_chosen_card: _,
        clear_imprinted: _,
        clear_triggers: _,
        clear_coin_flips: _,
    } = &ability.effect
    {
        // When transient state tracking (remembered, chosen, imprinted) is added
        // to GameState/GameObject, this handler will clear those fields based
        // on the typed booleans above.
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;

    #[test]
    fn cleanup_emits_effect_resolved() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::Cleanup {
                clear_remembered: true,
                clear_chosen_player: false,
                clear_chosen_color: false,
                clear_chosen_type: false,
                clear_chosen_card: false,
                clear_imprinted: false,
                clear_triggers: false,
                clear_coin_flips: false,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events.iter().any(
            |e| matches!(e, GameEvent::EffectResolved { kind: EffectKind::Cleanup, .. })
        ));
    }

    #[test]
    fn cleanup_succeeds_with_no_flags_set() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::Cleanup {
                clear_remembered: false,
                clear_chosen_player: false,
                clear_chosen_color: false,
                clear_chosen_type: false,
                clear_chosen_card: false,
                clear_imprinted: false,
                clear_triggers: false,
                clear_coin_flips: false,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        assert!(resolve(&mut state, &ability, &mut events).is_ok());
    }

    #[test]
    fn cleanup_succeeds_with_multiple_flags() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::Cleanup {
                clear_remembered: true,
                clear_chosen_player: true,
                clear_chosen_color: false,
                clear_chosen_type: false,
                clear_chosen_card: true,
                clear_imprinted: false,
                clear_triggers: false,
                clear_coin_flips: false,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        assert!(resolve(&mut state, &ability, &mut events).is_ok());
        assert_eq!(events.len(), 1);
    }
}
