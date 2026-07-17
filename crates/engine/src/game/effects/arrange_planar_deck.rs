use crate::game::quantity::resolve_quantity_with_targets;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};

/// CR 901.15 + CR 701.22a analogue: Look at the top N cards of the planar deck,
/// then put exactly `keep_on_top` of them on top in the submitted order with
/// the rest on the bottom in any order.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (look_count, keep_on_top) = match &ability.effect {
        Effect::ArrangePlanarDeckTop { count, keep_on_top } => (
            resolve_quantity_with_targets(state, count, ability).max(0) as usize,
            resolve_quantity_with_targets(state, keep_on_top, ability).max(0) as usize,
        ),
        _ => (0, 0),
    };

    let peek_count = look_count.min(state.planar_deck.len());
    if peek_count == 0 || keep_on_top > peek_count {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    }

    let cards: Vec<_> = state.planar_deck.iter().take(peek_count).copied().collect();

    state.waiting_for = WaitingFor::ArrangePlanarDeckTopChoice {
        player: ability.controller,
        cards,
        keep_on_top,
    };

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::game_object::GameObject;
    use crate::types::ability::QuantityExpr;
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_arrange_ability(count: i32, keep: i32) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::ArrangePlanarDeckTop {
                count: QuantityExpr::Fixed { value: count },
                keep_on_top: QuantityExpr::Fixed { value: keep },
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    fn make_deck_plane(state: &mut GameState, card_id: u32, name: &str) -> ObjectId {
        let id = ObjectId(state.next_object_id);
        state.next_object_id += 1;
        let mut obj = GameObject::new(
            id,
            CardId(u64::from(card_id)),
            PlayerId(0),
            name.to_string(),
            Zone::Command,
        );
        let mut card_type = CardType::default();
        card_type.core_types.push(CoreType::Plane);
        obj.card_types = card_type;
        obj.face_down = true;
        state.objects.insert(id, obj);
        id
    }

    #[test]
    fn arrange_planar_deck_top_sets_waiting_for() {
        let mut state = GameState::new_two_player(7);
        let p0 = PlayerId(0);
        state.format_config = crate::types::format::FormatConfig::planechase();
        let active = make_deck_plane(&mut state, 1, "Active Plane");
        if let Some(obj) = state.objects.get_mut(&active) {
            obj.face_down = false;
        }
        state.command_zone.push_back(active);
        let deck_a = make_deck_plane(&mut state, 2, "Deck A");
        let deck_b = make_deck_plane(&mut state, 3, "Deck B");
        state.planar_deck.push_back(deck_a);
        state.planar_deck.push_back(deck_b);
        state.planar_controller = Some(p0);
        let top_two = vec![deck_a, deck_b];

        let ability = make_arrange_ability(2, 1);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::ArrangePlanarDeckTopChoice {
                player,
                cards,
                keep_on_top,
            } => {
                assert_eq!(*player, p0);
                assert_eq!(*cards, top_two);
                assert_eq!(*keep_on_top, 1);
            }
            other => panic!("expected ArrangePlanarDeckTopChoice, got {other:?}"),
        }
    }
}
