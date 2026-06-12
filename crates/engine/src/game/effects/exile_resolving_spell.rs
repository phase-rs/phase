//! `Effect::ExileResolvingSpellInsteadOfGraveyard` — the "exile it instead of
//! putting it into a graveyard as it resolves" self-replacement rider applied by
//! a `WhenAPlayerCasts` trigger to the spell that caused the trigger (Rod of
//! Absorption).
//!
//! CR 614.1a + CR 608.2n: "instead" makes this a replacement effect that swaps
//! the resolving spell's normal CR 608.2n graveyard destination for exile.
//! CR 607.2b + CR 406.6: the exiled spell is recorded as "exiled with" the
//! trigger source so the source's linked ability ("cast any number of spells
//! from among cards exiled with this artifact") can refer to the accumulating
//! set.

use crate::game::targeting::extract_source_from_event;
use crate::types::ability::{EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::zones::Zone;

/// Stamp the per-object exile-instead-of-graveyard rider on the triggering spell
/// and link it to the trigger source.
///
/// The triggering spell is still on the stack when this effect resolves (the
/// trigger resolves above it), so this does NOT move the card — it sets the
/// marker the stack-resolution router reads when the spell finishes resolving.
/// The link source is stashed on the spell and turned into a real
/// `TrackedBySource` `ExileLink` only when the
/// spell actually reaches exile, so the linked set never lists a spell that was
/// countered or otherwise removed before it would have hit the graveyard.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 603.2 + CR 608.2c: the spell that caused the `WhenAPlayerCasts` trigger
    // is the trigger event's source object.
    let spell_id = state
        .current_trigger_event
        .as_ref()
        .and_then(extract_source_from_event);

    if let Some(spell_id) = spell_id {
        // CR 614.1a: only a spell still on the stack can be redirected as it
        // resolves; if it already left the stack (countered, fizzled) there is
        // nothing to replace.
        if let Some(obj) = state.objects.get_mut(&spell_id) {
            if obj.zone == Zone::Stack {
                // CR 607.2b: record the linking source so the eventual exile is
                // tracked as "exiled with [this source]". Presence of this
                // typed source is also the CR 614.1a exile-instead marker.
                obj.exile_from_stack_linked_source = Some(ability.source_id);
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ExileResolvingSpellInsteadOfGraveyard,
        source_id: ability.source_id,
    });
    Ok(())
}
