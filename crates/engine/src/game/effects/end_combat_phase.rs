use crate::game::effects::change_zone::{self, ZoneMoveResult};
use crate::types::ability::{EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, StackEntryKind};
use crate::types::phase::Phase;
use crate::types::zones::Zone;

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
    if !is_combat_phase(state.phase) {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::EndCombatPhase,
            source_id: ability.source_id,
        });
        return Ok(());
    }

    // CR 724.2a: Triggered abilities that triggered before this process began
    // but haven't been put on the stack yet cease to exist. Abilities that
    // trigger DURING this process (CR 724.2f) are created after this point and
    // ride to the stack through the following phase.
    state.pending_trigger = None;
    state.pending_trigger_entry = None;
    state.pending_trigger_order = None;
    state.pending_trigger_event_batch.clear();
    state.deferred_triggers.clear();

    // CR 724.2b: Exile every object on the stack. `resolve_top` already popped
    // this effect's own source entry before invoking the resolver; its
    // post-resolution routing also uses CR 724.2b and sends that resolving
    // object to exile. Here we exile every OTHER object still on the stack.
    // Spell entries are card-backed and move to exile through the shared
    // zone-change pipeline; ability entries (activated / triggered / keyword)
    // aren't represented by cards, so dropping the stack entry is sufficient
    // (they cease to exist at the next state-based-action check, CR 724.2b).
    while let Some(entry) = state.stack.pop_back() {
        state.stack_paid_facts.remove(&entry.id);
        if matches!(entry.kind, StackEntryKind::Spell { .. }) {
            match change_zone::execute_zone_move(
                state,
                entry.id,
                Zone::Stack,
                Zone::Exile,
                ability.source_id,
                None,
                false,
                false,
                None,
                &[],
                false,
                events,
            ) {
                ZoneMoveResult::Done => {}
                ZoneMoveResult::NeedsChoice(player) => {
                    state.waiting_for =
                        crate::game::replacement::replacement_choice_waiting_for(player, state);
                    return Ok(());
                }
                ZoneMoveResult::NeedsAuraAttachmentChoice => return Ok(()),
            }
        }
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

/// CR 506.1: The combat phase comprises five steps. CR 724.2g keys off whether
/// the current phase is one of them.
fn is_combat_phase(phase: Phase) -> bool {
    matches!(
        phase,
        Phase::BeginCombat
            | Phase::DeclareAttackers
            | Phase::DeclareBlockers
            | Phase::CombatDamage
            | Phase::EndCombat
    )
}
