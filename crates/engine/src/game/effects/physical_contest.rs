use crate::types::ability::{Effect, EffectError, ResolvedAbility};
use crate::types::contest::{ContestConfig, ContestState};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};

/// Engine-authored identity of the contest about to start.
///
/// Every host input (click, progress frame, AI schedule) is addressed to it, so a
/// frame delayed from an earlier contest is never counted in a later one. Two inputs
/// make it unique: the state revision at resolution (every contest resolves inside a
/// distinct action, and each action advances the revision) and the interaction
/// session, which an approved takeback rotates when it restores an earlier state.
/// The revision alone is rewindable — replaying the same actions after a restore would
/// reach the same revision — so without the session a frame stamped for the
/// discarded branch's contest could match the replayed one.
///
/// Folded into 53 bits so every host can carry it as a JavaScript number, and never
/// `0`, which is not a valid identity.
fn contest_identity(state: &GameState) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let session = state
        .interaction_session_id
        .as_ref()
        .map_or("", |session| session.0.as_str());
    let revision = state.state_revision.saturating_add(1);
    let hash = session
        .bytes()
        .chain(revision.to_le_bytes())
        .fold(FNV_OFFSET, |acc, byte| {
            (acc ^ u64::from(byte)).wrapping_mul(FNV_PRIME)
        });
    (hash & ((1 << 53) - 1)).max(1)
}

/// Eye to Eye / Mouth to Mouth: "You and [player] have a [staring | breath-holding]
/// contest."
///
/// The Comprehensive Rules define no physical contests, so there is no CR number for
/// the contest itself. The online adaptation (`types::contest`) pauses resolution on
/// `WaitingFor::PhysicalContest`; the host drives it through
/// `game::contest::{start_contest, advance_contest, contest_click, ...}` and the
/// settled result gates the printed "if you win" rider.
///
/// CR 608.2b: the second contestant is a player the ability references (a target's
/// controller, or a previously declared player target). If it no longer names a legal
/// player — an illegal target, or a player who left the game — the instruction cannot
/// be performed and nothing happens; the rider's `EventOutcomeWon` gate stays false.
/// Adaptation policy, not a CR rule: the online contest needs two distinct
/// participants, so a contestant that resolves to the controller likewise performs
/// nothing.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::PhysicalContest { kind, contestant } = &ability.effect else {
        return Ok(());
    };
    let controller = ability.controller;
    let Some(opponent) = super::resolve_player_for_context_ref(state, ability, contestant)
        .filter(|&player| player != controller && crate::game::players::is_alive(state, player))
    else {
        return Ok(());
    };

    // `Pending` until the host supplies a clock reading (`start_contest`); the
    // engine itself never reads time.
    // Identity: see `contest_identity`.
    let contest = ContestState::new(
        contest_identity(state),
        *kind,
        controller,
        opponent,
        ContestConfig::default(),
    );
    events.push(GameEvent::PhysicalContestStarted {
        kind: *kind,
        participants: contest.players(),
    });
    state.waiting_for = WaitingFor::PhysicalContest {
        contest: Box::new(contest),
        source_id: ability.source_id,
    };
    Ok(())
}
