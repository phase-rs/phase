use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;

/// CR 701.36a: Populate — choose a creature token you control, then create a
/// token that's a copy of that creature token.
///
/// CR 701.36b: If you control no creature tokens when instructed to populate,
/// you won't create a token.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // Collect creature tokens the controller controls on the battlefield.
    let valid_tokens: Vec<ObjectId> = state
        .battlefield
        .iter()
        .filter_map(|&id| {
            let obj = state.objects.get(&id)?;
            if obj.controller == ability.controller
                && obj.is_token
                && obj.card_types.core_types.contains(&CoreType::Creature)
            {
                Some(id)
            } else {
                None
            }
        })
        .collect();

    match valid_tokens.len() {
        // CR 701.36b: No creature tokens → no-op.
        0 => {
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::Populate,
                source_id: ability.source_id,
            });
        }
        // Exactly one → auto-select, no player choice needed.
        1 => {
            create_token_copy(state, valid_tokens[0], ability, events)?;
        }
        // Multiple → player chooses which token to copy.
        _ => {
            state.waiting_for = WaitingFor::PopulateChoice {
                player: ability.controller,
                source_id: ability.source_id,
                valid_tokens,
            };
        }
    }

    Ok(())
}

/// Create a token copy of the selected creature token by delegating to
/// the existing `token_copy::resolve()` handler with a synthetic ability.
pub fn create_token_copy(
    state: &mut GameState,
    token_to_copy: ObjectId,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 707.2: Build a synthetic CopyTokenOf ability targeting the selected token.
    let copy_ability = ResolvedAbility::new(
        Effect::CopyTokenOf {
            target: TargetFilter::Any,
            enters_attacking: false,
            tapped: false,
        },
        vec![TargetRef::Object(token_to_copy)],
        ability.source_id,
        ability.controller,
    );
    super::token_copy::resolve(state, &copy_ability, events)?;

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Populate,
        source_id: ability.source_id,
    });

    Ok(())
}
