use crate::game::filter::{matches_target_filter, FilterContext};
use crate::game::zones;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::zones::Zone;

/// CR 702.84a: Exile cards from the top of the controller's library one at a time
/// until a card matching the filter is found. The hit card's ObjectId is injected
/// as a target into the sub_ability chain.
///
/// If the library is exhausted without a match, the sub_ability chain is skipped.
/// Miss cards remain in exile (specific cleanup is the sub_ability's responsibility).
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let filter = match &ability.effect {
        Effect::ExileFromTopUntil { filter } => filter,
        _ => return Err(EffectError::MissingParam("filter".to_string())),
    };

    let player = state
        .players
        .iter()
        .find(|p| p.id == ability.controller)
        .ok_or(EffectError::PlayerNotFound)?;

    // Snapshot library (top = index 0) to iterate without borrow conflicts.
    let library: Vec<ObjectId> = player.library.iter().copied().collect();
    let mut hit_id: Option<ObjectId> = None;

    // CR 107.3a + CR 601.2b: ability-context evaluation so dynamic thresholds
    // resolve against the resolving ability's `chosen_x`.
    let ctx = FilterContext::from_ability(ability);

    for &obj_id in &library {
        // CR 702.84a: Exile each card one at a time.
        zones::move_to_zone(state, obj_id, Zone::Exile, events);

        // Check if the just-exiled card matches the hit filter.
        if matches_target_filter(state, obj_id, filter, &ctx) {
            hit_id = Some(obj_id);
            break;
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ExileFromTopUntil,
        source_id: ability.source_id,
    });

    // CR 400.7: An object that moves from one zone to another becomes a new object.
    // If a hit was found and there is a sub_ability, resolve it with the hit card as target.
    if let (Some(hit), Some(ref sub)) = (hit_id, &ability.sub_ability) {
        let mut sub_clone = sub.as_ref().clone();
        sub_clone.targets = vec![TargetRef::Object(hit)];
        sub_clone.context = ability.context.clone();
        // Resolve the sub_ability chain directly — return early so the caller's
        // resolve_ability_chain does not double-chain the sub_ability.
        super::resolve_ability_chain(state, &sub_clone, events, 1)?;
    }

    Ok(())
}
