//! Apply declarative scenario steps.

use engine::game::scenario::GameRunner;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::player::PlayerId;

use crate::assert::check_sbas_via_priority;
use crate::runner::{RunError, ScenarioContext};
use crate::schema::ScenarioStep;

pub fn apply_step(
    runner: &mut GameRunner,
    ctx: &mut ScenarioContext,
    step: &ScenarioStep,
) -> Result<(), RunError> {
    match step {
        ScenarioStep::PassPriority => {
            if matches!(runner.state().waiting_for, WaitingFor::GameOver { .. }) {
                return Ok(());
            }
            runner
                .act(GameAction::PassPriority)
                .map_err(|e| RunError::Step(format!("PassPriority: {e}")))?;
        }
        ScenarioStep::PassBoth => {
            runner.pass_both_players();
        }
        ScenarioStep::MarkDamage { creature, amount } => {
            let id = ctx.handles.get(creature).ok_or_else(|| {
                RunError::Step(format!("MarkDamage: unknown creature {creature:?}"))
            })?;
            let obj = runner
                .state_mut()
                .objects
                .get_mut(id)
                .ok_or_else(|| RunError::Step(format!("MarkDamage: missing object {id:?}")))?;
            obj.damage_marked = obj.damage_marked.saturating_add(*amount);
            // SBAs check after the next priority pass.
            let _ = check_sbas_via_priority(runner);
        }
        ScenarioStep::SetLife { player, life } => {
            let pid = PlayerId(*player);
            let p = runner
                .state_mut()
                .players
                .iter_mut()
                .find(|p| p.id == pid)
                .ok_or_else(|| RunError::Step(format!("SetLife: missing player {player}")))?;
            p.life = *life;
            let _ = check_sbas_via_priority(runner);
        }
        ScenarioStep::DamagePlayer { player, amount } => {
            let pid = PlayerId(*player);
            let p = runner
                .state_mut()
                .players
                .iter_mut()
                .find(|p| p.id == pid)
                .ok_or_else(|| RunError::Step(format!("DamagePlayer: missing player {player}")))?;
            p.life = p.life.saturating_sub(*amount);
            let _ = check_sbas_via_priority(runner);
        }
        ScenarioStep::AdvanceUntilStackEmpty => {
            if !runner.state().stack.is_empty() {
                runner.advance_until_stack_empty();
            }
        }
        ScenarioStep::CheckSbas => {
            check_sbas_via_priority(runner).map_err(RunError::Assertion)?;
        }
    }
    Ok(())
}
