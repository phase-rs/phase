//! Pure, explicitly observed draft time. Hosts own scheduling; this module owns
//! prompt/round scopes, deadlines and the single captured expiry transaction.
use crate::pick_pass::{self, SelectionTiming};
use crate::session::{open_fifo_round, snapshot_error};
use crate::types::*;

const SEAT_STEP_MS: [u32; 14] = [
    75_000, 70_000, 65_000, 58_000, 52_000, 46_000, 40_000, 34_000, 28_000, 23_000, 20_000, 18_000,
    16_000, 15_000,
];

pub(crate) fn increment(value: u64) -> Result<u64, DraftError> {
    value
        .checked_add(1)
        .filter(|next| *next <= MAX_DRAFT_JSON_INTEGER)
        .ok_or(DraftError::InvalidClockBounds)
}

pub(crate) fn clamped_now(session: &DraftSession, now: u64) -> Result<u64, DraftError> {
    if now > MAX_DRAFT_JSON_INTEGER {
        return Err(DraftError::InvalidClockBounds);
    }
    Ok(now.max(session.fifo_ref()?.last_now_ms))
}

pub(crate) fn pick_duration(policy: PickClockPolicy, steps: u64) -> Option<u32> {
    match policy {
        PickClockPolicy::Off => None,
        PickClockPolicy::Fixed { duration_ms } => Some(duration_ms),
        PickClockPolicy::SeatStep => {
            Some(SEAT_STEP_MS[usize::try_from(steps.min(13)).expect("bounded schedule index")])
        }
    }
}

pub(crate) fn running(
    target: DraftClockTarget,
    now: u64,
    duration: u32,
) -> Result<DraftClock, DraftError> {
    let deadline_ms = deadline(now, u64::from(duration))?;
    Ok(DraftClock {
        target,
        state: DraftClockState::Running { deadline_ms },
    })
}

fn deadline(now: u64, remaining: u64) -> Result<u64, DraftError> {
    now.checked_add(remaining)
        .filter(|value| *value <= MAX_DRAFT_JSON_INTEGER)
        .ok_or(DraftError::InvalidClockBounds)
}

pub(crate) fn reconcile_prompts(session: &mut DraftSession, now: u64) -> Result<(), DraftError> {
    let policy = session.config.clock_policy.pick;
    let fifo = session.fifo_mut()?;
    for (index, seat) in fifo.seats.iter_mut().enumerate() {
        let Some(head) = seat.queue.front() else {
            seat.prompt = None;
            continue;
        };
        if seat
            .prompt
            .as_ref()
            .is_some_and(|prompt| prompt.pack_id == head.pack_id)
        {
            continue;
        }
        seat.generation = increment(seat.generation)?;
        let id = DraftPromptId {
            session_id: fifo.session_id.clone(),
            seat: u8::try_from(index).expect("validated seat"),
            generation: seat.generation,
        };
        let clock = pick_duration(policy, seat.round_completed_steps)
            .map(|duration| {
                running(
                    DraftClockTarget::Pick {
                        prompt_id: id.clone(),
                    },
                    now,
                    duration,
                )
            })
            .transpose()?;
        seat.prompt = Some(DraftPrompt {
            id,
            pack_id: head.pack_id,
            clock,
        });
    }
    Ok(())
}

pub(crate) fn control(
    session: &mut DraftSession,
    control: DraftControl,
    now: u64,
) -> Result<Vec<DraftDelta>, DraftError> {
    match control {
        DraftControl::Pause { reason } => {
            let phase = match session.status {
                DraftStatus::Drafting => DraftActivePhase::Drafting,
                DraftStatus::Review => DraftActivePhase::Review,
                DraftStatus::Lobby
                | DraftStatus::Paused
                | DraftStatus::Deckbuilding
                | DraftStatus::Pairing
                | DraftStatus::MatchInProgress
                | DraftStatus::RoundComplete
                | DraftStatus::Complete
                | DraftStatus::Abandoned => {
                    return Err(DraftError::InvalidTransition {
                        from: session.status,
                        action: "Pause".into(),
                    })
                }
            };
            for_each_clock(session, |clock| {
                let DraftClockState::Running { deadline_ms } = clock.state else {
                    return Err(snapshot_error("pause expected running scope"));
                };
                clock.state = DraftClockState::Frozen {
                    remaining_ms: deadline_ms.saturating_sub(now),
                };
                Ok(())
            })?;
            session.fifo_mut()?.paused = Some(DraftPausedState { phase, reason });
            session.status = DraftStatus::Paused;
        }
        DraftControl::Resume => {
            if session.status != DraftStatus::Paused {
                return Err(DraftError::InvalidTransition {
                    from: session.status,
                    action: "Resume".into(),
                });
            }
            let paused = session
                .fifo_ref()?
                .paused
                .ok_or_else(|| snapshot_error("missing paused phase"))?;
            for_each_clock(session, |clock| {
                let DraftClockState::Frozen { remaining_ms } = clock.state else {
                    return Err(snapshot_error("resume expected frozen scope"));
                };
                clock.state = DraftClockState::Running {
                    deadline_ms: deadline(now, remaining_ms)?,
                };
                Ok(())
            })?;
            session.fifo_mut()?.paused = None;
            session.status = match paused.phase {
                DraftActivePhase::Drafting => DraftStatus::Drafting,
                DraftActivePhase::Review => DraftStatus::Review,
            };
        }
    }
    Ok(vec![DraftDelta::TransitionedTo {
        status: session.status,
    }])
}

fn for_each_clock(
    session: &mut DraftSession,
    mut f: impl FnMut(&mut DraftClock) -> Result<(), DraftError>,
) -> Result<(), DraftError> {
    let fifo = session.fifo_mut()?;
    for seat in &mut fifo.seats {
        if let Some(clock) = seat
            .prompt
            .as_mut()
            .and_then(|prompt| prompt.clock.as_mut())
        {
            f(clock)?;
        }
    }
    if let Some(review) = &mut fifo.review {
        f(&mut review.clock)?;
    }
    Ok(())
}

pub(crate) fn advance(session: &mut DraftSession, now: u64) -> Result<Vec<DraftDelta>, DraftError> {
    match session.status {
        DraftStatus::Review => {
            let review = session
                .fifo_ref()?
                .review
                .as_ref()
                .ok_or_else(|| snapshot_error("missing Review"))?;
            if matches!(review.clock.state, DraftClockState::Running { deadline_ms } if now >= deadline_ms)
            {
                let next_round = review.next_round;
                open_fifo_round(session, next_round, now)?;
                return Ok(vec![
                    DraftDelta::PackExhausted {
                        new_pack_number: next_round,
                    },
                    DraftDelta::TransitionedTo {
                        status: DraftStatus::Drafting,
                    },
                ]);
            }
            return Ok(Vec::new());
        }
        DraftStatus::Drafting => {}
        DraftStatus::Lobby
        | DraftStatus::Paused
        | DraftStatus::Deckbuilding
        | DraftStatus::Pairing
        | DraftStatus::MatchInProgress
        | DraftStatus::RoundComplete
        | DraftStatus::Complete
        | DraftStatus::Abandoned => return Ok(Vec::new()),
    }
    let captured: Vec<_> = session
        .fifo_ref()?
        .seats
        .iter()
        .filter_map(|seat| {
            let prompt = seat.prompt.as_ref()?;
            let DraftClockState::Running { deadline_ms } = prompt.clock.as_ref()?.state else {
                return None;
            };
            (deadline_ms <= now).then_some((prompt.id.clone(), prompt.pack_id, deadline_ms))
        })
        .collect();
    let mut deltas = Vec::new();
    for (prompt_id, pack_id, deadline_ms) in captured {
        let current = session.fifo_ref()?.seats[usize::from(prompt_id.seat)]
            .prompt
            .as_ref();
        if !current.is_some_and(|prompt| {
            prompt.id == prompt_id
                && prompt.pack_id == pack_id
                && prompt
                    .clock
                    .as_ref()
                    .is_some_and(|clock| clock.state == DraftClockState::Running { deadline_ms })
        }) {
            continue;
        }
        if let Some(DraftAction::Select {
            seat,
            prompt_id,
            picked_card_instance_ids,
            burned_card_instance_ids,
            effect_card_instance_ids,
        }) = pick_pass::legal_auto_selection(session, prompt_id.seat)
        {
            deltas.extend(pick_pass::select_fifo(
                session,
                seat,
                prompt_id,
                picked_card_instance_ids,
                burned_card_instance_ids,
                effect_card_instance_ids,
                SelectionTiming::Expiry {
                    pack_id,
                    deadline_ms,
                },
            )?);
        }
    }
    Ok(deltas)
}
