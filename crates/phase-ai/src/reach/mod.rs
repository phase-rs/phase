//! Lethal reach — "can I burn the opponent out from here, and with what?"
//!
//! ## The defect this closes
//!
//! The AI scored spells one candidate at a time. Holding Seal of Fire, Seal of
//! Fire, and Flame Rift with three lands against an opponent at 6 life, it cast
//! both Seals — each looked efficient on its own — for 4 damage and left the
//! opponent alive, while Seal + Flame Rift was exactly lethal. A Seal already on
//! the battlefield, whose damage costs no mana at all, was not folded into any
//! total either, so the AI could spend mana on new copies of an effect it
//! already held for free.
//!
//! ## Shape
//!
//! 1. [`sources`] prices every engine-issued cast or activation by the life it
//!    takes from one opponent (and from the AI), the mana it commits, and when
//!    it can be started. A permanent in hand whose own activated ability deals
//!    damage once it resolves (a Seal in hand) is priced as cast-then-activate;
//!    a permanent already on the battlefield is priced at its activation cost
//!    alone — for a sacrifice-only ability, no mana.
//! 2. [`solver`] searches SUBSETS of those sources — never hand order — for the
//!    cheapest combination whose total meets the opponent's life while the AI
//!    survives it (CR 104.4a: a simultaneous loss is a draw, not a win).
//! 3. [`driver`] certifies a proposed line by playing it on a cloned state
//!    through the engine reducer, assuming the opponent passes priority. With
//!    objects on the stack the line is priced from the position the stack
//!    settles into, and only the reducer's settlement reads that position —
//!    no estimate made before the stack resolves prunes it. The
//!    engine — not this module — decides what each spell actually does
//!    (prevention, protection, replacement, "can't lose life"), so an estimate
//!    can only propose a line; only a reducer-won game commits one.
//!
//! The certified line is consumed at the decision boundary in `search`: at
//! priority the AI takes the line's next step instead of scoring candidates
//! one at a time, and at the line's own prompts (mode, X, target) it takes the
//! answer that keeps the line lethal. A lethal line therefore always outranks
//! every non-lethal one, however efficient the alternative looks in isolation.
//!
//! Only a line that wins the game is committed (CR 104.2a): with more than one
//! opponent left, eliminating one of them is a strategic choice this module
//! does not make.

mod driver;
mod solver;
mod sources;

#[cfg(test)]
mod tests;

use std::collections::HashMap;

use engine::ai_support::flat_priority_actions;
use engine::game::engine::apply_as_current_for_simulation;
use engine::game::players;
use engine::game::static_abilities::player_has_cant_lose_life;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, PendingCast, WaitingFor};
use engine::types::player::PlayerId;

use driver::{DriveOutcome, DriveStop};
use sources::ReachSource;

/// Most distinct candidate lines simulated per decision. Combinations are
/// tried cheapest-first, so the first few are the ones worth a reducer
/// simulation.
const MAX_CERTIFIED_LINES: usize = 4;

/// Most answers to one prompt simulated per decision — enough for every mode
/// pairing of a five-mode "choose two" spell.
const MAX_SIMULATED_ANSWERS: usize = 10;

/// The caller's per-action admission rule for engine-issued priority actions
/// (targeted-exchange and loop-safety gates in `search`). Re-applied to the
/// actions issued in a simulated, settled state so a line never relies on an
/// action the real decision boundary would refuse.
pub(crate) type ActionAdmission<'a> = &'a dyn Fn(&GameState, &GameAction) -> bool;

/// One engine action of a lethal line, plus the mode it commits to when the
/// action opens a modal spell (CR 700.2a) and the X it announces when its
/// cost has one (CR 107.3a).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LineStep {
    pub(crate) action: GameAction,
    pub(crate) mode: Option<usize>,
    pub(crate) x: Option<u32>,
}

/// A reducer-certified sequence of actions that wins the game from the state
/// it was found in.
#[derive(Debug, Clone)]
pub(crate) struct LethalLine {
    /// Empty when what the AI already has on the stack wins once it resolves.
    pub(crate) steps: Vec<LineStep>,
}

/// The action that advances a certified lethal line from this priority
/// decision, or `None` when no line wins from here.
///
/// `issued` is the admitted, engine-issued priority domain of `state`.
pub(crate) fn lethal_priority_action(
    state: &GameState,
    ai_player: PlayerId,
    issued: &[GameAction],
    admission: ActionAdmission<'_>,
) -> Option<GameAction> {
    let line = find_lethal_line(state, ai_player, issued, admission)?;
    match line.steps.first() {
        Some(step) => issued_counterpart(issued, &step.action)
            .or_else(|| pass_to_resolve_stack(state, issued)),
        None => pass_to_resolve_stack(state, issued),
    }
}

/// The answer to one of the AI's own cast/activation prompts (mode, X, or
/// target) that keeps a game-winning line alive, or `None` when no answer does.
///
/// Each candidate answer is applied to a clone, the cast is driven back to the
/// AI's priority, and the result must either have won already or still hold a
/// certified lethal line.
pub(crate) fn lethal_prompt_action(
    state: &GameState,
    ai_player: PlayerId,
    issued: &[GameAction],
    admission: ActionAdmission<'_>,
) -> Option<GameAction> {
    let opponent = sole_opponent(state, ai_player)?;
    if !pending_source_could_finish(state, ai_player, opponent) {
        return None;
    }
    let verdict =
        |answer: &GameAction| answer_verdict(state, ai_player, opponent, answer, admission);
    match &state.waiting_for {
        WaitingFor::ChooseXValue { player, .. } if *player == ai_player => {
            let certified = certified_pending_step(state, ai_player, admission)
                .and_then(|step| step.x)
                .map(|x| GameAction::ChooseX { value: x });
            certified
                .filter(|answer| issued.contains(answer) && verdict(answer) == AnswerVerdict::Wins)
                .or_else(|| x_answer(issued, verdict))
        }
        _ => prompt_answers(state, ai_player, opponent, issued)
            .into_iter()
            .take(MAX_SIMULATED_ANSWERS)
            .find(|answer| verdict(answer) == AnswerVerdict::Wins),
    }
}

/// What one answer to the AI's own cast prompt leads to, read by the reducer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnswerVerdict {
    /// The AI wins: at once, or by a certified line from the priority the
    /// answer returns it to.
    Wins,
    /// The opponent is taken out, but so is the AI (CR 104.4a).
    Overkill,
    /// The opponent survives, or the drive stopped on a decision it doesn't make.
    Short,
}

/// Apply `answer` to a clone, finish the announcement only as far as the
/// caster's next priority (CR 117.3c), and judge the result from there.
///
/// The cast is still on the stack at that priority, and so are the AI's
/// responses to it: a certified line may need one played above the cast before
/// it resolves (CR 117.4), so the position is certified as a priority decision
/// — settlement first, then the responses — rather than settled outright.
fn answer_verdict(
    state: &GameState,
    ai_player: PlayerId,
    opponent: PlayerId,
    answer: &GameAction,
    admission: ActionAdmission<'_>,
) -> AnswerVerdict {
    let mut sim = state.clone();
    if apply_as_current_for_simulation(&mut sim, answer.clone()).is_err() {
        return AnswerVerdict::Short;
    }
    let at_priority = match driver::drive(
        sim,
        ai_player,
        opponent,
        &[],
        DriveStop::AiPriority,
        admission,
    ) {
        DriveOutcome::Won => return AnswerVerdict::Wins,
        DriveOutcome::Drawn => return AnswerVerdict::Overkill,
        DriveOutcome::Stuck => return AnswerVerdict::Short,
        DriveOutcome::AtPriority(at_priority) => at_priority,
    };
    let issued = admitted_priority_actions(&at_priority, admission);
    if find_lethal_line(&at_priority, ai_player, &issued, admission).is_some() {
        return AnswerVerdict::Wins;
    }
    // No line wins from here; whether settling still takes the opponent out
    // along with the AI is what an X search bisects on.
    match driver::drive(
        *at_priority,
        ai_player,
        opponent,
        &[],
        DriveStop::StackSettled,
        admission,
    ) {
        DriveOutcome::Drawn => AnswerVerdict::Overkill,
        DriveOutcome::Won | DriveOutcome::AtPriority(_) | DriveOutcome::Stuck => {
            AnswerVerdict::Short
        }
    }
}

/// The step of a certified line that started the cast whose prompt is
/// pending — the line certified from the priority decision the cast was
/// started at, reconstructed by backing the cast out on a clone through the
/// reducer's own `CancelCast` at the X prompt. The prompt's own
/// answers judge only how the stack settles from here, and an X that wins only
/// with the rest of the stack still below it (a smaller X keeps the AI alive
/// until an older spell resolves, CR 117.4) is invisible to a bounded scan;
/// the line knows the X it was certified with. `None` when the cast cannot be
/// backed out or no certified line starts it.
fn certified_pending_step(
    state: &GameState,
    ai_player: PlayerId,
    admission: ActionAdmission<'_>,
) -> Option<LineStep> {
    let pending = pending_cast(state)?;
    let (object_id, ability_index) = (pending.object_id, pending.activation_ability_index);
    let mut decision = state.clone();
    apply_as_current_for_simulation(&mut decision, GameAction::CancelCast).ok()?;
    // Backing out records the cast as cancelled so the AI does not retry it
    // in this priority window; the decision the cast was started from had no
    // such entry, or the cast would not have been admitted.
    decision.cancelled_casts.retain(|id| *id != object_id);
    let issued = admitted_priority_actions(&decision, admission);
    find_lethal_line(&decision, ai_player, &issued, admission)?
        .steps
        .into_iter()
        .find(|step| match (&step.action, ability_index) {
            (
                GameAction::CastSpell {
                    object_id: cast, ..
                },
                None,
            ) => *cast == object_id,
            (
                GameAction::ActivateAbility {
                    source_id,
                    ability_index: index,
                },
                Some(pending_index),
            ) => *source_id == object_id && *index == pending_index,
            _ => false,
        })
}

/// CR 107.3a: the AI announces X for a spell or ability in its lethal line.
///
/// The maximum is judged first. When it takes the opponent out but the AI too
/// (damage to each player, CR 104.4a), the line the root certified used the
/// least X that is still lethal, since life paid grows with X along with the
/// damage — so that X is found by bisecting the whole issued range on "the
/// opponent is out", however wide the range is. Only then does a bounded
/// ascending scan cover answers whose effect is not monotone in X (a smaller
/// X can leave mana for the rest of the line).
fn x_answer(
    issued: &[GameAction],
    verdict: impl Fn(&GameAction) -> AnswerVerdict,
) -> Option<GameAction> {
    let mut values: Vec<u32> = issued
        .iter()
        .filter_map(|action| match action {
            GameAction::ChooseX { value } => Some(*value),
            _ => None,
        })
        .collect();
    values.sort_unstable();
    values.dedup();
    let &max = values.last()?;
    let mut judged: HashMap<u32, AnswerVerdict> = HashMap::new();
    let mut judge = |value: u32| {
        *judged
            .entry(value)
            .or_insert_with(|| verdict(&GameAction::ChooseX { value }))
    };

    match judge(max) {
        AnswerVerdict::Wins => return Some(GameAction::ChooseX { value: max }),
        AnswerVerdict::Overkill => {
            // Invariant: values[high] takes the opponent out.
            let (mut low, mut high) = (0, values.len() - 1);
            while low < high {
                let mid = (low + high) / 2;
                match judge(values[mid]) {
                    AnswerVerdict::Short => low = mid + 1,
                    AnswerVerdict::Wins | AnswerVerdict::Overkill => high = mid,
                }
            }
            if judge(values[high]) == AnswerVerdict::Wins {
                return Some(GameAction::ChooseX {
                    value: values[high],
                });
            }
        }
        AnswerVerdict::Short => {}
    }
    values
        .into_iter()
        .filter(|value| *value != max)
        .take(MAX_SIMULATED_ANSWERS)
        .find(|&value| judge(value) == AnswerVerdict::Wins)
        .map(|value| GameAction::ChooseX { value })
}

/// The cheap structural gate in front of [`lethal_prompt_action`]: the AI owes
/// one of its own cast prompts, has a sole opponent it could burn out, and the
/// object being cast can take life from them.
pub(crate) fn prompt_could_finish(state: &GameState, ai_player: PlayerId) -> bool {
    state.waiting_for.acting_player() == Some(ai_player)
        && sole_opponent(state, ai_player)
            .is_some_and(|opponent| pending_source_could_finish(state, ai_player, opponent))
}

/// Search for and certify a game-winning line from an AI priority decision.
pub(crate) fn find_lethal_line(
    state: &GameState,
    ai_player: PlayerId,
    issued: &[GameAction],
    admission: ActionAdmission<'_>,
) -> Option<LethalLine> {
    if !matches!(state.waiting_for, WaitingFor::Priority { player } if player == ai_player) {
        return None;
    }
    let opponent = sole_opponent(state, ai_player)?;

    let current = sources::reach_sources(state, ai_player, opponent, issued);
    if state.stack.is_empty() {
        return certify_cheapest(
            state,
            state,
            ai_player,
            opponent,
            &current,
            admission,
            &mut Vec::new(),
        );
    }

    // CR 117.4: with objects on the stack, the line is priced from the state
    // the stack settles into once every player passes — noninstant spells only
    // become castable there (CR 117.1a), and damage already on the stack has
    // landed. CR 608.2h: what that work does is determined only as it
    // resolves, so the settle itself is the authority; it is gated only on the
    // AI holding anything that could take life from the opponent at all.
    if !sources::holds_reach(state, ai_player, opponent, &current) {
        return None;
    }
    let mut attempted = Vec::new();
    let after_settling = match driver::drive(
        state.clone(),
        ai_player,
        opponent,
        &[],
        DriveStop::StackSettled,
        admission,
    ) {
        DriveOutcome::Won => return Some(LethalLine { steps: Vec::new() }),
        DriveOutcome::AtPriority(settled) => {
            let settled_issued = admitted_priority_actions(&settled, admission);
            let settled_sources =
                sources::reach_sources(&settled, ai_player, opponent, &settled_issued);
            certify_cheapest(
                state,
                &settled,
                ai_player,
                opponent,
                &settled_sources,
                admission,
                &mut attempted,
            )
        }
        DriveOutcome::Drawn | DriveOutcome::Stuck => None,
    };
    // Settling the pending work first does not win: it takes the AI out
    // (CR 104.4a), needs a decision the driver will not make on anyone's
    // behalf, or leaves the AI without the reach it held — the resolving
    // work can destroy the very source a response needed (CR 704.5g). What
    // the AI can still do from here is respond (CR 117.3c: it keeps priority
    // after each cast or activation). The pending work is part of the
    // position those responses are played from, not something they have to
    // cover on their own, so they are certified as lines from this state:
    // first those lethal on their own against the opponent's present life,
    // then — the pending damage not being priced (CR 608.2h) — every
    // affordable response, the reducer deciding where each ends. Every search
    // here draws on the one certification budget.
    after_settling
        .or_else(|| {
            certify_cheapest(
                state,
                state,
                ai_player,
                opponent,
                &current,
                admission,
                &mut attempted,
            )
        })
        .or_else(|| {
            certify_responses(
                state,
                ai_player,
                opponent,
                &current,
                admission,
                &mut attempted,
            )
        })
}

/// Propose combinations priced against `priced` (the state whose life totals
/// and mana the sources were read from) and certify each from `origin` (the
/// real decision state), cheapest first. `attempted` holds the step sequences
/// already simulated for this decision; it bounds them to
/// [`MAX_CERTIFIED_LINES`] across every search that shares it.
fn certify_cheapest(
    origin: &GameState,
    priced: &GameState,
    ai_player: PlayerId,
    opponent: PlayerId,
    sources: &[ReachSource],
    admission: ActionAdmission<'_>,
    attempted: &mut Vec<Vec<LineStep>>,
) -> Option<LethalLine> {
    let opponent_life = life_of(priced, opponent);
    // Damage alone has to be able to get there before mana is worth pricing.
    let scales_with_x = sources.iter().any(|source| source.loss.opponent.per_x > 0);
    let fixed: u32 = sources
        .iter()
        .map(|source| source.loss.opponent.fixed)
        .sum();
    if !scales_with_x && fixed < opponent_life {
        return None;
    }
    let sources: Vec<_> = sources
        .iter()
        .filter_map(|source| source.priced(priced, ai_player))
        .collect();
    let budget = solver::Budget {
        opponent_life,
        controller_life: life_of(priced, ai_player),
        mana: sources::mana_capacity(priced, ai_player),
    };
    certify_combinations(
        origin,
        ai_player,
        opponent,
        &sources,
        solver::lethal_combinations(&sources, &budget),
        admission,
        attempted,
    )
}

/// Certify the AI's responses to work already on the stack: every affordable
/// combination of `sources`, most damage first, with no requirement that the
/// responses alone reach the opponent's present life.
fn certify_responses(
    origin: &GameState,
    ai_player: PlayerId,
    opponent: PlayerId,
    sources: &[ReachSource],
    admission: ActionAdmission<'_>,
    attempted: &mut Vec<Vec<LineStep>>,
) -> Option<LethalLine> {
    let priced: Vec<_> = sources
        .iter()
        .filter_map(|source| source.priced(origin, ai_player))
        .collect();
    let combinations =
        solver::response_combinations(&priced, sources::mana_capacity(origin, ai_player));
    certify_combinations(
        origin,
        ai_player,
        opponent,
        &priced,
        combinations,
        admission,
        attempted,
    )
}

/// Play each combination's steps through the reducer from `origin`, in order,
/// and return the first that wins. Copies of one repeatable activation make
/// many combinations that play out identically; each distinct step sequence is
/// certified once.
fn certify_combinations(
    origin: &GameState,
    ai_player: PlayerId,
    opponent: PlayerId,
    sources: &[sources::PricedSource],
    combinations: Vec<solver::Combination>,
    admission: ActionAdmission<'_>,
    attempted: &mut Vec<Vec<LineStep>>,
) -> Option<LethalLine> {
    for combination in combinations {
        let steps = solver::line_steps(sources, &combination);
        if attempted.contains(&steps) {
            continue;
        }
        if attempted.len() == MAX_CERTIFIED_LINES {
            break;
        }
        if matches!(
            driver::drive(
                origin.clone(),
                ai_player,
                opponent,
                &steps,
                DriveStop::StackSettled,
                admission
            ),
            DriveOutcome::Won
        ) {
            return Some(LethalLine { steps });
        }
        attempted.push(steps);
    }
    None
}

/// CR 104.2a: burning out an opponent wins the game only when they are the
/// last opponent left. CR 119.8: an opponent who can't lose life can't be
/// burned out at all.
fn sole_opponent(state: &GameState, ai_player: PlayerId) -> Option<PlayerId> {
    match players::opponents(state, ai_player)[..] {
        [opponent] if !player_has_cant_lose_life(state, opponent) => Some(opponent),
        _ => None,
    }
}

fn life_of(state: &GameState, player: PlayerId) -> u32 {
    state.players[player.0 as usize].life.max(0) as u32
}

/// The engine-issued priority domain of a (simulated) state, filtered through
/// the caller's admission rule.
fn admitted_priority_actions(state: &GameState, admission: ActionAdmission<'_>) -> Vec<GameAction> {
    flat_priority_actions(state)
        .into_iter()
        .filter(|action| admission(state, action))
        .collect()
}

/// The issued action that performs `step` — matched by the object and ability
/// it starts rather than by exact payload, so a line found in one state still
/// recognises its cast when the engine issues it with a different payment
/// route in another.
pub(crate) fn issued_counterpart(issued: &[GameAction], step: &GameAction) -> Option<GameAction> {
    issued
        .iter()
        .find(|action| match (action, step) {
            (
                GameAction::CastSpell { object_id: a, .. },
                GameAction::CastSpell { object_id: b, .. },
            ) => a == b,
            (
                GameAction::ActivateAbility {
                    source_id: a,
                    ability_index: i,
                },
                GameAction::ActivateAbility {
                    source_id: b,
                    ability_index: j,
                },
            ) => a == b && i == j,
            _ => *action == step,
        })
        .cloned()
}

/// CR 117.4: when the line's next step is not available yet, the AI's own
/// objects on the stack have to resolve first — pass so they do.
fn pass_to_resolve_stack(state: &GameState, issued: &[GameAction]) -> Option<GameAction> {
    (!state.stack.is_empty() && issued.contains(&GameAction::PassPriority))
        .then_some(GameAction::PassPriority)
}

/// The issued target and mode answers to the AI's own pending cast prompt
/// that a lethal line could use, in issued order. X is [`x_answer`]'s.
fn prompt_answers(
    state: &GameState,
    ai_player: PlayerId,
    opponent: PlayerId,
    issued: &[GameAction],
) -> Vec<GameAction> {
    match &state.waiting_for {
        // CR 601.2c: aim at the opponent.
        WaitingFor::TargetSelection { player, .. } if *player == ai_player => issued
            .iter()
            .filter(|action| {
                matches!(
                    action,
                    GameAction::ChooseTarget {
                        target: Some(TargetRef::Player(target)),
                    } if *target == opponent
                )
            })
            .cloned()
            .collect(),
        // CR 700.2a: any issued mode may be the damaging one.
        WaitingFor::ModeChoice { player, .. } if *player == ai_player => issued
            .iter()
            .filter(|action| matches!(action, GameAction::SelectModes { .. }))
            .cloned()
            .collect(),
        _ => Vec::new(),
    }
}

/// The structural gate in front of the prompt simulation: the spell or ability
/// whose prompt is pending must be able to take life from `opponent`. How much
/// is the simulation's to read (CR 608.2h).
fn pending_source_could_finish(state: &GameState, ai_player: PlayerId, opponent: PlayerId) -> bool {
    pending_cast(state)
        .and_then(|pending| state.objects.get(&pending.object_id))
        .is_some_and(|object| sources::object_reaches(state, ai_player, opponent, object))
}

/// The cast or activation whose mode, X, or target prompt is pending.
fn pending_cast(state: &GameState) -> Option<&PendingCast> {
    match &state.waiting_for {
        WaitingFor::TargetSelection { pending_cast, .. }
        | WaitingFor::ModeChoice { pending_cast, .. }
        | WaitingFor::ChooseXValue { pending_cast, .. } => Some(pending_cast),
        _ => None,
    }
}
