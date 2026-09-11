use crate::game::bending;
use crate::game::quantity::resolve_quantity_with_targets;
use crate::types::ability::{
    Effect, EffectError, EffectKind, QuantityExpr, QuantityRef, ResolvedAbility, TargetFilter,
    ThisWayCause,
};
use crate::types::events::{BendingType, GameEvent};
use crate::types::game_state::GameState;

pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let kind = match &ability.effect {
        Effect::RegisterBending { kind } => *kind,
        _ => return Err(EffectError::MissingParam("RegisterBending".to_string())),
    };

    if bend_performed(state, ability, kind) {
        bending::record_bending(state, events, kind, ability.source_id, ability.controller);
    }
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::RegisterBending,
        source_id: ability.source_id,
        subject: None,
    });
    Ok(())
}

/// Whether the bend this node registers actually happened.
///
/// CR 701.65b: a player airbends only when they exile one or more objects as a
/// result of the instruction to airbend. The airbend's exile publishes its
/// moved objects as the chain's "exiled this way" population — the same set the
/// sibling `{2}` cast permission is granted to (CR 701.65a) — so an airbend that
/// chose zero targets, or whose targets all left before resolution, exiled
/// nothing and is not a bend. Cause-bound, so an earlier producer merged into
/// the same chain set (a discard, a draw) cannot stand in for an exile. The set
/// is scoped to the resolution chain, not to this one instruction, so an
/// earlier exile in the same chain would also satisfy this check; no printed
/// airbend card has one.
///
/// CR 701.66b: an earthbend is registered after its delayed trigger is created,
/// which is exactly the node that precedes this one, so it always counts.
/// Firebend and waterbend are recorded by their own cost/mana paths; a node for
/// them registers unconditionally.
fn bend_performed(state: &GameState, ability: &ResolvedAbility, kind: BendingType) -> bool {
    match kind {
        BendingType::Air => {
            let exiled_this_way = QuantityExpr::Ref {
                qty: QuantityRef::FilteredTrackedSetSize {
                    filter: Box::new(TargetFilter::Any),
                    caused_by: Some(ThisWayCause::Exiled),
                },
            };
            resolve_quantity_with_targets(state, &exiled_this_way, ability) > 0
        }
        BendingType::Earth | BendingType::Fire | BendingType::Water => true,
    }
}
