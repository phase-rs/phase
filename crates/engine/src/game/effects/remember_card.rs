use crate::game::filter::{matches_target_filter, FilterContext};
use crate::game::targeting::resolve_tracked_set_sentinel;
use crate::types::ability::{ChosenAttribute, Effect, EffectError, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;

/// CR 608.2c + CR 613.1f: `Effect::RememberCard` — record the card chosen by a
/// preceding selection (typically `ChooseFromZone`) onto the resolving ability's
/// SOURCE as `ChosenAttribute::Card`. The companion Layer-6 static grant ("[this]
/// has all activated and triggered abilities of the last chosen card" — Koh, the
/// Face Stealer) then reads it via `TargetFilter::ChosenCard`.
///
/// Composable building block: `ChooseFromZone` owns the choice UI / zone search
/// and publishes its pick to the resolution chain's tracked set; this effect is
/// the persistent writer. The `target` filter names the chosen object(s) — Koh
/// passes `TrackedSet { id: TrackedSetId(0) }`, the sentinel for "the most recent
/// tracked set", bound here via [`resolve_tracked_set_sentinel`].
///
/// "The last chosen card" is singular: exactly one `ChosenAttribute::Card` is
/// stored, replacing any prior one (replace-on-rechoose), so the grant always
/// tracks the single latest choice. Storing it in `chosen_attributes` means it is
/// cleared automatically when the source changes zones (CR 400.7), which is
/// exactly the lifetime Koh's grant requires.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    _events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::RememberCard { target } = &ability.effect else {
        return Ok(());
    };

    // CR 608.2c: bind the `TrackedSetId(0)` sentinel to the resolution chain's
    // published pick (the cards chosen by the preceding `ChooseFromZone`).
    let resolved = resolve_tracked_set_sentinel(state, target.clone());
    let ctx = FilterContext::from_source(state, ability.source_id);
    let mut chosen: Vec<ObjectId> = state
        .objects
        .keys()
        .copied()
        .filter(|&id| matches_target_filter(state, id, &resolved, &ctx))
        .collect();
    // Deterministic order so a (degenerate) multi-pick records the most recent.
    chosen.sort_unstable_by_key(|id| id.0);

    let Some(&card_id) = chosen.last() else {
        // No card was chosen (e.g. an empty exile pool) — nothing to remember.
        return Ok(());
    };

    if let Some(src) = state.objects.get_mut(&ability.source_id) {
        // Replace-on-rechoose (CR 608.2c "the last chosen card"): a fresh choice
        // supersedes the prior one. `chosen_attributes` is cleared on zone change.
        src.chosen_attributes
            .retain(|a| !matches!(a, ChosenAttribute::Card(_)));
        src.chosen_attributes.push(ChosenAttribute::Card(card_id));
    }

    // CR 613.1f: the companion static grant reads `ChosenAttribute::Card` at layer
    // evaluation, which may have already run this turn — re-run so the grant takes
    // effect immediately.
    crate::game::layers::mark_layers_full(state);
    Ok(())
}
