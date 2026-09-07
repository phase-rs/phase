use rand::Rng;
use std::collections::HashSet;

use crate::game::quantity::resolve_quantity;
use crate::game::replacement::{self, ReplacementResult};
use crate::types::ability::{
    DieRollIgnoreRule, DieRollModifier, Effect, EffectError, EffectKind, ResolvedAbility,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::proposed_event::ProposedEvent;
use crate::types::resolution::{DieRollContinuation, PendingDieRoll, PendingDieRollInstruction};

use super::resolve_ability_chain;
use crate::game::ability_utils::build_resolved_from_def_with_targets;

/// CR 706.2: Draw one natural result from the game's seeded RNG — "the number
/// indicated on the top face of the die before any modifiers."
///
/// Touches ONLY the RNG: it emits no `GameEvent::DieRolled` and does not stamp
/// `die_result_this_resolution`. Rolling is separated from emission and stamping
/// because a die's fate is not known when it is rolled: under a CR 706.6 ignore
/// replacement (Barbarian Class, Pixie Guide, Wyll) the caller must roll every
/// die, decide which are ignored from the NATURALS, and only THEN emit
/// `DieRolled` — carrying the post-modifier value — for the survivors. An
/// ignored roll "is considered to have never happened" (CR 706.6), so it must
/// never reach the event log, which is also what keeps every events-slice reader
/// correct: the die-roll trigger layer, `game/contraptions.rs`, and
/// `game/effects/effect.rs`'s CR 611.2d snapshot.
///
/// `sides == 0` names no die and no distribution. `rand`'s `random_range`
/// asserts on an empty range, so it would panic mid-resolution; callers are
/// expected to filter that case out first (`execute_roll` treats it as a
/// prevented roll). This mirrors the guard defensively so no future caller can
/// reach the panic — a natural of 1 is the smallest legal face and keeps the
/// return total.
pub(crate) fn roll_natural(state: &mut GameState, sides: u8) -> u8 {
    if sides == 0 {
        debug_assert!(false, "CR 706.1: a die roll must have at least one side");
        return 1;
    }
    state.rng.random_range(1..=sides)
}

/// CR 706.2: Apply a die roll's (optional) modifier to a natural result.
///
/// The result is clamped to a u8-representable non-negative integer so a large
/// subtract doesn't wrap; branches with `min`/`max` already in u8 simply won't
/// match when the actual result is 0.
fn apply_modifier(
    state: &mut GameState,
    natural: u8,
    modifier: Option<&DieRollModifier>,
    controller: PlayerId,
    source_id: ObjectId,
) -> u8 {
    let Some(m) = modifier else {
        return natural;
    };
    // Carry the sign as the saturating operation rather than negating the
    // resolved delta: `-resolve_quantity(..)` would panic in debug builds (and
    // wrap in release) when the quantity resolves to `i32::MIN`.
    let combined =
        match m {
            DieRollModifier::Add { value } => (natural as i32)
                .saturating_add(resolve_quantity(state, value, controller, source_id)),
            DieRollModifier::Subtract { value } => (natural as i32)
                .saturating_sub(resolve_quantity(state, value, controller, source_id)),
        };
    combined.clamp(0, u8::MAX as i32) as u8
}

/// CR 706.1 + CR 614.1a: Outcome of routing a die-roll instruction through the
/// CR 614 replacement pipeline.
pub(crate) enum RollProposal {
    /// The instruction rolls this many dice, ignoring one roll per CR 706.6
    /// rule in `ignore_rules` (empty when no replacement applied).
    Execute {
        count: u32,
        ignore_rules: Vec<DieRollIgnoreRule>,
    },
    /// CR 614.6: a replacement prevented the roll entirely — it never happens.
    Prevented,
    /// CR 616.1: the affected player must order competing replacements first.
    Suspended,
}

/// CR 706.1 + CR 614.1a: Route a die-roll instruction through the CR 614
/// replacement pipeline before touching the RNG, mirroring
/// `flip_coin::flip_through_replacement` (CR 705.1).
///
/// CR 706.1 scopes the event to the whole INSTRUCTION ("how many of those dice
/// to roll"), not to each die, which is why "roll two six-sided dice" proposes a
/// single `RollDice { count: 2 }` and a "whenever you roll one or more dice"
/// ability still sees one batch.
pub(crate) fn propose_roll(
    state: &mut GameState,
    player_id: PlayerId,
    count: u32,
    sides: u8,
    events: &mut Vec<GameEvent>,
) -> RollProposal {
    let proposed = ProposedEvent::RollDice {
        player_id,
        count,
        sides,
        ignore_rules: Vec::new(),
        applied: HashSet::new(),
    };

    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => proposal_from_modified_event(event, count),
        ReplacementResult::Prevented => RollProposal::Prevented,
        ReplacementResult::NeedsChoice(choice_player) => {
            // CR 616.1: two or more applicable replacements — the affected
            // player orders them before the roll happens.
            state.waiting_for = replacement::replacement_choice_waiting_for(choice_player, state);
            RollProposal::Suspended
        }
    }
}

/// CR 614.1a: Read a fully-applied `RollDice` event back into a proposal.
///
/// Shared by the inline path (`propose_roll`) and the CR 616.1 resume path
/// (`resume_roll_dice_after_replacement`), so both interpret a modified event
/// identically — the resume path must not re-derive the count or drop the
/// accumulated ignore rules. `fallback_count` is the unmodified instruction
/// count, used only when the pipeline handed back a foreign event.
pub(crate) fn proposal_from_modified_event(
    event: ProposedEvent,
    fallback_count: u32,
) -> RollProposal {
    match event {
        ProposedEvent::RollDice {
            count,
            ignore_rules,
            ..
        } => {
            if count == 0 {
                // CR 614.6: a replacement reduced the instruction to no dice.
                RollProposal::Prevented
            } else {
                RollProposal::Execute {
                    count,
                    ignore_rules,
                }
            }
        }
        // A different event was substituted, or nothing matched cleanly — treat
        // as the unmodified instruction rather than guessing at a foreign event.
        _ => RollProposal::Execute {
            count: fallback_count,
            ignore_rules: Vec::new(),
        },
    }
}

/// CR 706: Roll a die and execute the matching result branch.
///
/// CR 706.1 + CR 614.1a: the instruction is first proposed to the replacement
/// pipeline, so "if you would roll one or more dice, instead roll that many dice
/// plus one and ignore the lowest roll" raises the count before the RNG runs.
///
/// CR 706.2: every natural roll is taken from a uniform 1..=sides distribution
/// using the game's seeded RNG. When a CR 706.6 ignore rule applies and more
/// than one roll is tied at the extreme, resolution suspends on
/// `WaitingFor::DieKeepChoice` so the roller can break the tie; otherwise the
/// ignore set is unambiguous and `resume_after_ignore` runs inline. Emission of
/// `GameEvent::DieRolled`, the modifier, the results table, and the aggregate
/// stamp all live in `resume_after_ignore` — the single authority — so an
/// ignored roll never reaches any of them.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (count_expr, sides, results, modifier) = match &ability.effect {
        Effect::RollDie {
            count,
            sides,
            results,
            modifier,
        } => (count, *sides, results, modifier.as_ref()),
        _ => return Err(EffectError::MissingParam("RollDie".to_string())),
    };

    // CR 706.1: Resolve how many dice of this kind to roll, in the ability's
    // context; clamp at zero (a 0-count roll is a no-op). Each die is rolled
    // independently with the same sides/modifier/results table.
    let count =
        resolve_quantity(state, count_expr, ability.controller, ability.source_id).max(0) as u32;

    let instruction = PendingDieRollInstruction {
        source_id: ability.source_id,
        controller: ability.controller,
        // CR 706.6: the roller is the player instructed to ignore, and so the
        // player who breaks a tie among equal lowest rolls.
        roller: ability.controller,
        targets: ability.targets.clone(),
        results_table: results.clone(),
        modifier: modifier.cloned(),
        // CR 706.4 + CR 608.2: carry the pre-existing die context across the
        // action boundary a replacement-ordering or keep choice would introduce.
        die_result: state.die_result_this_resolution,
        // CR 706.3a: an `Effect::RollDie` resolution finishes here, in
        // `execute_roll`.
        continuation: DieRollContinuation::Resolution,
    };

    // CR 706.1 + CR 614.1a: route the instruction through the replacement
    // pipeline before touching the RNG.
    //
    // CR 616.1: park the instruction context BEFORE proposing. Two applicable
    // die-roll replacements (Barbarian Class + Pixie Guide) suspend the resolver
    // on a `ReplacementChoice`, at which point `ability` is gone — the parked
    // frame is the only thing that carries the results table, the modifier, and
    // the targets to the resume path.
    state.pending_die_roll_instruction = Some(Box::new(instruction.clone()));
    let proposal = propose_roll(state, ability.controller, count, sides, events);
    if !matches!(proposal, RollProposal::Suspended) {
        // Only a suspension needs the parked frame; every other outcome finishes
        // inline and must not leave a stale instruction behind for an unrelated
        // later roll to pick up.
        state.pending_die_roll_instruction = None;
    }

    execute_roll(state, instruction, sides, proposal, events).map(|_| ())
}

/// CR 706.1 + CR 706.6: Carry out a die-roll instruction once its replacement
/// proposal is known.
///
/// The single authority shared by the inline path (`resolve`) and the CR 616.1
/// resume path (`resume_roll_dice_after_replacement`), so an ordering choice
/// cannot silently take a different route than an unreplaced roll takes.
///
/// Returns `Ok(Some(wf))` when the instruction suspended — for the CR 706.6
/// ignore choice, for a results branch's own choice, or because the proposal
/// itself is still suspended — and `Ok(None)` when it completed.
fn execute_roll(
    state: &mut GameState,
    instruction: PendingDieRollInstruction,
    sides: u8,
    proposal: RollProposal,
    events: &mut Vec<GameEvent>,
) -> Result<Option<WaitingFor>, EffectError> {
    // CR 706.1: a die roll is defined by its kind ("roll a d20"); a zero-sided
    // die names no distribution, so there is nothing to roll. This is not a
    // rules case but a fail-safe: `sides` reaches here from the proposed event,
    // and two callers can hand over 0 — the foreign-substitution fallback in
    // `resume_roll_dice_after_replacement` (a substituted non-`RollDice` event
    // carries no die kind) and the count-only replacement definition's
    // `Effect::RollDie { sides: 0, .. }`, whose sides ride the event rather than
    // the definition. Without this
    // guard `roll_natural`'s `random_range(1..=0)` panics mid-resolution inside
    // `rand`. Route it through the same no-op path as a prevented roll so the
    // resolution unwinds cleanly instead of aborting the game.
    let proposal = match proposal {
        RollProposal::Execute { .. } if sides == 0 => RollProposal::Prevented,
        other => other,
    };
    let (count, ignore_rules) = match proposal {
        RollProposal::Execute {
            count,
            ignore_rules,
        } => (count, ignore_rules),
        RollProposal::Prevented => {
            // CR 614.6: the roll never happened — no dice, no branches. Clear
            // any stale die result so "equal to the result" reads nothing.
            state.die_result_this_resolution = None;
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::RollDie,
                source_id: instruction.source_id,
                subject: None,
            });
            return Ok(None);
        }
        RollProposal::Suspended => return Ok(Some(state.waiting_for.clone())),
    };

    // CR 706.2: roll every die first, collecting NATURAL results. Nothing is
    // emitted yet — under CR 706.6 an ignored roll must never have happened.
    let naturals: Vec<u8> = (0..count).map(|_| roll_natural(state, sides)).collect();

    // CR 706.6: the ignore set is computed over the NATURALS, before any
    // modifier — no effect may apply to a roll that is about to be ignored. ONE
    // roll is ignored per applied replacement (CR 706.6 applies once per
    // instructing effect), which is why this consumes the whole rule list rather
    // than a single rule.
    //
    // The outcome separates the rolls that are FORCED out from the tie the
    // roller breaks. Barbarian Class's own ruling for its multi-copy case ("you
    // roll that many additional dice and ignore that many of the lowest rolls")
    // is exactly this: with two copies over `[4, 7, 7]` the 4 is determined and
    // only the two 7s are a choice. Offering a flat union instead would let the
    // roller drop both 7s and keep the 4.
    let outcome = DieRollIgnoreRule::ignore_outcome_for_rules(&ignore_rules, &naturals);

    let PendingDieRollInstruction {
        source_id,
        controller,
        roller,
        targets,
        results_table,
        modifier,
        die_result,
        // CR 706.3a: `execute_roll` is the RESOLUTION continuation. The
        // roll-to-visit tag is dispatched on before this point
        // (`resume_roll_dice_after_replacement`), so anything arriving here owns
        // the results-table route by construction.
        continuation: _,
    } = instruction;

    let pending = PendingDieRoll {
        source_id,
        controller,
        roller,
        targets,
        sides,
        results: naturals,
        ignore_rules,
        results_table,
        modifier,
        die_result,
        // CR 706.3a: a freshly rolled instruction starts its results-table loop
        // at the first die with an empty aggregate.
        next_index: 0,
        running_total: 0,
        rolled_any: false,
        // CR 706.6: the determined part of the ignore set, held so the resume
        // path drops it alongside whatever the roller picks from the tie.
        forced_ignored: outcome.forced.clone(),
    };

    // CR 706.6: when the ignored set is fully determined the roller has no
    // decision — resolve with no prompt. This covers "nothing is ignored" (no
    // replacement in play), "a unique lowest per rule", and the stacked-Lowest
    // run whose N lowest rolls are distinct.
    if !outcome.needs_choice() {
        return resume_after_ignore(state, pending, outcome.forced, events);
    }

    // CR 706.6, 2nd sentence: "if multiple results are tied for the lowest, the
    // player chooses one of those rolls to be ignored." Only the TIED rolls are
    // offered, and only for the picks the forced set did not already cover — a
    // forced roll is not the roller's to keep.
    let picks = outcome.picks_from_tied();
    debug_assert!(
        picks > 0 && outcome.tied.len() > picks,
        "CR 706.6: a prompt is only correct when the roller has a real choice",
    );
    let waiting = WaitingFor::DieKeepChoice {
        player: pending.roller,
        results: pending.results.clone(),
        ignorable_indices: outcome.tied,
        ignore_count: picks,
    };
    state.waiting_for = waiting.clone();
    state.push_die_roll_frame(pending);
    Ok(Some(waiting))
}

/// CR 616.1 + CR 706.1: Finish a die-roll instruction whose competing
/// replacements the affected player has now ordered.
///
/// Reached from the replacement-choice resume in `engine_replacement` once the
/// pipeline has applied every chosen replacement to the proposed event. The
/// bound modified event is authoritative — its raised count and its ACCUMULATED
/// ignore rules are read straight off it, never re-derived — and the parked
/// instruction frame supplies the resolution context (`results_table`,
/// `modifier`, `targets`, `source_id`) that the event does not carry.
///
/// Mirrors `resume_search_found_after_replacement`: the resume arm delegates
/// back into the effect's own authority instead of re-implementing it.
pub(crate) fn resume_roll_dice_after_replacement(
    state: &mut GameState,
    event: ProposedEvent,
    events: &mut Vec<GameEvent>,
) -> Result<Option<WaitingFor>, EffectError> {
    let Some(instruction) = state.pending_die_roll_instruction.take() else {
        // No parked instruction: nothing proposed this roll through a path that
        // owns a continuation. Both production origins (`Effect::RollDie` and
        // the CR 703.4g roll-to-visit) park before proposing, so this is only
        // reachable for a foreign/synthetic `RollDice` event.
        return Ok(None);
    };
    let sides = match &event {
        ProposedEvent::RollDice { sides, .. } => *sides,
        // CR 706.1: the die kind rides the event. A foreign substituted event
        // carries none, and `proposal_from_modified_event` treats that case as
        // the unmodified instruction.
        _ => 0,
    };
    // CR 706.1: the instruction's own count is already folded into the modified
    // event; the fallback only matters for a foreign substituted event, where
    // the single unmodified die is the honest reading.
    let proposal = proposal_from_modified_event(event, 1);

    // CR 701.52a + CR 703.4g: the roll-to-visit turn-based action owns its own
    // completion — it has no results table, applies no modifier, and stamps no
    // resolution context, so it must not be routed through `execute_roll`.
    // Dispatched BEFORE the resolution-context restore below: the turn-based
    // action is not a resolution, so `die_result_this_resolution` must stay
    // cleared for it (its parked `die_result` is `None` by construction, and
    // restoring nothing is what leaves no stale value behind).
    if instruction.continuation == DieRollContinuation::RollToVisitAttractions {
        crate::game::attractions::complete_roll_to_visit(
            state,
            instruction.roller,
            proposal,
            events,
        );
        return Ok(None);
    }

    // CR 706.4 + CR 608.2: the CR 616.1 ordering choice was an action boundary,
    // which cleared `die_result_this_resolution`. Restore the instruction's
    // context before rolling, exactly as the `DieKeepChoice` resume does.
    state.die_result_this_resolution = instruction.die_result;

    execute_roll(state, *instruction, sides, proposal, events)
}

/// CR 706.3a + CR 608.2c: Continue a die-roll instruction whose results-table
/// branch suspended for its own interactive choice.
///
/// Reached from the priority-time frame drain once that branch's choice has
/// settled. Only a MID-LOOP re-park is resumable here: such a frame carries a
/// non-zero `next_index`, set by `resume_after_ignore` when it re-parked. A
/// frame parked for the CR 706.6 ignore prompt still sits at cursor zero and is
/// owned exclusively by `GameAction::SelectDieRolls`, so this leaves it alone.
///
/// The ignore set is NOT recomputed. It was settled over the naturals before any
/// die was emitted (CR 706.6), and a tie the roller broke by hand is not
/// recoverable from the rules alone — re-deriving it could drop a different tied
/// roll than the one already committed to. The re-parked frame carries the
/// resolved set verbatim in `forced_ignored`, and this reuses it.
pub(crate) fn drain_active_die_roll(state: &mut GameState, events: &mut Vec<GameEvent>) {
    // Only a mid-loop re-park drains here; a pending CR 706.6 keep prompt must
    // stay parked for its own action handler, which is the only thing that knows
    // the roller's picks.
    let resumable = matches!(
        state.resolution_stack.active_die_roll(),
        Some(pending) if pending.next_index > 0
    );
    if !resumable {
        return;
    }
    let Ok(Some(pending)) = state.take_active_die_roll_frame() else {
        return;
    };

    // CR 706.4 + CR 608.2: restore the instruction's die context around the
    // continuation, exactly as the `SelectDieRolls` handler does.
    let prev_die_result = state.die_result_this_resolution;
    state.die_result_this_resolution = pending.die_result;
    let ignore_indices = pending.forced_ignored.clone();
    match resume_after_ignore(state, pending, ignore_indices, events) {
        // Re-suspended on yet another branch choice: the frame re-parked itself
        // with the cursor advanced, so leave the restored context in place for
        // that continuation to read.
        Ok(Some(_)) => {}
        Ok(None) | Err(_) => {
            state.die_result_this_resolution = prev_die_result;
        }
    }
}

/// CR 706.6 + CR 614.1a: Finish a die-roll instruction once the ignored rolls
/// are known.
///
/// This is the SINGLE authority for everything that happens to a die after it is
/// rolled: it emits `GameEvent::DieRolled` for the survivors only (carrying the
/// post-modifier value), applies the modifier, consults the results table, and
/// stamps `die_result_this_resolution`. `resolve` never duplicates any of it —
/// the no-prompt fast path calls straight into here.
///
/// `ignore_indices` names the rolls that "never happened" (CR 706.6): they emit
/// no event, receive no modifier, run no results branch, and contribute nothing
/// to the aggregate.
///
/// Returns `Ok(Some(wf))` when a results branch re-suspended for another
/// interactive choice, and `Ok(None)` when the whole instruction completed; the
/// caller then drains the continuation back to Priority.
pub fn resume_after_ignore(
    state: &mut GameState,
    pending: PendingDieRoll,
    ignore_indices: Vec<usize>,
    events: &mut Vec<GameEvent>,
) -> Result<Option<WaitingFor>, EffectError> {
    let PendingDieRoll {
        source_id,
        controller,
        roller,
        targets,
        sides,
        results,
        ignore_rules,
        results_table,
        modifier,
        die_result,
        // CR 706.3a + CR 608.2c: where the per-die loop resumes, and the
        // aggregate accumulated so far. Non-zero only when a previous pass
        // suspended inside an interactive results branch.
        next_index,
        running_total,
        rolled_any: already_rolled_any,
        // The already-settled ignore set arrives as the `ignore_indices`
        // argument (the caller unions the roller's picks into it), so the
        // frame's own copy is only a carrier across the suspension.
        forced_ignored: _,
    } = pending;

    // Clear any resolving keep choice so a re-suspension below is unambiguous
    // for `super::waits_for_resolution_choice`, mirroring `resume_after_keep`.
    if matches!(state.waiting_for, WaitingFor::DieKeepChoice { .. }) {
        state.waiting_for = WaitingFor::Priority { player: controller };
    }

    let mut total_actual = running_total;
    let mut rolled_any = already_rolled_any;

    // CR 706.3a: resume at the first die that has not yet run its branch. A
    // fresh instruction starts at 0; a resume after an interactive branch starts
    // just past the die that suspended, so no branch runs twice and none is
    // skipped.
    for (index, natural) in results
        .iter()
        .copied()
        .enumerate()
        .skip(next_index.min(results.len()))
    {
        // CR 706.6: an ignored roll is considered never to have happened — no
        // event, no modifier, no results branch, no aggregate contribution.
        if ignore_indices.contains(&index) {
            continue;
        }

        // CR 706.2: apply the modifier to produce this die's actual result.
        let actual = apply_modifier(state, natural, modifier.as_ref(), controller, source_id);

        // CR 706.2 + CR 706.6: emit the authoritative die-roll event ONCE, with
        // the post-modifier value already resolved, and only for a surviving
        // roll. This single emission rule is what keeps the die-roll trigger
        // layer, `game/contraptions.rs`'s roll-difference scan, and
        // `game/effects/effect.rs`'s CR 611.2d snapshot all seeing survivors
        // only.
        events.push(GameEvent::DieRolled {
            player_id: roller,
            sides,
            result: Some(actual),
        });

        let actual_amount = i32::from(actual);
        total_actual = total_actual.saturating_add(actual_amount);
        rolled_any = true;

        // CR 706.2 + CR 706.3a: The stored value is this die's actual result
        // while its results-table branch resolves.
        state.die_result_this_resolution = Some(actual_amount);

        // CR 706.3a: Find the matching result branch and resolve its effect.
        // Each die consults the same table independently.
        if let Some(branch) = results_table
            .iter()
            .find(|b| actual >= b.min && actual <= b.max)
        {
            // CR 608.2c: Branch bodies are full `AbilityDefinition`s (player_scope,
            // sub_abilities, conditions, etc.). `ResolvedAbility::new` with only the
            // effect drops `player_scope`, so "each opponent loses N life" on a d20
            // table (Herald of Hadar) incorrectly hit the controller (#2026).
            let sub = build_resolved_from_def_with_targets(
                &branch.effect,
                source_id,
                controller,
                targets.clone(),
            );
            resolve_ability_chain(state, &sub, events, 0)?;

            // CR 608.2c + CR 706.3a: a branch may itself have paused for a
            // player choice. The instruction is NOT finished — every die after
            // this one still owes its own branch, and CR 706.4's aggregate spans
            // all of them — so re-park the frame with the cursor advanced past
            // this die and the running aggregate carried, then suspend. The
            // choice's own resume path re-enters here and continues the loop.
            //
            // Without the re-park this would abandon the remaining dice: their
            // branches would never run and `die_result_this_resolution` would be
            // left at this single die's value rather than the survivors' total.
            if super::waits_for_resolution_choice(&state.waiting_for) {
                let resumed = PendingDieRoll {
                    source_id,
                    controller,
                    roller,
                    targets: targets.clone(),
                    sides,
                    results: results.clone(),
                    ignore_rules: ignore_rules.clone(),
                    results_table: results_table.clone(),
                    modifier: modifier.clone(),
                    die_result,
                    // Resume at the die AFTER the one whose branch suspended.
                    next_index: index + 1,
                    running_total: total_actual,
                    rolled_any,
                    // CR 706.6: carry the ALREADY-SETTLED ignore set forward
                    // verbatim. It was decided over the naturals before any die
                    // was emitted, and a tie the roller broke by hand is not
                    // re-derivable from the rules — recomputing it on resume
                    // could silently pick a different tied roll than the one the
                    // roller committed to.
                    forced_ignored: ignore_indices.clone(),
                };
                // CR 706.3a: park the owner in the structurally valid slot.
                // A results-table branch that suspended on its own prompt is
                // above us, and this frame is an `AfterChild` owner past cursor
                // 0 — so it must go BELOW that child, never on top of it. The
                // helper discriminates re-park / push / insert-below; the old
                // `.is_err()` fallback collapsed the last two and could bury an
                // active `DirectChoice`.
                state
                    .park_die_roll_frame_for_resume(resumed)
                    .map_err(|error| {
                        EffectError::InvalidParam(format!("die-roll frame re-park: {error}"))
                    })?;
                return Ok(Some(state.waiting_for.clone()));
            }
        }
    }

    if rolled_any {
        // CR 706.4 + CR 706.6: aggregate over SURVIVING dice only — an ignored
        // roll "never happened", so it contributes nothing to "equal to the
        // result(s)". For no-table rolls the outer sub_ability is resolved by
        // the caller after this function returns, so leave the aggregate
        // available: it must read all surviving dice, not just the last one.
        state.die_result_this_resolution = Some(total_actual);
    } else {
        state.die_result_this_resolution = None;
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::RollDie,
        source_id,
        subject: None,
    });

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{AbilityDefinition, AbilityKind, DieResultBranch, QuantityExpr};
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;

    #[test]
    fn roll_die_emits_event_and_resolves_branch() {
        let mut state = GameState::new_two_player(42);
        let branch = DieResultBranch {
            min: 1,
            max: 20,
            effect: Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: crate::types::ability::TargetFilter::Controller,
                },
            )),
        };
        let ability = ResolvedAbility::new(
            Effect::RollDie {
                count: QuantityExpr::Fixed { value: 1 },
                sides: 20,
                results: vec![branch],
                modifier: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        // Add a card to draw
        crate::game::zones::create_object(
            &mut state,
            crate::types::identifiers::CardId(1),
            PlayerId(0),
            "Card A".to_string(),
            crate::types::zones::Zone::Library,
        );

        let mut events = Vec::new();
        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());

        // Should have DieRolled event
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::DieRolled { sides: 20, .. })));
        // Branch covers 1-20, so it always matches — player drew a card
        assert_eq!(state.players[0].hand.len(), 1);
    }

    #[test]
    fn roll_die_no_matching_branch() {
        let mut state = GameState::new_two_player(42);
        // Branch only covers 21+ (impossible on d20), so no effect fires
        let branch = DieResultBranch {
            min: 21,
            max: 30,
            effect: Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: crate::types::ability::TargetFilter::Controller,
                },
            )),
        };
        let ability = ResolvedAbility::new(
            Effect::RollDie {
                count: QuantityExpr::Fixed { value: 1 },
                sides: 20,
                results: vec![branch],
                modifier: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::DieRolled { .. })));
        assert_eq!(state.players[0].hand.len(), 0);
    }

    #[test]
    fn roll_die_without_branches() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::RollDie {
                count: QuantityExpr::Fixed { value: 1 },
                sides: 6,
                results: vec![],
                modifier: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        // Just emits the die rolled event with no branch resolution
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::DieRolled { sides: 6, .. })));
    }

    /// CR 706.2: "Roll a d20 and add the number of cards in your hand" — the
    /// modifier shifts the natural roll upward. We choose a generous branch
    /// covering 1..=40 so the test is RNG-deterministic regardless of seed.
    #[test]
    fn roll_die_add_modifier_shifts_result_upward() {
        let mut state = GameState::new_two_player(42);
        // Seed two cards into the controller's hand so the modifier resolves to 2.
        state.players[0]
            .hand
            .push_back(crate::types::identifiers::ObjectId(100));
        state.players[0]
            .hand
            .push_back(crate::types::identifiers::ObjectId(101));
        let branch = DieResultBranch {
            min: 1,
            max: 40,
            effect: Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: crate::types::ability::TargetFilter::Controller,
                },
            )),
        };
        // Add a card to draw.
        crate::game::zones::create_object(
            &mut state,
            crate::types::identifiers::CardId(1),
            PlayerId(0),
            "Card A".to_string(),
            crate::types::zones::Zone::Library,
        );
        let ability = ResolvedAbility::new(
            Effect::RollDie {
                count: QuantityExpr::Fixed { value: 1 },
                sides: 20,
                results: vec![branch],
                modifier: Some(DieRollModifier::Add {
                    value: QuantityExpr::Ref {
                        qty: crate::types::ability::QuantityRef::HandSize {
                            player: crate::types::ability::PlayerScope::Controller,
                        },
                    },
                }),
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        let result = events
            .iter()
            .find_map(|e| match e {
                GameEvent::DieRolled { result, .. } => *result,
                _ => None,
            })
            .expect("DieRolled event should be present");
        // Natural roll ∈ 1..=20, modifier = +2 (two cards in hand), so actual ∈ 3..=22.
        assert!(
            (3..=22).contains(&result),
            "actual result {result} should reflect +2 modifier"
        );
    }

    /// CR 706.2: "Roll a d20 and subtract the number of cards in your hand" —
    /// the modifier shifts the natural roll downward. With many cards in
    /// hand, the actual result can be 0 or below, which clamps to 0.
    #[test]
    fn roll_die_subtract_modifier_clamps_at_zero() {
        let mut state = GameState::new_two_player(42);
        // Twenty-five cards in hand → modifier resolves to 25; any d20 roll
        // produces actual ≤ 0, which clamps to 0.
        for i in 0..25 {
            state.players[0]
                .hand
                .push_back(crate::types::identifiers::ObjectId(200 + i));
        }
        let ability = ResolvedAbility::new(
            Effect::RollDie {
                count: QuantityExpr::Fixed { value: 1 },
                sides: 20,
                results: vec![],
                modifier: Some(DieRollModifier::Subtract {
                    value: QuantityExpr::Ref {
                        qty: crate::types::ability::QuantityRef::HandSize {
                            player: crate::types::ability::PlayerScope::Controller,
                        },
                    },
                }),
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        let result = events
            .iter()
            .find_map(|e| match e {
                GameEvent::DieRolled { result, .. } => *result,
                _ => None,
            })
            .expect("DieRolled event should be present");
        assert_eq!(result, 0, "subtract modifier should clamp at 0");
    }

    /// CR 706.2: With a seeded RNG, rolling the same die twice from two
    /// identically-seeded states must produce the same natural result.
    /// This is foundational for replays and AI-search determinism.
    #[test]
    fn roll_die_with_seeded_rng_is_deterministic() {
        let mut state_a = GameState::new_two_player(7);
        let mut state_b = GameState::new_two_player(7);
        let ability = |state_seed: PlayerId| {
            ResolvedAbility::new(
                Effect::RollDie {
                    count: QuantityExpr::Fixed { value: 1 },
                    sides: 20,
                    results: vec![],
                    modifier: None,
                },
                vec![],
                ObjectId(1),
                state_seed,
            )
        };
        let mut ev_a = Vec::new();
        let mut ev_b = Vec::new();
        resolve(&mut state_a, &ability(PlayerId(0)), &mut ev_a).unwrap();
        resolve(&mut state_b, &ability(PlayerId(0)), &mut ev_b).unwrap();
        let r_a = ev_a
            .iter()
            .find_map(|e| match e {
                GameEvent::DieRolled { result, .. } => *result,
                _ => None,
            })
            .unwrap();
        let r_b = ev_b
            .iter()
            .find_map(|e| match e {
                GameEvent::DieRolled { result, .. } => *result,
                _ => None,
            })
            .unwrap();
        assert_eq!(r_a, r_b, "identically-seeded RNG must roll the same result");
    }

    /// CR 706.1: All sides in the supported set produce results in 1..=sides.
    /// This sweeps a representative slice of polyhedral dice to ensure the
    /// RNG range is correct for every die size used in Magic (d4, d6, d8,
    /// d10, d12, d20, d100).
    #[test]
    fn roll_die_produces_value_in_range_for_each_die_size() {
        for sides in [4_u8, 6, 8, 10, 12, 20, 100] {
            let mut state = GameState::new_two_player(sides as u64);
            let ability = ResolvedAbility::new(
                Effect::RollDie {
                    count: QuantityExpr::Fixed { value: 1 },
                    sides,
                    results: vec![],
                    modifier: None,
                },
                vec![],
                ObjectId(1),
                PlayerId(0),
            );
            // Roll fifty times per size; every roll must be in 1..=sides.
            for _ in 0..50 {
                let mut events = Vec::new();
                resolve(&mut state, &ability, &mut events).unwrap();
                let r = events
                    .iter()
                    .find_map(|e| match e {
                        GameEvent::DieRolled { result, .. } => *result,
                        _ => None,
                    })
                    .unwrap();
                assert!((1..=sides).contains(&r), "d{sides} result {r} out of range");
            }
        }
    }

    /// CR 706.2 + CR 608.2c: After a RollDie resolves, the actual result is
    /// stamped into `state.last_effect_amount` so a follow-up sub-ability with
    /// `AbilityCondition::PreviousEffectAmount` can gate on it. This is the
    /// channel that powers "If the result is 0 or less, discard your hand"
    /// (Deck of Many Things) and analogous result-conditional riders.
    #[test]
    fn roll_die_stamps_last_effect_amount_for_chain() {
        use crate::types::ability::{AbilityCondition, Comparator};
        let mut state = GameState::new_two_player(7);
        // No modifier: actual result == natural ∈ 1..=20, always > 0.
        let ability = ResolvedAbility::new(
            Effect::RollDie {
                count: QuantityExpr::Fixed { value: 1 },
                sides: 20,
                results: vec![],
                modifier: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        crate::game::effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        let result = events
            .iter()
            .find_map(|e| match e {
                GameEvent::DieRolled { result, .. } => result.map(i32::from),
                _ => None,
            })
            .expect("DieRolled event must be present");
        assert_eq!(
            state.last_effect_amount,
            Some(result),
            "last_effect_amount must mirror the actual rolled result so PreviousEffectAmount conditions can read it"
        );
        // And the AbilityCondition resolver consumes that channel correctly.
        let cond = AbilityCondition::PreviousEffectAmount {
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
            channel: crate::types::ability::DamageChannel::Total,
        };
        let dummy = ResolvedAbility::new(
            Effect::Unimplemented {
                name: "probe".into(),
                description: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        assert!(
            crate::game::effects::evaluate_condition(&cond, &state, &dummy),
            "result {result} ≥ 1, so the PreviousEffectAmount(GE, 1) condition must hold"
        );
    }

    /// CR 706.2 (Deck of Many Things, end-to-end): "Roll a d20 and subtract
    /// the number of cards in your hand. If the result is 0 or less, discard
    /// your hand." With 25 cards in hand the modifier dominates any d20 →
    /// actual clamps to 0, so the conditional Discard sub-ability MUST fire.
    #[test]
    fn roll_die_conditional_subability_fires_when_result_le_zero() {
        use crate::types::ability::{
            AbilityCondition, Comparator, DieRollModifier, PlayerScope, QuantityRef, TargetFilter,
        };
        let mut state = GameState::new_two_player(42);
        // Seed 25 real hand objects so the modifier (-25) overpowers any
        // d20 natural roll and the Discard has cards to actually move.
        for i in 0..25 {
            crate::game::zones::create_object(
                &mut state,
                crate::types::identifiers::CardId(2000 + i as u64),
                PlayerId(0),
                format!("Card {i}"),
                crate::types::zones::Zone::Hand,
            );
        }
        let hand_before = state.players[0].hand.len();
        // Conditional Discard guarded by "result ≤ 0".
        let discard = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize {
                        player: PlayerScope::Controller,
                    },
                },
                target: TargetFilter::Controller,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                unless_filter: None,
                filter: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        )
        .condition(AbilityCondition::PreviousEffectAmount {
            comparator: Comparator::LE,
            rhs: QuantityExpr::Fixed { value: 0 },
            channel: crate::types::ability::DamageChannel::Total,
        });
        let ability = ResolvedAbility::new(
            Effect::RollDie {
                count: QuantityExpr::Fixed { value: 1 },
                sides: 20,
                results: vec![],
                modifier: Some(DieRollModifier::Subtract {
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::HandSize {
                            player: PlayerScope::Controller,
                        },
                    },
                }),
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        )
        .sub_ability(discard);
        let mut events = Vec::new();
        crate::game::effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        // Roll clamped to 0; condition LE 0 holds; controller discards hand.
        assert_eq!(
            state.players[0].hand.len(),
            0,
            "result ≤ 0 should fire the guarded Discard, emptying the hand from {hand_before}"
        );
    }

    /// CR 706.2 (Deck of Many Things, end-to-end): "Roll a d20 and subtract
    /// the number of cards in your hand. If the result is 0 or less, discard
    /// your hand." With zero cards in hand the modifier is 0 → natural roll
    /// (≥ 1) wins → result ≥ 1, so the conditional Discard MUST NOT fire.
    #[test]
    fn roll_die_conditional_subability_skipped_when_result_positive() {
        use crate::types::ability::{
            AbilityCondition, AggregateFunction, Comparator, DieRollModifier, PlayerScope,
            QuantityRef, TargetFilter,
        };
        let mut state = GameState::new_two_player(7);
        // Seed two real hand objects; with 0 cards we'd test nothing — we want
        // visible objects that would have been discarded had the gate broken.
        for i in 0..2 {
            crate::game::zones::create_object(
                &mut state,
                crate::types::identifiers::CardId(3000 + i as u64),
                PlayerId(0),
                format!("Card {i}"),
                crate::types::zones::Zone::Hand,
            );
        }
        // Modifier reads opponent's hand size (which is 0) so the result
        // equals the natural d20 ≥ 1.
        let discard = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize {
                        player: PlayerScope::Controller,
                    },
                },
                target: TargetFilter::Controller,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                unless_filter: None,
                filter: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        )
        .condition(AbilityCondition::PreviousEffectAmount {
            comparator: Comparator::LE,
            rhs: QuantityExpr::Fixed { value: 0 },
            channel: crate::types::ability::DamageChannel::Total,
        });
        let ability = ResolvedAbility::new(
            Effect::RollDie {
                count: QuantityExpr::Fixed { value: 1 },
                sides: 20,
                results: vec![],
                modifier: Some(DieRollModifier::Subtract {
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::HandSize {
                            player: PlayerScope::Opponent {
                                aggregate: AggregateFunction::Sum,
                            },
                        },
                    },
                }),
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        )
        .sub_ability(discard);
        let mut events = Vec::new();
        crate::game::effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        // Result ≥ 1, so the LE 0 gate fails and the Discard is skipped.
        assert_eq!(
            state.players[0].hand.len(),
            2,
            "result ≥ 1 must not fire the guarded Discard"
        );
    }

    /// CR 706.2: After a RollDie resolves, the actual result is stamped into
    /// `state.die_result_this_resolution` so an inline "equal to
    /// the result" sub_ability (no results table) reads the roll via
    /// `QuantityRef::EventContextAmount`. The stamped value must equal the
    /// `DieRolled` event's result.
    ///
    /// CR 706.2 + CR 706.4: unchanged across the CR 706.6 ignore work. With no
    /// die-roll replacement registered, `resolve` takes the no-prompt path into
    /// `resume_after_ignore`, which emits one `DieRolled` for the single
    /// survivor and stamps the survivors-only aggregate — one survivor, so the
    /// aggregate equals that die's actual result and the event still agrees
    /// with the stamp. This test now covers that path, not the old inline loop.
    #[test]
    fn roll_die_stamps_die_result_this_resolution() {
        let mut state = GameState::new_two_player(7);
        let ability = ResolvedAbility::new(
            Effect::RollDie {
                count: QuantityExpr::Fixed { value: 1 },
                sides: 20,
                results: vec![],
                modifier: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        let result = events
            .iter()
            .find_map(|e| match e {
                GameEvent::DieRolled { result, .. } => *result,
                _ => None,
            })
            .expect("DieRolled event must be present");
        assert_eq!(
            state.die_result_this_resolution,
            Some(i32::from(result)),
            "die_result_this_resolution must mirror the actual rolled result"
        );
    }

    /// CR 706.4 (issue #1602, building-block guard): "roll a d20. You create a
    /// number of Treasure tokens equal to the result." With a
    /// triggering combat-damage event of 6 already set, the inline sub_ability
    /// whose count is `EventContextAmount` must consume the ROLL, not the 6.
    /// Modeled with a Draw sub_ability (count == EventContextAmount) so we can
    /// assert exactly `result` cards were drawn.
    #[test]
    fn roll_die_subability_reads_roll_not_trigger_event() {
        use crate::types::ability::{QuantityRef, TargetFilter, TargetRef};
        let mut state = GameState::new_two_player(7);
        // Seed enough library cards that any d20 result (≤ 20) can be drawn.
        for i in 0..20 {
            crate::game::zones::create_object(
                &mut state,
                crate::types::identifiers::CardId(4000 + i as u64),
                PlayerId(0),
                format!("Card {i}"),
                crate::types::zones::Zone::Library,
            );
        }
        // The triggering event carries amount 6 (combat damage). If the cascade
        // is wrong, the sub_ability would draw 6 instead of the rolled result.
        state.current_trigger_event = Some(GameEvent::DamageDealt {
            source_id: ObjectId(1),
            target: TargetRef::Player(PlayerId(1)),
            amount: 6,
            is_combat: true,
            excess: 0,
        });
        let draw = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let ability = ResolvedAbility::new(
            Effect::RollDie {
                count: QuantityExpr::Fixed { value: 1 },
                sides: 20,
                results: vec![],
                modifier: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        )
        .sub_ability(draw);
        let mut events = Vec::new();
        crate::game::effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        let rolled = events
            .iter()
            .find_map(|e| match e {
                GameEvent::DieRolled { result, .. } => result.map(usize::from),
                _ => None,
            })
            .expect("DieRolled event must be present");
        assert!(
            (1..=20).contains(&rolled),
            "d20 result out of range: {rolled}"
        );
        assert_eq!(
            state.players[0].hand.len(),
            rolled,
            "sub_ability must draw cards equal to the rolled result ({rolled}), not the combat damage (6)"
        );
    }

    /// CR 706.4: For a no-table effect that rolls multiple dice, an inline
    /// `EventContextAmount` consumer reads the total of all die results, not
    /// the final die's result.
    #[test]
    fn roll_die_multi_count_subability_reads_total_results() {
        use crate::types::ability::{QuantityRef, TargetFilter, TargetRef};
        let mut state = GameState::new_two_player(7);
        for i in 0..12 {
            crate::game::zones::create_object(
                &mut state,
                crate::types::identifiers::CardId(6000 + i as u64),
                PlayerId(0),
                format!("Card {i}"),
                crate::types::zones::Zone::Library,
            );
        }
        state.current_trigger_event = Some(GameEvent::DamageDealt {
            source_id: ObjectId(1),
            target: TargetRef::Player(PlayerId(1)),
            amount: 20,
            is_combat: true,
            excess: 0,
        });
        let draw = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let ability = ResolvedAbility::new(
            Effect::RollDie {
                count: QuantityExpr::Fixed { value: 2 },
                sides: 6,
                results: vec![],
                modifier: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        )
        .sub_ability(draw);
        let mut events = Vec::new();
        crate::game::effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        let rolls: Vec<usize> = events
            .iter()
            .filter_map(|e| match e {
                GameEvent::DieRolled {
                    result: Some(result),
                    sides: 6,
                    ..
                } => Some(usize::from(*result)),
                _ => None,
            })
            .collect();
        assert_eq!(rolls.len(), 2, "count == 2 must emit two rolls");
        let total: usize = rolls.iter().sum();
        assert_eq!(
            state.die_result_this_resolution,
            Some(total as i32),
            "resolution context must retain the aggregate die result"
        );
        assert_eq!(
            state.players[0].hand.len(),
            total,
            "sub_ability must draw the total of {rolls:?}, not the last die or the triggering damage"
        );
    }

    /// CR 706.1: If the requested number of dice resolves to 0, no die result
    /// exists for an inline `EventContextAmount` consumer.
    #[test]
    fn roll_die_count_zero_clears_stale_die_result() {
        use crate::types::ability::{QuantityRef, TargetFilter};
        let mut state = GameState::new_two_player(7);
        for i in 0..10 {
            crate::game::zones::create_object(
                &mut state,
                crate::types::identifiers::CardId(7000 + i as u64),
                PlayerId(0),
                format!("Card {i}"),
                crate::types::zones::Zone::Library,
            );
        }
        state.die_result_this_resolution = Some(9);
        let draw = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let ability = ResolvedAbility::new(
            Effect::RollDie {
                count: QuantityExpr::Fixed { value: 0 },
                sides: 6,
                results: vec![],
                modifier: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        )
        .sub_ability(draw);
        let mut events = Vec::new();
        crate::game::effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        assert!(
            events
                .iter()
                .all(|event| !matches!(event, GameEvent::DieRolled { .. })),
            "count 0 must not emit a die roll"
        );
        assert_eq!(state.die_result_this_resolution, None);
        assert_eq!(
            state.players[0].hand.len(),
            0,
            "zero dice must not leak a stale die result into the sub_ability"
        );
    }

    /// Issue #2026 (Herald of Hadar): d20 table branches with `player_scope:
    /// Opponent` must drain opponents, not the activator.
    #[test]
    fn roll_die_result_branch_preserves_opponent_player_scope() {
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::PlayerFilter;
        use crate::types::format::FormatConfig;

        let branch_def = parse_effect_chain("each opponent loses 2 life", AbilityKind::Spell);
        assert_eq!(
            branch_def.player_scope,
            Some(PlayerFilter::Opponent),
            "parser must stamp Opponent scope on each-opponent lose life"
        );

        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        let branch = DieResultBranch {
            min: 1,
            max: 20,
            effect: Box::new(branch_def),
        };
        let ability = ResolvedAbility::new(
            Effect::RollDie {
                count: QuantityExpr::Fixed { value: 1 },
                sides: 20,
                results: vec![branch],
                modifier: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.players[0].life, 20,
            "activator must not lose life from opponent-scoped branch"
        );
        assert_eq!(
            (state.players[1].life, state.players[2].life),
            (18, 18),
            "each opponent must lose 2 life"
        );
    }

    /// CR 706.1: "Roll two six-sided dice" rolls `count` independent dice,
    /// emitting one `DieRolled` event per die, each in 1..=sides.
    #[test]
    fn roll_die_count_two_emits_two_rolls() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::RollDie {
                count: QuantityExpr::Fixed { value: 2 },
                sides: 6,
                results: vec![],
                modifier: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        let rolls: Vec<u8> = events
            .iter()
            .filter_map(|e| match e {
                GameEvent::DieRolled {
                    result: Some(result),
                    sides: 6,
                    ..
                } => Some(*result),
                _ => None,
            })
            .collect();
        assert_eq!(rolls.len(), 2, "count == 2 must emit two DieRolled events");
        assert!(
            rolls.iter().all(|r| (1..=6).contains(r)),
            "every die result must be in 1..=6, got {rolls:?}"
        );
    }

    /// CR 706.1: Each die independently consults the results table, so a
    /// count-2 roll resolves the matching branch twice. With a branch covering
    /// the entire 1..=6 face range and a Draw effect, the controller draws once
    /// per die — two cards total.
    #[test]
    fn roll_die_count_two_resolves_branch_per_die() {
        let mut state = GameState::new_two_player(42);
        let branch = DieResultBranch {
            min: 1,
            max: 6,
            effect: Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: crate::types::ability::TargetFilter::Controller,
                },
            )),
        };
        // Seed enough library cards for both draws.
        for i in 0..5 {
            crate::game::zones::create_object(
                &mut state,
                crate::types::identifiers::CardId(5000 + i as u64),
                PlayerId(0),
                format!("Card {i}"),
                crate::types::zones::Zone::Library,
            );
        }
        let ability = ResolvedAbility::new(
            Effect::RollDie {
                count: QuantityExpr::Fixed { value: 2 },
                sides: 6,
                results: vec![branch],
                modifier: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        // Branch covers all faces, so it fires once per die — two draws.
        assert_eq!(
            state.players[0].hand.len(),
            2,
            "each of the two dice must resolve the 1..=6 branch, drawing one card per die"
        );
    }
}
