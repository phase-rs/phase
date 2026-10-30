//! Reducer certification: play a proposed line on a cloned state and see
//! whether the game ends in the AI's favour.
//!
//! The driver answers only the AI's own prompts for the line it is playing —
//! aim every player target at the opponent, announce the X and pick the mode
//! the line committed to — and passes priority for the opponent. Anything
//! else (an opponent's choice, an optional trigger, a payment prompt) stops the
//! drive: a line the driver cannot finish on its own is not certified.
//!
//! Every AI priority domain the drive reads is filtered through the caller's
//! [`ActionAdmission`] against the simulated state it is read in, so a step
//! the real decision boundary would refuse at that point (the per-card cast
//! cap after earlier steps, say) is never certified.

use engine::ai_support::flat_priority_actions;
use engine::game::engine::apply_as_current_for_simulation;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::player::PlayerId;

use super::{issued_counterpart, ActionAdmission, LineStep};

/// Hard bound on reducer applies per drive. A line of a dozen spells with
/// their prompts and the passes that resolve them stays well inside it.
const MAX_DRIVE_ACTIONS: usize = 128;

/// Where a drive that played every step stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DriveStop {
    /// Once everyone has passed and the stack is empty (CR 117.4).
    StackSettled,
    /// At the AI's first priority after the last step, with whatever is on
    /// the stack still pending (CR 117.3c) — so the AI can still respond.
    AiPriority,
}

pub(super) enum DriveOutcome {
    /// The game ended with the AI as the winner.
    Won,
    /// The game ended in a draw: every remaining player lost at once
    /// (CR 104.4a). The opponent is out, but so is the AI.
    Drawn,
    /// Every step was played and the AI holds priority where the drive was
    /// asked to stop; the game goes on from this state.
    AtPriority(Box<GameState>),
    /// The drive could not continue without a decision it does not make, or a
    /// step was no longer available.
    Stuck,
}

/// Play `steps` from `sim`, resolving the stack between them as needed, and
/// report how the game stands at `stop`. A step is played only when the
/// admitted, engine-issued priority domain of the simulated state offers it.
pub(super) fn drive(
    mut sim: GameState,
    ai_player: PlayerId,
    opponent: PlayerId,
    steps: &[LineStep],
    stop: DriveStop,
    admission: ActionAdmission<'_>,
) -> DriveOutcome {
    let mut next = 0;
    let mut mode = None;
    let mut x = None;
    for _ in 0..MAX_DRIVE_ACTIONS {
        let action = match &sim.waiting_for {
            // CR 104.4a: a draw (no winner) is not a win.
            WaitingFor::GameOver { winner } => {
                return match winner {
                    Some(winner) if *winner == ai_player => DriveOutcome::Won,
                    None => DriveOutcome::Drawn,
                    Some(_) => DriveOutcome::Stuck,
                };
            }
            WaitingFor::Priority { player } if *player == ai_player => match steps.get(next) {
                Some(step) => {
                    let admitted: Vec<GameAction> = flat_priority_actions(&sim)
                        .into_iter()
                        .filter(|action| admission(&sim, action))
                        .collect();
                    match issued_counterpart(&admitted, &step.action) {
                        Some(issued) => {
                            next += 1;
                            mode = step.mode;
                            x = step.x;
                            issued
                        }
                        // CR 117.1a: a noninstant step waits for the stack to
                        // empty; CR 117.4 resolves it once everyone passes.
                        None if !sim.stack.is_empty() => GameAction::PassPriority,
                        None => return DriveOutcome::Stuck,
                    }
                }
                None if stop == DriveStop::StackSettled && !sim.stack.is_empty() => {
                    GameAction::PassPriority
                }
                None => return DriveOutcome::AtPriority(Box::new(sim)),
            },
            // The line assumes the opponent passes rather than responds
            // (CR 117.3d).
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            // CR 601.2c: every player target of the line is the opponent.
            WaitingFor::TargetSelection {
                player, selection, ..
            }
            | WaitingFor::TriggerTargetSelection {
                player, selection, ..
            } if *player == ai_player => {
                let face = TargetRef::Player(opponent);
                if !selection.current_legal_targets.contains(&face) {
                    return DriveOutcome::Stuck;
                }
                GameAction::ChooseTarget { target: Some(face) }
            }
            // CR 700.2a: the mode the line priced.
            WaitingFor::ModeChoice { player, .. } if *player == ai_player => match mode.take() {
                Some(index) => GameAction::SelectModes {
                    indices: vec![index],
                },
                None => return DriveOutcome::Stuck,
            },
            // CR 107.3a: the X the line announces — its maximum when the line
            // left it open.
            WaitingFor::ChooseXValue { player, max, .. } if *player == ai_player => {
                GameAction::ChooseX {
                    value: x.take().map_or(*max, |x: u32| x.min(*max)),
                }
            }
            _ => return DriveOutcome::Stuck,
        };
        if apply_as_current_for_simulation(&mut sim, action).is_err() {
            return DriveOutcome::Stuck;
        }
    }
    DriveOutcome::Stuck
}
