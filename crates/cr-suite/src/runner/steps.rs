//! Apply declarative scenario steps through production engine APIs.

use engine::game::scenario::GameRunner;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
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
        ScenarioStep::CastLightningBolt {
            spell,
            target_player,
            target_creature,
        } => {
            cast_lightning_bolt(
                runner,
                ctx,
                spell,
                *target_player,
                target_creature.as_deref(),
            )?;
        }
        ScenarioStep::ResolveTop => {
            if runner.state().stack.is_empty() {
                return Err(RunError::Step("ResolveTop: stack is empty".to_string()));
            }
            runner.resolve_top();
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

/// Cast Lightning Bolt through `GameAction::CastSpell` + target selection.
///
/// Mirrors `crates/engine/tests/integration/rules/sba.rs` so CR fixtures share
/// the production damage pipeline (`Effect::DealDamage`).
fn cast_lightning_bolt(
    runner: &mut GameRunner,
    ctx: &ScenarioContext,
    spell: &str,
    target_player: Option<u8>,
    target_creature: Option<&str>,
) -> Result<(), RunError> {
    let bolt_id = *ctx.handles.get(spell).ok_or_else(|| {
        RunError::Step(format!("CastLightningBolt: unknown spell handle {spell:?}"))
    })?;
    let bolt_card_id = runner
        .state()
        .objects
        .get(&bolt_id)
        .ok_or_else(|| RunError::Step(format!("CastLightningBolt: missing object {bolt_id:?}")))?
        .card_id;

    let result = runner
        .act(GameAction::CastSpell {
            object_id: bolt_id,
            card_id: bolt_card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .map_err(|e| RunError::Step(format!("CastSpell: {e}")))?;

    if matches!(result.waiting_for, WaitingFor::TargetSelection { .. }) {
        let target = match (target_player, target_creature) {
            (Some(p), None) => TargetRef::Player(PlayerId(p)),
            (None, Some(creature)) => {
                let id = ctx.handles.get(creature).ok_or_else(|| {
                    RunError::Step(format!(
                        "CastLightningBolt: unknown creature handle {creature:?}"
                    ))
                })?;
                TargetRef::Object(*id)
            }
            (Some(_), Some(_)) => {
                return Err(RunError::Step(
                    "CastLightningBolt: specify target_player XOR target_creature".into(),
                ));
            }
            (None, None) => {
                return Err(RunError::Step(
                    "CastLightningBolt: requires target_player or target_creature".into(),
                ));
            }
        };
        runner
            .act(GameAction::SelectTargets {
                targets: vec![target],
            })
            .map_err(|e| RunError::Step(format!("SelectTargets: {e}")))?;
    }

    Ok(())
}
