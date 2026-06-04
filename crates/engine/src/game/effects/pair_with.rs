use crate::types::ability::{EffectError, EffectKind, ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;

pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let target_filter = ability.effect.target_filter().unwrap();
    let targets = if target_filter.is_context_ref() {
        crate::game::targeting::resolved_targets(ability, target_filter, state)
    } else {
        ability.targets.clone()
    };

    if let Some(target_id) = targets.iter().find_map(|target| match target {
        TargetRef::Object(id) => Some(*id),
        TargetRef::Player(_) => None,
    }) {
        pair_if_legal(state, ability, target_id);
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::PairWith,
            source_id: ability.source_id,
        });
        return Ok(());
    }

    let choices = crate::game::pairing::legal_pair_choices(
        state,
        ability.source_id,
        ability.controller,
        target_filter,
    );
    if choices.is_empty() {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::PairWith,
            source_id: ability.source_id,
        });
    } else {
        state.waiting_for = WaitingFor::PairChoice {
            player: ability.controller,
            source_id: ability.source_id,
            choices,
        };
    };

    Ok(())
}

pub(crate) fn pair_if_legal(state: &mut GameState, ability: &ResolvedAbility, target_id: ObjectId) {
    // CR 702.95c-d: Resolution re-checks both would-be partners. If either is
    // no longer an unpaired creature on the battlefield under the soulbond
    // ability controller's control, neither object becomes paired.
    if crate::game::pairing::is_unpaired_creature_you_control(
        state,
        ability.source_id,
        ability.controller,
    ) && crate::game::pairing::is_unpaired_creature_you_control(
        state,
        target_id,
        ability.controller,
    ) {
        crate::game::pairing::pair_objects(state, ability.source_id, target_id, ability.controller);
    }
}
