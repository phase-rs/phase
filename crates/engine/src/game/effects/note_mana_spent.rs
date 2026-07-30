use crate::types::ability::{ChosenAttribute, Effect, EffectError, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 106.1b + CR 602.2b + CR 608.2c: `Effect::NoteManaSpent` — record the mana
/// type(s) spent to pay this resolving ability's own activation cost onto its
/// source as `ChosenAttribute::NotedManaSpent` ("Note the type of mana spent to
/// pay this activation cost" — Jeweled Amulet, Ice Cauldron).
///
/// Composable building block: cost payment stays in the mana-payment funnel
/// (`pay_ability_mana_cost_with_choices_excluding_and_parent`, which stamps the
/// transient `GameObject::mana_spent_to_activate` latch); this effect is the
/// persistent writer, read back by `ManaProduction::NotedType`. Doing the write
/// at resolution — not at payment time — means a countered or otherwise
/// removed-from-stack ability never notes anything (CR 608.2c: instructions are
/// followed only on resolution).
///
/// "The last noted type" is singular per card, so this replaces any prior
/// `ChosenAttribute::NotedManaSpent` before pushing (replace-on-rechoose).
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    _events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::NoteManaSpent = &ability.effect else {
        return Ok(());
    };

    let Some(src) = state.objects.get(&ability.source_id) else {
        // CR 608.2c: the source has left the zone it was in — nothing to note.
        return Ok(());
    };
    let spent_types = src.mana_spent_to_activate.clone();

    let Some(src) = state.objects.get_mut(&ability.source_id) else {
        return Ok(());
    };
    src.chosen_attributes
        .retain(|a| !matches!(a, ChosenAttribute::NotedManaSpent(_)));
    src.chosen_attributes
        .push(ChosenAttribute::NotedManaSpent(spent_types));

    Ok(())
}
