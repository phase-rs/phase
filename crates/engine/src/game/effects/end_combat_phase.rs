use crate::types::ability::{EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 724.2: End the combat phase. Mandate of Peace.
///
/// The steps mirror the "end the turn" procedure (CR 724.1, see
/// [`super::end_the_turn`]) but stop at the postcombat main phase instead of
/// the cleanup step:
/// - CR 724.2g: if it isn't a combat phase, nothing happens.
/// - CR 724.2a: triggered abilities that fired before this process but are not
///   yet on the stack cease to exist.
/// - CR 724.2b: exile every object on the stack, including the resolving object.
/// - CR 724.2c: check state-based actions (no priority, no new triggers stacked).
/// - CR 724.2d: remove everything from combat, expire "until end of combat"
///   effects, and skip straight to the postcombat main phase (CR 724.2e: the
///   end-of-combat step and its "at end of combat" triggers are skipped).
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 724.2g: If an effect attempts to end the combat phase at any time
    // that's not a combat phase, nothing happens.
    if !state.phase.is_combat() {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::EndCombatPhase,
            source_id: ability.source_id,
        });
        return Ok(());
    }

    super::end_phase::clear_preexisting_unstacked_triggers(state);

    if !super::end_phase::exile_nonresolving_stack_objects(state, ability.source_id, events) {
        return Ok(());
    }

    // CR 724.2c: Check state-based actions. No player gets priority and no
    // triggered abilities are put on the stack as part of this step.
    crate::game::sba::check_state_based_actions(state, events);

    // CR 724.2d: Remove everything from combat, expire "until end of combat"
    // effects, and skip straight to the postcombat main phase (CR 724.2e skips
    // the end-of-combat step and its triggers).
    crate::game::turns::end_combat_phase_to_postcombat(state, events);

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::EndCombatPhase,
        source_id: ability.source_id,
    });
    Ok(())
}
