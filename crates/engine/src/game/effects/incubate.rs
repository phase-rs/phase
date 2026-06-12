use crate::game::effects::counters::{
    add_counter_with_replacement, stash_pending_counter_completion_with_actions,
};
use crate::game::game_object::DisplaySource;
use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::zones;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::card_type::CardType;
use crate::types::card_type::CoreType;
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, PendingCounterPostAction};
use crate::types::identifiers::CardId;
use crate::types::zones::Zone;

/// CR 701.53a: Incubate N — create an Incubator token that enters the
/// battlefield with N +1/+1 counters on it.
///
/// CR 111.10i: An Incubator token is a double-faced token. Its front face
/// is a colorless Incubator artifact with "{2}: Transform this token."
/// Its back face is a 0/0 colorless Phyrexian artifact creature named
/// "Phyrexian Token."
///
/// The transform activated ability is attached via `inject_predefined_token_abilities`.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let count_expr = match &ability.effect {
        Effect::Incubate { count } => count.clone(),
        _ => return Ok(()),
    };

    let controller = ability.controller;
    let n = resolve_quantity_with_targets(state, &count_expr, ability).max(0) as u32;

    // CR 701.53a: Create an Incubator token on the battlefield.
    let obj_id = zones::create_object(
        state,
        CardId(0),
        controller,
        "Incubator".to_string(),
        Zone::Battlefield,
    );

    if let Some(obj) = state.objects.get_mut(&obj_id) {
        obj.is_token = true;
        obj.display_source = DisplaySource::Token;
        // CR 111.10i: Front face is a colorless Incubator artifact.
        obj.card_types = CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Artifact],
            subtypes: vec!["Incubator".to_string()],
        };
        obj.base_card_types = obj.card_types.clone();
        obj.color = vec![];
        obj.base_color = vec![];
        // CR 400.7 + CR 302.6: Single authority for ETB state.
        obj.reset_for_battlefield_entry(state.turn_number);
    }

    // CR 701.53a: The Incubator enters with N +1/+1 counters.
    if n > 0
        && !add_counter_with_replacement(
            state,
            ability.controller,
            obj_id,
            CounterType::Plus1Plus1,
            n,
            events,
        )
    {
        stash_pending_counter_completion_with_actions(
            state,
            EffectKind::Incubate,
            ability.source_id,
            vec![PendingCounterPostAction::InjectPredefinedTokenAbilities { object_id: obj_id }],
        );
        return Ok(());
    }

    super::token::inject_predefined_token_abilities(state, obj_id);

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Incubate,
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{Effect, QuantityExpr};
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;

    fn make_incubate_ability(count: QuantityExpr) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Incubate { count },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn incubate_creates_artifact_token_with_counters() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();
        let ability = make_incubate_ability(QuantityExpr::Fixed { value: 3 });

        resolve(&mut state, &ability, &mut events).unwrap();

        // Should have created one artifact on the battlefield
        let incubators: Vec<_> = state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|obj| obj.card_types.subtypes.iter().any(|s| s == "Incubator"))
            .collect();
        assert_eq!(incubators.len(), 1);
        let inc = incubators[0];
        assert!(inc.is_token);
        assert!(inc.card_types.core_types.contains(&CoreType::Artifact));
        assert!(!inc.card_types.core_types.contains(&CoreType::Creature));
        assert!(inc.color.is_empty()); // colorless
        assert_eq!(inc.name, "Incubator");
        // 3 +1/+1 counters
        assert_eq!(inc.counters.get(&CounterType::Plus1Plus1).copied(), Some(3));
        assert_eq!(inc.abilities.len(), 1);
        assert!(matches!(*inc.abilities[0].effect, Effect::Transform { .. }));
        assert!(inc.back_face.is_some());
    }

    #[test]
    fn incubate_zero_creates_token_without_counters() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();
        let ability = make_incubate_ability(QuantityExpr::Fixed { value: 0 });

        resolve(&mut state, &ability, &mut events).unwrap();

        let incubators: Vec<_> = state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|obj| obj.card_types.subtypes.iter().any(|s| s == "Incubator"))
            .collect();
        assert_eq!(incubators.len(), 1);
        assert_eq!(
            incubators[0]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied(),
            None
        );
    }

    #[test]
    fn incubate_multiple_creates_separate_tokens() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        // Two separate incubate calls should create two tokens
        let ability = make_incubate_ability(QuantityExpr::Fixed { value: 2 });
        resolve(&mut state, &ability, &mut events).unwrap();
        resolve(&mut state, &ability, &mut events).unwrap();

        let incubators: Vec<_> = state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|obj| obj.card_types.subtypes.iter().any(|s| s == "Incubator"))
            .collect();
        assert_eq!(incubators.len(), 2);
    }
}
