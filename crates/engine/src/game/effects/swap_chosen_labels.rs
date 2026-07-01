use crate::types::ability::{ChosenAttribute, Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 311.7 + CR 607.2d / CR 607.2m (by analogy): resolve the symmetric per-player
/// anchor swap "each player who last chose `first` chooses `second`, and vice
/// versa" (Two Streams Facility's chaos ability).
///
/// Iterates every non-eliminated player and rewrites their durable
/// `ChosenAttribute::Label`: a `first`-anchor label becomes `second`, a
/// `second`-anchor label becomes `first`, and any other label (or absence) is
/// left untouched. The swap is symmetric and fans internally, so the effect
/// carries no `player_scope`. Comparison is case-insensitive so the stored
/// canonical label matches regardless of the parser's capitalization.
///
/// `player_last_chose_label` is the single read authority; here we mutate the
/// label directly (the write side of the same anchor subsystem), then re-run
/// layers because the anchor drives the land-drop static and the creature
/// anthem (CR 613.1).
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (first, second) = match &ability.effect {
        Effect::SwapChosenLabels { first, second } => (first.clone(), second.clone()),
        _ => {
            return Err(EffectError::InvalidParam(
                "expected SwapChosenLabels effect".to_string(),
            ))
        }
    };

    for player in state.players.iter_mut() {
        if player.is_eliminated {
            continue;
        }
        for attr in player.chosen_attributes.iter_mut() {
            if let ChosenAttribute::Label(label) = attr {
                if label.eq_ignore_ascii_case(&first) {
                    *label = second.clone();
                } else if label.eq_ignore_ascii_case(&second) {
                    *label = first.clone();
                }
            }
        }
    }

    // CR 613.1: the anchor labels gate the land-drop static and the creature
    // anthem; a swap changes both affected sets — recompute layers.
    crate::game::layers::mark_layers_full(state);

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::ResolvedAbility;
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;

    fn state_with_players() -> GameState {
        GameState {
            players: vec![
                crate::types::player::Player {
                    id: PlayerId(0),
                    ..Default::default()
                },
                crate::types::player::Player {
                    id: PlayerId(1),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn swap_flips_matching_labels_and_leaves_others() {
        let mut state = state_with_players();
        state.players[0]
            .chosen_attributes
            .push(ChosenAttribute::Label("Green anchor".to_string()));
        state.players[1]
            .chosen_attributes
            .push(ChosenAttribute::Label("Red waterfall".to_string()));

        let ability = ResolvedAbility::new(
            Effect::SwapChosenLabels {
                first: "Green anchor".to_string(),
                second: "Red waterfall".to_string(),
            },
            Vec::new(),
            ObjectId(0),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(crate::game::players::player_last_chose_label(
            &state,
            PlayerId(0),
            "Red waterfall"
        ));
        assert!(crate::game::players::player_last_chose_label(
            &state,
            PlayerId(1),
            "Green anchor"
        ));
    }

    #[test]
    fn swap_ignores_unrelated_label() {
        let mut state = state_with_players();
        state.players[0]
            .chosen_attributes
            .push(ChosenAttribute::Label("Something else".to_string()));

        let ability = ResolvedAbility::new(
            Effect::SwapChosenLabels {
                first: "Green anchor".to_string(),
                second: "Red waterfall".to_string(),
            },
            Vec::new(),
            ObjectId(0),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(crate::game::players::player_last_chose_label(
            &state,
            PlayerId(0),
            "Something else"
        ));
    }
}
