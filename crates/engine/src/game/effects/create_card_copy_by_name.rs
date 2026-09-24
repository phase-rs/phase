use crate::game::printed_cards::apply_card_face_to_object;
use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::zones;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::zones::Zone;

/// CR 707.12 + CR 201.2a: create a copy of a card identified by NAME.
///
/// The named card need not be anywhere in the game, so there is no object to read
/// copiable values from (which is why `cast_copy_of_card.rs` cannot serve): the
/// copy is materialized from `state.card_face_registry`, the same registry
/// `conjure.rs` draws on for a card from outside the game.
///
/// The product is a COPY, not a conjured card. It is stamped `is_copy`, so the
/// CR 704.5e state-based action in `game::sba` removes it as soon as it is in any
/// zone but the stack or the battlefield. No SBA check runs during a resolution
/// (CR 608.2), so a following `CastFromZone { LastCreated }` in the same chain
/// still gets to cast it — and a copy nobody casts disappears on its own rather
/// than leaving a stray card in exile forever.
///
/// An unknown name creates nothing: a nameless, characteristic-less object would
/// be castable-looking and uncastable, which is worse than the effect doing
/// nothing visible.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (name, destination, count) = match &ability.effect {
        Effect::CreateCardCopyByName {
            name,
            destination,
            count,
        } => (name.clone(), *destination, count.clone()),
        _ => {
            return Err(EffectError::MissingParam(
                "CreateCardCopyByName".to_string(),
            ))
        }
    };

    // CR 201.2a: an absent literal name means "the card with the chosen name" —
    // the name this source committed through a preceding `Effect::Choose`.
    let card_name = match name {
        Some(name) => Some(name),
        None => state
            .objects
            .get(&ability.source_id)
            .and_then(|source| source.chosen_card_name())
            .map(str::to_string),
    };
    let Some(card_name) = card_name else {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::CreateCardCopyByName,
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    };

    // The registry is keyed by lowercase name (see `conjure.rs`). A name that is
    // not in it is one the engine has no characteristics for.
    let Some(face) = state
        .card_face_registry
        .get(&card_name.to_lowercase())
        .cloned()
    else {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::CreateCardCopyByName,
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    };

    let copies = resolve_quantity_with_targets(state, &count, ability).max(0) as u32;
    let mut created_ids: Vec<ObjectId> = Vec::new();

    for _ in 0..copies {
        let obj_id = zones::create_object(
            state,
            CardId(0),
            ability.controller,
            face.name.clone(),
            destination,
        );

        // CR 613.7d: an object receives a timestamp when it enters a zone; only a
        // battlefield entry draws one in this engine's staging. Taken before the
        // `get_mut` borrow.
        let entry_timestamp = (destination == Zone::Battlefield).then(|| state.next_timestamp());

        if let Some(obj) = state.objects.get_mut(&obj_id) {
            apply_card_face_to_object(obj, &face);
            // CR 707.12a: the copy is not represented by a card. `is_token` stays
            // false (it is a copy of a card, not a token — CR 704.5d and CR 704.5e
            // are different sweeps with different legal zones), and `is_copy` is
            // what CR 704.5e reads.
            obj.is_token = false;
            obj.is_copy = true;

            if destination == Zone::Battlefield {
                obj.reset_for_battlefield_entry(
                    state.turn_number,
                    entry_timestamp.expect("battlefield entry draws a timestamp"),
                );
            }
        }

        if destination == Zone::Battlefield {
            crate::game::layers::mark_layers_entered(state, obj_id);
            // CR 400.7 + CR 608.2i: a copy created straight onto the battlefield is
            // a zone change from no zone, and enters-the-battlefield triggers have
            // to see it. Same authority `conjure.rs` routes through.
            crate::game::zones::record_and_emit_entry_from_no_zone(state, obj_id, events)
                .expect("copy object was just created");
        }

        created_ids.push(obj_id);
        events.push(GameEvent::CardCopyCreated {
            object_id: obj_id,
            name: face.name.clone(),
        });
    }

    // ASSIGNS (never appends), matching every other object producer: the
    // same-chain "the copy" / "it" referent is what this resolution created, and a
    // publication left over from an earlier clause must not leak into it.
    // `CastFromZone { LastCreated }` is what reads this.
    state.last_created_token_ids = created_ids;

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::CreateCardCopyByName,
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}
