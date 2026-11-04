use crate::game::printed_cards::apply_card_face_to_object;
use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::zones;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::zones::Zone;

/// The zone the copy is staged in until a following cast moves it to the stack.
///
/// CR 707.13 creates this copy *outside the game* (CR 400.11: outside the game
/// is not a zone). Every `GameObject` carries a `Zone` and the casting pipeline's
/// CR 601.2a origin is keyed on it, so the copy is staged in exile for the rest
/// of this resolution — and that staging is observationally equivalent to
/// "outside the game":
///
/// - The copy is CREATED in the staging zone (`zones::create_object`), never
///   moved there, so no zone-change event is emitted and no "whenever a card is
///   put into exile" ability can trigger on it.
/// - The only instruction that runs between creation and the end of the
///   resolution is the same-chain "you may cast the copy"; no player receives
///   priority mid-resolution, so nothing else can look at exile in between.
/// - CR 704.5e: "If a copy of a card is in any zone other than the stack or the
///   battlefield, it ceases to exist." An uncast copy is therefore gone at the
///   first state-based check, before any player gets priority, so no later
///   effect can count or interact with it.
///
/// Pinned by The War Doctor tests in
/// `tests/integration/create_card_copy_with_chosen_name.rs`.
const STAGING_ZONE: Zone = Zone::Exile;

/// CR 707.13: create a copy of a card defined by NAME (Garth One-Eye).
///
/// The named card need not be anywhere in the game, so there is no object to read
/// copiable values from (which is why `cast_copy_of_card.rs` cannot serve): the
/// player uses the Oracle card reference to determine the copy's
/// characteristics, which here is `state.card_face_registry` — the same registry
/// `conjure.rs` draws on for a card from outside the game.
///
/// The product is a COPY, not a conjured card. It is stamped `is_copy`, so the
/// CR 704.5e state-based action in `game::sba` removes it as soon as it is in any
/// zone but the stack or the battlefield. No SBA check runs during a resolution
/// (CR 704.3), so a following `CastFromZone { LastCreated }` in the same chain
/// still gets to cast it, and a copy nobody casts disappears on its own.
///
/// An unknown name creates nothing: a nameless, characteristic-less object would
/// be castable-looking and uncastable, which is worse than the effect doing
/// nothing visible.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (name, count) = match &ability.effect {
        Effect::CreateCardCopyByName { name, count } => (name.clone(), count.clone()),
        _ => {
            return Err(EffectError::MissingParam(
                "CreateCardCopyByName".to_string(),
            ))
        }
    };

    // CR 201.4: an absent literal name means "the card with the chosen name" —
    // the name this source committed through a preceding `Effect::Choose`.
    let card_name = name.or_else(|| {
        state
            .objects
            .get(&ability.source_id)
            .and_then(|source| source.chosen_card_name())
            .map(str::to_string)
    });

    // The registry is keyed by lowercase name (see `conjure.rs`). A name that is
    // not in it is one the engine has no characteristics for.
    let face = card_name.and_then(|card_name| {
        state
            .card_face_registry
            .get(&card_name.to_lowercase())
            .cloned()
    });
    let Some(face) = face else {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::CreateCardCopyByName,
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    };

    let copies = resolve_quantity_with_targets(state, &count, ability).max(0) as u32;
    let mut created_ids: Vec<ObjectId> = Vec::with_capacity(copies as usize);

    for _ in 0..copies {
        let obj_id = zones::create_object(
            state,
            CardId(0),
            ability.controller,
            face.name.clone(),
            STAGING_ZONE,
        );
        if let Some(obj) = state.objects.get_mut(&obj_id) {
            apply_card_face_to_object(obj, &face);
            // CR 707.10a: a copy of a card is not a card. `is_token` stays false
            // (CR 704.5d and CR 704.5e are different sweeps with different legal
            // zones), and `is_copy` is what CR 704.5e reads.
            obj.is_token = false;
            obj.is_copy = true;
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
