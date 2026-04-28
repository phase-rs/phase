use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::zones;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::zones::Zone;

pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (count, player_filter) = match &ability.effect {
        Effect::ExileTop { count, player } => (
            // Use resolve_quantity_with_targets so that TargetZoneCardCount (and
            // HalfRounded wrapping it) can resolve against the targeted player.
            resolve_quantity_with_targets(state, count, ability) as usize,
            player.clone(),
        ),
        _ => return Err(EffectError::MissingParam("ExileTop count".to_string())),
    };

    // CR 115.1: Mirror Draw/Mill/Discard — context-ref filters (Controller, etc.)
    // must consult state slots, not `ability.targets`. Otherwise a chained
    // sub-ability's "exile the top N cards of your library" would inherit the
    // parent's Player target and exile from the wrong library.
    let target_player = super::resolve_player_for_context_ref(state, ability, &player_filter);

    // CR 701.17b: A player can't mill/exile more cards than are in their library;
    // exile as many as possible.
    let player = state
        .players
        .iter()
        .find(|p| p.id == target_player)
        .ok_or(EffectError::PlayerNotFound)?;
    let count = count.min(player.library.len());
    let top_cards: Vec<_> = player
        .library
        .iter()
        .take(count)
        .copied()
        .collect::<Vec<_>>();

    for object_id in top_cards {
        zones::move_to_zone(state, object_id, Zone::Exile, events);
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ExileTop,
        source_id: ability.source_id,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{QuantityExpr, TargetFilter, TargetRef};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    fn make_exile_top_ability(count: u32) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::ExileTop {
                player: TargetFilter::Controller,
                count: QuantityExpr::Fixed {
                    value: count as i32,
                },
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn exile_top_moves_top_card_of_controller_library() {
        let mut state = GameState::new_two_player(42);
        let top = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Top".to_string(),
            Zone::Library,
        );
        let bottom = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bottom".to_string(),
            Zone::Library,
        );
        let ability = make_exile_top_ability(1);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects.get(&top).map(|obj| obj.zone),
            Some(Zone::Exile)
        );
        assert_eq!(
            state.objects.get(&bottom).map(|obj| obj.zone),
            Some(Zone::Library)
        );
    }

    #[test]
    fn exile_top_moves_multiple_cards() {
        let mut state = GameState::new_two_player(42);
        let top = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "First".to_string(),
            Zone::Library,
        );
        let second = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Second".to_string(),
            Zone::Library,
        );
        let third = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Third".to_string(),
            Zone::Library,
        );
        let ability = make_exile_top_ability(2);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects.get(&top).map(|obj| obj.zone),
            Some(Zone::Exile)
        );
        assert_eq!(
            state.objects.get(&second).map(|obj| obj.zone),
            Some(Zone::Exile)
        );
        assert_eq!(
            state.objects.get(&third).map(|obj| obj.zone),
            Some(Zone::Library)
        );
    }

    #[test]
    fn exile_top_controller_filter_does_not_inherit_parent_player_target() {
        // CR 115.1 regression: a chained ExileTop with `player: Controller`
        // must exile from the spell controller's library, not the parent's
        // inherited Player target.
        let mut state = GameState::new_two_player(42);
        let p0_top = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "P0 top".to_string(),
            Zone::Library,
        );
        let p1_top = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "P1 top".to_string(),
            Zone::Library,
        );

        let ability = ResolvedAbility::new(
            Effect::ExileTop {
                player: TargetFilter::Controller,
                count: QuantityExpr::Fixed { value: 1 },
            },
            vec![TargetRef::Player(PlayerId(1))], // inherited parent target
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects.get(&p0_top).map(|obj| obj.zone),
            Some(Zone::Exile),
            "P0's library top should be exiled (Controller filter resolves to caster)"
        );
        assert_eq!(
            state.objects.get(&p1_top).map(|obj| obj.zone),
            Some(Zone::Library),
            "P1's library must NOT be exiled — parent target inheritance must not override Controller filter"
        );
    }

    #[test]
    fn exile_top_with_empty_library_resolves_without_error() {
        let mut state = GameState::new_two_player(42);
        let ability = make_exile_top_ability(3);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::ExileTop,
                ..
            }
        )));
    }
}
