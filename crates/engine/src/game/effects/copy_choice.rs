//! CR 707.10c + CR 722.3c + CR 702.192a: the copy target walk
//! (`WaitingFor::CopyRetarget`).
//!
//! One walk shape serves two operations, selected by `CopyChoiceMode`:
//!
//! - **Retarget** (CR 707.10c): a copy that holds targets; its controller may
//!   choose new ones. Positions are the copy's chain-addressed declared targets
//!   (`chain_retarget_slots`). Each pick is `None` (keep) or `Some(t)` (choose),
//!   and the offered picks at every step are exactly those with a legal
//!   completion under the reducer's own validator
//!   (`retarget_completion::RetargetSearch`). Finalization submits the picks to
//!   `engine::validate_retarget_submission`.
//! - **Announce** (CR 722.3c / CR 702.192a): a freshly cast copy announces its
//!   targets (CR 601.2c). Nothing can be kept; the offered targets at every step
//!   come from the production casting walk
//!   (`build_target_selection_progress_for_ability`), and finalization is the
//!   production announcement assignment (`assign_selected_slots_in_chain`,
//!   which captures the announcement pins).
//!
//! The decided prefix is `picks`; `current_slot == picks.len()`. Permissions
//! (`CopyTargetSlot::can_keep`, `can_keep_rest`) and the current slot's
//! `legal_alternatives` are engine-derived at every open, advance and restore.

use crate::game::ability_utils::{
    assign_selected_slots_in_chain, build_target_selection_progress_for_ability,
    build_target_slots, choose_target_for_ability, TargetSelectionAdvance,
};
use crate::game::casting_costs::{
    assign_next_announcing_opponent, next_announcing_opponent_choice,
};
use crate::game::engine::EngineError;
use crate::game::players::{choosable_opponents, is_alive};
use crate::game::retarget_completion::{RetargetPick, RetargetSearch};
use crate::types::ability::{EffectKind, ResolvedAbility, TargetFilter};
use crate::types::game_state::{
    AnnouncerElection, CopyChoiceMode, CopyTargetSlot, GameState, PersistedRestoreError,
    TargetSelectionProgress, TargetSelectionSlot, WaitingFor,
};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;

/// The fixed fields of one walk, carried from prompt to prompt.
#[derive(Debug, Clone)]
pub(crate) struct CopyWalk {
    pub(crate) player: PlayerId,
    pub(crate) copy_id: ObjectId,
    pub(crate) effect_kind: EffectKind,
    pub(crate) effect_source_id: Option<ObjectId>,
    pub(crate) paradigm_remaining_offers: Option<Vec<ObjectId>>,
    pub(crate) mode: CopyChoiceMode,
}

/// What a walk step produced.
#[derive(Debug, Clone)]
pub(crate) enum CopyWalkStep {
    /// The walk continues at this prompt.
    Prompt(Box<WaitingFor>),
    /// Every position is decided; finalize with these picks.
    Complete(Vec<RetargetPick>),
}

fn copy_stack_index(state: &GameState, copy_id: ObjectId) -> Option<usize> {
    state.stack.iter().position(|entry| entry.id == copy_id)
}

/// The copy's spell ability. `Err` when the copy is no longer on the stack;
/// `Ok(None)` for a copy whose spell has no ability (a vanilla permanent spell,
/// CR 707.12): it announces nothing, a successful no-walk case.
fn copy_ability(
    state: &GameState,
    copy_id: ObjectId,
) -> Result<Option<&ResolvedAbility>, EngineError> {
    state
        .stack
        .iter()
        .find(|entry| entry.id == copy_id)
        .map(|entry| entry.ability())
        .ok_or_else(|| EngineError::InvalidAction("Copy is no longer on the stack".to_string()))
}

/// CR 601.2c: the production casting walk's view of a copy's announcement —
/// its slots and the progress replayed from the decided prefix (which also
/// auto-advances optional slots with no legal target, and announced binders).
struct Announcement<'s> {
    ability: &'s ResolvedAbility,
    slots: Vec<TargetSelectionSlot>,
    progress: TargetSelectionProgress,
}

/// `Ok(None)` when the copy announces nothing (no ability, or no slot).
fn announcement(
    state: &GameState,
    copy_id: ObjectId,
    picks: Vec<RetargetPick>,
) -> Result<Option<Announcement<'_>>, EngineError> {
    let Some(ability) = copy_ability(state, copy_id)? else {
        return Ok(None);
    };
    let slots = build_target_slots(state, ability)?;
    if slots.is_empty() {
        return Ok(None);
    }
    let progress = build_target_selection_progress_for_ability(
        state,
        ability,
        &slots,
        &ability.target_constraints,
        picks.len(),
        picks,
    )?;
    Ok(Some(Announcement {
        ability,
        slots,
        progress,
    }))
}

/// CR 707.10c: the retarget search over the copy's addressed positions.
pub(crate) fn copy_retarget_search(
    state: &GameState,
    copy_id: ObjectId,
) -> Option<RetargetSearch<'_>> {
    RetargetSearch::for_stack_entry(state, copy_stack_index(state, copy_id)?)
}

/// Build the prompt (or completion) for `walk` with decided prefix `picks`.
/// `Ok(None)` for a Retarget walk on a copy with no addressed position.
pub(crate) fn walk_step(
    state: &GameState,
    walk: &CopyWalk,
    picks: Vec<RetargetPick>,
) -> Result<Option<CopyWalkStep>, EngineError> {
    match walk.mode {
        CopyChoiceMode::Retarget => {
            let Some(search) = copy_retarget_search(state, walk.copy_id) else {
                return Ok(None);
            };
            if picks.len() >= search.len() {
                return Ok(Some(CopyWalkStep::Complete(picks)));
            }
            let offered = search.offered(&picks);
            let position = picks.len();
            let target_slots = (0..search.len())
                .map(|i| CopyTargetSlot {
                    current: Some(search.current_targets()[i].clone()),
                    legal_alternatives: if i == position {
                        offered.alternatives.clone()
                    } else {
                        search.slot_pools()[i].clone()
                    },
                    address: Some(search.slots()[i].clone()),
                    can_keep: i == position && offered.can_keep,
                    can_decline: false,
                })
                .collect();
            let can_keep_rest = search.can_keep_rest(&picks);
            Ok(Some(CopyWalkStep::Prompt(Box::new(
                WaitingFor::CopyRetarget {
                    player: walk.player,
                    controller: None,
                    copy_id: walk.copy_id,
                    target_slots,
                    effect_kind: walk.effect_kind,
                    effect_source_id: walk.effect_source_id,
                    current_slot: position,
                    paradigm_remaining_offers: walk.paradigm_remaining_offers.clone(),
                    mode: Some(CopyChoiceMode::Retarget),
                    picks: Some(picks),
                    can_keep_rest,
                    announcer_election: None,
                },
            ))))
        }
        CopyChoiceMode::Announce => {
            if let Some(election) = announcer_election(state, walk)? {
                // An announcer is elected before any target is announced. The
                // one later election is CR 800.4g's replacement for an
                // announcer who left the game
                // (`reconcile_copy_announcement_after_departure`), which keeps
                // the decided prefix.
                // CR 601.2c + CR 707.12: the election is published only for a
                // feasible announcement; a copy with no legal announcement
                // fails here, before any prompt (restore: fail closed).
                announcement(state, walk.copy_id, picks.clone())?;
                return Ok(Some(CopyWalkStep::Prompt(Box::new(
                    WaitingFor::CopyRetarget {
                        player: walk.player,
                        controller: None,
                        copy_id: walk.copy_id,
                        target_slots: Vec::new(),
                        effect_kind: walk.effect_kind,
                        effect_source_id: walk.effect_source_id,
                        current_slot: picks.len(),
                        paradigm_remaining_offers: walk.paradigm_remaining_offers.clone(),
                        mode: Some(CopyChoiceMode::Announce),
                        picks: Some(picks),
                        can_keep_rest: false,
                        announcer_election: Some(election),
                    },
                ))));
            }
            let Some(Announcement {
                ability,
                slots,
                progress,
            }) = announcement(state, walk.copy_id, picks.clone())?
            else {
                return Ok(Some(CopyWalkStep::Complete(picks)));
            };
            if progress.current_slot >= slots.len() {
                return Ok(Some(CopyWalkStep::Complete(progress.selected_slots)));
            }
            let position = progress.current_slot;
            // CR 115.6 + CR 601.2c: declining an optional slot is answerable
            // exactly when the casting walk's own advance accepts it.
            let can_decline = slots[position].optional
                && choose_target_for_ability(
                    state,
                    ability,
                    &slots,
                    &ability.target_constraints,
                    &progress,
                    None,
                )
                .is_ok();
            // CR 601.2c + CR 115.1: a slot "of an opponent's choice" is
            // announced by its chooser; CR 112.2: the copy stays its
            // controller's.
            let chooser = slots[position].chooser.unwrap_or(walk.player);
            let target_slots = slots
                .iter()
                .enumerate()
                .map(|(i, slot)| CopyTargetSlot {
                    current: progress.selected_slots.get(i).cloned().flatten(),
                    legal_alternatives: if i == position {
                        progress.current_legal_targets.clone()
                    } else {
                        slot.legal_targets.clone()
                    },
                    address: None,
                    can_keep: false,
                    can_decline: i == position && can_decline,
                })
                .collect();
            Ok(Some(CopyWalkStep::Prompt(Box::new(
                WaitingFor::CopyRetarget {
                    player: chooser,
                    controller: (chooser != walk.player).then_some(walk.player),
                    copy_id: walk.copy_id,
                    target_slots,
                    effect_kind: walk.effect_kind,
                    effect_source_id: walk.effect_source_id,
                    current_slot: position,
                    paradigm_remaining_offers: walk.paradigm_remaining_offers.clone(),
                    mode: Some(CopyChoiceMode::Announce),
                    picks: Some(progress.selected_slots),
                    can_keep_rest: false,
                    announcer_election: None,
                },
            ))))
        }
    }
}

/// CR 601.2c + CR 115.1 (CR 707.12): the copy announcement's next
/// announcing-opponent election, through the casting authority
/// (`next_announcing_opponent_choice`, `choosable_opponents`): before any
/// target is announced, while an "of an opponent's choice" group has no
/// announcer and the controller has two or more choosable opponents. With one
/// opponent there is no decision.
fn announcer_election(
    state: &GameState,
    walk: &CopyWalk,
) -> Result<Option<AnnouncerElection>, EngineError> {
    let Some(ability) = copy_ability(state, walk.copy_id)? else {
        return Ok(None);
    };
    let Some(choice) = next_announcing_opponent_choice(ability) else {
        return Ok(None);
    };
    let candidates = crate::game::players::choosable_opponents(state, walk.player);
    if candidates.len() < 2 {
        return Ok(None);
    }
    Ok(Some(AnnouncerElection {
        candidates,
        choice_index: choice.index,
        choice_count: choice.count,
        target_type: choice.target_type,
    }))
}

/// CR 601.2c + CR 115.1: answer the copy announcement's announcing-opponent
/// election: record `opponent` on the first unassigned opponent-choice group
/// of the copy on the stack (`assign_next_announcing_opponent`, the casting
/// authority). The walk then continues from its decided prefix (empty, unless
/// this replaces an announcer who left the game).
pub(crate) fn elect_announcing_opponent(
    state: &mut GameState,
    walk: &CopyWalk,
    opponent: PlayerId,
) -> Result<(), EngineError> {
    let election = announcer_election(state, walk)?.ok_or_else(|| {
        EngineError::InvalidAction(
            "No opponent-choice effect is awaiting an announcing opponent".to_string(),
        )
    })?;
    if !election.candidates.contains(&opponent) {
        return Err(EngineError::InvalidAction(format!(
            "Player {opponent:?} is not an eligible announcing opponent"
        )));
    }
    let ability = state
        .stack
        .iter_mut()
        .find(|entry| entry.id == walk.copy_id)
        .and_then(|entry| entry.ability_mut())
        .ok_or_else(|| EngineError::InvalidAction("Copy is no longer on the stack".to_string()))?;
    if !assign_next_announcing_opponent(ability, opponent) {
        return Err(EngineError::InvalidAction(
            "No opponent-choice effect is awaiting an announcing opponent".to_string(),
        ));
    }
    Ok(())
}

/// The `depth`-th link of `ability`'s sub-ability chain.
fn chain_link_mut(ability: &mut ResolvedAbility, depth: usize) -> Option<&mut ResolvedAbility> {
    let mut node = Some(ability);
    for _ in 0..depth {
        node = node?.sub_ability.as_deref_mut();
    }
    node
}

/// CR 707.10c / CR 601.2c: the longest prefix of `picks` the walk still
/// accepts on the current board. Each pick is replayed through the walk's own
/// gate and advance in published form (the casting walk's auto-advances
/// included); the replay stops at the first pick the board refuses, so the
/// walk re-asks from there (an unfinished choice stays its chooser's). Shared
/// by restore and the departure reconciliation.
fn replay_decided_picks(
    state: &GameState,
    walk: &CopyWalk,
    picks: &[RetargetPick],
) -> Vec<RetargetPick> {
    let mut accepted: Vec<RetargetPick> = Vec::with_capacity(picks.len());
    loop {
        match published_picks(state, walk, accepted.clone()) {
            Ok(published) if picks.starts_with(&published) => accepted = published,
            _ => break,
        }
        let Some(pick) = picks.get(accepted.len()) else {
            break;
        };
        match advance_walk(state, walk, &accepted, pick) {
            Ok(next) if next.len() > accepted.len() && picks.starts_with(&next) => {
                accepted = next;
            }
            _ => break,
        }
    }
    accepted
}

/// CR 800.4a + CR 800.4g + CR 601.2c + CR 115.1: reconcile a copy
/// announcement with the players still in the game. `None` when the prompt
/// is not a copy announcement or the copy's controller has left; otherwise
/// the walk and the decided prefix to re-derive the prompt from:
/// - CR 800.4a: the departed player's objects left the game, so the decided
///   prefix is replayed on the current board (`replay_decided_picks`) and is
///   re-asked from the first pick it refuses.
/// - CR 800.4g: each "of an opponent's choice" group that still owns an
///   undecided slot and has no announcer in the game (its announcer left, or a
///   replacement election is still open) goes to the only choosable opponent
///   when one remains; with two or more its announcer is cleared, and the
///   re-derived walk asks the copy's controller through the
///   announcing-opponent election, rebuilt against the remaining opponents.
///   A group whose slots are all decided is not re-asked.
pub(crate) fn reconcile_copy_announcement_after_departure(
    state: &mut GameState,
) -> Result<Option<(CopyWalk, Vec<RetargetPick>)>, EngineError> {
    let Some((walk, picks)) = walk_of(&state.waiting_for) else {
        return Ok(None);
    };
    if walk.mode != CopyChoiceMode::Announce || !is_alive(state, walk.player) {
        return Ok(None);
    }
    let Some(ability) = copy_ability(state, walk.copy_id)? else {
        return Ok(None);
    };
    let picks = replay_decided_picks(state, &walk, &picks);
    let baseline = build_target_slots(state, ability)?;
    // A group's slots are the ones whose announcer changes when only that
    // group's announcer is set to the controller (the CR 601.2c default).
    let mut unanswered = Vec::new();
    let mut node = Some(ability);
    let mut depth = 0;
    while let Some(link) = node {
        let needs_announcer = matches!(link.target_chooser, Some(TargetFilter::Opponent))
            && link
                .context
                .announcing_opponent
                .is_none_or(|announcer| !is_alive(state, announcer));
        if needs_announcer {
            let mut probe = ability.clone();
            let controller = probe.controller;
            if let Some(probe_link) = chain_link_mut(&mut probe, depth) {
                probe_link.context.announcing_opponent = Some(controller);
            }
            let probed = build_target_slots(state, &probe)?;
            let owns_undecided = probed.len() != baseline.len()
                || (picks.len()..baseline.len())
                    .any(|slot| probed[slot].chooser != baseline[slot].chooser);
            if owns_undecided {
                unanswered.push(depth);
            }
        }
        node = link.sub_ability.as_deref();
        depth += 1;
    }
    let replacement = match choosable_opponents(state, walk.player).as_slice() {
        [only] => Some(*only),
        _ => None,
    };
    let ability = state
        .stack
        .iter_mut()
        .find(|entry| entry.id == walk.copy_id)
        .and_then(|entry| entry.ability_mut())
        .ok_or_else(|| EngineError::InvalidAction("Copy is no longer on the stack".to_string()))?;
    for depth in unanswered {
        if let Some(link) = chain_link_mut(ability, depth) {
            link.context.announcing_opponent = replacement;
        }
    }
    Ok(Some((walk, picks)))
}

/// CR 707.10c / CR 601.2c: THE gate and advance for `ChooseTarget` — the
/// walk's decided picks after answering `pick` at the current position, or
/// `Err` when `pick` is not an answerable choice there. Retarget asks the
/// completion search (a keep only where keeping completes, a choice only where
/// it has a legal completion). Announce is the production casting advance
/// (`choose_target_for_ability`): a legal target, or a decline of an optional
/// slot, which also declines the rest of an "up to N" instance (CR 115.6).
pub(crate) fn advance_walk(
    state: &GameState,
    walk: &CopyWalk,
    picks: &[RetargetPick],
    pick: &RetargetPick,
) -> Result<Vec<RetargetPick>, EngineError> {
    match walk.mode {
        CopyChoiceMode::Retarget => {
            let admitted = copy_retarget_search(state, walk.copy_id)
                .is_some_and(|search| search.admits(picks, pick));
            if !admitted {
                return Err(EngineError::InvalidAction(format!(
                    "{pick:?} is not an answerable pick for copy slot {}",
                    picks.len()
                )));
            }
            let mut next = picks.to_vec();
            next.push(pick.clone());
            Ok(next)
        }
        CopyChoiceMode::Announce => {
            let Some(Announcement {
                ability,
                slots,
                progress,
            }) = announcement(state, walk.copy_id, picks.to_vec())?
            else {
                return Err(EngineError::InvalidAction(
                    "Copy announces no targets".to_string(),
                ));
            };
            if progress.current_slot >= slots.len() {
                return Err(EngineError::InvalidAction(
                    "Copy announcement has no undecided slot".to_string(),
                ));
            }
            match choose_target_for_ability(
                state,
                ability,
                &slots,
                &ability.target_constraints,
                &progress,
                pick.clone(),
            )? {
                TargetSelectionAdvance::InProgress(next) => Ok(next.selected_slots),
                TargetSelectionAdvance::Complete(selected) => Ok(selected),
            }
        }
    }
}

/// The walk's decided picks as the walk itself would publish them: the
/// casting walk auto-advances announcement slots it decides alone; a retarget
/// prefix is already in published form.
fn published_picks(
    state: &GameState,
    walk: &CopyWalk,
    picks: Vec<RetargetPick>,
) -> Result<Vec<RetargetPick>, EngineError> {
    match walk.mode {
        CopyChoiceMode::Retarget => Ok(picks),
        CopyChoiceMode::Announce => Ok(announcement(state, walk.copy_id, picks.clone())?
            .map_or(picks, |announcement| announcement.progress.selected_slots)),
    }
}

/// CR 707.10c: whether keeping every remaining position completes — the
/// reducer gate for `KeepAllCopyTargets`. Never for an announcement.
pub(crate) fn keep_rest_is_admissible(
    state: &GameState,
    walk: &CopyWalk,
    picks: &[RetargetPick],
) -> bool {
    walk.mode == CopyChoiceMode::Retarget
        && copy_retarget_search(state, walk.copy_id)
            .is_some_and(|search| search.can_keep_rest(picks))
}

/// The copy's ability as it stands after the walk's final picks, without
/// writing: the retarget validator's result (Retarget), or the production
/// announcement assignment with pin capture (Announce). `Ok(None)` for a copy
/// whose spell has no ability: there is nothing to write (CR 707.12).
pub(crate) fn finalized_copy_ability(
    state: &GameState,
    walk: &CopyWalk,
    mut picks: Vec<RetargetPick>,
) -> Result<Option<ResolvedAbility>, EngineError> {
    match walk.mode {
        CopyChoiceMode::Retarget => {
            let search = copy_retarget_search(state, walk.copy_id).ok_or_else(|| {
                EngineError::InvalidAction("Copy has no retargetable position".to_string())
            })?;
            picks.resize(search.len(), None);
            search.validate(&picks).map(Some)
        }
        CopyChoiceMode::Announce => {
            let Some(ability) = copy_ability(state, walk.copy_id)? else {
                return Ok(None);
            };
            let mut ability = ability.clone();
            assign_selected_slots_in_chain(state, &mut ability, &picks)?;
            Ok(Some(ability))
        }
    }
}

/// A pre-mode (legacy) `CopyRetarget` save: infer the walk mode from the
/// slot shape and rebuild the decided prefix. A fully filled walk is a
/// Retarget (a copy holds every target); a filled prefix with an unchosen
/// suffix is an Announce (a fresh copy announcing). A decided Retarget
/// position is rebuilt as an election of its recorded target (`Some`); the
/// shared changed verdict reads a same-live election as unchanged. An explicit
/// `mode` is never overwritten. Any other shape is structurally malformed.
pub(crate) fn legacy_walk_shape(
    target_slots: &[CopyTargetSlot],
    current_slot: usize,
    mode: Option<CopyChoiceMode>,
) -> Result<(CopyChoiceMode, Vec<RetargetPick>), String> {
    if current_slot > target_slots.len() {
        return Err(format!(
            "copy target walk cursor {current_slot} exceeds its {} slots",
            target_slots.len()
        ));
    }
    let prefix_filled = target_slots[..current_slot]
        .iter()
        .all(|slot| slot.current.is_some());
    let suffix_unchosen = target_slots[current_slot..]
        .iter()
        .all(|slot| slot.current.is_none());
    let all_filled = target_slots.iter().all(|slot| slot.current.is_some());
    let inferred = if all_filled {
        CopyChoiceMode::Retarget
    } else if prefix_filled && suffix_unchosen {
        CopyChoiceMode::Announce
    } else {
        return Err("copy target walk has an inconsistent slot shape".to_string());
    };
    let mode = mode.unwrap_or(inferred);
    if !prefix_filled {
        return Err("copy target walk has an unchosen decided slot".to_string());
    }
    let picks = target_slots[..current_slot]
        .iter()
        .map(|slot| slot.current.clone())
        .collect();
    Ok((mode, picks))
}

/// The walk fields of a `CopyRetarget` prompt, with its mode and picks. A
/// legacy prompt without them is read through `legacy_walk_shape`. `None` for
/// any other prompt or a malformed legacy shape.
pub(crate) fn walk_of(waiting_for: &WaitingFor) -> Option<(CopyWalk, Vec<RetargetPick>)> {
    let WaitingFor::CopyRetarget {
        player,
        controller,
        copy_id,
        target_slots,
        effect_kind,
        effect_source_id,
        current_slot,
        paradigm_remaining_offers,
        mode,
        picks,
        ..
    } = waiting_for
    else {
        return None;
    };
    let (mode, picks) = match (mode, picks) {
        (Some(mode), Some(picks)) => (*mode, picks.clone()),
        _ => legacy_walk_shape(target_slots, *current_slot, *mode).ok()?,
    };
    Some((
        CopyWalk {
            player: controller.unwrap_or(*player),
            copy_id: *copy_id,
            effect_kind: *effect_kind,
            effect_source_id: *effect_source_id,
            paradigm_remaining_offers: paradigm_remaining_offers.clone(),
            mode,
        },
        picks,
    ))
}

/// CR 707.10c: open the "may choose new targets" walk for a copy already on
/// the stack. Returns whether a prompt was armed: `false` when the copy has no
/// addressed position (B5: the chain census, not the root's `targets`, decides).
pub(crate) fn open_copy_retarget_walk(
    state: &mut GameState,
    player: PlayerId,
    copy_id: ObjectId,
    effect_kind: EffectKind,
    effect_source_id: ObjectId,
) -> bool {
    let walk = CopyWalk {
        player,
        copy_id,
        effect_kind,
        effect_source_id: Some(effect_source_id),
        paradigm_remaining_offers: None,
        mode: CopyChoiceMode::Retarget,
    };
    // W1: the seed (keep everything) must be a confirmed validator result.
    let seed_confirmed =
        copy_retarget_search(state, copy_id).is_some_and(|search| search.confirm_seed().is_ok());
    if !seed_confirmed {
        return false;
    }
    match walk_step(state, &walk, Vec::new()) {
        Ok(Some(CopyWalkStep::Prompt(prompt))) => {
            state.waiting_for = *prompt;
            true
        }
        _ => false,
    }
}

/// CR 707.10c / CR 601.2c: rebuild a restored `CopyRetarget` walk before the
/// state is published. A pre-mode save's mode and decided prefix are inferred
/// (`legacy_walk_shape`); an explicit mode is kept. The decided picks are
/// replayed through the reducer's own gate and advance (`advance_walk`) and the walk
/// is truncated at the first pick the current board refuses, so the player is
/// re-asked from there (CR 115.7d: an unfinished choice is still the
/// player's). The prompt — the current slot's offered targets, its keep
/// permission, `can_keep_rest`, and the reset suffix — is then re-derived.
/// A fresh announcement with no legal announcement at all is not resumable:
/// restore fails closed rather than publish an unanswerable prompt.
pub(crate) fn restore_copy_target_walk(state: &mut GameState) -> Result<(), PersistedRestoreError> {
    let WaitingFor::CopyRetarget {
        player,
        controller,
        copy_id,
        target_slots,
        effect_kind,
        effect_source_id,
        current_slot,
        paradigm_remaining_offers,
        mode,
        picks,
        announcer_election: persisted_election,
        ..
    } = &state.waiting_for
    else {
        return Ok(());
    };
    let legacy_unelected_prefix = persisted_election.is_none();
    let malformed = |reason: String| PersistedRestoreError::InvalidCopyTargetWalk(reason);
    let (mode, picks) = match (mode, picks) {
        (Some(mode), Some(picks)) => (*mode, picks.clone()),
        (mode, None) => legacy_walk_shape(target_slots, *current_slot, *mode).map_err(malformed)?,
        (None, Some(_)) => {
            return Err(malformed(
                "copy target walk records picks without a mode".to_string(),
            ))
        }
    };
    if picks.len() != *current_slot {
        return Err(malformed(format!(
            "copy target walk cursor {current_slot} does not match its {} decided picks",
            picks.len()
        )));
    }
    let walk = CopyWalk {
        player: controller.unwrap_or(*player),
        copy_id: *copy_id,
        effect_kind: *effect_kind,
        effect_source_id: *effect_source_id,
        paradigm_remaining_offers: paradigm_remaining_offers.clone(),
        mode,
    };
    if copy_ability(state, walk.copy_id).is_err() {
        return Err(malformed(format!(
            "copy target walk names {:?}, which is not a copy on the stack",
            walk.copy_id
        )));
    }
    // CR 601.2c + CR 115.1: an announcement that already has targets but an
    // "of an opponent's choice" group with no elected announcer (a save from
    // before the copy walk ran the election) cannot be resumed: the
    // election would have to precede the saved picks, and it is never
    // inferred from seat order. A saved election prompt with a prefix is the
    // CR 800.4g replacement of a departed announcer, and resumes.
    if mode == CopyChoiceMode::Announce
        && !picks.is_empty()
        && legacy_unelected_prefix
        && announcer_election(state, &walk)
            .map_err(|error| malformed(format!("{error:?}")))?
            .is_some()
    {
        return Err(PersistedRestoreError::UnelectedCopyAnnouncer {
            copy_id: walk.copy_id,
        });
    }
    let accepted = replay_decided_picks(state, &walk, &picks);
    match walk_step(state, &walk, accepted.clone()) {
        Ok(Some(CopyWalkStep::Prompt(prompt))) => {
            state.waiting_for = *prompt;
            Ok(())
        }
        // CR 601.2c: no legal announcement exists for a fresh copy.
        Err(_) if mode == CopyChoiceMode::Announce && accepted.is_empty() => {
            Err(PersistedRestoreError::NonResumableCopyAnnouncement {
                copy_id: walk.copy_id,
            })
        }
        Ok(Some(CopyWalkStep::Complete(_))) => Err(malformed(
            "copy target walk has no undecided position".to_string(),
        )),
        Ok(None) => Err(malformed(
            "copy target walk's copy has no retargetable position".to_string(),
        )),
        Err(error) => Err(malformed(format!(
            "copy target walk cannot resume: {error:?}"
        ))),
    }
}
