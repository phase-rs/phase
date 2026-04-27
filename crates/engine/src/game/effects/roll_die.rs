use rand::Rng;

use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

use super::resolve_ability_chain;

/// CR 706: Roll a die and execute the matching result branch.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (sides, results) = match &ability.effect {
        Effect::RollDie { sides, results } => (*sides, results),
        _ => return Err(EffectError::MissingParam("RollDie".to_string())),
    };

    // CR 706.2: Roll the die using the game's seeded RNG.
    let result = state.rng.random_range(1..=sides);

    events.push(GameEvent::DieRolled {
        player_id: ability.controller,
        sides,
        result,
    });

    // CR 706.2: Find the matching result branch and resolve its effect.
    if let Some(branch) = results.iter().find(|b| result >= b.min && result <= b.max) {
        let sub = ResolvedAbility::new(
            *branch.effect.effect.clone(),
            ability.targets.clone(),
            ability.source_id,
            ability.controller,
        );
        resolve_ability_chain(state, &sub, events, 0)?;
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::RollDie,
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{AbilityDefinition, AbilityKind, DieResultBranch, QuantityExpr};
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;

    #[test]
    fn roll_die_emits_event_and_resolves_branch() {
        let mut state = GameState::new_two_player(42);
        let branch = DieResultBranch {
            min: 1,
            max: 20,
            effect: Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: crate::types::ability::TargetFilter::Controller,
                    up_to: false,
                },
            )),
        };
        let ability = ResolvedAbility::new(
            Effect::RollDie {
                sides: 20,
                results: vec![branch],
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        // Add a card to draw
        crate::game::zones::create_object(
            &mut state,
            crate::types::identifiers::CardId(1),
            PlayerId(0),
            "Card A".to_string(),
            crate::types::zones::Zone::Library,
        );

        let mut events = Vec::new();
        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());

        // Should have DieRolled event
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::DieRolled { sides: 20, .. })));
        // Branch covers 1-20, so it always matches — player drew a card
        assert_eq!(state.players[0].hand.len(), 1);
    }

    #[test]
    fn roll_die_no_matching_branch() {
        let mut state = GameState::new_two_player(42);
        // Branch only covers 21+ (impossible on d20), so no effect fires
        let branch = DieResultBranch {
            min: 21,
            max: 30,
            effect: Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: crate::types::ability::TargetFilter::Controller,
                    up_to: false,
                },
            )),
        };
        let ability = ResolvedAbility::new(
            Effect::RollDie {
                sides: 20,
                results: vec![branch],
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::DieRolled { .. })));
        assert_eq!(state.players[0].hand.len(), 0);
    }

    #[test]
    fn roll_die_without_branches() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::RollDie {
                sides: 6,
                results: vec![],
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        // Just emits the die rolled event with no branch resolution
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::DieRolled { sides: 6, .. })));
    }
}
