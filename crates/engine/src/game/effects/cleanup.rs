use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 514.3: Remove all damage marked on permanents and end "until end of turn" effects.
pub fn resolve(
    _state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 514.3a: "Until end of turn" and "this turn" effects end during cleanup.
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
        subject: None,
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

        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::Cleanup,
                ..
            }
        )));
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
