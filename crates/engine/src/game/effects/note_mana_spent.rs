use crate::types::ability::{ChosenAttribute, Effect, EffectError, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 106.1b + CR 602.2b + CR 608.2c: `Effect::NoteManaSpent` — record the mana
/// type(s) spent to pay this resolving ability's own activation cost onto its
/// source as `ChosenAttribute::NotedManaSpent` ("Note the type of mana spent to
/// pay this activation cost" — Jeweled Amulet). Scoped to the singular-type
/// wording only: Ice Cauldron's sibling "note the type AND AMOUNT..." needs an
/// exact stored multiset plus a spend restriction, which this building block
/// does not model — that text is intentionally left unmatched by the parser
/// and still reports `Effect::unimplemented`.
///
/// Composable building block: cost payment stays in the mana-payment funnel;
/// `push_ability_entry` (the single authority where an activated ability
/// reaches the stack) snapshots what was spent, paired with the source's
/// incarnation at that moment, directly onto THIS activation's own
/// `ResolvedAbility::noted_mana_payment` (issue #6504) — never a per-object
/// mutable field, so a permanent untapped and reactivated with a different
/// payment while this ability still sits unresolved on the stack cannot
/// corrupt what this instance observed. This effect is the persistent writer,
/// read back by `ManaProduction::NotedType`. Doing the write at resolution —
/// not at payment time — means a countered or otherwise removed-from-stack
/// ability never notes anything (CR 608.2c: instructions are followed only on
/// resolution).
///
/// "The last noted type" is singular per card, so this replaces any prior
/// `ChosenAttribute::NotedManaSpent` before pushing (replace-on-rechoose).
///
/// CR 400.7: a source that leaves and returns (bounce/flicker) while this
/// SAME activation is still unresolved on the stack becomes a new object at
/// the same storage id — a new incarnation with no memory of the old
/// payment. Refuses to write unless the object's current incarnation still
/// matches `noted_mana_payment.source_incarnation`, mirroring the engine's
/// existing incarnation-pairing idiom (`ResolvedAbility::source_is_current`,
/// `TargetFilter::SelfRef` resolution).
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    _events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::NoteManaSpent = &ability.effect else {
        return Ok(());
    };

    let Some(payment) = ability.noted_mana_payment.as_ref() else {
        // Nothing was captured at activation (e.g. the cost had no mana
        // component to observe) — nothing to note.
        return Ok(());
    };

    let Some(src) = state.objects.get(&ability.source_id) else {
        // CR 608.2c: the source has left the zone it was in — nothing to note.
        return Ok(());
    };
    if src.incarnation != payment.source_incarnation {
        // CR 400.7: this activation's payment was captured on a prior
        // incarnation of this object (bounced/flickered since). The CURRENT
        // incarnation never paid anything itself — nothing to note.
        return Ok(());
    }
    let spent_types = payment.types.clone();

    let Some(src) = state.objects.get_mut(&ability.source_id) else {
        return Ok(());
    };
    src.chosen_attributes
        .retain(|a| !matches!(a, ChosenAttribute::NotedManaSpent(_)));
    src.chosen_attributes
        .push(ChosenAttribute::NotedManaSpent(spent_types));

    Ok(())
}
