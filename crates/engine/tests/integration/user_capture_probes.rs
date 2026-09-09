//! USER-CAPTURE ROWS — the user's own 4-player Dina / Bloodthirsty Conqueror capture, driven
//! through the production `apply()` beat policy, asserting where the CR 732.2a bounded offer does
//! and does not appear.
//!
//! **These are TRACKED acceptance rows and run on every CI run.** They were env-gated diagnostics
//! over an untracked bug-report attachment until that capture was derived into
//! `fixtures/dina_conqueror_phase5_no_offer_4p.json.gz` by the lane's recipe —
//! `jq -c '{gameState}' <dump> | gzip -9 -n`, `-n` so the archive carries no timestamp and is
//! byte-reproducible (769 606 B, sha256
//! `97fd7dc70aeccecdd62c3376d7e86ea12c794b23623ca50b332e51b95af3eaad`). Re-gzipping the same way
//! is still NECESSARY — but it is no longer SUFFICIENT. This capture predates U5, so its bare
//! `"deck_size": 100` must first become the adjacently-tagged `DeckSizeRule` form
//! `{"type":"Exactly","data":100}` — the variant taken from the sibling `format` field,
//! `Commander` here. Piping the raw dump straight through the recipe above does not merely miss
//! the digest; it yields a fixture `PersistedGameState` cannot decode, so `load_dina_raw`'s
//! `.expect("dina gameState decodes through the production decoder")` aborts — a red test on a
//! green engine. `dina_noff_turn5_loader.rs` is the worked example of this two-step form.
//!
//! No env var gates anything here: the headline result — the offer firing on the user's own board
//! with a FOREIGN driving period live in state — is reproducible by anyone who can run the suite.
//!
//! # The capture (2026-08-03T19-29-36-888Z) — what it measured
//!
//! A MANDATORY gain/drain trigger chain with no per-iteration choices, at `Priority{1}`, turn 5,
//! life `[48, 31, 36, 36]`. Its distinguishing field is a length-1 `last_loop_action_sequence`:
//!
//! ```text
//! [{ action: Activate { source_id: 268, ability_index: 1 }, controller: 2, pins: [] }]
//! ```
//!
//! 268 is Currency Converter, controlled by an OPPONENT — an activation unrelated to the drain.
//! Before the seat-relative fix, conjunct (1b) refused the bounded offer on ANY non-empty sequence
//! (it was named `DrivingSequenceNotEmpty` then, `ProposerHasDrivingPeriod` now), so that one
//! foreign step suppressed the proposer's offer for the rest of the game. The already-tracked
//! `dina_conqueror_4p.json.gz` is a DIFFERENT capture of the same deck (life `[46, 37, 33, 38]`,
//! no recorded period) and cannot stand in for it.
//!
//! # Why tracking the dump is not by itself enough
//!
//! `GameState::migrate_transient_loop_sequence` CLEARS the field at every load that is not a
//! shortcut window, so `into_game_state()` wipes the one field this file is about no matter where
//! the bytes came from — tracking alone would give ARM D1 twice. ARM D2 therefore reads the field
//! back out of the fixture's OWN serialized JSON and puts it back: the dump's own bytes, not a
//! synthesized step. **That asymmetry is the whole point of the pair below**: ARM D1 is what every
//! in-process test sees, ARM D2 is what the running game actually held, and they differ in exactly
//! one field.

use engine::game::engine::apply;
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, PersistedGameState, WaitingFor};
use engine::types::player::PlayerId;

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

/// The TYPED refusal of the bounded-offer mint at this exact frame — the discriminating
/// instrument: it names WHICH conjunct refused instead of collapsing to "no offer". That is what
/// makes a per-beat census non-vacuous; a bare "did not fire" could not distinguish this defect
/// from a board that simply never certified.
fn mint_verdict(state: &GameState) -> String {
    match engine::game::engine::try_offer_bounded_cycle_shortcut(state, false) {
        Ok(_) => "OFFER".to_string(),
        Err(e) => format!("{e:?}"),
    }
}

/// The board as PRODUCTION loads it, paired with the driving period the capture actually
/// serialized — which the load migration has by then already dropped from the board.
///
/// Decodes AS `PersistedGameState` rather than as a bare `GameState`: only the former runs the
/// production restore chokepoint (`reject_legacy_raw_prompt_authority`,
/// `decode_persisted_resolution_state`, `migrate_transient_loop_sequence`) that both the server's
/// `from_persisted` and WASM's `decode_restored_game_state` funnel through. The dump was captured
/// with the detector OFF and every row here is about the CR 732.2a interactive offer, so the mode
/// is set at load — the same thing the user's own toggle does.
fn load_dina_raw() -> (GameState, serde_json::Value) {
    use std::io::Read;
    let mut json = String::new();
    flate2::read::GzDecoder::new(DINA_PHASE5_GZ)
        .read_to_string(&mut json)
        .expect("fixture .json.gz must inflate to UTF-8 JSON");
    let envelope: serde_json::Value = serde_json::from_str(&json).expect("dina dump parses");
    let mut state = serde_json::from_value::<PersistedGameState>(envelope["gameState"].clone())
        .expect("dina gameState decodes through the production decoder")
        .into_game_state()
        .expect("persisted test snapshot satisfies the checked restore contract");
    state.loop_detection = engine::types::game_state::LoopDetectionMode::Interactive;
    let raw_seq = envelope["gameState"]["last_loop_action_sequence"].clone();
    (state, raw_seq)
}

/// Mirror of `GameState::loop_period_controller`, which is `pub(crate)` and therefore unreachable
/// from an integration test: the seat every recorded step shares, or `None` for a heterogeneous
/// run (which every routing site fail-closes on).
fn period_controller(state: &GameState) -> Option<PlayerId> {
    let owner = state.last_loop_action_sequence.first()?.controller;
    state
        .last_loop_action_sequence
        .iter()
        .all(|step| step.controller == owner)
        .then_some(owner)
}

/// How the recorded period relates to this frame's proposer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PeriodRelation {
    /// The period is this proposer's own, so step (1b) must refuse.
    Own,
    /// The period belongs to another seat, so it must not affect this proposer.
    Foreign,
    /// There is no uniformly owned period, or this is not a proposing frame.
    AbsentOrHeterogeneous,
}

/// One driven beat's mint census entry, in the ISOLATED form.
///
/// `live` is the verdict on the board as driven; `cleared` is the verdict the SAME board returns
/// with `last_loop_action_sequence` emptied and nothing else touched. The mint runs (1b) BEFORE
/// (2), so a `live` refusal reported against (1b) is EVIDENCE about (1b) only where `cleared`
/// differs from it — otherwise a later conjunct refuses at that frame anyway and the attribution
/// is dominated. The round-2 census published a residual (1b) figure without this second column,
/// and the figure did not mean what it said.
struct MintFrame {
    beat: u32,
    /// The seat proposing at this frame; `None` when the frame is not a `Priority` beat, in which
    /// case no seat proposes and neither classification below applies.
    proposer: Option<PlayerId>,
    relation: PeriodRelation,
    live: String,
    cleared: String,
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

/// The ISOLATED census entry for the board as it stands after one driven beat.
fn mint_frame(state: &GameState, beat: u32) -> MintFrame {
    let proposer = match state.waiting_for {
        WaitingFor::Priority { player } => Some(player),
        _ => None,
    };
    let mut cleared_board = state.clone();
    cleared_board.last_loop_action_sequence.clear();
    let relation = match (proposer, period_controller(state)) {
        (Some(proposer), Some(controller)) if controller == proposer => PeriodRelation::Own,
        (Some(_), Some(_)) => PeriodRelation::Foreign,
        _ => PeriodRelation::AbsentOrHeterogeneous,
    };
    MintFrame {
        beat,
        proposer,
        relation,
        live: mint_verdict(state),
        cleared: mint_verdict(&cleared_board),
    }
}

/// Drives IN PLACE (`&mut`), so the caller can inspect the board the drive stopped on — the
/// offer's proposer, the ring depth, the surviving driving sequence — instead of only the beat.
///
/// Returns the beat the drive stopped on and the per-beat ISOLATED mint census.
fn dina_drive_and_report(
    state: &mut GameState,
    label: &str,
    beats: u32,
) -> (Option<u32>, Vec<MintFrame>) {
    eprintln!(
        "[{label}] START turn={} active={} wf={} stack={} ring={} seq={} life={:?}",
        state.turn_number,
        state.active_player.0,
        wf_label(&state.waiting_for),
        state.stack.len(),
        state.loop_detect_ring.len(),
        state.last_loop_action_sequence.len(),
        state.players.iter().map(|p| p.life).collect::<Vec<_>>(),
    );
    let mut fired = None;
    let mut census: Vec<MintFrame> = vec![];
    for beat in 0..beats {
        if matches!(
            state.waiting_for,
            WaitingFor::LoopShortcut { .. } | WaitingFor::GameOver { .. }
        ) {
            fired = Some(beat);
            break;
        }
        let at_priority = matches!(state.waiting_for, WaitingFor::Priority { .. });
        let before = wf_label(&state.waiting_for);
        match dina_drive_one_beat(state) {
            Ok(act) => {
                let after_priority = matches!(state.waiting_for, WaitingFor::Priority { .. });
                // Same frame selection the published census used, so the two are comparable.
                let verdicts = (after_priority || at_priority).then(|| {
                    let frame = mint_frame(state, beat);
                    let rendered = format!("{}/cleared={}", frame.live, frame.cleared);
                    census.push(frame);
                    rendered
                });
                eprintln!(
                    "[{label}] beat {beat:3} {before:>26} -> {:<26} stack={} ring={} seq={} life={:?} mint={} act={}",
                    wf_label(&state.waiting_for),
                    state.stack.len(),
                    state.loop_detect_ring.len(),
                    state.last_loop_action_sequence.len(),
                    state.players.iter().map(|p| p.life).collect::<Vec<_>>(),
                    verdicts.unwrap_or_else(|| "-".to_string()),
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
        state.last_loop_action_sequence.len(),
        state.players.iter().map(|p| p.life).collect::<Vec<_>>(),
    );
    report_census(label, &census);
    (fired, census)
}

/// The census, reduced and printed in the ISOLATED form: every (1b) refusal is split into the ones
/// that were the SOLE reason this frame raised no offer and the ones a later conjunct refused at
/// anyway. Only the first count is evidence about (1b); the second is dominated and proves nothing
/// about the conjunct it is attributed to.
fn report_census(label: &str, census: &[MintFrame]) {
    let mut tally: std::collections::BTreeMap<(&str, &str), usize> = Default::default();
    for f in census {
        *tally
            .entry((f.live.as_str(), f.cleared.as_str()))
            .or_default() += 1;
    }
    for ((live, cleared), n) in &tally {
        eprintln!("[{label}] CENSUS {n:3} x mint={live} cleared={cleared}");
    }
    let one_b = |load_bearing: bool| {
        census
            .iter()
            .filter(|f| {
                f.live == "ProposerHasDrivingPeriod" && (f.cleared == "OFFER") == load_bearing
            })
            .count()
    };
    eprintln!(
        "[{label}] CENSUS step-(1b) refusals: {} LOAD-BEARING (the cleared twin would have \
         OFFERED) + {} DOMINATED (a later conjunct refuses that frame anyway)",
        one_b(true),
        one_b(false),
    );
}

/// (beat the offer fired at, ring depth, life vector) — the axes the two arms are compared on.
fn offer_signature(state: &GameState, fired: Option<u32>) -> (Option<u32>, usize, Vec<i32>) {
    (
        fired,
        state.loop_detect_ring.len(),
        state.players.iter().map(|p| p.life).collect(),
    )
}

/// ARM D1 — the capture as PRODUCTION loads it: `migrate_transient_loop_sequence` has already
/// dropped the driving sequence, so this is the state every in-process test would see. This is
/// the CONTROL: the offer this board raises with the field cleared is the one ARM D2 must match.
///
/// It also pins the load migration itself against this fixture — the fixture SERIALIZES a period
/// (asserted here from its own JSON) and the loaded board does not carry it. That is what makes
/// ARM D2's re-injection a restoration rather than an invention.
#[test]
fn the_user_captures_offer_is_reached_with_its_driving_period_cleared() {
    let (state, raw_seq) = load_dina_raw();
    eprintln!("[DINA-LOADED] raw serialized sequence = {raw_seq}");
    assert_eq!(
        raw_seq.as_array().map(|a| a.len()),
        Some(1),
        "REACH-GUARD: the tracked fixture must still SERIALIZE the capture's own single recorded \
         step, else ARM D2 has nothing of the user's to put back; got {raw_seq}"
    );
    assert!(
        state.last_loop_action_sequence.is_empty(),
        "reach-guard: the production restore hook must have DROPPED the sequence at load"
    );
    assert!(
        state.may_trigger_auto_choices.is_empty(),
        "reach-guard: the Dina dump carries no may-trigger auto choice (the F4 mechanism \
         cannot apply here)"
    );
    let mut state = state;
    let (fired, _) = dina_drive_and_report(&mut state, "DINA-LOADED", 140);
    eprintln!("[DINA-LOADED] fired={fired:?}");
    assert!(
        fired.is_some(),
        "REACH-GUARD: the field-cleared control must reach the offer, else ARM D2 has nothing \
         to be identical TO and the pair proves nothing"
    );

    // WHICH SITES THIS FIXTURE CAN AND CANNOT HOST, asserted rather than claimed in prose
    // elsewhere. `handle_declare_shortcut`'s `template: None` arm (site F) sits under
    // `if !offer.schema.points.is_empty()`, and the `UntilLethal` drive (site D) is reachable only
    // through an offer that states NO narrowed bound. This capture's offer publishes neither, so it
    // can host neither site — which is why those two rows ride other fixtures. Pinning it here
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
        predicted_winner, &None,
        "this capture reaches the BOUNDED mint (CR 732.2a), not Path A's crowned offer — the \
         whole file is about the bounded mint's step (1b)"
    );
    assert!(
        schema.points.is_empty(),
        "SCOPE: this capture's offer publishes no per-iteration choice point, so site F is \
         STRUCTURALLY unreachable from it; got {:?}",
        schema.points
    );
    assert!(
        schema.is_bounded(),
        "SCOPE: a bounded offer is exactly what makes `handle_declare_shortcut` reject \
         `UntilLethal`, so site D is unreachable from this capture too"
    );
}

/// ARM D2 — the same board with the LIVE sequence put back, i.e. what the running game actually
/// held. Only that one field differs from ARM D1.
///
/// **THE FIX BAR, on the user's own capture.** The recorded step is
/// `Activate { source_id: 268 }` controlled by PlayerId(2) — an OPPONENT'S activation, unrelated
/// to the drain. Before the seat-relative (1b) this board answered `Priority(2)` for all 140
/// beats with a step-(1b) refusal census; after it, the drive must reach the SAME offer
/// the field-cleared control reaches, at the same beat, with the same ring depth and the same
/// life vector — while the foreign step is still sitting in state (`seq` stays at 1). That is
/// "a foreign period is inert", measured end-to-end rather than argued.
///
/// The comparison is against ARM D1 re-driven HERE rather than against transcribed numbers: a
/// hardcoded beat/life tuple would decay into a fixture the next drive-policy change reds for a
/// reason that has nothing to do with this defect.
///
/// **THE PER-FRAME BAR, which the endpoint equality above cannot state.** Two seats propose over
/// this drive, so the recorded period is FOREIGN at some frames and the proposer's OWN at others,
/// and the two shapes carry opposite obligations (CR 732.2a). Each is asserted against the same
/// frame's CLEARED twin — the identical board with only `last_loop_action_sequence` emptied — so
/// no assertion rests on a refusal an earlier or later conjunct would have produced anyway:
/// * FOREIGN frames: the live verdict must EQUAL the cleared verdict. The period changes nothing,
///   which is inertness stated frame by frame rather than only at the endpoint.
/// * OWN frames: the live verdict must be `ProposerHasDrivingPeriod`. That is the load-bearing
///   half of the guard, and it is asserted HERE rather than inferred from a residual count.
///
/// **TWO-SIDED CONTROL, PER ASSERTION** — each direction flips a DIFFERENT named assertion:
/// * **DROP** the seat test in (1b) (restore `!last_loop_action_sequence.is_empty()`) ⇒ every
///   FOREIGN frame answers `ProposerHasDrivingPeriod` while its cleared twin does not ⇒ the
///   FOREIGN-INERTNESS assertion fails (and so does the endpoint fix bar, which stops firing).
/// * **TRIVIALIZE** (1b) to never refuse ⇒ OWN frames answer `ProposerIsNotActivePlayer` ⇒ the
///   OWN-PERIOD assertion fails while FOREIGN-INERTNESS still passes.
///
/// ⚠ **WHAT THE CENSUS IS NOT EVIDENCE FOR.** Round 2 published the residual `ProposerHasDrivingPeriod`
/// count as the guard "working as designed". `report_census` now splits that count by its cleared
/// twin, and on this board every one of those frames is DOMINATED — the proposer there is also not
/// the active player, so conjunct (2) refuses the same frame with the field empty. The residual
/// count is therefore not evidence about (1b); the OWN-PERIOD assertion below and the `ⓑ`/`ⓔ` arms
/// of `a_foreign_driving_period_neither_refuses_nor_recertifies_a_bounded_offer` are.
#[test]
fn the_user_captures_offer_is_reached_with_its_own_foreign_period_live() {
    let (mut state, raw_seq) = load_dina_raw();
    state.last_loop_action_sequence =
        serde_json::from_value(raw_seq.clone()).expect("the dump's own sequence re-parses");
    assert_eq!(
        state.last_loop_action_sequence.len(),
        1,
        "reach-guard: the re-injected live sequence must be the dump's own single step"
    );
    let foreign = state.last_loop_action_sequence[0].controller;
    eprintln!("[DINA-LIVE] re-injected {raw_seq}");

    let (fired, census) = dina_drive_and_report(&mut state, "DINA-LIVE", 140);
    eprintln!("[DINA-LIVE] fired={fired:?}");

    // The CONTROL, re-driven in this process: same board, the one field cleared.
    let (mut control, _) = load_dina_raw();
    control.last_loop_action_sequence.clear();
    let (control_fired, _) = dina_drive_and_report(&mut control, "DINA-CONTROL", 140);

    // IN-ROW REACH-GUARD, not inherited from ARM D1's: the equality below compares this arm to a
    // control re-driven HERE, so on a board where NEITHER side reaches an offer both sides are
    // `(None, (None, ring, life))` and the assertion passes having measured nothing. ARM D1's own
    // guard cannot cover that — a `#[test]` that is skipped, filtered, or reds independently
    // leaves this row still "green". The control must PROVABLY reach the offer in this process.
    assert!(
        control_fired.is_some(),
        "REACH-GUARD: the field-cleared control re-driven in THIS row must reach the offer, else \
         the fix bar below compares two absences and passes vacuously"
    );

    // ── THE PER-FRAME BAR, asserted BEFORE the endpoint one on purpose: it is the finer
    //    instrument, and under the DROP mutant the endpoint bar would otherwise panic first and
    //    leave FOREIGN-INERTNESS unobserved. Reach-guards first: both frame shapes must actually
    //    occur, and the instrument must demonstrably be able to answer more than one way. ──
    let (own, foreign_frames): (Vec<&MintFrame>, Vec<&MintFrame>) = census
        .iter()
        .filter(|f| f.proposer.is_some())
        .filter(|f| f.relation != PeriodRelation::AbsentOrHeterogeneous)
        .partition(|f| f.relation == PeriodRelation::Own);
    let distinct: std::collections::BTreeSet<&str> =
        census.iter().map(|f| f.live.as_str()).collect();
    assert!(
        distinct.len() >= 2,
        "REACH-GUARD: a mint that answered one constant across the whole drive would satisfy both \
         assertions below without discriminating anything; got {distinct:?}"
    );
    assert!(
        !foreign_frames.is_empty(),
        "REACH-GUARD: no beat had a seat OTHER than {foreign:?} proposing, so FOREIGN-INERTNESS \
         below quantifies over an empty set and passes having measured nothing"
    );
    assert!(
        !own.is_empty(),
        "REACH-GUARD: no beat had {foreign:?} — the seat that recorded the period — proposing, so \
         the OWN-PERIOD assertion below quantifies over an empty set. This board reaches both \
         shapes; if it stops doing so the arm must move, not soften"
    );

    let diverged: Vec<_> = foreign_frames
        .iter()
        .filter(|f| f.live != f.cleared)
        .map(|f| (f.beat, f.proposer, f.live.as_str(), f.cleared.as_str()))
        .collect();
    assert!(
        diverged.is_empty(),
        "CR 732.2a FOREIGN-INERTNESS, per frame: at every beat where the recorded period belongs \
         to a seat OTHER than the proposer, the mint must return exactly what it returns with the \
         field empty — a period recorded by another seat describes no sequence this proposer can \
         take, so it may not change their verdict. {} of {} foreign frames diverged: {diverged:?}",
        diverged.len(),
        foreign_frames.len()
    );

    let leaked: Vec<_> = own
        .iter()
        .filter(|f| f.live != "ProposerHasDrivingPeriod")
        .map(|f| (f.beat, f.proposer, f.live.as_str(), f.cleared.as_str()))
        .collect();
    assert!(
        leaked.is_empty(),
        "CR 732.2a OWN-PERIOD: at every beat where the recorded period is the PROPOSER'S OWN, \
         step (1b) must refuse — an offer minted there would be accepted and routed to the \
         object-growth materializer, committing ZERO bounded cycles. {} of {} own frames did not: \
         {leaked:?}",
        leaked.len(),
        own.len()
    );

    // ── THE ENDPOINT BAR: the whole trajectory, not one frame. ──
    let (proposer, control_proposer) = (offer_proposer(&state), offer_proposer(&control));
    assert_ne!(
        Some(foreign),
        control_proposer,
        "REACH-GUARD: the recorded step must belong to a seat OTHER than the proposer, else \
         this arm is measuring the legitimate own-period case"
    );
    assert_eq!(
        (proposer, offer_signature(&state, fired)),
        (control_proposer, offer_signature(&control, control_fired)),
        "CR 732.2a THE FIX BAR: with an opponent's recorded activation sitting in state, the \
         proposer's own bounded offer must be reached at the same beat, with the same ring \
         depth and the same life vector, as the field-cleared control. A `None` on the left is \
         the original defect: one foreign step suppressing the offer for the whole drive"
    );
    assert_eq!(
        state.last_loop_action_sequence.len(),
        1,
        "and the foreign step is still THERE — the offer was reached with it in state, not by \
         the drive quietly clearing it"
    );
}

fn offer_proposer(state: &GameState) -> Option<engine::types::player::PlayerId> {
    match &state.waiting_for {
        WaitingFor::LoopShortcut { proposer, .. } => Some(*proposer),
        _ => None,
    }
}
