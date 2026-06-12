use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::player::PlayerId;
use crate::types::statics::StaticMode;

/// CR 702.179f: Effects that refer to speed treat "no speed" as 0.
pub fn effective_speed(state: &GameState, player: PlayerId) -> u8 {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .and_then(|p| p.speed)
        .unwrap_or(0)
}

/// CR 702.179e: A player has max speed if their speed is 4.
/// Some effects allow speed to exceed 4 and still count as max speed at 4 or greater.
pub fn has_max_speed(state: &GameState, player: PlayerId) -> bool {
    let speed = effective_speed(state, player);
    if can_increase_speed_beyond_4(state, player) {
        speed >= 4
    } else {
        speed == 4
    }
}

pub fn set_speed(
    state: &mut GameState,
    player: PlayerId,
    new_speed: Option<u8>,
    events: &mut Vec<GameEvent>,
) {
    let Some(player_state) = state.players.iter_mut().find(|p| p.id == player) else {
        return;
    };
    let old_speed = player_state.speed;
    if old_speed == new_speed {
        return;
    }
    player_state.speed = new_speed;
    events.push(GameEvent::SpeedChanged {
        player,
        old_speed,
        new_speed,
    });
}

/// CR 702.179c-d: Increasing speed sets absent speed to the increase amount and otherwise
/// increments it, subject to the default cap of 4 unless a static ability says otherwise.
pub fn increase_speed(
    state: &mut GameState,
    player: PlayerId,
    amount: u8,
    events: &mut Vec<GameEvent>,
) {
    let current = state
        .players
        .iter()
        .find(|p| p.id == player)
        .and_then(|p| p.speed);
    let Some(base) = current else {
        set_speed(state, player, Some(amount), events);
        return;
    };

    let increased = base.saturating_add(amount);
    let capped = if can_increase_speed_beyond_4(state, player) {
        increased
    } else {
        increased.min(4)
    };
    set_speed(state, player, Some(capped), events);
}

/// CR 702.179c-d: Decreasing speed subtracts `amount`, treating absent speed
/// as 0 (CR 702.179f). `saturating_sub` floors the result at 0; an optional
/// card-text-derived `floor` raises that minimum (e.g. Spikeshell Harrier's
/// "can't reduce their speed below 1"). `None` floor adds no further guard —
/// `saturating_sub` already floors at 0.
pub fn decrease_speed(
    state: &mut GameState,
    player: PlayerId,
    amount: u8,
    floor: Option<u8>,
    events: &mut Vec<GameEvent>,
) {
    let current = state
        .players
        .iter()
        .find(|p| p.id == player)
        .and_then(|p| p.speed);
    // CR 702.179f: a player with no speed is treated as having speed 0.
    let decreased = current.unwrap_or(0).saturating_sub(amount);
    let floored = match floor {
        Some(f) => decreased.max(f),
        None => decreased,
    };
    // CR 702.179f: a result of 0 from an already-no-speed player stays "no
    // speed" — don't materialize `None → Some(0)`.
    let new_speed = if current.is_none() && floored == 0 {
        None
    } else {
        Some(floored)
    };
    set_speed(state, player, new_speed, events);
}

/// CR 702.179a: Start your engines checks whether a player controls a permanent with the keyword.
pub fn controls_start_your_engines(state: &GameState, player: PlayerId) -> bool {
    state.battlefield.iter().any(|id| {
        state.objects.get(id).is_some_and(|obj| {
            // CR 702.26b: a phased-out permanent is treated as though it does not exist.
            obj.controller == player
                && obj.is_phased_in()
                && obj.has_keyword(&Keyword::StartYourEngines)
        })
    })
}

// Re-entrancy guard for `can_increase_speed_beyond_4`.
//
// `can_increase_speed_beyond_4` scans `active_static_definitions`, which
// evaluates each static's CR 604.1 functioning condition. A
// `StaticCondition::HasMaxSpeed` condition maps (layers.rs) back to
// `has_max_speed`, which calls `can_increase_speed_beyond_4` again — an
// implementation-level recursion loop between two DISTINCT statics (the
// HasMaxSpeed-gated static being condition-checked, and Gomif's
// `SpeedCanIncreaseBeyondFour` static being searched for). Without a guard this
// overflows the stack on any board where the controller has a HasMaxSpeed-gated
// static (e.g. Racers' Scoreboard).
//
// THREAD-LOCAL (not a `GameState` field, not `AtomicBool`): engine layer
// resolution is synchronous, so production code invoked by a test/AI-search runs
// on that caller's own thread. A process-global `AtomicBool` races under cargo's
// parallel test runner and across AI clone-search threads. This mirrors the
// `Cell<bool>` thread-local idiom in layers.rs (REBUILD_STATIC_INDEX_AT_TOP) and
// the TLS save/restore pattern in quantity.rs (`with_detection_trigger_event`).
thread_local! {
    static SPEED_CAP_GUARD: core::cell::Cell<bool> = const { core::cell::Cell::new(false) };
}

/// RAII guard for the `can_increase_speed_beyond_4` re-entrancy flag. The outer
/// call sets the flag to `true`; `Drop` restores the previous value (panic-safe).
/// A nested (inner) call observes `true` and returns the base cap WITHOUT
/// touching the flag, so the outer call's `Drop` still restores correctly.
struct SpeedCapGuard {
    prev: bool,
}

impl SpeedCapGuard {
    fn enter() -> Self {
        let prev = SPEED_CAP_GUARD.with(|g| g.replace(true));
        Self { prev }
    }
}

impl Drop for SpeedCapGuard {
    fn drop(&mut self) {
        SPEED_CAP_GUARD.with(|g| g.set(self.prev));
    }
}

/// Card-specific rule modification for effects like Gomif.
pub fn can_increase_speed_beyond_4(state: &GameState, player: PlayerId) -> bool {
    // CR 613.8b: when effects form a dependency loop, the loop is broken and
    // values are taken in timestamp order rather than circularly. Here the loop
    // is mostly implementation recursion across two distinct statics (the
    // HasMaxSpeed-gated static's functioning-condition check vs. the search for
    // Gomif's `SpeedCanIncreaseBeyondFour`), not a literal single-effect CR 613
    // dependency loop — but 613.8b is the rules basis for returning the BASE cap
    // (no circular contribution) on re-entry rather than recursing. The inner
    // answer is never a consumed result: the real layer pass re-evaluates
    // `HasMaxSpeed` through the UNGUARDED outer `has_max_speed`. Returning `false`
    // (cap = 4) here only affects the throwaway condition-evaluation of a
    // HasMaxSpeed-gated static during the scan, which searches for a DIFFERENT
    // mode (`SpeedCanIncreaseBeyondFour`, whose `condition` is `None` so it is
    // found regardless of this guard).
    if SPEED_CAP_GUARD.with(core::cell::Cell::get) {
        return false;
    }
    let _g = SpeedCapGuard::enter();
    // CR 702.26b + CR 604.1: `active_static_definitions` owns the gating.
    state.battlefield.iter().any(|&id| {
        state.objects.get(&id).is_some_and(|obj| {
            if obj.controller != player {
                return false;
            }
            crate::game::functioning_abilities::active_static_definitions(state, obj)
                .any(|def| def.mode == StaticMode::SpeedCanIncreaseBeyondFour)
        })
    })
}

pub fn mark_speed_trigger_used(state: &mut GameState, player: PlayerId) {
    if let Some(player_state) = state.players.iter_mut().find(|p| p.id == player) {
        player_state.speed_trigger_used_this_turn = true;
    }
}

pub fn speed_trigger_available(state: &GameState, player: PlayerId) -> bool {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .is_some_and(|p| !p.speed_trigger_used_this_turn)
}

pub fn speed_key_source() -> ObjectId {
    ObjectId(0)
}
