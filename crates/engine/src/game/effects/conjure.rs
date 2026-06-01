use crate::game::printed_cards::apply_card_face_to_object;
use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::zones;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::CardId;
use crate::types::zones::Zone;

/// Digital-only keyword action (no CR entry): Conjure creates a card from outside
/// the game and places it into a specified zone. Unlike tokens, conjured cards are
/// "real" cards with full card characteristics (mana value, types, abilities, etc.).
///
/// The handler looks up the named card from `state.card_face_registry` (populated
/// at game init by `rehydrate_game_from_card_db`) and applies full characteristics
/// via `apply_card_face_to_object`.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (cards, destination, tapped) = match &ability.effect {
        Effect::Conjure {
            cards,
            destination,
            tapped,
        } => (cards, *destination, *tapped),
        _ => return Ok(()),
    };

    for conjure_card in cards {
        let count =
            resolve_quantity_with_targets(state, &conjure_card.count, ability).max(0) as u32;

        // Look up the card face data from the registry (populated at game init).
        let card_face = state
            .card_face_registry
            .get(&conjure_card.name.to_lowercase())
            .cloned();

        for _ in 0..count {
            let obj_id = zones::create_object(
                state,
                CardId(0),
                ability.controller,
                conjure_card.name.clone(),
                destination,
            );

            if let Some(obj) = state.objects.get_mut(&obj_id) {
                // Conjured cards are real cards, not tokens.
                obj.is_token = false;

                // Apply full card characteristics from the database if available.
                if let Some(ref face) = card_face {
                    apply_card_face_to_object(obj, face);
                }

                if destination == Zone::Battlefield {
                    // CR 302.6: A creature entering the battlefield has summoning
                    // sickness unless its controller has controlled it continuously
                    // since their most recent turn began. A conjured permanent is a
                    // brand-new object, so it must run the same entry reset (summoning
                    // sickness, marked damage, per-turn activation flags) as any other
                    // battlefield entry — otherwise a conjured creature could attack or
                    // tap for {T} costs the turn it appears. Delegate to the single
                    // authority rather than setting flags ad hoc.
                    obj.reset_for_battlefield_entry(state.turn_number);

                    // Apply tapped state for "onto the battlefield tapped" patterns.
                    if tapped {
                        obj.tapped = true;
                    }
                }
            }

            // Record battlefield entry for restriction tracking.
            if destination == Zone::Battlefield {
                crate::game::restrictions::record_battlefield_entry(state, obj_id);
                // Battlefield entry: incremental re-derive candidate for this
                // conjured object (escalates to Full if it sources effects/etc.).
                crate::game::layers::mark_layers_entered(state, obj_id);

                // CR 603.6a: Conjuring places a card from outside the game
                // directly onto the battlefield — a zone change from `None`.
                // Emit `ZoneChanged { from: None, to: Battlefield }` (in addition to
                // `ObjectConjured`, which animation/logging consumers still read) so
                // every enters-the-battlefield triggered ability fires through the
                // same matcher path used for normal entries and token creation
                // (e.g. Verdant Dread's "another Verdant Dread enters" manifest-dread
                // trigger, Soul Warden, Panharmonicon). Without this the conjured
                // permanent enters silently and no ETB ability ever triggers.
                let zone_change_record = state
                    .objects
                    .get(&obj_id)
                    .expect("conjured object was just created")
                    .snapshot_for_zone_change(obj_id, None, Zone::Battlefield);
                state
                    .zone_changes_this_turn
                    .push(zone_change_record.clone());
                events.push(GameEvent::ZoneChanged {
                    object_id: obj_id,
                    from: None,
                    to: Zone::Battlefield,
                    record: Box::new(zone_change_record),
                });
            }

            events.push(GameEvent::ObjectConjured {
                object_id: obj_id,
                name: conjure_card.name.clone(),
            });
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Conjure,
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{ConjureCard, QuantityExpr};
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;

    #[test]
    fn battlefield_conjure_records_zone_change_for_turn_history() {
        let mut state = GameState::new_two_player(7);
        let ability = ResolvedAbility::new(
            Effect::Conjure {
                cards: vec![ConjureCard {
                    name: "Verdant Dread".to_string(),
                    count: QuantityExpr::Fixed { value: 1 },
                }],
                destination: Zone::Battlefield,
                tapped: false,
            },
            vec![],
            ObjectId(99),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let zone_change = events
            .iter()
            .find_map(|event| match event {
                GameEvent::ZoneChanged {
                    object_id,
                    from,
                    to,
                    ..
                } => Some((*object_id, *from, *to)),
                _ => None,
            })
            .expect("conjuring onto the battlefield emits ZoneChanged");

        assert_eq!(zone_change.1, None);
        assert_eq!(zone_change.2, Zone::Battlefield);
        assert_eq!(state.zone_changes_this_turn.len(), 1);
        assert_eq!(state.zone_changes_this_turn[0].object_id, zone_change.0);
        assert_eq!(state.zone_changes_this_turn[0].from_zone, None);
        assert_eq!(state.zone_changes_this_turn[0].to_zone, Zone::Battlefield);
    }
}
