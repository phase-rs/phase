//! CR 701.31 / CR 901.8: Resolver for the synthetic "planeswalking ability."
//!
//! CR 901.8 / CR 901.9c: when a player rolls the Planeswalker symbol on the
//! planar die, the planeswalking ability triggers and is put on the stack
//! (see `planechase::roll_planar_die`). On resolution, its controller — the
//! roller, CR 901.8 — planeswalks (CR 701.31).

use crate::game::replacement::{self, ReplacementResult};
use crate::types::ability::{EffectError, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::proposed_event::ProposedEvent;

/// CR 901.8 / CR 901.9c / CR 701.31: resolve the planeswalking ability — the
/// controller (the roller, CR 901.8) planeswalks (CR 701.31).
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 901.9c → CR 901.8: only the planeswalking ability (the planar-die
    // Planeswalker symbol) is "as a result of rolling the planar die" and thus
    // replaceable by Fixed Point in Time. Card-instruction, SBA, and leave-game
    // planeswalks (CR 701.31c) bypass the pipeline and are never replaced. We
    // discriminate structurally on the synthetic sentinel source (CR 901.8: the
    // ability "has no source", so it uses `planar_ability_sentinel_id`).
    if !crate::game::planechase::is_planar_ability_source(ability.source_id) {
        crate::game::planechase::planeswalk(state, ability.controller, events);
        return Ok(());
    }

    let proposed = ProposedEvent::planeswalk(ability.controller);
    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(_) => {
            // CR 614.1a: no replacement applied — the planeswalk happens.
            crate::game::planechase::planeswalk(state, ability.controller, events);
        }
        ReplacementResult::Prevented => {
            // CR 614.6: the planeswalk is fully replaced and never happens.
            // `apply_single_replacement`'s Prevented arm has already stashed the
            // shield's `runtime_execute` (chaos ensues) as a
            // `PostReplacementContinuation::Resolved` and emitted
            // `ReplacementApplied`. Drain it here EXACTLY ONCE so the substitute
            // fires in this same resolution step (mirrors
            // `draw_through_replacement`'s Execute-arm drain; `replace_event`
            // does not drain — the caller must). No planeswalk occurs.
            if state.has_post_replacement_drain() {
                let _ = crate::game::engine_replacement::apply_pending_post_replacement_effect(
                    state, None, None, None, events,
                );
            }
        }
        ReplacementResult::NeedsChoice(player) => {
            // Unreachable with current cards: a single mandatory candidate is
            // applied inline by `pipeline_loop` and never surfaces a choice.
            // Defensive only — parking on the choice in a resolution context is
            // safe (CR 616.1).
            state.waiting_for = replacement::replacement_choice_waiting_for(player, state);
        }
    }
    Ok(())
}
