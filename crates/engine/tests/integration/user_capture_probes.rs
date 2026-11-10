//! USER-CAPTURE ROWS — the user's own 4-player Dina / Bloodthirsty Conqueror capture, driven
//! through the production `apply()` beat policy, asserting where the CR 732.2a bounded offer does
//! and does not appear.
//!
//! **These are TRACKED acceptance rows and run on every CI run.** They were env-gated diagnostics
//! over an untracked bug-report attachment until that capture was derived into
//! `fixtures/dina_conqueror_phase5_no_offer_4p.json.gz` by the lane's recipe —
//! `jq -c '{gameState}' <dump> | gzip -9 -n`, `-n` so the archive carries no timestamp and is
//! byte-reproducible (769 608 B, sha256
//! `c25e214df271bc1adecb1df34d16b3f6978b7853abdcca084e323fe2132e97c9`). Re-gzipping the same way
//! is still NECESSARY — but it is no longer SUFFICIENT. This capture predates U5, so its bare
//! `"deck_size": 100` must first become the adjacently-tagged `DeckSizeRule` form
//! `{"type":"Exactly","data":100}` — the variant taken from the sibling `format` field,
//! `Commander` here. Piping the raw dump straight through the recipe above does not merely miss
//! the digest; it yields a fixture `PersistedGameState` cannot decode, so `load_dina_raw`'s
//! `.expect("dina gameState decodes through the production decoder")` aborts — a red test on a
//! green engine. `dina_noff_turn5_loader.rs` is the worked example of this two-step form. Then
//! the retired `combat_phases_started_this_turn` / `end_steps_started_this_turn` keys must be
//! rewritten to `steps_started_this_turn` (`{"BeginCombat": n, "End": m}`, zeros dropped, placed
//! at the first old key).
//!
//! No env var gates anything here: the headline result — the offer firing on the user's own board
//! with a FOREIGN play live in the window — is reproducible by anyone who can run the suite.
//!
//! # The capture (2026-08-03T19-29-36-888Z) — what it measured
//!
//! A MANDATORY gain/drain trigger chain with no per-iteration choices, at `Priority{1}`, turn 5,
//! life `[48, 31, 36, 36]`, captured right after an OPPONENT's activation unrelated to the drain:
//! seat 2's Currency Converter (object 268), ability 1. The already-tracked
//! `dina_conqueror_4p.json.gz` is a DIFFERENT capture of the same deck (life `[46, 37, 33, 38]`)
//! and cannot stand in for it.
//!
//! # The pair
//!
//! A save restores with no play trace, so ARM D1 is what every in-process test sees, and ARM D2
//! puts the opponent's activation back into the window's trace — what the running game held. They
//! differ in exactly that play.

use engine::analysis::decision_template::IterationCount;
use engine::game::engine::apply;
use engine::game::{EntryKind, PlayLocus};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, PersistedGameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;

/// The opponent's activation the capture was taken after: seat 2's Currency Converter, ability 1.
const FOREIGN_PLAY: (PlayerId, PlayLocus) = (PlayerId(2), PlayLocus::Activate(ObjectId(268), 1));

/// The user's capture, `{gameState}`-only and gzipped. Provenance:
/// `dina-conqueror-phase5-no-offer.zip` / `game-state-turn-5-2026-08-03T19-29-36-888Z.json`.
const DINA_PHASE5_GZ: &[u8] =
    include_bytes!("../fixtures/dina_conqueror_phase5_no_offer_4p.json.gz");

fn wf_label(w: &WaitingFor) -> String {
    match w {
        WaitingFor::Priority { player } => format!("Priority({})", player.0),
        WaitingFor::LoopShortcut { proposer, .. } => format!("LoopShortcut({})", proposer.0),
        other => format!("{other:?}")
            .split_whitespace()
            .next()
            .unwrap_or("?")
            .trim_end_matches('{')
            .to_string(),
    }
}

/// The board as PRODUCTION loads it.
///
/// Decodes AS `PersistedGameState` rather than as a bare `GameState`: only the former runs the
/// production restore chokepoint (`reject_legacy_raw_prompt_authority`,
/// `decode_persisted_resolution_state`) that both the server's
/// `from_persisted` and WASM's `decode_restored_game_state` funnel through. The dump was captured
/// with the detector OFF and every row here is about the CR 732.2a interactive offer, so the mode
/// is set at load — the same thing the user's own toggle does.
fn load_dina_raw() -> GameState {
    use std::io::Read;
    let mut json = String::new();
    flate2::read::GzDecoder::new(DINA_PHASE5_GZ)
        .read_to_string(&mut json)
        .expect("fixture .json.gz must inflate to UTF-8 JSON");
    let mut envelope: serde_json::Value = serde_json::from_str(&json).expect("dina dump parses");
    // Committed pre-onset capture; onset-independent test (see record_captured_onsets).
    engine::types::game_state::record_captured_onsets(&mut envelope);
    let mut state = serde_json::from_value::<PersistedGameState>(envelope["gameState"].clone())
        .expect("dina gameState decodes through the production decoder")
        .into_game_state()
        .expect("persisted test snapshot satisfies the checked restore contract");
    state.loop_detection = engine::types::game_state::LoopDetectionMode::Interactive;
    state
}

/// The seats of the window's plays.
fn play_seats(state: &GameState) -> Vec<PlayerId> {
    engine::game::play_trace_view(state).map_or_else(Vec::new, |view| {
        view.entries
            .iter()
            .filter(|entry| matches!(entry.kind, EntryKind::Play { .. }))
            .map(|entry| entry.seat)
            .collect()
    })
}

/// The one seat every play of the window shares, if the window holds plays.
fn period_driver(state: &GameState) -> Option<PlayerId> {
    let seats = play_seats(state);
    let owner = *seats.first()?;
    seats.iter().all(|seat| *seat == owner).then_some(owner)
}

/// Generic mandatory-chain beat: pass at `Priority`, otherwise take the first legal action.
/// The Dina chain opens no player choices, so no preference ordering is needed.
fn dina_drive_one_beat(state: &mut GameState) -> Result<String, String> {
    // CR 117.3d: at a priority window this policy always passes, so dispatch the pass instead of
    // enumerating the whole per-viewer candidate set to find it. This reproduces both halves of the
    // enumerator's hatch — its structural predicate and the submitter identity it authorizes
    // (CR 723.5) — so this arm stays inside the subset that hatch asserts equivalent to a
    // simulated pass; every other shape falls through to the enumerating path below.
    if let WaitingFor::Priority { player } = state.waiting_for {
        if engine::game::priority::pass_priority_structurally_legal(state, player) {
            let action = GameAction::PassPriority;
            let label = format!("{action:?}");
            let actor = engine::game::turn_control::authorized_submitter_for_player(state, player);
            return apply(state, actor, action)
                .map(|_| label)
                .map_err(|e| format!("apply err (PassPriority): {e:?}"));
        }
    }
    let who = state
        .waiting_for
        .acting_player()
        .or_else(|| state.waiting_for.acting_players().first().copied())
        .ok_or_else(|| format!("no acting player at {:?}", state.waiting_for))?;
    let (actions, _costs, _grouped) = engine::ai_support::legal_actions_for_viewer(state, who);
    let chosen = if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        actions
            .iter()
            .find(|a| matches!(a, GameAction::PassPriority))
            .cloned()
    } else {
        actions
            .iter()
            .find(|a| !matches!(a, GameAction::PassPriority))
            .or_else(|| actions.first())
            .cloned()
    };
    let action = chosen.ok_or_else(|| {
        format!(
            "no action at {:?}; legal = {:?}",
            state.waiting_for,
            actions.iter().take(8).collect::<Vec<_>>()
        )
    })?;
    let label = format!("{action:?}");
    let actor = engine::game::turn_control::authorized_submitter_for_player(state, who);
    apply(state, actor, action.clone())
        .map(|_| label)
        .map_err(|e| format!("apply err ({action:?}): {e:?}"))
}

/// Drives IN PLACE (`&mut`), so the caller can inspect the board the drive stopped on — the
/// offer's proposer, the ring depth, the window's plays — instead of only the beat.
///
/// Returns the beat the drive stopped on.
fn dina_drive_and_report(state: &mut GameState, label: &str, beats: u32) -> Option<u32> {
    eprintln!(
        "[{label}] START turn={} active={} wf={} stack={} ring={} seq={} life={:?}",
        state.turn_number,
        state.active_player.0,
        wf_label(&state.waiting_for),
        state.stack.len(),
        state.loop_detect_ring.len(),
        play_seats(state).len(),
        state.players.iter().map(|p| p.life).collect::<Vec<_>>(),
    );
    let mut fired = None;
    for beat in 0..beats {
        if matches!(
            state.waiting_for,
            WaitingFor::LoopShortcut { .. } | WaitingFor::GameOver { .. }
        ) {
            fired = Some(beat);
            break;
        }
        let before = wf_label(&state.waiting_for);
        match dina_drive_one_beat(state) {
            Ok(act) => {
                eprintln!(
                    "[{label}] beat {beat:3} {before:>26} -> {:<26} stack={} ring={} seq={} life={:?} act={}",
                    wf_label(&state.waiting_for),
                    state.stack.len(),
                    state.loop_detect_ring.len(),
                    play_seats(state).len(),
                    state.players.iter().map(|p| p.life).collect::<Vec<_>>(),
                    &act.chars().take(40).collect::<String>()
                );
            }
            Err(e) => {
                eprintln!("[{label}] beat {beat:3} ABORT at {before}: {e}");
                break;
            }
        }
    }
    eprintln!(
        "[{label}] END fired={fired:?} wf={} ring={} seq={} life={:?}",
        wf_label(&state.waiting_for),
        state.loop_detect_ring.len(),
        play_seats(state).len(),
        state.players.iter().map(|p| p.life).collect::<Vec<_>>(),
    );
    fired
}

/// (beat the offer fired at, ring depth, life vector) — the axes the two arms are compared on.
fn offer_signature(state: &GameState, fired: Option<u32>) -> (Option<u32>, usize, Vec<i32>) {
    (
        fired,
        state.loop_detect_ring.len(),
        state.players.iter().map(|p| p.life).collect(),
    )
}

/// ARM D1 — the capture as PRODUCTION loads it, with no play trace, so this is the state every
/// in-process test would see. This is the CONTROL: the offer this board raises is the one ARM D2
/// must match.
#[test]
fn the_user_captures_offer_is_reached_with_its_driving_period_cleared() {
    let state = load_dina_raw();
    assert!(
        engine::game::play_trace_view(&state).is_none(),
        "reach-guard: a restored board carries no play trace"
    );
    assert!(
        state.may_trigger_auto_choices.is_empty(),
        "reach-guard: the Dina dump carries no may-trigger auto choice (the F4 mechanism \
         cannot apply here)"
    );
    let mut state = state;
    let fired = dina_drive_and_report(&mut state, "DINA-LOADED", 140);
    eprintln!("[DINA-LOADED] fired={fired:?}");
    assert!(
        fired.is_some(),
        "REACH-GUARD: the field-cleared control must reach the offer, else ARM D2 has nothing \
         to be identical TO and the pair proves nothing"
    );

    // WHICH SITES THIS FIXTURE CAN AND CANNOT HOST, asserted rather than claimed in prose
    // elsewhere. `handle_declare_shortcut`'s `template: None` arm (site F) sits under
    // `if !offer.schema.points.is_empty()`. This capture's offer publishes no point, so it cannot
    // host site F, which is why that row rides another fixture. Pinning it here
    // means a future capture that DOES publish points reds this line instead of silently making
    // the sibling rows' scope claims stale.
    let WaitingFor::LoopShortcut {
        predicted_winner,
        schema,
        ..
    } = &state.waiting_for
    else {
        panic!(
            "`fired` is Some, so the drive stopped on the offer; got {:?}",
            state.waiting_for
        )
    };
    assert_eq!(
        (predicted_winner, &schema.iteration_count),
        (&Some(PlayerId(0)), &IterationCount::Fixed(35)),
        "CR 104.2a: this capture's offer is the count that eliminates every opponent"
    );
    assert!(
        schema.points.is_empty(),
        "SCOPE: this capture's offer publishes no per-iteration choice point, so site F is \
         STRUCTURALLY unreachable from it; got {:?}",
        schema.points
    );
    assert!(
        schema.is_bounded(),
        "SCOPE: this capture's offer is bounded"
    );
}

/// ARM D2 — the same board with the opponent's activation put back into the window's trace, i.e.
/// what the running game held. Only that play differs from ARM D1.
///
/// **THE FIX BAR, on the user's own capture.** The drive must reach the SAME offer the trace-free
/// control reaches, at the same beat, with the same ring depth and the same life vector: a play
/// another seat made describes no sequence this proposer takes (CR 732.2a), so it is inert.
#[test]
fn the_user_captures_offer_is_reached_with_its_own_foreign_period_live() {
    let mut state = load_dina_raw();
    engine::game::install_plays_for_tests(&mut state, &[FOREIGN_PLAY]);
    let foreign = FOREIGN_PLAY.0;
    assert_eq!(
        period_driver(&state),
        Some(foreign),
        "reach-guard: the window holds the opponent's play"
    );

    let fired = dina_drive_and_report(&mut state, "DINA-LIVE", 140);
    eprintln!("[DINA-LIVE] fired={fired:?}");

    // The CONTROL, re-driven in this process: same board, no trace.
    let mut control = load_dina_raw();
    let control_fired = dina_drive_and_report(&mut control, "DINA-CONTROL", 140);

    // IN-ROW REACH-GUARD, not inherited from ARM D1's: the equality below compares this arm to a
    // control re-driven HERE, so on a board where NEITHER side reaches an offer both sides are
    // `(None, (None, ring, life))` and the assertion passes having measured nothing. ARM D1's own
    // guard cannot cover that — a `#[test]` that is skipped, filtered, or reds independently
    // leaves this row still "green". The control must PROVABLY reach the offer in this process.
    assert!(
        control_fired.is_some(),
        "REACH-GUARD: the trace-free control re-driven in THIS row must reach the offer, else \
         the fix bar below compares two absences and passes vacuously"
    );

    // ── THE ENDPOINT BAR: the whole trajectory, not one frame. ──
    let (proposer, control_proposer) = (offer_proposer(&state), offer_proposer(&control));
    assert_ne!(
        Some(foreign),
        control_proposer,
        "REACH-GUARD: the injected play must belong to a seat OTHER than the proposer"
    );
    assert_eq!(
        (proposer, offer_signature(&state, fired)),
        (control_proposer, offer_signature(&control, control_fired)),
        "CR 732.2a THE FIX BAR: with an opponent's activation in the window's trace, the \
         proposer's own bounded offer must be reached at the same beat, with the same ring \
         depth and the same life vector, as the trace-free control"
    );
}

fn offer_proposer(state: &GameState) -> Option<engine::types::player::PlayerId> {
    match &state.waiting_for {
        WaitingFor::LoopShortcut { proposer, .. } => Some(*proposer),
        _ => None,
    }
}
