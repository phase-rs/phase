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

                // Apply tapped state for "onto the battlefield tapped" patterns.
                if tapped && destination == Zone::Battlefield {
                    obj.tapped = true;
                }
            }

            // Record battlefield entry for restriction tracking.
            if destination == Zone::Battlefield {
                crate::game::restrictions::record_battlefield_entry(state, obj_id);
                // Battlefield entry: incremental re-derive candidate for this
                // conjured object (escalates to Full if it sources effects/etc.).
                crate::game::layers::mark_layers_entered(state, obj_id);
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
