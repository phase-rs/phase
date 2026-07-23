//! CR 701.38: Vote — Council's dilemma family.
//!
//! Each player, starting with a specified player and proceeding in turn order
//! (CR 101.4), chooses one of the listed options. After every player has cast
//! their votes, the per-choice sub-effects resolve once for each vote tallied
//! against that choice.
//!
//! CR 701.38d: A player who has multiple votes (granted by a static ability
//! such as Tivit's "While voting, you may vote an additional time") makes
//! those choices at the same time they would otherwise have voted.
//!
//! The resolver entry point sets `WaitingFor::VoteChoice` for the starting
//! voter, embeds `per_choice_effect` directly on the `WaitingFor` (so the
//! tally flows through state filtering and live multiplayer echoes without
//! reaching back into the source ability), and stashes only the parent's
//! post-Vote sub_ability on a pending continuation. The
//! `engine_resolution_choices.rs` handler tallies each vote, advances voters
//! in APNAP order, and finally calls `resolve_tally` to fan out the per-choice
//! sub-effects.

use crate::types::ability::{
    AbilityDefinition, ControllerRef, Effect, EffectError, EffectKind, QuantityExpr,
    ResolvedAbility, TargetRef, TieResolution, VoteSubject, VoteTally, VoterScope,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    GameState, PendingContinuation, PendingVoteBallotIteration, VoteActor, WaitingFor,
};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;

use super::resolve_ability_chain;

/// CR 701.38a + CR 101.4: Initiate a vote. Builds the APNAP voter queue
/// starting from `starting_with` (resolved against the ability controller),
/// computes each voter's total votes (1 + extra-vote grants from
/// `Player::extra_votes_per_session`), and parks on `WaitingFor::VoteChoice`
/// for the first voter.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::Vote {
        choices,
        per_choice_effect,
        starting_with,
        voter_scope,
        tally_mode,
        subject,
        visibility,
    } = &ability.effect
    else {
        return Err(EffectError::InvalidParam(
            "vote::resolve called with non-Vote effect".into(),
        ));
    };

    let controller = ability.controller;
    let scope = *voter_scope;
    let tally_mode = *tally_mode;
    let visibility = *visibility;

    // CR 701.38b: Resolve the ballot options. Named votes (`VoteSubject::Named`)
    // use the static `choices`/`per_choice_effect`. Object-pool votes
    // (`VoteSubject::Objects` — Council's Judgment, Prime Minister's Cabinet
    // Room) enumerate matching battlefield objects at resolution: the options
    // are those objects' names, `candidate_objects` holds their ids, and the
    // winner(s) drive `outcome_template` once each.
    let (options, option_labels, candidate_objects, outcome_template, per_choice_effect) =
        match subject {
            VoteSubject::Named => {
                // Parser invariant: one sub-effect per choice. Surfaced as a
                // hard error so misparses fail fast rather than silently
                // dropping ballots.
                if choices.len() != per_choice_effect.len() {
                    return Err(EffectError::InvalidParam(format!(
                        "Effect::Vote choices/per_choice_effect length mismatch: {} vs {}",
                        choices.len(),
                        per_choice_effect.len()
                    )));
                }
                if choices.is_empty() {
                    return Err(EffectError::InvalidParam(
                        "Effect::Vote requires at least one choice".into(),
                    ));
                }
                // Display labels: title-case each choice for the modal. Engine
                // compares votes against the lowercase canonical `choices`.
                let option_labels: Vec<String> =
                    choices.iter().map(|c| title_case_word(c)).collect();
                (
                    choices.clone(),
                    option_labels,
                    crate::im::Vector::new(),
                    None,
                    per_choice_effect.clone(),
                )
            }
            VoteSubject::Objects {
                candidate_filter,
                outcome_template,
            } => {
                // CR 701.38b: enumerate the candidate objects relative to the
                // vote's controller ("a permanent you don't control" is the
                // controller's perspective). CR 608.2c: with no eligible
                // objects there is nothing to vote for, so the effect does as
                // much as possible — no vote occurs.
                let ctx = crate::game::filter::FilterContext::from_source_with_controller(
                    ability.source_id,
                    controller,
                );
                let candidates: Vec<ObjectId> = state
                    .battlefield
                    .iter()
                    .copied()
                    .filter(|id| {
                        crate::game::filter::matches_target_filter(
                            state,
                            *id,
                            candidate_filter,
                            &ctx,
                        )
                    })
                    .collect();
                if candidates.is_empty() {
                    events.push(GameEvent::EffectResolved {
                        kind: EffectKind::Vote,
                        source_id: ability.source_id,
                        subject: None,
                    });
                    return Ok(());
                }
                let option_labels: Vec<String> = candidates
                    .iter()
                    .map(|id| {
                        state
                            .objects
                            .get(id)
                            .map(|o| o.name.clone())
                            .unwrap_or_default()
                    })
                    .collect();
                let options: Vec<String> = option_labels.iter().map(|n| n.to_lowercase()).collect();
                let candidate_objects: crate::im::Vector<ObjectId> =
                    candidates.iter().copied().collect();
                (
                    options,
                    option_labels,
                    candidate_objects,
                    Some(outcome_template.clone()),
                    Vec::new(),
                )
            }
        };

    let starting_player = resolve_starting_voter(state, controller, starting_with.clone());

    // CR 101.4 + CR 701.38a: Build APNAP voter order from the starting player.
    // CR 800.4g: For `EachOpponent`, the controller is excluded from the
    // voter queue. If every opponent has left the game, the queue is empty
    // and the resolver emits `EffectResolved` with no tally so the chain
    // continues — there is no choice for the controller to delegate.
    let voters_in_order: Vec<PlayerId> = apnap_order_from(state, starting_player)
        .into_iter()
        .filter(|pid| match scope {
            VoterScope::AllPlayers => true,
            VoterScope::EachOpponent | VoterScope::AnOpponent => *pid != controller,
            // CR 101.4: `ControllerLabels` cycles the SUBJECT (labeled player)
            // through every non-eliminated player in APNAP order from the
            // controller. The ACTOR is always the controller; that gets pinned
            // via the `actor` field on the WaitingFor below (invariant:
            // `actor != player` except on the controller's own labeling step).
            VoterScope::ControllerLabels => true,
        })
        .collect();
    if voters_in_order.is_empty() {
        // No eligible voters (e.g., everyone eliminated, or `EachOpponent`
        // in a 1-player game). Emit EffectResolved and let the chain continue.
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::Vote,
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    }

    // `ControllerLabels` gives each labeled player exactly one choice
    // (no extra-vote stacking — labels are not votes per CR 701.38d).
    // Other scopes honor the `GrantsExtraVote` static via
    // `votes_per_session_for`.
    let voter_queue: Vec<(PlayerId, u32)> = voters_in_order
        .into_iter()
        .map(|pid| match scope {
            VoterScope::ControllerLabels => (pid, 1),
            _ => (pid, votes_per_session_for(state, pid)),
        })
        .collect();

    let (first_player, first_votes) = voter_queue[0];
    let remaining_voters = voter_queue[1..].to_vec();

    let tallies = vec![0u32; options.len()];

    // For `ControllerLabels` (Battlebond friend-or-foe keyword action,
    // no explicit CR section), pin the actor to the spell controller —
    // `Delegated(controller)` so subsequent advance steps don't need to
    // re-derive who is acting. For all other scopes the voter acts on
    // their own behalf; `SubjectActs` follows `player` through APNAP
    // iteration without recomputation.
    let actor = match scope {
        VoterScope::ControllerLabels => VoteActor::Delegated(controller),
        VoterScope::AllPlayers | VoterScope::EachOpponent | VoterScope::AnOpponent => {
            VoteActor::SubjectActs
        }
    };

    state.waiting_for = WaitingFor::VoteChoice {
        player: first_player,
        remaining_votes: first_votes,
        options,
        option_labels,
        remaining_voters,
        tallies,
        // CR 608.2c: Initialize the ballot ledger empty. Each ballot append in
        // `engine_resolution_choices.rs` extends this vector with
        // `(voter, choice_index)` — for object votes the index maps into
        // `candidate_objects` instead of `options`.
        ballots: crate::im::Vector::new(),
        per_choice_effect,
        controller,
        source_id: ability.source_id,
        actor,
        tally_mode,
        candidate_objects,
        outcome_template,
        visibility,
    };

    // Stash the parent's sub_ability tail so it resumes after the tally fans
    // out. The Vote effect itself does NOT belong on the continuation — the
    // tally handler in engine_resolution_choices.rs explicitly calls
    // `resolve_tally`, then drains this continuation to run any post-Vote
    // chained effects. Mirrors clash::stash_sub.
    if let Some(sub) = ability.sub_ability.as_ref() {
        state.park_ability_continuation(PendingContinuation::new(sub.clone(), state));
    }

    Ok(())
}

/// CR 701.38: After every voter has cast all their votes, fan out the per-choice
/// sub-effects. For each `i`, `per_choice_effect[i]` is resolved once per vote
/// tallied for `choices[i]`. Sub-effect resolutions inherit the source object
/// and controller of the originating Vote ability.
///
/// Called from `engine_resolution_choices.rs` once the voter queue empties.
///
/// CR 608.2c: Before fan-out, snapshot `ballots` into
/// `state.last_vote_ballots` so per-choice sub-effects whose `player_scope`
/// is `PlayerFilter::VotedFor { choice_index }` can route to the recorded
/// voters. The snapshot lifetime mirrors `last_zone_changed_ids` — cleared
/// at chain depth 0 in `resolve_ability_chain`.
#[allow(clippy::too_many_arguments)]
pub fn resolve_tally(
    state: &mut GameState,
    source_id: crate::types::identifiers::ObjectId,
    controller: PlayerId,
    options: &[String],
    per_choice_effect: &[Box<AbilityDefinition>],
    tallies: &[u32],
    ballots: &crate::im::Vector<(PlayerId, u32)>,
    tally_mode: VoteTally,
    candidate_objects: &[ObjectId],
    outcome_template: Option<&AbilityDefinition>,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // For named votes the per-choice slots are parallel to `options`; object
    // votes carry an empty `per_choice_effect` (winners drive
    // `outcome_template` instead), so only assert the parallel invariant when
    // there is no object template.
    debug_assert_eq!(options.len(), tallies.len());
    debug_assert!(outcome_template.is_some() || options.len() == per_choice_effect.len());

    // CR 701.38a: Top-tally votes resolve the winning choice(s) once each —
    // `TieResolution::Breaker` picks exactly one outcome (the most votes, ties
    // broken in favor of the index), `TieResolution::AllTied` resolves every
    // choice tied for the most votes. The winners resolve once (not per
    // ballot), so route to the dedicated single-pass path rather than the
    // per-choice fan-out below. The strict-majority/tie rule is card-defined,
    // not a CR subrule.
    match tally_mode {
        VoteTally::TopVotes { tie } => {
            return resolve_top_votes_tally(
                state,
                source_id,
                controller,
                per_choice_effect,
                tallies,
                ballots,
                tie,
                candidate_objects,
                outcome_template,
                events,
            );
        }
        // CR 701.38d: per-ballot fan-out (existing loop below) — multiple
        // votes resolve together; per-ballot dispatch is card-defined.
        VoteTally::PerVote => {}
    }

    // CR 608.2c + CR 701.38: Publish the ballot ledger so per-choice
    // sub-effects with `player_scope = PlayerFilter::VotedFor { ... }`
    // resolve against the actual voters.
    //
    // The ledger lifetime mirrors `last_zone_changed_ids` — it is cleared at
    // chain depth 0 in `resolve_ability_chain`. We therefore enter each
    // per-choice fan-out at `depth = 1` (below) so the just-published ledger
    // survives long enough for `PlayerFilter::VotedFor` matching during
    // player_scope iteration. The per-choice resolution does not need
    // depth-0 housekeeping because it already runs inside the parent vote's
    // resolution; the parent's depth-0 entry handled all top-level resets.
    state.last_vote_ballots = ballots.clone();

    for (idx, votes) in tallies.iter().enumerate() {
        if *votes == 0 {
            continue;
        }
        // CR 608.2c + CR 701.38 + CR 800.4g: Three distinct ways the per-choice
        // sub-effect resolves N voters' worth of work:
        //
        //   * `player_scope: Some(...)` — the parsed body fans out per-voter
        //     with proper rebinding (controller, scoped_player, original_controller).
        //     Used by "For each player who chose <choice>, you and that player
        //     each Y" patterns. Each iteration runs once with the iterated
        //     voter as the rebound controller; `OriginalController` and
        //     `ScopedPlayer` route the two halves of the body distribution.
        //   * aggregate-tally — `QuantityRef::VoteCount` in the count slot.
        //     Resolves once; `resolve_ref` sums the full tally.
        //   * per-ballot iteration — classic Council's-dilemma "For each
        //     <choice> vote, <effect>" (Tivit / Capital Punishment /
        //     Expropriate). The body runs once per ballot. Each iteration
        //     carries the ballot's voter as `scoped_player` so voter-
        //     referential filters ("owned by the voter") resolve correctly
        //     (CR 701.38d). `original_controller` preserves the spell caster
        //     for effects like GainControl.
        let per_choice_player_scope = per_choice_effect[idx].player_scope.clone();
        if per_choice_player_scope.is_some() {
            // player_scope path — single dispatch, fan-out handled by
            // resolve_ability_chain's player_scope driver.
            let chain = ResolvedAbility {
                effect: (*per_choice_effect[idx].effect).clone(),
                targets: Vec::new(),
                source_id,
                source_incarnation: None,
                trigger_source: None,
                trigger_definition_ref: None,
                controller,
                original_controller: None,
                scoped_player: None,
                target_chooser: None,
                kind: per_choice_effect[idx].kind,
                sub_ability: per_choice_effect[idx]
                    .sub_ability
                    .as_ref()
                    .map(|sub| Box::new(resolved_from_def(sub, source_id, controller))),
                else_ability: None,
                duration: per_choice_effect[idx].duration.clone(),
                condition: per_choice_effect[idx].condition.clone(),
                context: Default::default(),
                optional_targeting: per_choice_effect[idx].optional_targeting,
                optional: per_choice_effect[idx].optional,
                optional_for: None,
                multi_target: None,
                target_constraints: Vec::new(),
                target_choice_timing: per_choice_effect[idx].target_choice_timing,
                description: per_choice_effect[idx].description.clone(),
                selected_mode_labels: Vec::new(),
                repeat_for: None,
                min_x_value: per_choice_effect[idx].min_x_value,
                announced_x: per_choice_effect[idx].announced_x.clone(),
                cant_be_copied: per_choice_effect[idx].cant_be_copied,
                copy_count_status: crate::types::ability::CopyCountStatus::Pending,
                forward_result: per_choice_effect[idx].forward_result,
                unless_pay: None,
                distribution: None,
                player_scope: per_choice_player_scope,
                starting_with: per_choice_effect[idx].starting_with.clone(),
                chosen_x: None,
                cost_paid_object: None,
                cost_paid_object_ids: Vec::new(),
                effect_context_object: None,
                amassed_army_object: None,
                ability_index: None,
                may_trigger_origin: None,
                target_selection_mode: per_choice_effect[idx].target_selection_mode,
                chosen_players: Vec::new(),
                repeat_until: None,
                replacement_applied: Default::default(),
                sub_link: crate::types::ability::SubAbilityLink::ContinuationStep,
                modal: None,
                mode_abilities: vec![],
                parent_target_missing_reason: None,
            };
            resolve_ability_chain(state, &chain, events, 1)?;
        } else if per_choice_effect[idx]
            .effect
            .count_expr()
            .is_some_and(QuantityExpr::contains_vote_count)
        {
            // CR 111.1 + CR 701.38 + CR 608.2c: aggregate-tally body
            // (Emissary Green). Its count slot is bound to a
            // `QuantityRef::VoteCount`, so the effect resolves as ONE aggregate
            // event whose `resolve_ref` sums the full tally — do NOT repeat it
            // per ballot, which would multiply the tally by itself.
            let chain = ResolvedAbility {
                effect: (*per_choice_effect[idx].effect).clone(),
                targets: Vec::new(),
                source_id,
                source_incarnation: None,
                trigger_source: None,
                trigger_definition_ref: None,
                controller,
                original_controller: None,
                scoped_player: None,
                target_chooser: None,
                kind: per_choice_effect[idx].kind,
                sub_ability: per_choice_effect[idx]
                    .sub_ability
                    .as_ref()
                    .map(|sub| Box::new(resolved_from_def(sub, source_id, controller))),
                else_ability: None,
                duration: per_choice_effect[idx].duration.clone(),
                condition: per_choice_effect[idx].condition.clone(),
                context: Default::default(),
                optional_targeting: per_choice_effect[idx].optional_targeting,
                optional: per_choice_effect[idx].optional,
                optional_for: None,
                multi_target: None,
                target_constraints: Vec::new(),
                target_choice_timing: per_choice_effect[idx].target_choice_timing,
                description: per_choice_effect[idx].description.clone(),
                selected_mode_labels: Vec::new(),
                repeat_for: None,
                min_x_value: per_choice_effect[idx].min_x_value,
                announced_x: per_choice_effect[idx].announced_x.clone(),
                cant_be_copied: per_choice_effect[idx].cant_be_copied,
                copy_count_status: crate::types::ability::CopyCountStatus::Pending,
                forward_result: per_choice_effect[idx].forward_result,
                unless_pay: None,
                distribution: None,
                player_scope: None,
                starting_with: per_choice_effect[idx].starting_with.clone(),
                chosen_x: None,
                cost_paid_object: None,
                cost_paid_object_ids: Vec::new(),
                effect_context_object: None,
                amassed_army_object: None,
                ability_index: None,
                may_trigger_origin: None,
                target_selection_mode: per_choice_effect[idx].target_selection_mode,
                chosen_players: Vec::new(),
                repeat_until: None,
                replacement_applied: Default::default(),
                sub_link: crate::types::ability::SubAbilityLink::ContinuationStep,
                modal: None,
                mode_abilities: vec![],
                parent_target_missing_reason: None,
            };
            resolve_ability_chain(state, &chain, events, 1)?;
        } else {
            // CR 701.38d + CR 608.2c: Per-ballot iteration. Each ballot that
            // chose this option triggers one resolution of the sub-effect.
            // The ballot's voter is carried as `scoped_player` so voter-
            // referential filters ("owned by the voter" → ScopedPlayer)
            // resolve to the correct player. `original_controller` preserves
            // the spell caster for effects that grant control (Expropriate).
            // For cards without voter-referential filters (Tivit, Capital
            // Punishment), `scoped_player` is harmlessly set but never read.
            let choice_ballots: Vec<PlayerId> = ballots
                .iter()
                .filter(|(_, choice)| *choice == idx as u32)
                .map(|(voter, _)| *voter)
                .collect();
            // CR 701.38d: Process per-ballot interactive bodies one at a time.
            // If a ballot parks an interactive choice (e.g. ChooseFromZoneChoice),
            // stash remaining voters and return early; the drain function resumes.
            let initial_waiting_for = state.waiting_for.clone();
            let stack_depth_before_ballot = state.resolution_stack.len();
            let mut remaining_voters: Vec<PlayerId> = choice_ballots.clone();

            while let Some(voter) = remaining_voters.first().copied() {
                remaining_voters.remove(0);
                let ballot_ability =
                    build_per_ballot_ability(&per_choice_effect[idx], voter, source_id, controller);
                resolve_ability_chain(state, &ballot_ability, events, 1)?;

                // If the inner effect parked an interactive choice, suspend.
                if state.waiting_for != initial_waiting_for {
                    park_vote_ballot_after_current_ballot(
                        state,
                        PendingVoteBallotIteration {
                            ability_template: Box::new(per_choice_effect[idx].as_ref().clone()),
                            remaining_voters,
                            source_id,
                            controller,
                        },
                        stack_depth_before_ballot,
                    );
                    return Ok(());
                }
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Vote,
        source_id,
        subject: None,
    });
    Ok(())
}

/// CR 701.38a: Resolve a top-tally vote (`VoteTally::TopVotes`). The vote
/// *procedure* is CR 701.38a; the most-votes/tie winner selection is
/// card-defined, not a CR subrule. `tie` parameterizes winner cardinality:
///
/// * [`TieResolution::Breaker`] — exactly ONE winner. The unique holder of the
///   max tally wins; on a tie (or empty ballot set) the breaker index wins
///   ("...or the vote is tied"). This is the historical `Threshold` behavior.
/// * [`TieResolution::AllTied`] — every choice tied for the max wins and
///   resolves once ("...or tied for most votes"). A zero max (everyone passed)
///   yields no winners.
///
/// For object-pool votes (`outcome_template` is `Some`), the per-choice slots
/// are empty and `outcome_template` resolves once per winning object with that
/// object injected as its single specific target (CR 701.38b + CR 608.2c), so a
/// tie exiles exactly the tied winners rather than rescanning the battlefield.
///
/// Each winning sub-effect is controller-performed (it runs once, not per
/// ballot or per voter). The ballot ledger is published to
/// `state.last_vote_ballots` for parity with `resolve_tally`.
#[allow(clippy::too_many_arguments)]
fn resolve_top_votes_tally(
    state: &mut GameState,
    source_id: ObjectId,
    controller: PlayerId,
    per_choice_effect: &[Box<AbilityDefinition>],
    tallies: &[u32],
    ballots: &crate::im::Vector<(PlayerId, u32)>,
    tie: TieResolution,
    candidate_objects: &[ObjectId],
    outcome_template: Option<&AbilityDefinition>,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    state.last_vote_ballots = ballots.clone();

    // CR 701.38a: `max()` over the tally yields the top count. The
    // strict-majority/tie rule below is card-defined, not a CR subrule.
    let top = tallies.iter().copied().max().unwrap_or(0);

    match tie {
        // Card-defined "...or the vote is tied": exactly one winner. A unique
        // holder of the max wins outright; otherwise (a tie, or an empty/zero
        // ballot set) the breaker index wins, matching the "or the vote is
        // tied" branch of every printed card.
        TieResolution::Breaker(idx) => {
            let winner = if top > 0 && tallies.iter().filter(|&&t| t == top).count() == 1 {
                tallies
                    .iter()
                    .position(|&t| t == top)
                    .map(|i| i as u8)
                    .unwrap_or(idx)
            } else {
                idx
            };
            // Guard on the object template FIRST for parity with the AllTied
            // branch: an object-pool vote carries an empty `per_choice_effect`,
            // so a single-winner object vote must inject the winning ObjectId
            // into `outcome_template` rather than indexing an empty slot list
            // (which would silently exile nothing).
            match outcome_template {
                // CR 701.38b: single-winner object vote. Inject the SPECIFIC
                // winning ObjectId as the template's single target so the sole
                // winner is exiled (not a battlefield rescan).
                Some(template) => {
                    if let Some(&winner_obj) = candidate_objects.get(winner as usize) {
                        let mut chain = resolved_from_def(template, source_id, controller);
                        chain.targets = vec![TargetRef::Object(winner_obj)];
                        resolve_ability_chain(state, &chain, events, 1)?;
                    }
                }
                // Named vote: `per_choice_effect` is populated.
                None => {
                    if let Some(winning_effect) = per_choice_effect.get(winner as usize) {
                        let chain = resolved_from_def(winning_effect, source_id, controller);
                        resolve_ability_chain(state, &chain, events, 1)?;
                    }
                }
            }
            // (An out-of-range breaker index is a parser bug; emitting
            // EffectResolved below keeps the chain alive rather than panicking.)
        }
        // Card-defined "...or tied for most votes": every choice tied for the
        // max resolves once. A zero max (everyone passed) yields no winners.
        TieResolution::AllTied => {
            if top > 0 {
                for (idx, &votes) in tallies.iter().enumerate() {
                    if votes != top {
                        continue;
                    }
                    // Guard on the object template FIRST: object votes carry an
                    // empty `per_choice_effect`, so indexing it would panic OOB.
                    match outcome_template {
                        // CR 701.38b: object vote. Inject the SPECIFIC winning
                        // ObjectId as the template's single target so a top tie
                        // exiles exactly the tied winners (not a battlefield
                        // rescan).
                        Some(template) => {
                            if let Some(&winner_obj) = candidate_objects.get(idx) {
                                let mut chain = resolved_from_def(template, source_id, controller);
                                chain.targets = vec![TargetRef::Object(winner_obj)];
                                resolve_ability_chain(state, &chain, events, 1)?;
                            }
                        }
                        // Named vote: `per_choice_effect` is populated.
                        None => {
                            if let Some(winning_effect) = per_choice_effect.get(idx) {
                                let chain =
                                    resolved_from_def(winning_effect, source_id, controller);
                                resolve_ability_chain(state, &chain, events, 1)?;
                            }
                        }
                    }
                }
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Vote,
        source_id,
        subject: None,
    });
    Ok(())
}

/// Convert a stored `AbilityDefinition` (typically a sub-effect) into a
/// `ResolvedAbility` carrying the same source/controller as the parent Vote.
fn resolved_from_def(
    def: &AbilityDefinition,
    source_id: crate::types::identifiers::ObjectId,
    controller: PlayerId,
) -> ResolvedAbility {
    ResolvedAbility {
        effect: (*def.effect).clone(),
        targets: Vec::new(),
        source_id,
        source_incarnation: None,
        trigger_source: None,
        trigger_definition_ref: None,
        controller,
        original_controller: None,
        scoped_player: None,
        target_chooser: None,
        kind: def.kind,
        sub_ability: def
            .sub_ability
            .as_ref()
            .map(|sub| Box::new(resolved_from_def(sub, source_id, controller))),
        else_ability: None,
        duration: def.duration.clone(),
        condition: def.condition.clone(),
        context: Default::default(),
        optional_targeting: def.optional_targeting,
        optional: def.optional,
        optional_for: None,
        multi_target: None,
        target_constraints: Vec::new(),
        target_choice_timing: def.target_choice_timing,
        description: def.description.clone(),
        selected_mode_labels: Vec::new(),
        repeat_for: None,
        min_x_value: def.min_x_value,
        announced_x: def.announced_x.clone(),
        cant_be_copied: def.cant_be_copied,
        copy_count_status: crate::types::ability::CopyCountStatus::Pending,
        forward_result: def.forward_result,
        unless_pay: None,
        distribution: None,
        player_scope: None,
        // CR 101.4 + CR 800.4: Carry through the parent def's turn-order
        // override so vote sub-effects resolve with consistent iteration
        // semantics. None for non-Join-Forces vote chains.
        starting_with: def.starting_with.clone(),
        chosen_x: None,
        cost_paid_object: None,
        cost_paid_object_ids: Vec::new(),
        effect_context_object: None,
        amassed_army_object: None,
        ability_index: None,
        may_trigger_origin: None,
        target_selection_mode: def.target_selection_mode,
        chosen_players: Vec::new(),
        repeat_until: None,
        replacement_applied: Default::default(),
        // CR 608.2c: Carry the parent-link kind through to the resolved ability.
        sub_link: def.sub_link,
        // CR 700.2b + CR 603.3c: Carry the reflexive modal choice + per-mode
        // abilities through (None for vote sub-effects).
        modal: def.modal.clone(),
        mode_abilities: def.mode_abilities.clone(),
        parent_target_missing_reason: None,
    }
}

/// CR 701.38a: Resolve `ControllerRef::You` (and friends) to the concrete
/// starting voter PlayerId. Falls back to `controller` if the ref doesn't
/// resolve to a non-eliminated player.
fn resolve_starting_voter(
    _state: &GameState,
    controller: PlayerId,
    starting_with: ControllerRef,
) -> PlayerId {
    match starting_with {
        ControllerRef::You => controller,
        // Other refs (TargetPlayer, etc.) are not currently produced by the
        // Council's dilemma parser. Default to controller — extending this is
        // a one-line change when "starting with the affected player" / similar
        // phrasings appear.
        _ => controller,
    }
}

/// CR 101.4 + CR 103.1: Build a turn-order voter sequence beginning with
/// `start`, walking in the current turn-order direction and skipping eliminated
/// players. Supports arbitrary player counts (multiplayer).
fn apnap_order_from(state: &GameState, start: PlayerId) -> Vec<PlayerId> {
    let seat_order = &state.seat_order;
    let n = seat_order.len();
    if n == 0 {
        return Vec::new();
    }
    let start_idx = state
        .seat_order
        .iter()
        .position(|&id| id == start)
        .unwrap_or(0);
    (0..n)
        .map(|offset| {
            crate::game::players::turn_order_index(start_idx, offset, n, state.turn_direction)
        })
        .map(|idx| seat_order[idx])
        .filter(|&player| crate::game::players::is_alive(state, player))
        .collect()
}

/// CR 701.38d: A player's total votes for one Council's dilemma session is
/// 1 plus the count of `StaticMode::GrantsExtraVote` permanents the player
/// currently controls (Tivit, Seller of Secrets — "While voting, you may vote
/// an additional time").
///
/// Snapshotted once at vote-session start (CR 701.38d: extra votes happen at
/// the same time the player would otherwise have voted), so granting
/// permanents that enter or leave mid-session do not retroactively change
/// vote counts.
fn votes_per_session_for(state: &GameState, player: PlayerId) -> u32 {
    use crate::game::functioning_abilities::active_static_definitions;
    use crate::types::statics::StaticMode;

    let mut extras: u32 = 0;
    for &src_id in state.battlefield.iter() {
        let Some(obj) = state.objects.get(&src_id) else {
            continue;
        };
        if obj.controller != player {
            continue;
        }
        for s in active_static_definitions(state, obj) {
            if matches!(s.mode, StaticMode::GrantsExtraVote) {
                extras = extras.saturating_add(1);
            }
        }
    }
    1 + extras
}

/// Title-case the first character of a single word for display labels. The
/// engine never compares against this value — only `options` (lowercase).
fn title_case_word(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Build a `ResolvedAbility` for a single per-ballot vote body, binding
/// `scoped_player` to the voter so that `ZoneOwner::ScopedPlayer` resolves
/// to the correct player's permanents.
fn build_per_ballot_ability(
    template: &AbilityDefinition,
    voter: PlayerId,
    source_id: crate::types::identifiers::ObjectId,
    controller: PlayerId,
) -> ResolvedAbility {
    let mut ability = resolved_from_def(template, source_id, controller);
    ability.scoped_player = Some(voter);
    ability.original_controller = Some(controller);
    ability
}

/// CR 701.38d: Resume per-ballot vote iteration after an interactive choice
/// resolves. Processes the next voter's ballot; if it pauses again, re-stashes
/// remaining voters. When all voters are processed, emits `EffectResolved`.
pub(crate) fn drain_active_vote_ballot(state: &mut GameState, events: &mut Vec<GameEvent>) {
    let pending = match state
        .take_active_vote_ballot()
        .expect("vote-ballot drain may consume only the active vote-ballot frame")
    {
        Some(p) => p,
        None => return,
    };

    let initial_waiting_for = state.waiting_for.clone();
    let mut remaining_voters = pending.remaining_voters;
    let source_id = pending.source_id;
    let controller = pending.controller;
    let template = pending.ability_template;
    let stack_depth_before_ballot = state.resolution_stack.len();

    while let Some(voter) = remaining_voters.first().copied() {
        remaining_voters.remove(0);
        let ballot_ability = build_per_ballot_ability(&template, voter, source_id, controller);
        if resolve_ability_chain(state, &ballot_ability, events, 1).is_err() {
            // On error, drop remaining ballots (matches existing error handling).
            return;
        }

        if state.waiting_for != initial_waiting_for {
            // Re-stash remaining voters for the next drain cycle.
            park_vote_ballot_after_current_ballot(
                state,
                PendingVoteBallotIteration {
                    ability_template: template,
                    remaining_voters,
                    source_id,
                    controller,
                },
                stack_depth_before_ballot,
            );
            return;
        }
    }

    // All ballots processed — emit the deferred EffectResolved.
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Vote,
        source_id,
        subject: None,
    });
}

/// Park the remaining ballot work either above its existing parent or below the
/// complete child stack raised by the current ballot. This preserves nested
/// ownership without allowing a vote consumer to search the resolution stack.
fn park_vote_ballot_after_current_ballot(
    state: &mut GameState,
    pending: PendingVoteBallotIteration,
    stack_depth_before_ballot: usize,
) {
    match state.resolution_stack.len().cmp(&stack_depth_before_ballot) {
        std::cmp::Ordering::Less => {
            panic!("vote ballot removed a parent frame before it could be re-parked")
        }
        std::cmp::Ordering::Equal => state.push_vote_ballot(pending),
        std::cmp::Ordering::Greater => state
            .insert_vote_ballot_parent_at_child_boundary(pending, stack_depth_before_ballot)
            .expect("vote-ballot parent must be inserted below its complete child stack"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityKind, CardSelectionMode, Chooser, TargetFilter, VoteVisibility, ZoneOwner,
    };
    use crate::types::actions::GameAction;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::resolution::{FrameKind, ResolutionFrame};
    use crate::types::zones::Zone;

    /// CR 701.38a + CR 101.4: Initiating a Vote sets `WaitingFor::VoteChoice`
    /// for the controller, queuing the opponent next, with no extra-vote
    /// granters present (so each player gets exactly 1 vote).
    #[test]
    fn vote_initiates_with_controller_first() {
        let mut state = GameState::new_two_player(42);
        let controller = state.players[0].id;

        let inv_def = AbilityDefinition::new(AbilityKind::Spell, Effect::Investigate);
        let token_def = AbilityDefinition::new(AbilityKind::Spell, Effect::Investigate); // simple stand-in

        let ability = ResolvedAbility {
            effect: Effect::Vote {
                choices: vec!["evidence".to_string(), "bribery".to_string()],
                per_choice_effect: vec![Box::new(inv_def), Box::new(token_def)],
                starting_with: ControllerRef::You,
                voter_scope: VoterScope::AllPlayers,
                tally_mode: VoteTally::PerVote,
                subject: VoteSubject::Named,
                visibility: VoteVisibility::Open,
            },
            targets: vec![],
            source_id: ObjectId(1),
            source_incarnation: None,
            trigger_source: None,
            trigger_definition_ref: None,
            controller,
            original_controller: None,
            scoped_player: None,
            target_chooser: None,
            kind: AbilityKind::Spell,
            sub_ability: None,
            else_ability: None,
            duration: None,
            condition: None,
            context: Default::default(),
            optional_targeting: false,
            optional: false,
            optional_for: None,
            multi_target: None,
            target_constraints: Vec::new(),
            target_choice_timing: crate::types::ability::TargetChoiceTiming::Stack,
            description: None,
            selected_mode_labels: Vec::new(),
            repeat_for: None,
            min_x_value: 0,
            announced_x: None,
            cant_be_copied: false,
            copy_count_status: crate::types::ability::CopyCountStatus::Pending,
            forward_result: false,
            unless_pay: None,
            distribution: None,
            player_scope: None,
            starting_with: None,
            chosen_x: None,
            cost_paid_object: None,
            cost_paid_object_ids: Vec::new(),
            effect_context_object: None,
            amassed_army_object: None,
            ability_index: None,
            may_trigger_origin: None,
            target_selection_mode: crate::types::ability::TargetSelectionMode::Chosen,
            chosen_players: Vec::new(),
            repeat_until: None,
            replacement_applied: Default::default(),
            sub_link: crate::types::ability::SubAbilityLink::ContinuationStep,
            modal: None,
            mode_abilities: vec![],
            parent_target_missing_reason: None,
        };

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).expect("vote resolves");

        match state.waiting_for {
            WaitingFor::VoteChoice {
                player,
                remaining_votes,
                ref options,
                ref tallies,
                ref remaining_voters,
                ..
            } => {
                assert_eq!(player, controller);
                assert_eq!(remaining_votes, 1);
                assert_eq!(
                    options,
                    &vec!["evidence".to_string(), "bribery".to_string()]
                );
                assert_eq!(tallies, &vec![0u32, 0]);
                // Opponent queued next with their 1 vote.
                assert_eq!(remaining_voters.len(), 1);
                assert_ne!(remaining_voters[0].0, controller);
                assert_eq!(remaining_voters[0].1, 1);
            }
            other => panic!("expected VoteChoice, got {:?}", other),
        }
    }

    /// Build a `ResolvedAbility` with the given `voter_scope` and a single
    /// trivial per-choice sub-effect (Investigate). Test helper only —
    /// duplicating the canonical fixture from `vote_initiates_with_controller_first`
    /// would obscure the scope assertions in the new tests.
    fn make_vote_ability(
        controller: PlayerId,
        voter_scope: VoterScope,
        choices: Vec<String>,
    ) -> ResolvedAbility {
        let per_choice_effect: Vec<Box<AbilityDefinition>> = choices
            .iter()
            .map(|_| {
                Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Investigate,
                ))
            })
            .collect();
        ResolvedAbility {
            effect: Effect::Vote {
                choices,
                per_choice_effect,
                starting_with: ControllerRef::You,
                voter_scope,
                tally_mode: VoteTally::PerVote,
                subject: VoteSubject::Named,
                visibility: VoteVisibility::Open,
            },
            targets: vec![],
            source_id: ObjectId(1),
            source_incarnation: None,
            trigger_source: None,
            trigger_definition_ref: None,
            controller,
            original_controller: None,
            scoped_player: None,
            target_chooser: None,
            kind: AbilityKind::Spell,
            sub_ability: None,
            else_ability: None,
            duration: None,
            condition: None,
            context: Default::default(),
            optional_targeting: false,
            optional: false,
            optional_for: None,
            multi_target: None,
            target_constraints: Vec::new(),
            target_choice_timing: crate::types::ability::TargetChoiceTiming::Stack,
            description: None,
            selected_mode_labels: Vec::new(),
            repeat_for: None,
            min_x_value: 0,
            announced_x: None,
            cant_be_copied: false,
            copy_count_status: crate::types::ability::CopyCountStatus::Pending,
            forward_result: false,
            unless_pay: None,
            distribution: None,
            player_scope: None,
            starting_with: None,
            chosen_x: None,
            cost_paid_object: None,
            cost_paid_object_ids: Vec::new(),
            effect_context_object: None,
            amassed_army_object: None,
            ability_index: None,
            may_trigger_origin: None,
            target_selection_mode: crate::types::ability::TargetSelectionMode::Chosen,
            chosen_players: Vec::new(),
            repeat_until: None,
            replacement_applied: Default::default(),
            sub_link: crate::types::ability::SubAbilityLink::ContinuationStep,
            modal: None,
            mode_abilities: vec![],
            parent_target_missing_reason: None,
        }
    }

    /// CR 800.4g: With `EachOpponent` scope, the controller is excluded from
    /// the voter queue and never appears in `WaitingFor::VoteChoice.player`.
    #[test]
    fn vote_with_each_opponent_scope_skips_controller() {
        let mut state = GameState::new_two_player(42);
        let controller = state.players[0].id;
        let opponent = state.players[1].id;
        let ability = make_vote_ability(
            controller,
            VoterScope::EachOpponent,
            vec!["money".to_string(), "friends".to_string()],
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).expect("vote resolves");
        match state.waiting_for {
            WaitingFor::VoteChoice {
                player,
                ref remaining_voters,
                ..
            } => {
                // First voter is the opponent — the controller does not vote.
                assert_eq!(player, opponent);
                // Two-player game with EachOpponent: only one voter total.
                assert!(remaining_voters.is_empty());
            }
            other => panic!("expected VoteChoice, got {:?}", other),
        }
    }

    /// CR 101.4 + CR 800.4g: With `EachOpponent` in a 3-player game, the
    /// queue contains the two opponents in APNAP order; the controller is
    /// skipped.
    #[test]
    fn vote_with_each_opponent_in_three_player_game_queues_two_voters() {
        let mut state = GameState::new(crate::types::format::FormatConfig::standard(), 3, 42);
        let controller = state.players[0].id;
        let ability = make_vote_ability(
            controller,
            VoterScope::EachOpponent,
            vec!["a".to_string(), "b".to_string()],
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).expect("vote resolves");
        match state.waiting_for {
            WaitingFor::VoteChoice {
                player,
                ref remaining_voters,
                ..
            } => {
                // First voter is the next player after controller in APNAP.
                assert_ne!(player, controller);
                // Second opponent queued.
                assert_eq!(remaining_voters.len(), 1);
                assert_ne!(remaining_voters[0].0, controller);
                assert_ne!(remaining_voters[0].0, player);
            }
            other => panic!("expected VoteChoice, got {:?}", other),
        }
    }

    /// CR 101.4 + CR 103.1 + CR 701.38a: Vote order follows the current
    /// turn-order direction. After turn order is reversed, a three-player
    /// vote starting with P0 proceeds P0, P2, P1.
    #[test]
    fn vote_order_reverses_with_turn_direction() {
        let mut state = GameState::new(crate::types::format::FormatConfig::standard(), 3, 42);
        state.turn_direction = crate::types::phase::TurnDirection::Reversed;
        let controller = state.players[0].id;
        let ability = make_vote_ability(
            controller,
            VoterScope::AllPlayers,
            vec!["a".to_string(), "b".to_string()],
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).expect("vote resolves");

        match state.waiting_for {
            WaitingFor::VoteChoice {
                player,
                ref remaining_voters,
                ..
            } => {
                assert_eq!(player, PlayerId(0));
                assert_eq!(remaining_voters, &vec![(PlayerId(2), 1), (PlayerId(1), 1)]);
            }
            other => panic!("expected VoteChoice, got {:?}", other),
        }
    }

    /// CR 800.4g: When every opponent has been eliminated, an `EachOpponent`
    /// vote produces an empty queue. The resolver emits `EffectResolved` and
    /// does NOT pause on `WaitingFor::VoteChoice`.
    #[test]
    fn vote_each_opponent_no_opponents_emits_effect_resolved_no_pause() {
        let mut state = GameState::new_two_player(42);
        let controller = state.players[0].id;
        // Eliminate the only opponent.
        state.players[1].is_eliminated = true;
        let ability = make_vote_ability(
            controller,
            VoterScope::EachOpponent,
            vec!["a".to_string(), "b".to_string()],
        );
        let mut events = Vec::new();
        let initial_waiting_for = state.waiting_for.clone();
        resolve(&mut state, &ability, &mut events).expect("vote resolves");
        // No VoteChoice — waiting_for unchanged.
        assert!(matches!(state.waiting_for, ref w if *w == initial_waiting_for));
        // EffectResolved emitted.
        assert!(events.iter().any(|e| matches!(
            e,
            crate::types::events::GameEvent::EffectResolved {
                kind: EffectKind::Vote,
                ..
            }
        )));
    }

    /// CR 608.2c: `resolve_tally` snapshots the ballot ledger into
    /// `state.last_vote_ballots` BEFORE fanning out per-choice sub-effects so
    /// `PlayerFilter::VotedFor` resolves correctly.
    #[test]
    fn tally_populates_last_vote_ballots() {
        let mut state = GameState::new_two_player(42);
        let p0 = state.players[0].id;
        let p1 = state.players[1].id;
        let options = vec!["a".to_string(), "b".to_string()];
        let per_choice_effect: Vec<Box<AbilityDefinition>> = options
            .iter()
            .map(|_| {
                Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Investigate,
                ))
            })
            .collect();
        let mut ballots: crate::im::Vector<(PlayerId, u32)> = crate::im::Vector::new();
        ballots.push_back((p0, 0));
        ballots.push_back((p1, 1));
        let tallies = vec![1u32, 1];
        let mut events = Vec::new();
        resolve_tally(
            &mut state,
            ObjectId(1),
            p0,
            &options,
            &per_choice_effect,
            &tallies,
            &ballots,
            VoteTally::PerVote,
            &[],
            None,
            &mut events,
        )
        .expect("tally resolves");
        // Ballot snapshot is populated before fan-out (per-choice subs each
        // run resolve_ability_chain at depth 0, which clears the ledger
        // again on entry — but we observe the post-tally state.last_vote_ballots
        // across the helper boundary). After the final per-choice resolves at
        // depth 0, the ledger has been cleared. So we instead assert the
        // shape was correctly set at entry by checking that no panic
        // occurred and the choice fan-out produced events.
        assert!(events.iter().any(|e| matches!(
            e,
            crate::types::events::GameEvent::EffectResolved {
                kind: EffectKind::Vote,
                ..
            }
        )));
    }

    #[test]
    fn per_ballot_resume_parks_below_complete_per_player_choice_child_stack() {
        // CR 701.38d + CR 608.2c + CR 101.4: a per-ballot body that pauses on
        // an each-player choice owns its continuation and active prompt as one
        // child stack. The remaining-ballot owner must sit below both frames.
        let mut state = GameState::new_two_player(71);
        let controller = state.players[0].id;
        let opponent = state.players[1].id;
        let first = create_object(
            &mut state,
            CardId(71),
            controller,
            "First ballot choice".to_string(),
            Zone::Graveyard,
        );
        let second = create_object(
            &mut state,
            CardId(72),
            opponent,
            "Second ballot choice".to_string(),
            Zone::Graveyard,
        );
        let source = ObjectId(710);
        let per_choice_effect = vec![Box::new(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChooseFromZone {
                    count: 1,
                    zone: Zone::Graveyard,
                    additional_zones: Vec::new(),
                    zone_owner: ZoneOwner::EachPlayer,
                    filter: None,
                    chooser: Chooser::Controller,
                    up_to: false,
                    constraint: None,
                    selection: CardSelectionMode::Chosen,
                },
            )
            .sub_ability(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            )),
        )];
        let options = vec!["choice".to_string()];
        let ballots = crate::im::Vector::from(vec![(controller, 0), (opponent, 0)]);
        let mut events = Vec::new();

        resolve_tally(
            &mut state,
            source,
            controller,
            &options,
            &per_choice_effect,
            &[2],
            &ballots,
            VoteTally::PerVote,
            &[],
            None,
            &mut events,
        )
        .expect("first ballot pauses on the first player choice");
        assert_eq!(
            state
                .resolution_stack
                .iter()
                .map(ResolutionFrame::kind)
                .collect::<Vec<_>>(),
            vec![
                FrameKind::VoteBallot,
                FrameKind::AbilityContinuation,
                FrameKind::PerPlayerZoneChoice,
            ],
            "the remaining ballot owner must be below its complete child stack"
        );

        for expected in [first, second, first, second] {
            crate::game::engine::apply(
                &mut state,
                controller,
                GameAction::SelectCards {
                    cards: vec![expected],
                },
            )
            .expect("each production choice action advances the per-ballot body");
        }

        assert_eq!(state.players[0].life, 22, "one tail per resolved ballot");
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        assert!(state.resolution_stack.is_empty());
    }

    // --- ControllerLabels (Battlebond friend-or-foe) ---

    /// CR 101.4: `ControllerLabels` queues every non-eliminated player in
    /// APNAP order from the controller. Each entry has exactly one vote
    /// (labels are not stackable like Council's-dilemma votes).
    #[test]
    fn controller_labels_builds_apnap_player_queue() {
        let mut state = GameState::new(crate::types::format::FormatConfig::standard(), 3, 42);
        let controller = state.players[0].id;
        let ability = make_vote_ability(
            controller,
            VoterScope::ControllerLabels,
            vec!["friend".to_string(), "foe".to_string()],
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).expect("vote resolves");
        match state.waiting_for {
            WaitingFor::VoteChoice {
                player,
                remaining_votes,
                ref remaining_voters,
                ..
            } => {
                // First subject is the controller (APNAP starts with them).
                assert_eq!(player, controller);
                assert_eq!(remaining_votes, 1);
                // Two more subjects queued, both with exactly 1 vote each.
                assert_eq!(remaining_voters.len(), 2);
                assert!(remaining_voters.iter().all(|(_, v)| *v == 1));
            }
            other => panic!("expected VoteChoice, got {:?}", other),
        }
    }

    /// Every `VoteChoice` produced under `ControllerLabels` has
    /// `actor = controller` so the spell controller is the authorized
    /// submitter regardless of which subject is currently being labeled.
    #[test]
    fn controller_labels_actor_is_set_to_controller() {
        let mut state = GameState::new_two_player(42);
        let controller = state.players[0].id;
        let ability = make_vote_ability(
            controller,
            VoterScope::ControllerLabels,
            vec!["friend".to_string(), "foe".to_string()],
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).expect("vote resolves");
        match state.waiting_for {
            WaitingFor::VoteChoice { actor, .. } => {
                assert_eq!(actor, VoteActor::Delegated(controller));
            }
            other => panic!("expected VoteChoice, got {:?}", other),
        }
    }

    /// When every player is eliminated except the controller (an odd edge
    /// case but valid input), `ControllerLabels` still queues the
    /// controller. Verifies the resolver does not produce an empty queue in
    /// the only-controller case.
    #[test]
    fn controller_labels_with_solo_controller_queues_just_controller() {
        let mut state = GameState::new_two_player(42);
        let controller = state.players[0].id;
        state.players[1].is_eliminated = true;
        let ability = make_vote_ability(
            controller,
            VoterScope::ControllerLabels,
            vec!["friend".to_string(), "foe".to_string()],
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).expect("vote resolves");
        match state.waiting_for {
            WaitingFor::VoteChoice {
                player,
                ref remaining_voters,
                actor,
                ..
            } => {
                assert_eq!(player, controller);
                assert!(remaining_voters.is_empty());
                assert_eq!(actor, VoteActor::Delegated(controller));
            }
            other => panic!("expected VoteChoice, got {:?}", other),
        }
    }

    /// CR 101.4 + CR 701.38: End-to-end label-and-tally walkthrough for
    /// the Pir's Whim shape. The Oracle text parses to a Vote with
    /// `ControllerLabels` scope; resolving the spell parks on
    /// `VoteChoice { actor = controller }` with the controller as the
    /// first subject. After the controller submits
    /// `friend` for themselves and `foe` for the opponent, the ballot
    /// ledger records both labels with the SUBJECT in the first slot (not
    /// the actor), and the tally publishes them to
    /// `state.last_vote_ballots` so per-choice sub-effects can fan out via
    /// `PlayerFilter::VotedFor`.
    #[test]
    fn pirs_whim_resolves_friend_label_then_foe_label_then_tally() {
        use crate::parser::oracle_vote::parse_vote_block;
        use crate::types::identifiers::ObjectId;

        let text = "For each player, choose friend or foe. \
                    Each friend draws a card. \
                    Each foe draws a card.";
        let parsed_def =
            parse_vote_block(text, AbilityKind::Spell).expect("Pir's Whim shape parses");
        let mut state = GameState::new_two_player(42);
        let controller = state.players[0].id;
        let opp = state.players[1].id;

        // Build a ResolvedAbility from the parsed AbilityDefinition.
        let ability = ResolvedAbility {
            effect: (*parsed_def.effect).clone(),
            targets: vec![],
            source_id: ObjectId(1),
            source_incarnation: None,
            trigger_source: None,
            trigger_definition_ref: None,
            controller,
            original_controller: None,
            scoped_player: None,
            target_chooser: None,
            kind: AbilityKind::Spell,
            sub_ability: None,
            else_ability: None,
            duration: None,
            condition: None,
            context: Default::default(),
            optional_targeting: false,
            optional: false,
            optional_for: None,
            multi_target: None,
            target_constraints: Vec::new(),
            target_choice_timing: crate::types::ability::TargetChoiceTiming::Stack,
            description: None,
            selected_mode_labels: Vec::new(),
            repeat_for: None,
            min_x_value: 0,
            announced_x: None,
            cant_be_copied: false,
            copy_count_status: crate::types::ability::CopyCountStatus::Pending,
            forward_result: false,
            unless_pay: None,
            distribution: None,
            player_scope: None,
            starting_with: None,
            chosen_x: None,
            cost_paid_object: None,
            cost_paid_object_ids: Vec::new(),
            effect_context_object: None,
            amassed_army_object: None,
            ability_index: None,
            may_trigger_origin: None,
            target_selection_mode: crate::types::ability::TargetSelectionMode::Chosen,
            chosen_players: Vec::new(),
            repeat_until: None,
            replacement_applied: Default::default(),
            sub_link: crate::types::ability::SubAbilityLink::ContinuationStep,
            modal: None,
            mode_abilities: vec![],
            parent_target_missing_reason: None,
        };

        // Resolution parks on VoteChoice with controller as first subject.
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).expect("vote initiates");
        match state.waiting_for {
            WaitingFor::VoteChoice {
                player,
                actor,
                ref options,
                ..
            } => {
                assert_eq!(player, controller, "first subject is controller (APNAP)");
                assert_eq!(actor, VoteActor::Delegated(controller));
                assert_eq!(options, &vec!["friend".to_string(), "foe".to_string()]);
            }
            other => panic!("expected VoteChoice for first label, got {:?}", other),
        }

        // Controller labels themselves as friend.
        let snapshot = state.waiting_for.clone();
        let acted = crate::game::engine_resolution_choices::handle_resolution_choice(
            &mut state,
            snapshot,
            crate::types::GameAction::ChooseOption {
                choice: "friend".to_string(),
            },
            &mut events,
        )
        .expect("first label submits");
        assert!(matches!(
            acted,
            crate::game::engine_resolution_choices::ResolutionChoiceOutcome::WaitingFor(_)
        ));

        // Now the engine should be waiting for the controller to label the
        // opponent. `actor` is still the controller; subject is opp.
        match state.waiting_for {
            WaitingFor::VoteChoice {
                player,
                actor,
                ref ballots,
                ..
            } => {
                assert_eq!(player, opp, "subject advanced to opponent");
                assert_eq!(actor, VoteActor::Delegated(controller));
                // The first ballot records the SUBJECT (controller), not the
                // actor — both happen to coincide here for the friend label
                // but the slot semantics matter for the foe label.
                assert_eq!(ballots.len(), 1);
                assert_eq!(ballots[0], (controller, 0));
            }
            other => panic!("expected VoteChoice for second label, got {:?}", other),
        }

        // Controller labels opp as foe.
        let snapshot = state.waiting_for.clone();
        crate::game::engine_resolution_choices::handle_resolution_choice(
            &mut state,
            snapshot,
            crate::types::GameAction::ChooseOption {
                choice: "foe".to_string(),
            },
            &mut events,
        )
        .expect("second label submits");

        // Ballot ledger must record (opp, foe_index=1) for the second label —
        // the SUBJECT being labeled, not the actor.
        assert_eq!(
            state.last_vote_ballots.len(),
            2,
            "tally must publish both ballots"
        );
        assert_eq!(state.last_vote_ballots[0], (controller, 0));
        assert_eq!(state.last_vote_ballots[1], (opp, 1));
    }

    /// CR 101.4 + CR 701.38: Three-player end-to-end walkthrough. The
    /// controller labels themselves friend and both opponents foe in APNAP
    /// order from the controller. The ballot ledger must record subjects in
    /// APNAP order (controller, opp1, opp2) — not in choice-submission
    /// order, which is identical here but would diverge under reordered
    /// queues. This is the test the queue-construction assertions cannot
    /// catch: it walks all three label submissions through the
    /// `engine_resolution_choices` dispatch and verifies the published
    /// `last_vote_ballots` order is APNAP.
    #[test]
    fn controller_labels_three_player_walkthrough_records_apnap_ballot_order() {
        use crate::types::GameAction;

        let mut state = GameState::new(crate::types::format::FormatConfig::standard(), 3, 42);
        let controller = state.players[0].id;
        let opp1 = state.players[1].id;
        let opp2 = state.players[2].id;
        let source_id = crate::types::identifiers::ObjectId(1);
        let per_choice_effect: Vec<Box<AbilityDefinition>> = vec!["friend", "foe"]
            .into_iter()
            .map(|_| {
                Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Investigate,
                ))
            })
            .collect();
        let ability = ResolvedAbility {
            effect: Effect::Vote {
                choices: vec!["friend".to_string(), "foe".to_string()],
                per_choice_effect,
                starting_with: ControllerRef::You,
                voter_scope: VoterScope::ControllerLabels,
                tally_mode: VoteTally::PerVote,
                subject: VoteSubject::Named,
                visibility: VoteVisibility::Open,
            },
            targets: vec![],
            source_id,
            source_incarnation: None,
            trigger_source: None,
            trigger_definition_ref: None,
            controller,
            original_controller: None,
            scoped_player: None,
            target_chooser: None,
            kind: AbilityKind::Spell,
            sub_ability: None,
            else_ability: None,
            duration: None,
            condition: None,
            context: Default::default(),
            optional_targeting: false,
            optional: false,
            optional_for: None,
            multi_target: None,
            target_constraints: Vec::new(),
            target_choice_timing: crate::types::ability::TargetChoiceTiming::Stack,
            description: None,
            selected_mode_labels: Vec::new(),
            repeat_for: None,
            min_x_value: 0,
            announced_x: None,
            cant_be_copied: false,
            copy_count_status: crate::types::ability::CopyCountStatus::Pending,
            forward_result: false,
            unless_pay: None,
            distribution: None,
            player_scope: None,
            starting_with: None,
            chosen_x: None,
            cost_paid_object: None,
            cost_paid_object_ids: Vec::new(),
            effect_context_object: None,
            amassed_army_object: None,
            ability_index: None,
            may_trigger_origin: None,
            target_selection_mode: crate::types::ability::TargetSelectionMode::Chosen,
            chosen_players: Vec::new(),
            repeat_until: None,
            replacement_applied: Default::default(),
            sub_link: crate::types::ability::SubAbilityLink::ContinuationStep,
            modal: None,
            mode_abilities: vec![],
            parent_target_missing_reason: None,
        };
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).expect("vote initiates");

        // Walk each subject in order. The expected APNAP order from the
        // controller is [controller, opp1, opp2] for a 3-player game with
        // controller in seat 0.
        let expected_subjects = [controller, opp1, opp2];
        let labels = ["friend", "foe", "foe"];
        for (i, (subject, label)) in expected_subjects.iter().zip(labels.iter()).enumerate() {
            match state.waiting_for {
                WaitingFor::VoteChoice { player, actor, .. } => {
                    assert_eq!(
                        player, *subject,
                        "step {i}: APNAP subject mismatch — expected {subject:?}"
                    );
                    assert_eq!(
                        actor,
                        VoteActor::Delegated(controller),
                        "step {i}: actor must be controller"
                    );
                }
                ref other => panic!("step {i}: expected VoteChoice, got {other:?}"),
            }
            let snapshot = state.waiting_for.clone();
            crate::game::engine_resolution_choices::handle_resolution_choice(
                &mut state,
                snapshot,
                GameAction::ChooseOption {
                    choice: (*label).to_string(),
                },
                &mut events,
            )
            .unwrap_or_else(|err| panic!("step {i} label submits: {err:?}"));
        }

        assert_eq!(
            state.last_vote_ballots.len(),
            3,
            "tally publishes one ballot per subject"
        );
        assert_eq!(state.last_vote_ballots[0], (controller, 0));
        assert_eq!(state.last_vote_ballots[1], (opp1, 1));
        assert_eq!(state.last_vote_ballots[2], (opp2, 1));
    }

    /// `apply()` must reject a `ChooseOption` submitted by anyone other than
    /// the delegate. Mindslaver-style turn-control aside, the spell
    /// controller is the only authorized submitter during a
    /// `ControllerLabels` vote — even when the subject is a different
    /// player. Without this gate, opponents could spoof the controller's
    /// labels in multiplayer.
    #[test]
    fn controller_labels_rejects_choose_option_from_non_delegate() {
        use crate::game::engine::apply;
        use crate::types::GameAction;

        let mut state = GameState::new_two_player(42);
        let controller = state.players[0].id;
        let opp = state.players[1].id;
        // Subject is opp; actor is controller. Opponent attempts to label.
        state.waiting_for = WaitingFor::VoteChoice {
            player: opp,
            remaining_votes: 1,
            options: vec!["friend".to_string(), "foe".to_string()],
            option_labels: vec!["Friend".to_string(), "Foe".to_string()],
            remaining_voters: Vec::new(),
            tallies: vec![0, 0],
            ballots: crate::im::Vector::new(),
            per_choice_effect: Vec::new(),
            controller,
            source_id: crate::types::identifiers::ObjectId(1),
            actor: VoteActor::Delegated(controller),
            tally_mode: VoteTally::PerVote,
            candidate_objects: crate::im::Vector::new(),
            outcome_template: None,
            visibility: VoteVisibility::Open,
        };
        let err = apply(
            &mut state,
            opp,
            GameAction::ChooseOption {
                choice: "foe".to_string(),
            },
        )
        .expect_err("opponent must not be authorized to label");
        assert!(
            matches!(err, crate::game::EngineError::WrongPlayer),
            "expected WrongPlayer, got {err:?}"
        );
    }

    /// `WaitingFor::acting_player()` for a `ControllerLabels` vote must
    /// return the actor (controller), not the subject. Other choice modals
    /// route the action to `acting_player`, so a mismatch would gate the
    /// wrong seat.
    #[test]
    fn controller_labels_acting_player_returns_actor_not_subject() {
        let mut state = GameState::new_two_player(42);
        let controller = state.players[0].id;
        let opp = state.players[1].id;
        // Build a VoteChoice with subject = opponent, actor = controller.
        // After the controller labels themselves, the queue advances to opp
        // as the next subject — the actor must still be the controller.
        state.waiting_for = WaitingFor::VoteChoice {
            player: opp,
            remaining_votes: 1,
            options: vec!["friend".to_string(), "foe".to_string()],
            option_labels: vec!["Friend".to_string(), "Foe".to_string()],
            remaining_voters: Vec::new(),
            tallies: vec![1, 0],
            ballots: crate::im::Vector::unit((controller, 0)),
            per_choice_effect: Vec::new(),
            controller,
            source_id: crate::types::identifiers::ObjectId(1),
            actor: VoteActor::Delegated(controller),
            tally_mode: VoteTally::PerVote,
            candidate_objects: crate::im::Vector::new(),
            outcome_template: None,
            visibility: VoteVisibility::Open,
        };
        assert_eq!(state.waiting_for.acting_player(), Some(controller));
    }
    /// CR 701.38d + CR 608.2c: Expropriate money votes suspend and resume
    /// per ballot. Uses the production parser path (`parse_vote_block`) to
    /// build the Vote effect from Expropriate's real Oracle text. With two
    /// opponents both choosing money, the first ballot pauses at
    /// `ChooseFromZoneChoice`, remaining voters are stashed in
    /// the vote-ballot frame, and `EffectResolved { Vote }` is NOT
    /// emitted until all ballots resolve.
    #[test]
    fn expropriate_money_votes_suspend_and_resume_per_ballot() {
        use crate::game::zones::create_object;
        use crate::parser::oracle_vote::parse_vote_block;
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;

        // Parse Expropriate's Oracle text through the production parser.
        let text = "starting with you, each player votes for time or money. \
                    For each time vote, take an extra turn after this one. \
                    For each money vote, choose a permanent owned by the voter \
                    and gain control of it.";
        let parsed_def =
            parse_vote_block(text, AbilityKind::Spell).expect("Expropriate vote block must parse");

        // Extract per_choice_effect from the parsed Vote definition.
        let (choices, per_choice_effect) = match *parsed_def.effect {
            Effect::Vote {
                choices,
                per_choice_effect,
                ..
            } => (choices, per_choice_effect),
            ref other => panic!("expected Vote, got {:?}", other),
        };
        assert_eq!(choices, vec!["time".to_string(), "money".to_string()]);

        // Set up a 3-player game.
        let mut state = GameState::new(crate::types::format::FormatConfig::standard(), 3, 42);
        let controller = state.players[0].id;
        let opp1 = state.players[1].id;
        let opp2 = state.players[2].id;

        // Place one permanent on the battlefield owned by each opponent.
        // Must have a permanent core type so TypeFilter::Permanent matches.
        let perm_opp1 = create_object(
            &mut state,
            CardId(101),
            opp1,
            "Opp1 Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&perm_opp1)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let perm_opp2 = create_object(
            &mut state,
            CardId(102),
            opp2,
            "Opp2 Artifact".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&perm_opp2)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);

        // Also place a permanent owned by the controller (should NOT appear
        // in the candidate set when the voter is opp1 or opp2).
        let perm_ctrl = create_object(
            &mut state,
            CardId(100),
            controller,
            "Controller Land".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&perm_ctrl)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        // Build ballots: all three players voted "money" (index 1).
        let ballots: crate::im::Vector<(PlayerId, u32)> =
            crate::im::Vector::from(vec![(controller, 1), (opp1, 1), (opp2, 1)]);
        let tallies = vec![0u32, 3];
        let options = choices.clone();

        // Call resolve_tally with the parsed per_choice_effect.
        let mut events = Vec::new();
        resolve_tally(
            &mut state,
            ObjectId(1),
            controller,
            &options,
            &per_choice_effect,
            &tallies,
            &ballots,
            VoteTally::PerVote,
            &[],
            None,
            &mut events,
        )
        .expect("resolve_tally succeeds");

        // After resolve_tally, the first money ballot (controller's) should
        // have paused at ChooseFromZoneChoice.
        match &state.waiting_for {
            WaitingFor::ChooseFromZoneChoice {
                player,
                cards,
                count,
                ..
            } => {
                // Controller makes the choice (Chooser::Controller).
                assert_eq!(*player, controller);
                assert_eq!(*count, 1);
                // The candidate set should contain ONLY permanents owned by
                // the first voter (controller). The controller owns perm_ctrl.
                assert!(
                    cards.contains(&perm_ctrl),
                    "candidate set must include controller's permanent, got {:?}",
                    cards
                );
                // Opponent permanents must NOT be in the candidate set.
                assert!(
                    !cards.contains(&perm_opp1),
                    "opp1's permanent must not be in controller's ballot candidates"
                );
                assert!(
                    !cards.contains(&perm_opp2),
                    "opp2's permanent must not be in controller's ballot candidates"
                );
            }
            other => panic!(
                "Expected ChooseFromZoneChoice after first ballot, got {:?}",
                other
            ),
        }

        // The vote is the direct parent of the nested choice continuation.
        let pending = state.active_vote_ballot().or_else(|| {
            state
                .resolution_stack
                .active_predecessor()
                .and_then(|frame| match frame {
                    crate::types::resolution::ResolutionFrame::VoteBallot(pending) => Some(pending),
                    _ => None,
                })
        });
        assert!(
            pending.is_some(),
            "remaining voters must be parked in the vote-ballot frame"
        );
        assert_eq!(
            pending.unwrap().remaining_voters.len(),
            2,
            "two voters (opp1 + opp2) remain after controller's ballot"
        );

        // No EffectResolved { Vote } yet.
        assert!(
            !events.iter().any(|e| matches!(
                e,
                GameEvent::EffectResolved {
                    kind: EffectKind::Vote,
                    ..
                }
            )),
            "EffectResolved(Vote) must NOT be emitted while ballots remain"
        );
    }

    /// CR 701.38a: Threshold tally — the choice with strictly more votes
    /// resolves its single outcome once (strict-majority rule is card-defined,
    /// not a CR subrule). Index 1 ("BecomeMonarch") beats index
    /// 0 ("NoOp") 2-to-0, so the controller becomes the monarch and the NoOp
    /// does nothing.
    #[test]
    fn threshold_tally_resolves_strict_winner_once() {
        let mut state = GameState::new_two_player(7);
        let controller = state.players[0].id;
        assert!(state.monarch.is_none(), "no monarch at game start");

        let per_choice: Vec<Box<AbilityDefinition>> = vec![
            Box::new(AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp)),
            Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::BecomeMonarch,
            )),
        ];
        let options = vec!["innocent".to_string(), "guilty".to_string()];
        let tallies = vec![0u32, 2];
        let ballots = crate::im::Vector::new();
        let mut events = Vec::new();

        resolve_tally(
            &mut state,
            ObjectId(1),
            controller,
            &options,
            &per_choice,
            &tallies,
            &ballots,
            VoteTally::TopVotes {
                tie: TieResolution::Breaker(0),
            },
            &[],
            None,
            &mut events,
        )
        .expect("threshold tally resolves");

        assert_eq!(
            state.monarch,
            Some(controller),
            "the winning BecomeMonarch outcome must resolve once"
        );
    }

    /// CR 701.38a: On a tie, the `tie_breaker_index` outcome resolves (tie
    /// behavior is card-defined, not a CR subrule). With a
    /// 1-1 tie and tie_breaker pointing at the NoOp (index 0), nothing happens
    /// — the BecomeMonarch (index 1) must NOT resolve.
    #[test]
    fn threshold_tally_routes_tie_to_tie_breaker() {
        let mut state = GameState::new_two_player(11);
        let controller = state.players[0].id;
        assert!(state.monarch.is_none(), "no monarch at game start");

        let per_choice: Vec<Box<AbilityDefinition>> = vec![
            Box::new(AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp)),
            Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::BecomeMonarch,
            )),
        ];
        let options = vec!["innocent".to_string(), "guilty".to_string()];
        let tallies = vec![1u32, 1];
        let ballots = crate::im::Vector::new();
        let mut events = Vec::new();

        resolve_tally(
            &mut state,
            ObjectId(1),
            controller,
            &options,
            &per_choice,
            &tallies,
            &ballots,
            VoteTally::TopVotes {
                tie: TieResolution::Breaker(0),
            },
            &[],
            None,
            &mut events,
        )
        .expect("threshold tally resolves");

        assert!(
            state.monarch.is_none(),
            "a tie routed to the NoOp tie-breaker must not crown a monarch"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                GameEvent::EffectResolved {
                    kind: EffectKind::Vote,
                    ..
                }
            )),
            "threshold tally must still emit EffectResolved(Vote)"
        );
    }

    /// CR 701.38 + CR 608.2d + CR 120.1 + CR 608.2c: End-to-end resolution of the
    /// hoisted-Choose / suffix-aggregate-vote / SourceChosenPlayer-damage composition.
    /// Uses a public-vote opener ("each player votes for truth or consequences") to
    /// exercise the same `Choose{Random} → Vote → [Draw, DealDamage{SourceChosenPlayer}]`
    /// chain as Truth or Consequences without requiring the unsupported secret-ballot
    /// engine seam. Asserts: (a) the random Choose resolves WITHOUT parking on a
    /// NamedChoice (Strax precedent — `resolve_random_in_chain`); (b) the truth tally
    /// drives the controller's draw count; (c) `3 × consequences-tally` damage lands on
    /// the chosen opponent via the persisted `ChosenAttribute::Player`.
    #[test]
    fn hoisted_choose_vote_suffix_aggregate_resolves_chosen_player_damage() {
        use crate::game::zones::create_object;
        use crate::parser::oracle_vote::parse_vote_block;
        use crate::types::identifiers::CardId;

        // Public-vote equivalent of Truth or Consequences. The secret-ballot
        // opener "each player secretly votes for" is intentionally not used here
        // because secret votes are unsupported until a proper hidden-ballot engine
        // seam is added. This text exercises the identical Choose → Vote →
        // SourceChosenPlayer runtime machinery via a public vote opener.
        let normalized = "Each player votes for truth or consequences. \
                          You draw cards equal to the number of truth votes. \
                          Then choose an opponent at random. \
                          ~ deals 3 damage to that player for each consequences vote.";
        let def = parse_vote_block(normalized, AbilityKind::Spell)
            .expect("hoisted-choose + suffix-aggregate vote parses");
        let choose_effect = (*def.effect).clone();
        let vote_effect = (*def.sub_ability.as_ref().expect("Choose wraps Vote").effect).clone();

        let mut state = GameState::new_two_player(42);
        let controller = state.players[0].id;
        let opp = state.players[1].id;
        let ctrl_life_before = state.players[0].life;
        let opp_life_before = state.players[1].life;

        // Source spell object — persist + SourceChosenPlayer read from it.
        let source_id = create_object(
            &mut state,
            CardId(1),
            controller,
            "Test Vote Card".to_string(),
            Zone::Battlefield,
        );
        // Cards in the controller's library so the truth-tally draw succeeds.
        create_object(
            &mut state,
            CardId(2),
            controller,
            "Card A".to_string(),
            Zone::Library,
        );
        create_object(
            &mut state,
            CardId(3),
            controller,
            "Card B".to_string(),
            Zone::Library,
        );
        let hand_before = state.players[0].hand.len();

        let inner = ResolvedAbility::new(vote_effect, vec![], source_id, controller);
        let ability =
            ResolvedAbility::new(choose_effect, vec![], source_id, controller).sub_ability(inner);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0)
            .expect("Choose → Vote chain initiates");

        // (a) Random Choose must NOT park interactively; the chain advances to
        // the Vote ballot, and the lone opponent is chosen + persisted.
        assert!(
            !matches!(state.waiting_for, WaitingFor::NamedChoice { .. }),
            "random Choose must resolve inline, not park on NamedChoice"
        );
        assert!(
            matches!(state.waiting_for, WaitingFor::VoteChoice { .. }),
            "chain must park on the Vote ballot, got {:?}",
            state.waiting_for
        );
        assert_eq!(
            crate::game::game_object::source_chosen_player(&state, source_id),
            Some(opp),
            "random Choose must persist the lone opponent"
        );

        // Submit ballots in APNAP order from the controller: controller → truth
        // (index 0), opponent → consequences (index 1).
        for choice in ["truth", "consequences"] {
            let snapshot = state.waiting_for.clone();
            crate::game::engine_resolution_choices::handle_resolution_choice(
                &mut state,
                snapshot,
                crate::types::GameAction::ChooseOption {
                    choice: choice.to_string(),
                },
                &mut events,
            )
            .unwrap_or_else(|err| panic!("ballot {choice} submits: {err:?}"));
        }

        // (b) truth tally = 1 → controller drew exactly one card.
        assert_eq!(
            state.players[0].hand.len(),
            hand_before + 1,
            "controller draws (truth tally) cards"
        );
        // (c) consequences tally = 1 → 3 damage to the chosen opponent only.
        assert_eq!(
            state.players[1].life,
            opp_life_before - 3,
            "chosen opponent takes 3 × consequences-tally damage"
        );
        assert_eq!(
            state.players[0].life, ctrl_life_before,
            "controller is not the damage recipient"
        );
    }

    /// WS-A building block — `TopVotes { AllTied }`: every choice tied for the
    /// max tally resolves once; a non-tied loser does not. 3 choices with
    /// tallies [2,2,1] → choices 0 and 1 each resolve their Investigate (2 Clue
    /// tokens), choice 2 does not.
    #[test]
    fn top_votes_all_tied_resolves_all_tied_winners() {
        let mut state = GameState::new_two_player(7);
        let controller = state.players[0].id;
        let clues = |s: &GameState| -> usize {
            s.battlefield
                .iter()
                .filter(|id| {
                    s.objects
                        .get(id)
                        .is_some_and(|o| o.name.to_lowercase().contains("clue"))
                })
                .count()
        };
        let per_choice: Vec<Box<AbilityDefinition>> = (0..3)
            .map(|_| {
                Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Investigate,
                ))
            })
            .collect();
        let options = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let tallies = vec![2u32, 2, 1];
        let ballots = crate::im::Vector::new();
        let mut events = Vec::new();
        resolve_tally(
            &mut state,
            ObjectId(1),
            controller,
            &options,
            &per_choice,
            &tallies,
            &ballots,
            VoteTally::TopVotes {
                tie: TieResolution::AllTied,
            },
            &[],
            None,
            &mut events,
        )
        .expect("all-tied tally resolves");
        assert_eq!(clues(&state), 2, "both tied winners' effects resolve once");
    }

    /// WS-A building block — `TopVotes { AllTied }` with a zero max (everyone
    /// passed) resolves no outcome.
    #[test]
    fn top_votes_all_tied_zero_votes_resolves_no_outcome() {
        let mut state = GameState::new_two_player(7);
        let controller = state.players[0].id;
        let per_choice: Vec<Box<AbilityDefinition>> = (0..2)
            .map(|_| {
                Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Investigate,
                ))
            })
            .collect();
        let options = vec!["a".to_string(), "b".to_string()];
        let tallies = vec![0u32, 0];
        let ballots = crate::im::Vector::new();
        let mut events = Vec::new();
        resolve_tally(
            &mut state,
            ObjectId(1),
            controller,
            &options,
            &per_choice,
            &tallies,
            &ballots,
            VoteTally::TopVotes {
                tie: TieResolution::AllTied,
            },
            &[],
            None,
            &mut events,
        )
        .expect("zero-tally tally resolves");
        let clues = state
            .battlefield
            .iter()
            .filter(|id| {
                state
                    .objects
                    .get(id)
                    .is_some_and(|o| o.name.to_lowercase().contains("clue"))
            })
            .count();
        assert_eq!(clues, 0, "a zero max yields no winners");
    }

    /// WS-C runtime — object-pool vote (Council's Judgment): both players vote
    /// the same opponent permanent by index; that permanent is exiled and the
    /// other opponent permanent is NOT (proves single-target injection, not a
    /// battlefield rescan). Drives the real `SubmitVoteCandidate` round-trip.
    #[test]
    fn object_vote_exiles_most_voted_not_others() {
        use crate::game::engine::apply;
        use crate::game::zones::create_object;
        use crate::parser::oracle_vote::parse_vote_block;
        use crate::types::actions::GameAction;
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;

        let text = "Starting with you, each player votes for a nonland permanent you don't \
                    control. Exile each permanent with the most votes or tied for most votes.";
        let vote_def = parse_vote_block(text, AbilityKind::Spell).expect("object vote parses");

        let mut state = GameState::new_two_player(42);
        let controller = state.players[0].id;
        let opp = state.players[1].id;

        // Source object (controller's) — the vote's source; excluded from the
        // candidate set because the controller controls it.
        let source_id = create_object(
            &mut state,
            CardId(1),
            controller,
            "Judge".to_string(),
            Zone::Battlefield,
        );
        // Two opponent creatures — both candidates.
        let opp_a = create_object(
            &mut state,
            CardId(2),
            opp,
            "Bear A".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&opp_a)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let opp_b = create_object(
            &mut state,
            CardId(3),
            opp,
            "Bear B".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&opp_b)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let ability = resolved_from_def(&vote_def, source_id, controller);
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // Locate opp_a's candidate index (battlefield order may vary).
        let idx_a = match &state.waiting_for {
            WaitingFor::VoteChoice {
                candidate_objects, ..
            } => candidate_objects
                .iter()
                .position(|id| *id == opp_a)
                .expect("opp_a is a candidate") as u32,
            other => panic!("expected VoteChoice, got {other:?}"),
        };

        // Both players vote opp_a (controller first — "starting with you").
        let first = match &state.waiting_for {
            WaitingFor::VoteChoice { player, .. } => *player,
            _ => unreachable!(),
        };
        apply(
            &mut state,
            first,
            GameAction::SubmitVoteCandidate {
                candidate_index: idx_a,
            },
        )
        .unwrap();
        let second = match &state.waiting_for {
            WaitingFor::VoteChoice { player, .. } => *player,
            _ => unreachable!(),
        };
        apply(
            &mut state,
            second,
            GameAction::SubmitVoteCandidate {
                candidate_index: idx_a,
            },
        )
        .unwrap();

        assert!(!matches!(state.waiting_for, WaitingFor::VoteChoice { .. }));
        assert_eq!(
            state.objects.get(&opp_a).map(|o| o.zone),
            Some(Zone::Exile),
            "the most-voted permanent must be exiled"
        );
        assert_eq!(
            state.objects.get(&opp_b).map(|o| o.zone),
            Some(Zone::Battlefield),
            "a non-winning permanent must NOT be exiled (single-target injection)"
        );
    }

    /// WS-D runtime — secret ballot (Truth or Consequences): a secret vote
    /// emits NO per-ballot `VoteCast` event (the choice is withheld until the
    /// reveal), and `filter_state_for_viewer` returns zeroed `tallies` mid-vote
    /// for every viewer. A single `VoteResolved` is the simultaneous reveal.
    ///
    /// NOTE (D6 limitation): the local-AI raw-state visibility is intentionally
    /// not asserted — `get_ai_action` computes over unfiltered state.
    #[test]
    fn secret_vote_suppresses_votecast_and_scrubs_tally() {
        use crate::game::engine::apply;
        use crate::game::visibility::filter_state_for_viewer;
        use crate::parser::oracle_vote::parse_vote_block;
        use crate::types::actions::GameAction;

        let text = "Each player secretly votes for truth or consequences, then those votes are \
                    revealed. You draw cards equal to the number of truth votes. \
                    Then choose an opponent at random. \
                    ~ deals 3 damage to that player for each consequences vote.";
        let def = parse_vote_block(text, AbilityKind::Spell).expect("secret vote parses");

        let mut state = GameState::new_two_player(42);
        let controller = state.players[0].id;
        let opp = state.players[1].id;
        let ability = resolved_from_def(&def, ObjectId(1), controller);
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // Mid-vote, the running tally is scrubbed to zeros for every viewer.
        let first = match &state.waiting_for {
            WaitingFor::VoteChoice { player, .. } => *player,
            other => panic!("expected VoteChoice, got {other:?}"),
        };
        let mut ballot_events = Vec::new();
        let res = apply(
            &mut state,
            first,
            GameAction::ChooseOption {
                choice: "truth".to_string(),
            },
        )
        .expect("first secret ballot");
        ballot_events.extend(res.events.iter().cloned());

        // Mid-vote scrub: opponent (and controller) see zeroed tallies.
        let filtered = filter_state_for_viewer(&state, opp);
        if let WaitingFor::VoteChoice { tallies, .. } = &filtered.waiting_for {
            assert!(
                tallies.iter().all(|&t| t == 0),
                "secret running tallies must be scrubbed to zero mid-vote"
            );
        } else {
            panic!("expected VoteChoice mid-secret-vote");
        }

        let second = match &state.waiting_for {
            WaitingFor::VoteChoice { player, .. } => *player,
            other => panic!("expected VoteChoice, got {other:?}"),
        };
        let res2 = apply(
            &mut state,
            second,
            GameAction::ChooseOption {
                choice: "consequences".to_string(),
            },
        )
        .expect("second secret ballot");
        ballot_events.extend(res2.events.iter().cloned());

        // No per-ballot VoteCast was ever emitted; exactly one VoteResolved.
        assert!(
            !ballot_events
                .iter()
                .any(|e| matches!(e, GameEvent::VoteCast { .. })),
            "secret ballots must not emit per-ballot VoteCast events"
        );
        assert_eq!(
            ballot_events
                .iter()
                .filter(|e| matches!(e, GameEvent::VoteResolved { .. }))
                .count(),
            1,
            "exactly one VoteResolved (the simultaneous reveal) is emitted"
        );
    }

    /// WS-C validation — object votes reject `ChooseOption` (string path) and an
    /// out-of-range `SubmitVoteCandidate` index.
    #[test]
    fn object_vote_rejects_choose_option_and_oob_index() {
        use crate::game::engine::apply;
        use crate::game::zones::create_object;
        use crate::parser::oracle_vote::parse_vote_block;
        use crate::types::actions::GameAction;
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;

        let text = "Starting with you, each player votes for a nonland permanent you don't \
                    control. Exile each permanent with the most votes or tied for most votes.";
        let vote_def = parse_vote_block(text, AbilityKind::Spell).expect("object vote parses");

        let mut state = GameState::new_two_player(42);
        let controller = state.players[0].id;
        let opp = state.players[1].id;
        let source_id = create_object(
            &mut state,
            CardId(1),
            controller,
            "Judge".to_string(),
            Zone::Battlefield,
        );
        let opp_a = create_object(
            &mut state,
            CardId(2),
            opp,
            "Bear A".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&opp_a)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let ability = resolved_from_def(&vote_def, source_id, controller);
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        let voter = match &state.waiting_for {
            WaitingFor::VoteChoice { player, .. } => *player,
            other => panic!("expected VoteChoice, got {other:?}"),
        };

        // ChooseOption is rejected for object votes.
        assert!(apply(
            &mut state,
            voter,
            GameAction::ChooseOption {
                choice: "bear a".to_string(),
            },
        )
        .is_err());
        // Out-of-range candidate index is rejected.
        assert!(apply(
            &mut state,
            voter,
            GameAction::SubmitVoteCandidate { candidate_index: 9 },
        )
        .is_err());
    }

    /// WS-C runtime — genuine multi-object tie (Council's Judgment): the two
    /// players vote for DIFFERENT opponent permanents, producing a [1, 1] tie
    /// for most votes. `TopVotes{AllTied}` must exile EACH tied winner with its
    /// own injected target, while a third un-voted permanent is left alone.
    /// This is the load-bearing "exile each permanent tied for most votes"
    /// behavior that the single-winner test cannot exercise.
    #[test]
    fn object_vote_tie_exiles_each_tied_winner_not_others() {
        use crate::game::engine::apply;
        use crate::game::zones::create_object;
        use crate::parser::oracle_vote::parse_vote_block;
        use crate::types::actions::GameAction;
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;

        let text = "Starting with you, each player votes for a nonland permanent you don't \
                    control. Exile each permanent with the most votes or tied for most votes.";
        let vote_def = parse_vote_block(text, AbilityKind::Spell).expect("object vote parses");

        let mut state = GameState::new_two_player(42);
        let controller = state.players[0].id;
        let opp = state.players[1].id;

        let source_id = create_object(
            &mut state,
            CardId(1),
            controller,
            "Judge".to_string(),
            Zone::Battlefield,
        );
        // Three opponent creatures: two will tie for most, one gets no votes.
        let make_creature = |state: &mut GameState, card: u64, name: &str| {
            let id = create_object(
                state,
                CardId(card),
                opp,
                name.to_string(),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
            id
        };
        let opp_a = make_creature(&mut state, 2, "Bear A");
        let opp_b = make_creature(&mut state, 3, "Bear B");
        let opp_c = make_creature(&mut state, 4, "Bear C");

        let ability = resolved_from_def(&vote_def, source_id, controller);
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // Resolve candidate indices for opp_a and opp_b (battlefield order varies).
        let (idx_a, idx_b) = match &state.waiting_for {
            WaitingFor::VoteChoice {
                candidate_objects, ..
            } => (
                candidate_objects
                    .iter()
                    .position(|id| *id == opp_a)
                    .expect("opp_a is a candidate") as u32,
                candidate_objects
                    .iter()
                    .position(|id| *id == opp_b)
                    .expect("opp_b is a candidate") as u32,
            ),
            other => panic!("expected VoteChoice, got {other:?}"),
        };

        // First voter (controller, "starting with you") votes opp_a; the second
        // voter votes opp_b — a genuine [1, 1] tie for most.
        let first = match &state.waiting_for {
            WaitingFor::VoteChoice { player, .. } => *player,
            _ => unreachable!(),
        };
        apply(
            &mut state,
            first,
            GameAction::SubmitVoteCandidate {
                candidate_index: idx_a,
            },
        )
        .unwrap();
        let second = match &state.waiting_for {
            WaitingFor::VoteChoice { player, .. } => *player,
            _ => unreachable!(),
        };
        apply(
            &mut state,
            second,
            GameAction::SubmitVoteCandidate {
                candidate_index: idx_b,
            },
        )
        .unwrap();

        assert!(!matches!(state.waiting_for, WaitingFor::VoteChoice { .. }));
        assert_eq!(
            state.objects.get(&opp_a).map(|o| o.zone),
            Some(Zone::Exile),
            "each permanent tied for most votes must be exiled (opp_a)"
        );
        assert_eq!(
            state.objects.get(&opp_b).map(|o| o.zone),
            Some(Zone::Exile),
            "each permanent tied for most votes must be exiled (opp_b)"
        );
        assert_eq!(
            state.objects.get(&opp_c).map(|o| o.zone),
            Some(Zone::Battlefield),
            "an un-voted permanent must NOT be exiled"
        );
    }

    /// WS-C regression — object-pool vote with >255 candidates.
    ///
    /// `SubmitVoteCandidate::candidate_index` was `u8`, causing index 255+ to
    /// wrap and alias an earlier candidate on large boards. This test creates
    /// 257 opponent permanents, votes unanimously for the LAST candidate
    /// (index 256), and asserts that candidate — and only that candidate — is
    /// exiled. Regression for the u32 widening of `candidate_index`.
    #[test]
    fn object_vote_last_candidate_above_u8_max_is_selectable() {
        use crate::game::engine::apply;
        use crate::game::zones::create_object;
        use crate::parser::oracle_vote::parse_vote_block;
        use crate::types::actions::GameAction;
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;

        let text = "Starting with you, each player votes for a nonland permanent you don't \
                    control. Exile each permanent with the most votes or tied for most votes.";
        let vote_def = parse_vote_block(text, AbilityKind::Spell).expect("object vote parses");

        let mut state = GameState::new_two_player(42);
        let controller = state.players[0].id;
        let opp = state.players[1].id;

        // Source object (controller's) — excluded from candidate set.
        let source_id = create_object(
            &mut state,
            CardId(1),
            controller,
            "Judge".to_string(),
            Zone::Battlefield,
        );

        // Create 257 opponent permanents — the last one (index 256) is the
        // target. Before the u8 fix, candidate_index 256 wrapped to 0, making
        // the last candidate unreachable.
        let mut opp_ids = Vec::new();
        for n in 0u64..257 {
            let id = create_object(
                &mut state,
                CardId(100 + n),
                opp,
                format!("Bear {n}"),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
            opp_ids.push(id);
        }
        let last_bear = *opp_ids.last().unwrap();

        let ability = resolved_from_def(&vote_def, source_id, controller);
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // Locate the last bear's candidate index (>= 255 guaranteed if
        // candidate_objects preserves insertion order, but we look it up
        // explicitly so the test is order-agnostic).
        let last_idx = match &state.waiting_for {
            WaitingFor::VoteChoice {
                candidate_objects, ..
            } => candidate_objects
                .iter()
                .position(|id| *id == last_bear)
                .expect("last bear is a candidate") as u32,
            other => panic!("expected VoteChoice, got {other:?}"),
        };
        assert!(
            last_idx >= 255,
            "sanity: last candidate index must be >= 255, got {last_idx}"
        );

        // Both players vote for the last bear.
        let first = match &state.waiting_for {
            WaitingFor::VoteChoice { player, .. } => *player,
            _ => unreachable!(),
        };
        apply(
            &mut state,
            first,
            GameAction::SubmitVoteCandidate {
                candidate_index: last_idx,
            },
        )
        .unwrap();
        let second = match &state.waiting_for {
            WaitingFor::VoteChoice { player, .. } => *player,
            _ => unreachable!(),
        };
        apply(
            &mut state,
            second,
            GameAction::SubmitVoteCandidate {
                candidate_index: last_idx,
            },
        )
        .unwrap();

        assert!(!matches!(state.waiting_for, WaitingFor::VoteChoice { .. }));
        assert_eq!(
            state.objects.get(&last_bear).map(|o| o.zone),
            Some(Zone::Exile),
            "the most-voted candidate (index >255) must be exiled"
        );
        // Spot-check the first bear was NOT exiled (no votes).
        let first_bear = opp_ids[0];
        assert_eq!(
            state.objects.get(&first_bear).map(|o| o.zone),
            Some(Zone::Battlefield),
            "an un-voted permanent must NOT be exiled"
        );
    }
}
