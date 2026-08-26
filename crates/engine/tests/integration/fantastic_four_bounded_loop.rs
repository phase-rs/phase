//! 5d U5 — the Fantastic Four bounded-loop acceptance rows, driven from the REAL 4-player
//! playtest dump through the production `apply()` boundary.
//!
//! This module is the first commit that TRACKS
//! `crates/engine/tests/fixtures/fantastic_four_bounded_loop_4p.json.gz`; it ships with its
//! loader in the same change (a tracked fixture with no tracked loader is residue).
//!
//! # The board (CR 732.2a)
//!
//! Four Fantastic Four permanents, all P0-controlled, chained into one self-sustaining cycle:
//!
//! * **Human Torch, Johnny Storm** (`403`) — *"Whenever you draw a card, if you control another
//!   Hero, ~ deals 1 damage to target opponent."* — a CR 608.2b TARGET choice, three legal
//!   opponents.
//! * **The Thing, Ben Grimm** (`404`) — mandatory `PutCounter`, no choice.
//! * **Invisible Woman, Sue Storm** (`402`) — an `optional` (CR 603.5 "may") token creation.
//! * **Mister Fantastic, Reed Richards** (`401`) — *"Whenever one or more tokens you control
//!   enter, you may draw a card."* — a second CR 603.5 "may", whose draw re-triggers Torch.
//!
//! Per cycle: P1 loses 1 life, P0's library loses 1 card, two `+1/+1` counters are added.
//!
//! # MEASURED SCOPE OF THIS MODULE — read this before adding a row
//!
//! The bounded offer FIRES on this dump (that is 5d's headline and [`r1_the_bounded_offer_fires_
//! on_the_real_f4_dump`] is the row). It publishes **all three** per-iteration choices this
//! cycle opens — Sue's `MayChoice`, Reed's `MayChoice` and Torch's `Targets` slot — and the
//! mechanism is measured and pinned by
//! [`r1b_the_published_point_set_is_exactly_what_the_retained_window_announces`].
//!
//! ⚠ **THE PREVIOUS PARAGRAPH SAID THE OPPOSITE, AND IT WAS A MEASUREMENT OF A BLIND SPOT, NOT
//! OF THE BOARD.** The CR 732.2a ring sampler used to fire only at `Priority { player ==
//! active_player }` after a non-shrinking resolution, so on this board the retained frames
//! alternated strictly between the `404` and `402` stack entries; `certified_period_touch`'s
//! `announced` set is "entries in a frame's stack that were absent from the previous frame's",
//! which made the `403` and `401` entries structurally invisible to conjunct (6) and to
//! `bounded_cycle_pin_slots_for_window`. Torch and Reed resolve ACROSS a forced pre-priority
//! window, and that window is exactly what the old site could not see. The second sampling site
//! records a frame at the beat such a window is **ANSWERED**, so those two entries are now
//! announced like the other two — a widening of what the offer can publish, not a change to
//! what the board does.
//!
//! CONSEQUENCE, also measured and pinned
//! ([`r2a_an_accepted_declaration_commits_exactly_n_cycles_because_reeds_may_is_announced`]): an
//! accepted `Fixed(n)` declaration carrying the full published pin set now **commits exactly
//! `n` repetitions** — P1 loses `n` life, P0's library loses `n` cards — and `n = 1` and `n = 3`
//! are DISTINGUISHABLE. The former zero-commit was the fail-closed abort on Reed's unpinned
//! "may"; with Reed published there is nothing left to abort on.

use engine::analysis::decision_template::{DecisionKind, DecisionPointKind, IterationCount};
use engine::analysis::resource::ResourceVector;
use engine::game::engine::apply;
use engine::types::ability::{ReplacementMode, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, PersistedGameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

/// The four F4 permanents, by their **comma printings** — verified verbatim against the card
/// faces in the dump itself (`objects[401..404].name`). The plain names ("Mister Fantastic",
/// "Human Torch", …) are DIFFERENT cards with different text.
const REED: &str = "Mister Fantastic, Reed Richards";
const SUE: &str = "Invisible Woman, Sue Storm";
const TORCH: &str = "Human Torch, Johnny Storm";
const THING: &str = "The Thing, Ben Grimm";

/// `game::engine::MAX_SHORTCUT_CYCLES`, mirrored because it is `pub(crate)` and this binary
/// cannot name it. Only ever used as the "the bound was NARROWED" ceiling; the row's real
/// assertion is the re-derived arithmetic below it, so a drift in the constant cannot make the
/// row pass wrongly — it can only weaken the ceiling half.
const MAX_SHORTCUT_CYCLES_MIRROR: u32 = 1_000;

fn gunzip(gz: &[u8]) -> String {
    use std::io::Read;
    let mut json = String::new();
    flate2::read::GzDecoder::new(gz)
        .read_to_string(&mut json)
        .expect("fixture .json.gz must inflate to UTF-8 JSON");
    json
}

/// Load the tracked F4 dump's `["gameState"]` through the REAL production restore chokepoint
/// `PersistedGameState::into_game_state` (both the server's `from_persisted` and WASM's
/// `decode_restored_game_state` funnel through it) — never a bare `GameState` decode, which
/// would skip `reject_legacy_raw_prompt_authority` and `decode_persisted_resolution_state`.
///
/// The chokepoint now rehydrates the `#[serde(skip)]` ChaCha20 stream, which it did not always:
/// the repair lived only in `engine-wasm`'s `restore_game_state`, so a load that ENDED at the
/// chokepoint — as `load_f4` does — left the live stream rewound to word 0 under a saved
/// `rng_word_pos` of 379. Every caller now inherits it and WASM's own call became an idempotent
/// repeat. It does NOT make the shipped load paths identical: `server-core`'s
/// `GameSession::from_persisted` re-seeds afterwards and zeroes `rng_word_pos` with it, so the
/// server deliberately DISCARDS the saved position instead of resuming it as `load_f4` does.
///
/// The dump was captured with the detector OFF; every row here is about the CR 732.2a
/// interactive offer, so the mode is set to `Interactive` at load — the same thing the user's
/// own toggle does.
fn load_dump(gz: &[u8]) -> GameState {
    let json = gunzip(gz);
    let envelope: serde_json::Value =
        serde_json::from_str(&json).expect("dump envelope parses as JSON");
    let mut state = serde_json::from_value::<PersistedGameState>(envelope["gameState"].clone())
        .expect("gameState deserializes through the production decoder")
        .into_game_state();
    state.loop_detection = engine::types::game_state::LoopDetectionMode::Interactive;
    state
}

fn load_f4() -> GameState {
    load_dump(include_bytes!(
        "../fixtures/fantastic_four_bounded_loop_4p.json.gz"
    ))
}

/// **MODE1** — the user's own 2026-08-03 capture of the board that raised NO offer at all
/// (`fastastic-four-no-offer-phase5.zip`, `game-state-turn-5-…19-09-15-030Z.json`), derived by
/// `jq -c '{gameState}' … | gzip -9 -n` (860,451 B; the raw envelope is 20.5 MB, of which
/// `turnCheckpoints` alone is 16.4 MB and no loader reads it).
///
/// Its distinguishing field is `may_trigger_auto_choices`: it carries the user's stored
/// "always take" for Sue's CR 603.5 `may`, so guard (b) withholds that pin slot and gate (6)
/// can only be discharged by the CR 603.5 auto-answer relief.
fn load_mode1() -> GameState {
    load_dump(include_bytes!(
        "../fixtures/f4_user_mode1_no_offer_4p.json.gz"
    ))
}

/// **MODE2** — the user's own 2026-08-03 capture of the board where the offer DID fire, the
/// declaration WAS accepted, and the drive then committed **nothing** and re-offered
/// (`f4-offer-fires-no-ff.zip`, `game-state-turn-5-…19-56-54-597Z.json`), derived by the same
/// `jq -c '{gameState}' … | gzip -9 -n` (971,617 B).
///
/// Its distinguishing field is the COMPLEMENT of MODE1's: `may_trigger_auto_choices` is EMPTY
/// (the user cleared the "always take" as a workaround), so this board reaches the offer
/// through the ordinary CR 603.5 publication path — and the accepted grant aborted on a `may`
/// the offer had not published. The two dumps are therefore one field apart on the axis this
/// change is about, which is why both are tracked.
fn load_mode2() -> GameState {
    load_dump(include_bytes!(
        "../fixtures/f4_user_mode2_accept_commits_nothing_4p.json.gz"
    ))
}

/// The four axes ONE committed cycle of this loop moves: every seat's life, every seat's
/// library size, The Thing's counters, and the token population.
///
/// All four, not one: a commit that moved only life could be a stray drain, while a commit
/// that moves all four is the CYCLE. `u32::MAX` for a missing Thing is deliberate — an absent
/// permanent must fail an equality loudly rather than read as "zero counters".
fn commit_axes(state: &GameState) -> (Vec<i32>, Vec<usize>, u32, usize) {
    let thing = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .find(|o| o.name == THING)
        .map(|o| o.counters.values().copied().sum::<u32>())
        .unwrap_or(u32::MAX);
    let tokens = state
        .battlefield
        .iter()
        .filter(|id| state.objects.get(id).is_some_and(|o| o.is_token))
        .count();
    (
        state.players.iter().map(|p| p.life).collect(),
        state.players.iter().map(|p| p.library.len()).collect(),
        thing,
        tokens,
    )
}

/// Every living opponent Accepts the CR 732.2c window, returning how many did. A zero return
/// means the window never opened, which every caller turns into a loud failure.
fn accept_all_opponents(state: &mut GameState) -> usize {
    use engine::analysis::loop_check::ShortcutResponse;
    let mut responders = 0;
    while let WaitingFor::RespondToShortcut { player, .. } = state.waiting_for.clone() {
        apply(
            state,
            player,
            GameAction::RespondToShortcut {
                response: ShortcutResponse::Accept,
            },
        )
        .expect("each living opponent accepts (CR 732.2c)");
        responders += 1;
    }
    responders
}

/// R18 / §3 D6 TARGET A — resolve a fixture object by CARD NAME, never by literal `ObjectId`.
///
/// The user has announced a re-dump of this same board (*"I will then provide a new F4 .zip
/// game state"*), and a fresh dump RENUMBERS every `ObjectId`. A silent first-match would then
/// bind the acceptance rows to the wrong object and still go green; this fails LOUD on both
/// ambiguity and absence instead. [`r18_the_name_resolver_fails_loud_in_both_directions`] is
/// the row that proves it.
fn resolve_by_name(state: &GameState, name: &str) -> ObjectId {
    let hits: Vec<ObjectId> = state
        .battlefield
        .iter()
        .filter(|id| state.objects.get(id).is_some_and(|o| o.name == name))
        .copied()
        .collect();
    match hits.as_slice() {
        [one] => *one,
        [] => panic!("fixture name resolution: NO battlefield object named {name:?}"),
        many => panic!(
            "fixture name resolution: AMBIGUOUS — {} battlefield objects named {name:?}: {many:?}",
            many.len()
        ),
    }
}

/// One beat of the F4 drive policy, every beat crossing the public `apply()` boundary.
///
/// At `Priority` ALWAYS pass: the mandatory chain resolves and re-triggers, and that IS the
/// loop — casting here wanders off it. At Torch's CR 608.2b target choice aim `seat` (a
/// CONSTANT seat, so the cycle is board-stable and the detector can certify it); at either
/// CR 603.5 "may" prompt TAKE (declining Sue's token breaks the chain to Reed).
///
/// The aimed seat is a PARAMETER, not a constant, so a row can prove the journal FOLLOWS the
/// announcement instead of coinciding with one hard-coded seat. MEASURED: constant P1,
/// constant P2 and constant P3 all certify and reach the offer; it is the VARIATION between
/// iterations, not the seat, that blocks certification.
///
/// ⚠ This is deliberately NOT `loop_shortcut.rs`'s shared `dump_drive_one_beat`: that helper's
/// victim preference matches `GameAction::SelectTargets`, and this dump raises
/// `GameAction::ChooseTarget`, so its pin is inert here and its "first legal non-terminal
/// action" fallback answers Sue's "may" with whichever `DecideOptionalEffect` is enumerated
/// first. MEASURED: under that policy this dump reaches no offering beat at all.
fn f4_drive_one_beat(state: &mut GameState) -> Result<(), String> {
    f4_drive_one_beat_at(state, P1)
}

fn f4_drive_one_beat_at(state: &mut GameState, seat: PlayerId) -> Result<(), String> {
    let who = state
        .waiting_for
        .acting_player()
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
            .find(|a| {
                matches!(
                    a,
                    GameAction::ChooseTarget { target: Some(TargetRef::Player(p)) } if *p == seat
                )
            })
            .or_else(|| {
                actions
                    .iter()
                    .find(|a| matches!(a, GameAction::DecideOptionalEffect { accept: true }))
            })
            .cloned()
    };
    let action = chosen.ok_or_else(|| {
        format!(
            "the F4 policy answers every beat this drive reaches; unhandled {:?}",
            state.waiting_for
        )
    })?;
    apply(state, who, action.clone())
        .map(|_| ())
        .map_err(|e| format!("apply err ({action:?}): {e:?}"))
}

/// Drive the loaded dump until the ENGINE raises the CR 732.2a bounded offer, returning that
/// beat index. The beat is SEARCHED, never hardcoded — a hardcoded index is a fixture that
/// drifts silently when the drive policy moves.
fn drive_f4_to_offer(state: &mut GameState, cap: u32) -> Option<u32> {
    drive_f4_to_offer_at(state, cap, P1)
}

fn drive_f4_to_offer_at(state: &mut GameState, cap: u32, seat: PlayerId) -> Option<u32> {
    for beat in 0..cap {
        if matches!(state.waiting_for, WaitingFor::LoopShortcut { .. }) {
            return Some(beat);
        }
        f4_drive_one_beat_at(state, seat).ok()?;
    }
    None
}

fn offer_parts(
    state: &GameState,
) -> (
    PlayerId,
    &engine::analysis::loop_check::LoopCertificate,
    &engine::analysis::decision_template::ShortcutDecisionSchema,
) {
    match &state.waiting_for {
        WaitingFor::LoopShortcut {
            proposer,
            certificate,
            schema,
            ..
        } => (*proposer, certificate, schema),
        other => panic!("expected the CR 732.2a bounded offer, got {other:?}"),
    }
}

/// Build the CONFORMANT declaration template for a published schema: one pin per published
/// point, `owner` and `count` supplied by the caller.
///
/// This is the shape `handle_declare_shortcut` ACCEPTS (measured in
/// [`u6_the_generators_own_candidate_opens_the_window_and_the_accepted_shape_is_measured`]),
/// so every row that needs either an accepted declaration or a one-axis hostile variant of one
/// builds it here rather than re-deriving the mapping. Keyed off `schema.points` — never off a
/// hard-coded slot — so a re-dump that renumbers objects, or a remedy that widens the announced
/// set, flows through without edit.
///
/// The per-kind mapping is deliberately total and LOUD on the kinds F4 cannot produce: a
/// silently-skipped point would build a template that `predictability_gate` refuses, and the
/// refusal would be read as the row's subject rather than as the fixture's own gap.
fn f4_pin_template(
    schema: &engine::analysis::decision_template::ShortcutDecisionSchema,
    owner: PlayerId,
    count: u32,
) -> engine::analysis::decision_template::DecisionTemplate {
    use engine::analysis::decision_template::{
        AnnouncementSubject, DecisionGroupKey, DecisionTemplate, MayChoiceOption, PinnedDecision,
        Ranking, ReplayMode, TargetPin, TargetSchedule,
    };
    DecisionTemplate {
        owner,
        decisions: schema
            .points
            .iter()
            .map(|p| match &p.kind {
                DecisionPointKind::MayChoice => PinnedDecision::MayChoice {
                    slot: p.slot.clone(),
                    take: MayChoiceOption::Take,
                },
                // CR 603.3d + CR 608.2b: F4's only target point is Torch's "target opponent",
                // chosen when the trigger goes on the stack and re-checked for legality at
                // each resolution. P1 is the constant seat `f4_drive_one_beat` aims at and is
                // living on this board, so the pin stays legal for every driven cycle.
                //
                // CR 601.2c: "target opponent" makes this an ANNOUNCED target, so the
                // reference spells the TARGET class — a one-entry `Ranking` naming the seat —
                // and not the CR 115.10a `TargetPin::Player` choice class. This literal is
                // the conformance oracle row D1 compares the live publisher against, so it
                // has to track the publisher's spelling exactly.
                DecisionPointKind::Targets { .. } => PinnedDecision::Targets {
                    slot: p.slot.clone(),
                    targets: vec![TargetPin::Scheduled(TargetSchedule::Constant(
                        Ranking::one(AnnouncementSubject::Seat(P1)),
                    ))],
                },
                other => panic!("unexpected point kind {other:?}"),
            })
            .collect(),
        replay: ReplayMode::Scheduled {
            count: IterationCount::Fixed(count),
        },
        key: DecisionGroupKey::from_sources(
            &schema
                .points
                .iter()
                .map(|p| p.slot.source.clone())
                .collect::<Vec<_>>(),
            DecisionKind::LoopChoice,
        ),
    }
}

/// Restore the `Priority` window the reconcile bridge consumed when it raised the offer, so
/// the mint can be re-run on the offer beat's OWN board. Every caller proves the
/// reconstruction faithful by requiring the same outcome the production path produced.
fn replay_at_priority(state: &GameState, proposer: PlayerId) -> GameState {
    let mut replay = state.clone();
    replay.waiting_for = WaitingFor::Priority { player: proposer };
    replay
}

// ─────────────────────────────────────────────────────────────────────────────────────────
// C0 — the tracked F4 dump's RNG stream survives the restore chokepoint
// ─────────────────────────────────────────────────────────────────────────────────────────

/// **Row 9, tracked-loader arm.** The RNG chokepoint gap was never confined to the untracked Dina
/// board: this TRACKED dump carries `rng_word_pos = 379` and used to restore with the live stream
/// at word 0, so the very next export-time `capture_rng_word_pos` panicked
/// `HighWaterRegression { current: 379, requested: 0 }`. Every row that loads an F4 board loads
/// through `load_f4`, so the gap sat under all of those. NOT literally every row here: `b5f_` and
/// `m1_` load through `load_mode1`, `a1_` through `load_mode2`, and `c1_` loads no board at all
/// (it walks source). Stated as loaders rather than as a count, because a count rots on the next
/// row added and this sentence has already been false once. Scope: this row measures the CHOKEPOINT's
/// postcondition, which is not every shipped ingress's postcondition — `server-core`'s
/// `GameSession::from_persisted` re-seeds after the chokepoint and zeroes `rng_word_pos` with it,
/// ending at an agreed live-0 / high-water-0 pair rather than at this row's resumed position.
///
/// Two-sided on one axis, like its Dina sibling: the restored stream is AT the high-water and the
/// capture is legal; the same board with the live position rewound to 0 — the exact pre-fix decode
/// state — still panics (`c0_the_unrehydrated_tracked_f4_dump_still_panics`). Revert-probe:
/// deleting `state.rehydrate_rng()` from `PersistedGameState::into_game_state` reds the
/// `get_word_pos() == rng_word_pos` assertion with `0 != 379`.
#[test]
fn c0_the_tracked_f4_dump_restores_a_coherent_rng_stream() {
    let mut state = load_f4();

    // Reach-guard: the real board, carrying a NON-ZERO saved high-water.
    assert_eq!(state.players.len(), 4, "the real 4p board must have loaded");
    assert_eq!(
        state.rng_word_pos, 379,
        "the tracked F4 dump's captured ChaCha20 high-water",
    );
    assert_eq!(
        state.rng.get_word_pos(),
        state.rng_word_pos,
        "into_game_state must fast-forward the live stream on the TRACKED dump too",
    );

    state.capture_rng_word_pos();
    assert_eq!(
        state.rng_word_pos, 379,
        "a capture at the restored position must not move the high-water",
    );
}

/// The negative control for the row above: without the rehydrate the same board panics.
#[test]
#[should_panic(expected = "HighWaterRegression")]
fn c0_the_unrehydrated_tracked_f4_dump_still_panics() {
    let mut state = load_f4();
    state.rng.set_word_pos(0);
    state.capture_rng_word_pos();
}

// ─────────────────────────────────────────────────────────────────────────────────────────
// R18 — fail-loud fixture name resolution
// ─────────────────────────────────────────────────────────────────────────────────────────

/// §6 R18 (a)/(b)/(c) — the resolver every acceptance row's identity flows through.
///
/// * **(c) the paired positive reach-guard, asserted FIRST**: on the UNMODIFIED dump all four
///   comma printings resolve, to four DISTINCT `ObjectId`s. Without this, (a)/(b) could pass
///   over a resolver that never resolves anything.
/// * **(a)** two battlefield objects sharing the resolved name ⇒ PANIC, not first-match.
/// * **(b)** zero matches ⇒ PANIC, not a `None`-swallow.
///
/// REVERT-PROBES (both RUN, see the journal): replace the unique-match with a `.find(..)`
/// first-match ⇒ (a) stops panicking ⇒ FLIPS; delete the empty-slice panic arm ⇒ (b) FLIPS.
#[test]
fn r18_the_name_resolver_fails_loud_in_both_directions() {
    use std::panic::{catch_unwind, AssertUnwindSafe};

    let state = load_f4();

    // ── (c) the anti-vacuity leg: four printings, four DISTINCT ids ──
    let ids: Vec<ObjectId> = [REED, SUE, TORCH, THING]
        .iter()
        .map(|n| resolve_by_name(&state, n))
        .collect();
    let mut sorted = ids.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        4,
        "(c) the unmodified F4 dump must resolve all four comma printings to four DISTINCT \
         ObjectIds — otherwise (a)/(b) are asserted over a resolver that never resolves \
         anything; got {ids:?}"
    );

    // ── (a) AMBIGUITY ⇒ panic. A second battlefield object is given Torch's exact name; the
    //    id-literal precedent would have silently taken the first. ──
    let ambiguous = {
        let mut s = state.clone();
        let clone_target = *s
            .battlefield
            .iter()
            .find(|id| **id != ids[2])
            .expect("the dump's battlefield holds more than one permanent");
        s.objects
            .get_mut(&clone_target)
            .expect("battlefield ids index live objects")
            .name = TORCH.to_string();
        s
    };
    let ambiguous_err = catch_unwind(AssertUnwindSafe(|| resolve_by_name(&ambiguous, TORCH)))
        .expect_err(
            "(a) CR-neutral fixture hygiene: two battlefield objects sharing the resolved name \
             must PANIC, not silently first-match — a re-dump that duplicates a name would \
             otherwise bind the acceptance rows to the wrong object and still go green",
        );
    assert!(
        panic_message(&ambiguous_err).contains("AMBIGUOUS"),
        "(a) the panic must NAME the failure mode so a re-dump reads as a fixture problem, \
         got {:?}",
        panic_message(&ambiguous_err)
    );

    // ── (b) ABSENCE ⇒ panic. ──
    let absent_err = catch_unwind(AssertUnwindSafe(|| {
        resolve_by_name(&state, "Doctor Doom, Victor Von Doom")
    }))
    .expect_err("(b) a name with zero battlefield matches must PANIC, not be swallowed");
    assert!(
        panic_message(&absent_err).contains("NO battlefield object"),
        "(b) the panic must name the failure mode, got {:?}",
        panic_message(&absent_err)
    );
}

fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_string()))
        .unwrap_or_default()
}

// ─────────────────────────────────────────────────────────────────────────────────────────
// R1 — the offer fires on the real dump, with an independently re-derived bound
// ─────────────────────────────────────────────────────────────────────────────────────────

/// §6 R1 — the CR 732.2a bounded offer FIRES on the REAL 4-player F4 dump, driven through
/// `apply()`, and its `max_iterations` equals the bound re-derived by this row from the
/// offer-beat board.
///
/// **STATUS: §6 R1's other half is now MEASURED TRUE, in two sibling rows.** R1 as planned also
/// expected the offer to publish three decision points and to be TAKEABLE (commit ≥ 1 cycle).
/// ⚠ THE NOTE THAT STOOD HERE — *"measured on this tree it publishes ONE point and commits ZERO
/// cycles (see `r1b` and `r2`)"* — IS FALSIFIED by this branch's own rows, and is replaced
/// rather than softened:
///
/// * [`r1b_the_published_point_set_is_exactly_what_the_retained_window_announces`] pins
///   **THREE** points — `[Sue MayChoice, Reed MayChoice, Torch Targets]` — not one;
/// * [`r2a_an_accepted_declaration_commits_exactly_n_cycles_because_reeds_may_is_announced`]
///   commits **exactly `n`**, run at `n = 1` and `n = 3` so the two outcomes are
///   distinguishable — not zero;
/// * the row that measured zero was `r2_an_accepted_declaration_commits_zero_cycles_…`, and it
///   NO LONGER EXISTS. This branch RENAMED it to `r2a_…` once the answer-beat sampler announced
///   the frame Reed's entry sits on, which removed the unannounced `may` the zero-commit was
///   fail-closing on. Any surviving cross-reference to `r2` resolves to nothing.
///
/// This row keeps the half it always owned — the offer fires, and its bound arithmetic is
/// correct. `r2b`/`r4`/`r5` and the interruptibility pair are still unwritten: no `fn r2b_`,
/// `fn r4_` or `fn r5_` row exists in this file, and `interruptibility` appears nowhere in it
/// outside this sentence. **`r3` IS written** —
/// `fn r3_placement_a_restored_foreign_owner_declaration_is_refused`, added by the same commit
/// that made a template-free declaration resolve against the offer's published declaration. That
/// commit is why this sentence needed repairing at all: it was measured true when written, and a
/// row added later falsified it silently, because no sweep in this lane reads prose under
/// `crates/engine/`. Locate the row by NAME — this is a measurement of one tree, not a standing
/// property, and the next row added re-opens it.
///
/// # What the assertion is bound to, and why it is not `f(x) == f(x)`
///
/// The expectation is computed HERE from (i) each living seat's life and library on the
/// offer-beat board and (ii) the per-period delta the ENGINE published on the certificate — it
/// never calls `elimination_bounds`, which is the function under test. ⚠ THE ANCHOR THAT STOOD
/// HERE — *"anchored to the in-tree MAX form … the additive per-victim form is a tracked
/// follow-up (R1-fu), not a prerequisite … measured on this board `victim_slot` is EMPTY, so the
/// two forms coincide"* — IS FALSIFIED ON BOTH CLAUSES. The in-tree form IS the additive one
/// (`resource.rs` `observed_life_loss.max(0) + declared_life_magnitude` under the
/// `declarable_victims` guard), and `victim_slot` is NON-EMPTY on this board, so the two forms
/// do NOT coincide here — which is why this row's own assertion message states the additive form
/// it assumes, and names what actually remains tracked as F1: the additive form OVER-CHARGES
/// wherever a published slot IS the observed drain.
///
/// # Reach-guards (each excludes a way this could pass degenerately)
///
/// * the pre-offer beats really ran the cycle — P1's life FELL and P0's library SHRANK;
/// * the published per-period delta is non-zero on both axes, so the division is not by zero
///   and the `min` is not taken over an empty set;
/// * the bound is NARROWED (`< MAX_SHORTCUT_CYCLES`), so the row is not satisfied by the
///   unnarrowed default every pre-bounded offer carries.
#[test]
fn r1_the_bounded_offer_fires_on_the_real_f4_dump() {
    let mut state = load_f4();
    let life_before: Vec<i64> = state.players.iter().map(|p| p.life as i64).collect();
    let libs_before: Vec<usize> = state.players.iter().map(|p| p.library.len()).collect();

    let beat = drive_f4_to_offer(&mut state, 400).expect(
        "CR 732.2a: the bounded offer must FIRE on this real 4p board. A failure here is the \
         offer never being raised, not a fixture accident — the pre-5d baseline drove 400 \
         beats on this same dump and reached zero LoopShortcut beats",
    );
    let (proposer, certificate, schema) = offer_parts(&state);

    assert_eq!(
        proposer, P0,
        "the proposer is the seat holding priority in the cycle it controls"
    );

    // ── reach-guard: the cycle really ran before the offer ──
    let life_now: Vec<i64> = state.players.iter().map(|p| p.life as i64).collect();
    let libs_now: Vec<usize> = state.players.iter().map(|p| p.library.len()).collect();
    assert!(
        life_now[1] < life_before[1] && libs_now[0] < libs_before[0],
        "reach-guard: the pre-offer beats must show the cycle RUNNING (P1 life falls, P0 \
         library shrinks). life {life_before:?} -> {life_now:?}, libs {libs_before:?} -> \
         {libs_now:?} over {beat} beats"
    );

    let per_cycle = certificate
        .per_cycle
        .clone()
        .expect("a bounded offer publishes the per-period signature its bound was divided by");
    let life_loss_p1 = -per_cycle.delta.life.get(&P1).copied().unwrap_or(0);
    let library_drain_p0 = -per_cycle.delta.library_delta.get(&P0).copied().unwrap_or(0);
    assert!(
        life_loss_p1 > 0 && library_drain_p0 > 0,
        "reach-guard: both live axes must carry a strictly positive per-cycle consumption, \
         else the divisions below are vacuous; delta {:?}",
        per_cycle.delta
    );

    // ── the expectation, re-derived independently of `elimination_bounds` ──
    // CR 704.5a headroom is `life - 1`: a seat at exactly 0 has LOST, so a legal shortcut must
    // stop one point above it. CR 104.3c: an empty library is only lethal on the next draw, so
    // the library axis divides the whole remaining library.
    // CR 704.5a: a published re-aimable `Targets` slot may be pointed at ANY of its legal
    // player targets in EVERY remaining repetition, so each of them is charged that slot's
    // magnitude ON TOP of its own observed drain. Both terms come off the offer's OWN
    // published data — `certificate.per_cycle.victim_slot` and `schema.points` — never from
    // `elimination_bounds`, so this stays an independent re-derivation.
    let declared_life_magnitude: i64 = per_cycle
        .victim_slot
        .iter()
        .map(|(_, m)| *m)
        .filter(|m| *m > 0)
        .sum();
    let declarable_victims: std::collections::BTreeSet<PlayerId> = schema
        .points
        .iter()
        .filter_map(|p| match &p.kind {
            DecisionPointKind::Targets { legal_targets, .. } => Some(legal_targets),
            _ => None,
        })
        .flatten()
        .filter_map(|t| match t {
            TargetRef::Player(p) => Some(*p),
            _ => None,
        })
        .collect();
    let mut bounds: Vec<i64> = vec![];
    for player in state.players.iter().filter(|p| !p.is_eliminated) {
        let observed = -per_cycle.delta.life.get(&player.id).copied().unwrap_or(0);
        let loss = if declarable_victims.contains(&player.id) {
            observed.max(0) + declared_life_magnitude
        } else {
            observed
        };
        if loss > 0 {
            bounds.push((player.life as i64 - 1) / loss);
        }
        let drain = -per_cycle
            .delta
            .library_delta
            .get(&player.id)
            .copied()
            .unwrap_or(0);
        if drain > 0 {
            bounds.push(player.library.len() as i64 / drain);
        }
    }
    let expected = bounds
        .iter()
        .copied()
        .min()
        .expect("at least one consumed axis, guaranteed by the reach-guard above")
        .clamp(0, i64::from(MAX_SHORTCUT_CYCLES_MIRROR));

    assert_eq!(
        i64::from(schema.max_iterations),
        expected,
        "CR 732.2a + CR 704.5a: `max_iterations` is the MIN over every living seat's \
         elimination headroom, divided by the per-period consumption the certificate itself \
         published, PLUS the published `victim_slot` magnitude charged to every declarable \
         victim. Re-derived here as {bounds:?} -> {expected} with declared={declared_life_magnitude} \
         over victims {declarable_victims:?}; the offer published {}. (The additive per-victim \
         form is now BOTH the in-tree form and this re-derivation, because `victim_slot` is \
         non-empty on this board for the first time. It is NOT the follow-up discharged: the \
         same additive form OVER-CHARGES wherever a published slot IS the observed drain — \
         MEASURED one life point wide by the B5f pair — and that remains tracked as F1.)",
        schema.max_iterations
    );
    assert!(
        schema.max_iterations < MAX_SHORTCUT_CYCLES_MIRROR,
        "the bound must be NARROWED, else this row is satisfied by the unnarrowed default \
         every pre-bounded offer carries"
    );
}

/// **Row R2-a — REAL DUMP.** The MAINTAINED-INVARIANT row for the provenance split: after both
/// TARGET-class producers moved to the ranked spelling, this real 4p board still fires its
/// CR 732.2a bounded offer, still publishes a `Some` declaration, still carries the same bound —
/// and Torch's `Targets` pin is now the CR 601.2c TARGET-class spelling.
///
/// # The two halves, and why one without the other is worthless
///
/// `declaration.is_some()` alone passes on the OLD spelling, so it cannot see the migration at
/// all. The pin-VALUE assertion alone would pass on a publisher that emitted the right shape
/// while the offer machinery had quietly broken. Both are asserted, on one board, in one run.
///
/// # Discrimination
///
/// REVERT-PROBE (the commit itself): restore `record_trigger_target_answer`'s player arm to
/// `Some(TargetPin::Player(*pl))` ⇒ the journal holds the CHOICE-class spelling ⇒
/// `build_bounded_declaration` copies it through ⇒ the pin-value assertion FAILS while
/// `is_some()` stays green. That asymmetry is the row.
///
/// # The hostile arm, and the ORDERING that makes it reachable
///
/// The split's whole content is WHICH AUTHORITY judges a seat, so the hostile fixture makes the
/// seat untargetable and requires the declaration to be REFUSED. The hexproof is applied AFTER
/// the real drive has latched the pin, and the ordering is load-bearing rather than convenient:
/// Torch's "target opponent" has three legal opponents on this board, so a board that was
/// hexproofed BEFORE the drive would let the announcement name a different opponent, and the
/// row would be measuring the announcement's choice instead of the pin's legality. Latch first,
/// then remove the seat from the target set, is also the CR 115.7a shape — "the original target
/// is unchanged, even if the original target is itself illegal by then".
///
/// PAIRED POSITIVE, same board, same instrument: `validate_pins` on the very same
/// (schema, declaration) pair is `Ok` BEFORE the grantor lands. Without it, `Err` afterwards is
/// equally explained by a seat pin that never validates at all.
///
/// REVERT-PROBE (hostile arm): the same restore of the producer ⇒ the pin is a
/// `TargetPin::Player`, `resolve_target`'s CHOICE arm asks existence only, the hexproof is not
/// consulted, `validate_pins` returns `Ok` ⇒ the refusal assertion FAILS. This is the real-dump
/// sibling of the resolver-level row in `loop_shortcut_ranking.rs`.
#[test]
fn r2a_split_the_bounded_offer_still_publishes_a_ranked_seat_pin_and_refuses_a_hexproofed_one() {
    use engine::analysis::decision_template::{
        validate_pins, AnnouncementSubject, PinnedDecision, Ranking, TargetPin, TargetSchedule,
    };
    use engine::types::ability::{ControllerRef, StaticDefinition, TypedFilter};
    use engine::types::game_state::LayersDirty;
    use engine::types::identifiers::CardId;
    use engine::types::statics::StaticMode;
    use engine::types::zones::Zone;

    let mut state = load_f4();
    let beat = drive_f4_to_offer(&mut state, 400)
        .expect("REACH-GUARD: the bounded offer must still FIRE after the provenance split");
    let (proposer, _certificate, schema) = offer_parts(&state);
    let schema = schema.clone();

    assert_eq!(
        schema.max_iterations, 18,
        "MAINTAINED INVARIANT: the CR 704.5a-derived bound at beat {beat} is unchanged by a \
         change of pin SPELLING — the split moves which authority judges a seat, not how much \
         the loop consumes"
    );

    let declaration = offer_declaration(&state)
        .expect("MAINTAINED INVARIANT: the offer still publishes a declaration");
    assert_eq!(
        declaration.owner, proposer,
        "reach-guard: the published declaration is the proposer's own"
    );

    let target_slot = schema
        .points
        .iter()
        .find(|p| matches!(p.kind, DecisionPointKind::Targets { .. }))
        .map(|p| p.slot.clone())
        .expect("reach-guard: the offer publishes Torch's CR 601.2c Targets point");
    let pinned = declaration
        .decisions
        .iter()
        .find_map(|pin| match pin {
            PinnedDecision::Targets { slot, targets } if *slot == target_slot => Some(targets),
            _ => None,
        })
        .expect("reach-guard: the declaration pins the published Targets slot");
    assert_eq!(
        *pinned,
        vec![TargetPin::Scheduled(TargetSchedule::Constant(
            Ranking::one(AnnouncementSubject::Seat(P1))
        ))],
        "CR 601.2c: Torch's announced opponent is a TARGET, so the published pin carries the \
         TARGET-class spelling. Without this half the row passes unchanged on the pre-split \
         `TargetPin::Player(P1)`"
    );

    // ── PAIRED POSITIVE: the pin is LEGAL against the offer's own schema, before the hostile
    //    change lands ──
    assert!(
        validate_pins(&schema, &declaration, schema.max_iterations, &state).is_ok(),
        "paired positive: the ranked pin validates at the FULL declared range on the \
         un-hexproofed board — otherwise the refusal below is explained by a seat pin that \
         never validates at all"
    );

    // ── HOSTILE: P1 gains hexproof from a permanent P1 controls, AFTER the pin is latched ──
    let mut hostile = state.clone();
    // Built with production `zones::create_object` rather than a raw `objects.insert`: a raw
    // insert never joins `state.battlefield`, so the grantor would be invisible to
    // `game_functioning_statics` and the hexproof would silently never apply.
    let grantor = engine::game::zones::create_object(
        &mut hostile,
        CardId(9401),
        P1,
        "You Have Hexproof Source".to_string(),
        Zone::Battlefield,
    );
    hostile
        .objects
        .get_mut(&grantor)
        .expect("the grantor was just created")
        .static_definitions = vec![StaticDefinition::new(StaticMode::Hexproof).affected(
        engine::types::ability::TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::You),
        ),
    )]
    .into();
    // MEASURED, and the reach-guard below is what caught it: after a completed drive this
    // board's `layers_dirty` is `Clean`, and `create_object` does not re-dirty it — so a bare
    // `flush_layers` returns immediately, `refresh_static_mode_presence` never runs, and the
    // O(1) `static_mode_presence` gate answers `false` for `Hexproof` no matter what the
    // grantor carries. Marking the pass dirty is fixture bookkeeping, not a rule: it requests
    // exactly the re-evaluation an ETB would have requested.
    hostile.layers_dirty = LayersDirty::Full;
    engine::game::layers::flush_layers(&mut hostile);

    // The grant must actually bite at the TARGET seam, or the refusal below proves nothing.
    // CR 702.11c is opponent-scoped, so it is asked with Torch's own controller as the source
    // controller — the same question `evaluate_schedule`'s `Seat` arm asks.
    let torch = resolve_by_name(&hostile, TORCH);
    let torch_controller = hostile.objects[&torch].controller;
    assert!(
        engine::game::players::is_opponent(&hostile, P1, torch_controller),
        "reach-guard: CR 702.11c only excludes OPPONENTS' spells and abilities, so Torch's \
         controller {torch_controller:?} must be P1's opponent"
    );
    assert!(
        engine::game::static_abilities::player_cannot_be_targeted_by(
            &hostile,
            P1,
            torch,
            torch_controller
        ),
        "reach-guard: the hexproof grant must bite at the TARGET seam for Torch's ability. \
         grantor_on_battlefield={} player_has_hexproof={} — if the second is false while the \
         first is true, the layers pass did not re-run and the O(1) `static_mode_presence` \
         gate is stale",
        hostile.battlefield.contains(&grantor),
        engine::game::static_abilities::player_has_hexproof(&hostile, P1),
    );
    assert!(
        !engine::game::static_abilities::player_cannot_be_targeted_by(
            &hostile,
            PlayerId(2),
            torch,
            torch_controller
        ),
        "reach-guard: a DIFFERENT seat on the same board is still targetable, so the exclusion \
         above is the hexproof and not a blanket refusal"
    );

    assert!(
        validate_pins(&schema, &declaration, schema.max_iterations, &hostile).is_err(),
        "CR 601.2c + CR 702.11c: a TARGET-class seat that has become untargetable is an \
         ILLEGAL pin value, so the declaration is REFUSED rather than driven at a wrong seat. \
         Under the pre-split `TargetPin::Player` this returns Ok — existence alone — which is \
         exactly the over-veto-free CHOICE authority the split moved this pin off"
    );
}

/// §6 R1, SECOND HALF — the published point set, pinned so it cannot drift silently.
///
/// R1 as written expects `points ≡ {Targets(403 Torch), MayChoice(401 Reed),
/// MayChoice(402 Sue)}`. ⚠ THE MEASUREMENT THAT STOOD HERE — *"the `403` / `401` entries only
/// ever sit on the stack across a `TriggerTargetSelection` / `OptionalEffectChoice` window …
/// so `403` and `401` are never announced … therefore `bounded_cycle_pin_slots_for_window`
/// publishes exactly ONE point — Sue's `MayChoice`"* — IS FALSIFIED BY THIS ROW'S OWN BODY, and
/// is replaced rather than softened. Measured on this tree now:
///
/// * ALL FOUR cycle sources are retained on some sample's stack — the `framed_sources` census
///   below asserts `{Thing, Sue, Torch, Reed}` exactly, and states `Torch`/`Reed` as its own
///   conjunct because they are the load-bearing half;
/// * `403` and `401` do still resolve ACROSS a forced pre-priority window, but the answer-beat
///   sampling site in `apply_action` records a frame at the beat that window is ANSWERED — so
///   `certified_period_touch`'s `announced` set, still exactly "entries in a frame's stack
///   absent from the previous frame's", now contains them;
/// * therefore `bounded_cycle_pin_slots_for_window` publishes all THREE points, and R1's
///   planned expectation is MET rather than corrected.
///
/// The row asserts the MEASUREMENT, with the sources named, and the frame census as its own
/// reach-guard. **If a future change NARROWS the announced set again this row FAILS LOUDLY** —
/// which is what it is for: that shrink is exactly the regression
/// [`r2a_an_accepted_declaration_commits_exactly_n_cycles_because_reeds_may_is_announced`],
/// written on the strength of Reed being published, would otherwise silently lose.
#[test]
fn r1b_the_published_point_set_is_exactly_what_the_retained_window_announces() {
    let mut state = load_f4();
    let (torch, sue, reed, thing) = (
        resolve_by_name(&state, TORCH),
        resolve_by_name(&state, SUE),
        resolve_by_name(&state, REED),
        resolve_by_name(&state, THING),
    );
    drive_f4_to_offer(&mut state, 400).expect("the bounded offer fires (see R1)");
    let (_proposer, _certificate, schema) = offer_parts(&state);

    // ── reach-guard: the ring really is populated, and its frames really do alternate over
    //    exactly {THING, SUE} — this is the fact that EXPLAINS the point set ──
    assert!(
        state.loop_detect_ring.len() >= 2,
        "reach-guard: a window needs at least two retained samples; ring = {}",
        state.loop_detect_ring.len()
    );
    let framed_sources: std::collections::BTreeSet<ObjectId> = state
        .loop_detect_ring
        .iter()
        .flat_map(|f| f.live.stack.iter().map(|e| e.source_id))
        .collect();
    assert_eq!(
        framed_sources,
        [thing, sue, torch, reed].into_iter().collect(),
        "MEASURED: every one of the four cycle sources is retained on some sample's stack. \
         {TORCH:?} ({torch:?}) and {REED:?} ({reed:?}) resolve ACROSS a forced pre-priority \
         window, and the second sampling site in `apply_action` records a frame at the beat \
         that window is ANSWERED — so they are announced exactly like {THING:?} ({thing:?}) \
         and {SUE:?} ({sue:?}). This is the reach-guard for the point-set assertion below"
    );
    assert!(
        framed_sources.contains(&torch) && framed_sources.contains(&reed),
        "stated as its own conjunct because it is the load-bearing half: the two sources whose \
         choices used to go unpublished are exactly the two the answer-beat sampler adds"
    );

    let published: Vec<(ObjectId, &'static str)> = schema
        .points
        .iter()
        .map(|p| {
            let source = match &p.slot.source {
                engine::types::game_state::YieldTarget::ThisObject { source_id, .. } => *source_id,
                other => panic!("unexpected decision source {other:?}"),
            };
            let kind = match &p.kind {
                DecisionPointKind::MayChoice => "MayChoice",
                DecisionPointKind::Targets { .. } => "Targets",
                other => panic!("unexpected point kind {other:?}"),
            };
            (source, kind)
        })
        .collect();
    assert_eq!(
        published,
        vec![(sue, "MayChoice"), (reed, "MayChoice"), (torch, "Targets")],
        "MEASURED: the window mint publishes all THREE per-iteration choices this cycle \
         opens — Sue's and Reed's CR 603.5 `may` gates and Torch's CR 608.2b `Targets` slot. \
         The set is exactly the announced set from the census above; if it SHRINKS again the \
         answer-beat sampling site regressed"
    );
}

/// §6 R2a, **as measured** — the accepted declaration COMMITS, driven end to end.
///
/// A `Fixed(n)` declaration carrying the FULL published pin set is ACCEPTED at declare
/// (`predictability_gate` + `validate_pins` both pass — the published set is covered), every
/// living opponent Accepts (CR 732.2c), and the drive then commits **exactly `n`** repetitions
/// of the published per-cycle delta: cycle 0 answers Sue's `OptionalEffectChoice` from the pin
/// (U4's `inject_pinned_answer` arm, on the real dump), then Reed's from ITS pin, and the cycle
/// closes at the published period boundary.
///
/// ⚠ **THIS ROW USED TO ASSERT THE OPPOSITE** (`r2_..._commits_zero_cycles_because_reeds_may_
/// is_unannounced`) and the rename is the point: the zero-commit was the fail-closed abort on
/// Reed's UNPINNED `may`, which existed only because the sampler could not see the frame
/// Reed's entry announced on. With Reed published there is nothing left to abort on, so §6
/// R2a's *"exactly N cycles commit"* finally has a non-vacuous form on the real dump.
///
/// The row pins the commit **together with its cause**, so it cannot be read as "some delta
/// appeared":
///
/// * the same declaration is run at `n = 1` and `n = 3` and the two outcomes must be
///   DISTINGUISHABLE — the discriminator `bounded_fixed_count_commits_exactly_n_periods` uses,
///   and the guard against an instrument that would satisfy the per-`n` equalities vacuously;
/// * the declaration is asserted to have been ACCEPTED (`RespondToShortcut` raised), so the
///   commit is the DRIVE's and not a declare-time artefact;
/// * Reed's `may` is asserted PUBLISHED on the same offer, naming the cause.
#[test]
fn r2a_an_accepted_declaration_commits_exactly_n_cycles_because_reeds_may_is_announced() {
    use engine::analysis::loop_check::ShortcutResponse;

    let mut committed_per_n = vec![];
    for n in [1u32, 3] {
        let mut state = load_f4();
        let reed = resolve_by_name(&state, REED);
        drive_f4_to_offer(&mut state, 400).expect("the bounded offer fires (see R1)");
        let (proposer, certificate, schema) = offer_parts(&state);
        // The row's failure message CLAIMS the published per-cycle delta, so the assertion
        // has to READ it. This binding used to be `_certificate` and the expectation two
        // literal `1`s — a re-dump that changed the rate reddened the row for a reason that
        // has nothing to do with the property under test.
        let per_cycle = certificate
            .per_cycle
            .clone()
            .expect("a bounded offer publishes its per-period signature");
        let schema = schema.clone();

        assert!(
            schema.points.iter().any(|p| matches!(&p.slot.source,
                    engine::types::game_state::YieldTarget::ThisObject { source_id, .. }
                        if *source_id == reed)),
            "the CAUSE this row is about: Reed's CR 603.5 `may` IS among the published \
             points, so a legal declaration can pin it and the drive has nothing left to \
             abort on"
        );

        let template = f4_pin_template(&schema, proposer, n);

        let life_before: Vec<i64> = state.players.iter().map(|p| p.life as i64).collect();
        let libs_before: Vec<usize> = state.players.iter().map(|p| p.library.len()).collect();
        // Seat ids read POSITIONALLY, from the same order the two vectors above index, so the
        // published rate looked up below belongs to the seat whose movement is measured.
        let seats: Vec<PlayerId> = state.players.iter().map(|p| p.id).collect();

        apply(
            &mut state,
            proposer,
            GameAction::DeclareShortcut {
                count: IterationCount::Fixed(n),
                template: Some(template),
            },
        )
        .expect("the declaration is dispatched");
        // THE DISCRIMINATOR between "declare refused it" and "the drive aborted": a refused
        // declaration hands priority straight back and never opens the APNAP window.
        assert!(
            matches!(state.waiting_for, WaitingFor::RespondToShortcut { .. }),
            "n={n}: the declaration carrying the FULL published pin set must be ACCEPTED and \
             open the CR 732.2b APNAP window — a `Priority` here would mean the zero-commit \
             below is a declare-time refusal, not the drive's abort. got {:?}",
            state.waiting_for
        );
        while let WaitingFor::RespondToShortcut { player, .. } = state.waiting_for.clone() {
            apply(
                &mut state,
                player,
                GameAction::RespondToShortcut {
                    response: ShortcutResponse::Accept,
                },
            )
            .expect("each living opponent accepts (CR 732.2c)");
        }

        let life_after: Vec<i64> = state.players.iter().map(|p| p.life as i64).collect();
        let libs_after: Vec<usize> = state.players.iter().map(|p| p.library.len()).collect();
        // Both axes are measured as LOSSES (`before - after`), so the published signed rates
        // are negated to match. `libs_*` are `usize`: cast EACH side before subtracting, or a
        // library that fails to shrink — the exact zero-commit regression this row guards —
        // aborts on an arithmetic overflow instead of printing the diagnostic below.
        let life_rate = -per_cycle.delta.life.get(&seats[1]).copied().unwrap_or(0);
        let lib_rate = -per_cycle
            .delta
            .library_delta
            .get(&seats[0])
            .copied()
            .unwrap_or(0);
        assert!(
            life_rate > 0 && lib_rate > 0,
            "n={n}: ANTI-VACUITY — both published per-cycle rates must be strictly positive, \
             else the equality below degenerates to `0 == 0 * {n}` and asserts nothing. \
             published life={:?} library={:?}",
            per_cycle.delta.life,
            per_cycle.delta.library_delta
        );
        assert_eq!(
            (
                life_before[1] - life_after[1],
                libs_before[0] as i64 - libs_after[0] as i64
            ),
            (life_rate * i64::from(n), lib_rate * i64::from(n)),
            "n={n}: CR 732.2a — the accepted shortcut commits EXACTLY n repetitions of the \
             published per-cycle delta ({:?} loses {life_rate} life and {:?}'s library loses \
             {lib_rate} card(s) per repetition). life {life_before:?} -> {life_after:?}, libs \
             {libs_before:?} -> {libs_after:?}",
            seats[1],
            seats[0]
        );
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "n={n}: CR 732.2a — the taken shortcut's ending point is a place where a player \
             has priority, got {:?}",
            state.waiting_for
        );
        committed_per_n.push((life_after, libs_after));
    }
    assert_ne!(
        committed_per_n[0], committed_per_n[1],
        "n=1 and n=3 must be DISTINGUISHABLE: the declared count is the whole content of a \
         CR 732.2a `Fixed(n)` grant, so an instrument that cannot separate them would satisfy \
         the per-n assertions above vacuously. This is the discriminator \
         `bounded_fixed_count_commits_exactly_n_periods` uses, adopted here now that this \
         board actually grants"
    );
}

/// **R3-a** — the CR 732.2a EPISODE BOUNDARY, driven on the real dump: a completed drive
/// hands back at the priority point with the detection window CLEARED (`loop_detect_ring`
/// empty, `loop_answer_journal == None`), and this same `apply()` does NOT re-offer.
///
/// The seam is the drive-end block in `game::engine` — `*state = committed;`, then the ring
/// clear and journal clear, then the priority handback. That is **site 2** of the eight
/// ring-clear sites [`c1_every_ring_clear_site_also_clears_the_loop_answer_journal`]
/// enumerates, and that census covers site 2 STRUCTURALLY only (its own doc says so). This
/// row drives it.
///
/// # Why the f4 board, and why it is not substitutable
///
/// Four shipped fixtures reach this seam. MEASURED, this dump is the only one whose journal
/// is non-empty there (`answers=3`; the three `loop_shortcut.rs` fixtures arrive at
/// `answers=0`). The `loop_answer_journal` half of the claim is therefore unpinnable
/// anywhere else — which is what makes this row REAL-DUMP rather than convenient. The ABORT
/// entry to the same seam is covered where its fixtures already live, on
/// `bounded_fixed_drive_rolls_back_a_partial_crossing_cycle` in `loop_shortcut.rs`.
///
/// # Discrimination — REVERT-PROBE, RUN, not adopted from a code read
///
/// Delete the seam's `state.loop_detect_ring.clear();` + `state.loop_answer_journal = None;`
/// ⇒ MEASURED `ring=12, answers=3, wf=LoopShortcut` against this drive's `0, 0, Priority`:
/// all three assertions below flip together and the engine re-offers within the same
/// `apply()`.
///
/// The ANTI-PROBE, also run: deleting `apply_action`'s PRE-ACTION clear instead leaves the
/// final state MEASURED-unchanged at `0, 0, Priority`. This row keys on the drive-end seam
/// and not on the upstream clear, and must not be attributed to it.
///
/// ⚠ **Do NOT assert that the ring/journal are non-empty immediately before the seam.**
/// MEASURED: they read `0/0` at the post-declare beat, because `apply_action`'s pre-action
/// clear fires on `DeclareShortcut`. The `12/3` the seam itself receives is internal and
/// unobservable from a test. The paired positive below is taken at the OFFER beat, which is
/// observable.
///
/// ⚠ **Do NOT add a revert-probe on the `WaitingFor::Priority` re-seat** that follows the
/// clear: MEASURED VACUOUS on all four fixtures reaching this seam — they are already at
/// `Priority` on entry. The seam's own comment block carries that as labelled
/// interpretation, deliberately not as a pinned claim.
#[test]
fn r3a_the_accepted_drive_ends_at_the_priority_point_with_the_window_cleared() {
    let mut state = load_f4();
    drive_f4_to_offer(&mut state, 400).expect("the bounded offer fires (see R1)");

    // ── PAIRED POSITIVE (i): the window is LIVE at the offer beat, same board, same run.
    //    MEASURED at `2160f6e2c`: ring=9, answers=3. Without it every zero below is
    //    satisfiable by a board that never sampled or never answered a `may`. ──
    let ring_at_offer = state.loop_detect_ring.len();
    let answers_at_offer = state.loop_answers_recorded();
    assert!(
        ring_at_offer > 0 && answers_at_offer > 0,
        "paired positive: at the CR 732.2a offer beat this board must carry BOTH a populated \
         detection ring and a populated CR 603.5 answer journal, else the cleared-window \
         assertions after the drive are vacuous. ring={ring_at_offer} answers={answers_at_offer}"
    );

    let (proposer, _certificate, schema) = offer_parts(&state);
    let schema = schema.clone();
    let template = f4_pin_template(&schema, proposer, 3);
    apply(
        &mut state,
        proposer,
        GameAction::DeclareShortcut {
            count: IterationCount::Fixed(3),
            template: Some(template),
        },
    )
    .expect("the declaration is dispatched");
    // Reach-guard, not the claim: a REFUSED declaration hands priority straight back, and
    // then the cleared window below would be the pre-action clear's work rather than the
    // drive-end seam's.
    assert!(
        matches!(state.waiting_for, WaitingFor::RespondToShortcut { .. }),
        "reach-guard: the declaration carrying the full published pin set must be accepted \
         and open the CR 732.2b window, got {:?}",
        state.waiting_for
    );
    let responders = accept_all_opponents(&mut state);
    assert!(
        responders > 0,
        "reach-guard: the CR 732.2b response window must actually have opened and been \
         answered — the shortcut is taken only once the last opponent has accepted \
         (CR 732.2c) — else the drive never ran and no seam was reached"
    );

    // ── THE CLAIM: the drive ended at the CR 732.2a ending point with the window discarded ──
    assert!(
        state.loop_detect_ring.is_empty(),
        "CR 732.2a: the accepted drive ends at the ending point with the detection window \
         DISCARDED, so the next episode re-detects from scratch. ring still carries {} \
         sample(s) (it carried {ring_at_offer} at the offer beat)",
        state.loop_detect_ring.len()
    );
    assert_eq!(
        state.loop_answers_recorded(),
        0,
        "CR 603.5: the recorded `may` answers describe the window that just ended, and the \
         drive-end seam drops them with the ring (it carried {answers_at_offer} at the offer \
         beat)"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "CR 732.2a: the ending point of the taken sequence is a place where a player has \
         priority — and a `LoopShortcut` here would be the re-offer the seam's ring clear \
         exists to prevent. got {:?}",
        state.waiting_for
    );

    // ── PAIRED POSITIVE (ii): the sampler is still ON at handback, so an empty ring is a
    //    CLEARED ring and not a disabled detector. ──
    assert!(
        state.loop_detection.samples(),
        "paired positive: the detector must still be sampling after the handback ({:?}), \
         else `ring.is_empty()` above says nothing about the seam",
        state.loop_detection
    );
}

/// **R3-b, DRIVEN ARM** — the CROSS-EPISODE CARRIER claim, taken on the real 4-player board
/// across a whole accepted drive rather than at helper level.
///
/// `analysis::decision_template::DecisionKind`'s doc states that a `LoopChoice` template
/// SURVIVES the CR 603.3b batch boundary and is therefore the vehicle a later episode's
/// declaration rides. Its sibling `loop_shortcut_ranking::r3b_*` states the same property at
/// the seam — it calls `clear_ephemeral_trigger_order_templates()` directly — which pins WHICH
/// CELL the predicate removes but says nothing about whether an accepted production drive ever
/// reaches that predicate, or reaches it only once, or leaves the survivor intact afterwards.
/// This row is that missing half: `DeclareShortcut` → the full CR 732.2b APNAP window →
/// `apply_confirmed_shortcut` → `materialize_fixed_shortcut`, every beat through `apply()`.
///
/// # Non-vacuity
///
/// The `TriggerOrdering` + ephemeral cell is the paired positive: it is REMOVED by the same
/// drive that keeps the `LoopChoice` one, so "the drive never reached the boundary" and "the
/// drive dropped everything" both fail here. MEASURED `3 → 2`.
///
/// # Discrimination
///
/// The asserted vector is two-sided, and each side names the mutant that flips it:
///
/// * drop the seam's `kind ==` conjunct ⇒ the `(LoopChoice, ephemeral)` element disappears;
/// * never reach the seam at all ⇒ the `(TriggerOrdering, ephemeral)` element is still there.
///
/// The second is MEASURED by this row passing (`3 → 2`, with that cell and only that cell
/// gone). The first is attributed rather than mutated HERE, and the attribution is licensed by
/// a census rather than by a code read: over `crates/engine/src` the only `retain` on a LIVE
/// `decision_templates` is `GameState::clear_ephemeral_trigger_order_templates` — `visibility`'s
/// retain runs on the per-viewer CLONE (`filtered.decision_templates`), and no other site
/// clears, drains, removes or reassigns the Vec. So a drive that demonstrably removed one cell
/// ran that predicate, and the survivor beside it is that predicate's `kind ==` conjunct doing
/// work. The predicate-level mutant itself is RUN on the seam-level sibling
/// `loop_shortcut_ranking::r3b_*`, which is where a production-source mutation belongs.
///
/// The planted cells are inert as far as the drive is concerned — they key on a source no F4
/// trigger raises — so they observe the boundary without steering it.
#[test]
fn r3b_driven_a_loop_choice_carrier_survives_a_whole_accepted_f4_drive() {
    use super::loop_shortcut_ranking::grid_template;

    let mut state = load_f4();
    drive_f4_to_offer(&mut state, 400).expect("the bounded offer fires (see R1)");
    let (proposer, _certificate, schema) = offer_parts(&state);
    let schema = schema.clone();
    let template = f4_pin_template(&schema, proposer, 3);

    // Planted at the OFFER beat, keyed to a real battlefield object resolved BY NAME so a
    // re-dump that renumbers `ObjectId`s flows through (see `resolve_by_name`).
    //
    // The plant is purely ADDITIVE, and that is ASSERTED rather than assumed: had the drive
    // left real templates here, overwriting them could steer the very drive this row observes,
    // and the survivor set below would be reporting the fixture's own damage.
    let anchor = resolve_by_name(&state, THING);
    assert!(
        state.decision_templates.is_empty(),
        "reach-guard: the F4 drive reaches its offer beat carrying NO templates, so the grid \
         below is planted onto an empty vector and displaces nothing; got {:?}",
        state
            .decision_templates
            .iter()
            .map(|t| (t.key.kind, t.key.is_ephemeral()))
            .collect::<Vec<_>>()
    );
    state.decision_templates = vec![
        grid_template(P0, DecisionKind::LoopChoice, true, anchor),
        grid_template(P0, DecisionKind::TriggerOrdering, true, anchor),
        grid_template(P0, DecisionKind::TriggerOrdering, false, anchor),
    ];
    let cells = |state: &GameState| -> Vec<(DecisionKind, bool)> {
        state
            .decision_templates
            .iter()
            .map(|t| (t.key.kind, t.key.is_ephemeral()))
            .collect()
    };
    assert_eq!(
        cells(&state),
        vec![
            (DecisionKind::LoopChoice, true),
            (DecisionKind::TriggerOrdering, true),
            (DecisionKind::TriggerOrdering, false),
        ],
        "reach-guard on the INSTRUMENT: both axes must be genuinely distinguishable on the real
         board too, else 'exactly one cell removed' could be an artefact of three identical keys"
    );

    apply(
        &mut state,
        proposer,
        GameAction::DeclareShortcut {
            count: IterationCount::Fixed(3),
            template: Some(template),
        },
    )
    .expect("the declaration is dispatched");
    assert!(
        matches!(state.waiting_for, WaitingFor::RespondToShortcut { .. }),
        "reach-guard: the declaration carrying the full published pin set must be accepted and \
         open the CR 732.2b window, got {:?}",
        state.waiting_for
    );
    let responders = accept_all_opponents(&mut state);
    assert!(
        responders > 0,
        "reach-guard: the CR 732.2b window must actually have opened and been answered \
         (CR 732.2c), else no drive ran and no batch boundary was crossed"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "reach-guard: the accepted drive ran to its CR 732.2a ending point, got {:?}",
        state.waiting_for
    );

    assert_eq!(
        cells(&state),
        vec![
            (DecisionKind::LoopChoice, true),
            (DecisionKind::TriggerOrdering, false),
        ],
        "CR 732.2a + CR 603.3b: across a whole ACCEPTED drive the ephemeral `LoopChoice` \
         carrier SURVIVES — it is the cross-episode vehicle P4 rides — while the ephemeral \
         `TriggerOrdering` cell beside it is dropped at the batch boundary the drive crosses. \
         A missing `LoopChoice` means the retain predicate lost its KIND conjunct; a surviving \
         ephemeral `TriggerOrdering` means the drive never reached the boundary at all"
    );
}

// ─────────────────────────────────────────────────────────────────────────────────────────
// R23 conjunct (5-reach) — the beat guard's reachability on the real dump
// ─────────────────────────────────────────────────────────────────────────────────────────

/// §6 R23, the (5-reach) arm — **U4's CR 603.3c beat guard never fires on the acceptance
/// fixture, and the fire population it guards against is measurably NON-EMPTY on the same
/// drive.**
///
/// The guard is `if work.pending_trigger.is_some() { return Err(RecastAbort) }` at the head of
/// `inject_pinned_answer`'s `OptionalEffectChoice` arm: a live CR 603.3c construction cursor
/// means the prompt in hand may be the ANNOUNCEMENT-time optional-modal question rather than
/// the resolution-time "may" the pin answers, and `slot_source_prompted` matches only the
/// SOURCE OBJECT, which both questions share.
///
/// ⚠ DISCLOSED INSTRUMENT LIMIT: the guard reads the drive's private `work` board, which no
/// test can observe. This row asserts the same property on the OUTER drive — every
/// `OptionalEffectChoice` beat this dump reaches carries `pending_trigger == None` — which is
/// the beat structure the drive replays. **Its non-vacuity is the paired positive**: the same
/// drive DOES visit beats carrying a live cursor (`pending_trigger == Some(TORCH)`), so the
/// instrument demonstrably can report one.
///
/// **If the `is_none()` assertion ever fires, the remedy is NOT to weaken it**: it is to scope
/// the guard to the prompt's own `source_id` (§5 U2's alternative placement), which changes
/// what the guard MEANS and is an escalation, not a local edit.
#[test]
fn r23_5_reach_no_may_beat_of_the_f4_drive_carries_a_construction_cursor() {
    let mut state = load_f4();
    let mut may_beats = 0usize;
    let mut cursor_beats = 0usize;
    for beat in 0..400u32 {
        if matches!(state.waiting_for, WaitingFor::LoopShortcut { .. }) {
            break;
        }
        if let WaitingFor::OptionalEffectChoice { source_id, .. } = &state.waiting_for {
            may_beats += 1;
            assert!(
                state.pending_trigger.is_none(),
                "R23 (5-reach): a CR 603.5 `may` beat carrying a LIVE CR 603.3c construction \
                 cursor is exactly the configuration U4's beat guard fail-closes on, and it \
                 must not occur on the acceptance fixture. beat {beat}, prompt source \
                 {source_id:?}, cursor source {:?}. REMEDY IS AN ESCALATION (scope the guard \
                 to the prompt's own source_id), NEVER a weakening of this assertion",
                state.pending_trigger.as_ref().map(|t| t.source_id)
            );
        }
        if state.pending_trigger.is_some() {
            cursor_beats += 1;
        }
        if f4_drive_one_beat(&mut state).is_err() {
            break;
        }
    }
    // ── the paired positive: both populations are non-empty, so neither half is vacuous ──
    assert!(
        may_beats > 0,
        "reach-guard: the drive must actually REACH CR 603.5 `may` beats, else the assertion \
         above quantifies over nothing"
    );
    assert!(
        cursor_beats > 0,
        "reach-guard: the same drive must visit beats that DO carry a live construction \
         cursor (this dump ships `pending_trigger` on Torch), else `is_none()` is satisfied by \
         an instrument that can never report `Some`"
    );
}

// ─────────────────────────────────────────────────────────────────────────────────────────
// R9 — the environmental discharge, on the production path
// ─────────────────────────────────────────────────────────────────────────────────────────

/// §6 R9 — **the environmental-discharge row round 2's def-scan design could not fail** — with
/// its keying RE-DERIVED from measurement.
///
/// # What the row asserts
///
/// CR ANCHORS, CORRECTED: this row cited **CR 614.1a** for the choice. `614.1a` is
/// "effects that use the word *instead*" — a sub-rule, and not the one that makes an
/// optional replacement a choice. **CR 614.1** is the DEFINITION (replacement effects watch
/// for an event and replace it) and **CR 732.2a** is the LOAD-BEARING half: a shortcut
/// "can't include conditional actions, where the outcome of a game event determines the next
/// action a player takes". CR 616.1 stays where it belongs — the two-or-more ORDERING branch.
///
/// On the offer-beat board, ONE CR 614.1 replacement definition that the resolver's OWN
/// derivation draws turns the OFFER into `UnspecifiedChoiceWindow`; six definitions the
/// resolver's derivation does NOT draw leave the offer standing. That contrast IS the claim:
/// the obligation is **event-derived**, read off what the resolution proposes through
/// `find_applicable_replacements`, and is NOT a scan over `def.event` NAMES. A name scan
/// cannot distinguish the seven definitions below — they differ only in their `event` name.
///
/// # MEASURED PLAN CORRECTION — the plan's ChangeZone/token keying does not fire
///
/// §6 R9 keys this row on *"a def carrying `event: ReplacementEvent::ChangeZone` … because
/// Sue's `ProposedEvent::CreateToken` draws it via the `ChangeZone` registry key"*. Measured on
/// this board, that def leaves the offer standing — and the reason is already on record in this
/// lane: U1-fin measured that `Effect::Token` never sets `CreateToken.copy`, and
/// `apply_create_token_after_replacement_with_created_ids` gates the whole `TokenEntry` route
/// on `if let Some(copy) = copy`, so **an `Effect::Token` resolution derives no token-entry
/// event at all** (the same fact that re-keyed R19a). Sue's trigger IS an `Effect::Token`
/// (`Wall` 0/4), so the plan's board cannot reach its own stated mechanism.
///
/// The row is therefore re-keyed onto the announced entry whose derivation the resolver DOES
/// produce: **The Thing's mandatory `PutCounter P1P1 ×2`, deriving `ProposedEvent::AddCounter`**
/// — same class, same seam, same conjunct, on the same real board. The falsified keys are not
/// dropped: they ship as arm (b), where their NON-firing is the discriminator.
///
/// # Arms
///
/// * **(pos)** the UNMODIFIED offer-beat board OFFERS through the metered seam — asserted
///   FIRST, so every refusal below is attributable to the definition and not to the replay.
/// * **(a)** one OPTIONAL `AddCounter` definition ⇒ `UnspecifiedChoiceWindow` (CR 732.2a +
///   CR 614.1: an optional replacement is a genuine resolution-time choice, and a described
///   sequence may not contain one ⇒ the period is not choice-free).
/// * **(a′)** the SAME definition, MANDATORY ⇒ still OFFERS. CR 616.1: a lone quantity
///   modification commutes with nothing, so there is no ordering choice to make. This is what
///   keeps (a) keyed to OPTIONALITY rather than to "a definition exists".
/// * **(b)** six definitions whose events this board's resolutions never propose
///   (`ChangeZone`, `Moved`, `CreateToken`, `Draw`, `DamageDone`, `RemoveCounter`), each
///   OPTIONAL ⇒ all still OFFER. A `def.event`-name scan would have to refuse these too.
///
/// # Reach-guard
///
/// The live candidate authority is asked directly for the `ProposedEvent::AddCounter` The
/// Thing's resolution proposes, and must return a non-empty set — otherwise (a)'s refusal
/// could belong to some other conjunct.
///
/// # REVERT-PROBES — RUN, and the FIRST FOUR MEASURED **NOT** TO FLIP, which is the finding
///
/// The refusal is carried by **two independent authorities**, and each one alone is sufficient:
///
/// | probe (one production edit) | (a) |
/// |---|---|
/// | delete `resolution_events_are_discharged`'s `!causes.is_empty()` conjunct | still REFUSES |
/// | disable `probe_resolution`'s `waiting_for`-discriminant arm | still REFUSES |
/// | … + its `events.is_empty()` arm | still REFUSES |
/// | … + its `event_is_accounted` arm (all three prompt arms) | still REFUSES |
/// | **all three prompt arms AND the discharge conjunct** | **OFFERS ⇒ (a) FAILS** |
///
/// Measured at the seam with a throwaway instrument (run, read, deleted): on the unprobed tree
/// The Thing's entry classifies `MayPrompt` — the resolver's OWN probe detects the pending
/// optional replacement — and a MANDATORY entry publishes no `may`, so
/// `pinned_may_choice_relief` returns `None` and conjunct (6) refuses there. Disable that
/// detection and the entry classifies `FreeUnlessReplacements([AddCounter])`, whereupon the
/// CR 732.2a + CR 614.1 discharge conjunct refuses instead. Defence in depth is the property; a row that
/// flipped on either single edit would have been asserting over only one of the two.
///
/// ⚠ §6 R9's stated probe (*"swap `proposed_event_prompt_cause` back to a def-scan over
/// `def.event` names"*) is not runnable — that scan and its class map were DELETED at U1 — and
/// its predicted single-edit flip is refuted by the table above. Arm (b) covers what that probe
/// was for: it exhibits six definitions a name scan could not distinguish from (a)'s.
#[test]
fn r9_the_offer_refuses_on_a_derived_replacement_obligation_not_on_a_definition_name() {
    use engine::game::engine::{try_offer_bounded_cycle_shortcut_metered, ProbeCap};
    use engine::types::ability::{QuantityModification, ReplacementDefinition};
    use engine::types::counter::CounterType;
    use engine::types::proposed_event::{CounterPlacement, ProposedEvent};

    let mut state = load_f4();
    let thing = resolve_by_name(&state, THING);
    drive_f4_to_offer(&mut state, 400).expect("the bounded offer fires (see R1)");
    let (proposer, _certificate, _schema) = offer_parts(&state);

    // ── (pos) the matched positive, asserted first ──
    let healthy = replay_at_priority(&state, proposer);
    let (healthy_out, healthy_meter) =
        try_offer_bounded_cycle_shortcut_metered(&healthy, false, ProbeCap::Shipped);
    assert!(
        healthy_out.is_ok(),
        "matched positive: the UNMODIFIED offer-beat board must still OFFER through the \
         metered seam, else every negative below is asserted over a board that refuses \
         anyway. got {healthy_out:?}, meter {healthy_meter:?}"
    );

    // One definition, installed on an EXISTING P0-controlled permanent (never a new object),
    // so board membership — and therefore every certification premise — is untouched.
    // CR ANCHOR CORRECTED with the two above it: this said "CR 614.1a scopes a definition to
    // its controller's events". It does not — `614.1a` is the "effects that use the word
    // *instead*" sub-rule and says nothing about controllers. CR 614.1 is the definition a
    // replacement definition answers to: it watches for the event its own text names.
    let with_def = |event: ReplacementEvent, optional: bool| -> GameState {
        let mut hostile = healthy.clone();
        let mut def = ReplacementDefinition::new(event.clone());
        if optional {
            def.mode = ReplacementMode::Optional { decline: None };
        }
        if matches!(event, ReplacementEvent::Draw) {
            // CR 121.2: a Draw definition must declare its stage or the pipeline debug-asserts.
            def.draw_scope = Some(engine::types::ability::DrawReplacementScope::IndividualDraw);
        }
        def.quantity_modification = Some(QuantityModification::Plus { value: 1 });
        hostile
            .objects
            .get_mut(&thing)
            .expect("The Thing is on the battlefield")
            .replacement_definitions
            .push(def);
        hostile
    };
    let outcome = |board: &GameState| {
        try_offer_bounded_cycle_shortcut_metered(board, false, ProbeCap::Shipped)
    };

    // ── reach-guard: the LIVE candidate authority draws the optional AddCounter definition
    //    for the very event The Thing's announced resolution proposes ──
    let optional_counter_board = with_def(ReplacementEvent::AddCounter, true);
    let proposed = ProposedEvent::AddCounter {
        placement: CounterPlacement::Object {
            actor: proposer,
            object_id: thing,
            counter_type: CounterType::Plus1Plus1,
        },
        count: 2,
        applied: Default::default(),
    };
    let candidates = engine::game::replacement::find_applicable_replacements(
        &optional_counter_board,
        &proposed,
        engine::game::replacement::replacement_registry(),
    );
    assert!(
        !candidates.is_empty(),
        "reach-guard: the live candidate authority must draw the definition for the \
         `ProposedEvent::AddCounter` The Thing's `PutCounter P1P1 x2` proposes — a refusal \
         over an EMPTY candidate set would belong to some other conjunct entirely"
    );

    // ── (a) the optional definition refuses ──
    let (a_out, a_meter) = outcome(&optional_counter_board);
    assert!(
        matches!(
            a_out,
            Err(engine::game::engine::BoundedOfferRefusal::UnspecifiedChoiceWindow)
        ),
        "(a) CR 732.2a + CR 614.1: an OPTIONAL replacement candidate applicable to an \
         ANNOUNCED entry's DERIVED event is a real resolution-time choice, so the period is \
         not choice-free and the offer must be refused. got {a_out:?}, meter {a_meter:?}"
    );

    // ── (a′) the same definition, mandatory, still offers ──
    let (a2_out, a2_meter) = outcome(&with_def(ReplacementEvent::AddCounter, false));
    assert!(
        a2_out.is_ok(),
        "(a′) CR 616.1: a LONE mandatory quantity modification commutes with nothing, so it \
         opens no ordering choice and the offer stands. Without this arm (a) would be keyed \
         to `a definition exists` rather than to OPTIONALITY. got {a2_out:?}, meter {a2_meter:?}"
    );

    // ── (b) the def-NAME discriminator, RE-DERIVED: the one optional definition whose event
    //    this board's announced resolutions still never propose ──
    let b_event = ReplacementEvent::RemoveCounter;
    let (b_out, b_meter) = outcome(&with_def(b_event.clone(), true));
    assert!(
        b_out.is_ok(),
        "(b) {b_event:?}: this board's announced resolutions never PROPOSE this event, so an \
         event-derived obligation must ignore the definition entirely and the offer must \
         stand. A scan over `def.event` NAMES would refuse here exactly as it refuses in (a), \
         which is what makes this arm the discriminator. got {b_out:?}, meter {b_meter:?}"
    );

    // ── (b′) the five events the WIDENED announced set really does propose ──
    // Once Torch's damage and Reed's draw are announced, `ChangeZone`/`Moved`/`CreateToken`/
    // `Draw`/`DamageDone` are genuinely derivable from this period's resolutions, so an
    // OPTIONAL definition on any of them is a real CR 616.1 choice and must refuse. This arm
    // is the paired positive control for (b): without it, (b) shrinking to one event could be
    // read as the obligation going blind rather than as the proposal set widening.
    for event in [
        ReplacementEvent::ChangeZone,
        ReplacementEvent::Moved,
        ReplacementEvent::CreateToken,
        ReplacementEvent::Draw,
        ReplacementEvent::DamageDone,
    ] {
        let (c_out, c_meter) = outcome(&with_def(event.clone(), true));
        assert!(
            matches!(
                c_out,
                Err(engine::game::engine::BoundedOfferRefusal::UnspecifiedChoiceWindow)
            ),
            "(b′) {event:?}: the widened announced set PROPOSES this event, so an OPTIONAL \
             replacement applicable to it is a genuine resolution-time choice and the offer \
             must refuse. got {c_out:?}, meter {c_meter:?}"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────────
// R16 — the probe budget does not starve the F4 acceptance fixture
// ─────────────────────────────────────────────────────────────────────────────────────────

/// §6 R16 (i) + the exact-demand pin, on the newly tracked F4 fixture.
///
/// The shipped `PROBE_BUDGET` was re-derived at U3 from dina's offering beat (demand 13). F4
/// was UNTRACKED then, so the acceptance fixture this whole lane exists for had never been
/// measured against the budget at all. Measured here, at F4's own offering beat, through the
/// metered seam:
///
/// * the demand is EXACT — `Lowered(d)` offers and every `Lowered(n < d)` refuses, over the
///   seam's closed cap domain. That is a sweep, not a single reading, so the number cannot be
///   an artifact of one call;
/// * `denied == false` at the shipped cap — the budget is not binding on this fixture;
/// * the certification basis at that beat is recorded (`ResourceSignatureOnly`, basis B),
///   because the meter is the only surface on which the basis is observable.
///
/// # (iii-b) THE ORDERING PIN, re-keyed onto an instrument that exists
///
/// §6 R16 (iii-b) asks for *"the honest count is 1"* at a beat carrying a non-exempt `optional`
/// entry, with the revert *"move `try_charge_one` above the `optional` gate ⇒ the entry burns a
/// charge in its primary pass as well as its residual pass ⇒ the count rises 1 → 2"*. The
/// count-per-entry is not a `MintMeter` field, but the property is: the meter carries BOTH
/// `spent` and `conjunct6_asks`, so **`spent == conjunct6_asks`** says exactly "every ask
/// charged once", which is the invariant the ordering protects. Under the stated revert an
/// `optional` ask charges twice and `spent > asks`.
///
/// Its reach-guard is the population the plan asks for: at least one entry the door is asked
/// about must be CR 603.5 `optional` — asserted on the retained window, since Sue's announced
/// entries are the optional ones (the current stack's single entry is The Thing's mandatory
/// `PutCounter`).
///
/// # (iii-a) — DISCLOSED, the plan's instrument does not exist on this tree
///
/// §6 R16 (iii-a) pins the *"CURRENT-FRAME charge subcount"* at 1. `MintMeter` has no
/// current-frame subcount, and adding one is a production change with no other consumer. What
/// this row establishes instead, and states as a derivation rather than a reading: the offering
/// beat's `current.stack` holds exactly ONE entry (asserted) and every ask charges exactly once
/// (asserted above) ⇒ the current frame contributes exactly one charge. The unqualified TOTAL
/// is (ii-a)'s figure and is measured directly by the sweep below.
#[test]
fn r16_the_f4_offering_beats_probe_demand_is_exactly_measured() {
    use engine::game::engine::{try_offer_bounded_cycle_shortcut_metered, ProbeCap};

    let mut state = load_f4();
    drive_f4_to_offer(&mut state, 400).expect("the bounded offer fires (see R1)");
    let (proposer, _certificate, _schema) = offer_parts(&state);
    let replay = replay_at_priority(&state, proposer);

    let (shipped_out, shipped) =
        try_offer_bounded_cycle_shortcut_metered(&replay, false, ProbeCap::Shipped);
    assert!(
        shipped_out.is_ok(),
        "reach-guard: the replay must reproduce the production OFFER, else every figure below \
         is measured on a different board. got {shipped_out:?}"
    );
    assert!(
        !shipped.denied,
        "R16 (i): the shipped budget must not STARVE the acceptance fixture — a denied budget \
         at the one beat this corpus offers on is the defect U3's re-derivation fixed for \
         dina, measured here for F4. meter {shipped:?}"
    );
    assert_eq!(
        shipped.certification,
        Some(engine::analysis::resource::PeriodCertification::ResourceSignatureOnly),
        "the F4 offering beat certifies through BASIS B; the meter is the only surface on \
         which that is observable (both bases publish `frames_per_period`)"
    );

    // ── (iii-b) the ordering pin: every ask charges EXACTLY ONE ──
    let optional_in_window = state
        .loop_detect_ring
        .iter()
        .map(|f| optional_entries(&f.live))
        .sum::<usize>();
    assert!(
        optional_in_window > 0,
        "(iii-b) reach-guard: the door must be asked about at least one CR 603.5 `optional` \
         entry, else the ordering property below is asserted over a population that never \
         reaches the `optional` gate at all — the exact defect §6 R16's ROUND-10 (MED-2) \
         re-keying was about"
    );
    assert!(
        shipped.conjunct6_asks > 0,
        "(iii-b) reach-guard: conjunct (6) must actually ASK, else `spent == asks` is `0 == 0`"
    );
    assert_eq!(
        shipped.spent, shipped.conjunct6_asks,
        "(iii-b) CR 603.5: `try_charge_one` sits BELOW the `optional` gate, so an entry pays \
         for its residual pass and never additionally for a primary pass it exits early. \
         Hoisting the charge above that gate makes every optional ask charge TWICE and \
         `spent` exceed `asks`. meter {shipped:?}"
    );
    assert_eq!(
        state.stack.len(),
        1,
        "(iii-a) the derivation's premise: the offering beat's current frame holds exactly ONE \
         entry, so with `spent == asks` the current frame contributes exactly one charge. \
         (`MintMeter` has no current-frame subcount — see this row's doc.)"
    );

    // ── the exact-demand sweep over the seam's closed cap domain ──
    let demand = shipped.spent;
    assert!(
        demand > 0,
        "reach-guard: a zero-demand beat would make every `Lowered(n)` below identical to \
         `Lowered(0)` and the sweep vacuous"
    );
    let (at_demand, _) =
        try_offer_bounded_cycle_shortcut_metered(&replay, false, ProbeCap::Lowered(demand));
    assert!(
        at_demand.is_ok(),
        "the measured demand {demand} must be SUFFICIENT — `Lowered(demand)` still offers"
    );
    for n in 0..demand {
        let (out, meter) =
            try_offer_bounded_cycle_shortcut_metered(&replay, false, ProbeCap::Lowered(n));
        assert!(
            matches!(
                out,
                Err(engine::game::engine::BoundedOfferRefusal::UnspecifiedChoiceWindow)
            ) && meter.denied,
            "every cap BELOW the measured demand must exhaust and refuse FAIL-CLOSED, so the \
             demand figure is a boundary and not one lucky reading. cap {n} gave {out:?}, \
             meter {meter:?}"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────────
// R27 (a1) — the F4 arm of the split-sample row
// ─────────────────────────────────────────────────────────────────────────────────────────

/// §6 R27 (a1), the F4 arm the plan sites at U5 (*"on BOTH real dumps"*; U0 landed the dellian
/// arm only, because this fixture was untracked until now).
///
/// CR 104.4b: the loop-detection COMPARAND is `normalize_for_loop`d, which zeroes the object
/// allocator so two structurally identical boards compare equal. CR 732.2a: the EVALUATION
/// board must keep the live allocator cursor, or every downstream consumer is reading a board
/// the game was never in. `LoopDetectSample` splits the two, and this row asserts the split on
/// the real F4 dump, on the allocator axis, at a sample the PRODUCTION sampler wrote.
#[test]
fn r27_a1_the_f4_dumps_recorded_sample_keeps_a_live_half_normalization_would_have_erased() {
    let mut state = load_f4();
    assert!(
        state.loop_detect_ring.is_empty(),
        "reach-guard: the restored dump starts with an EMPTY ring, so the sample asserted on \
         below is one THIS drive's production sampler wrote"
    );
    let mut witness = None;
    for _ in 0..400u32 {
        let before = state.next_object_id;
        let ring_before = state.loop_detect_ring.len();
        if f4_drive_one_beat(&mut state).is_err() {
            break;
        }
        if state.loop_detect_ring.len() > ring_before {
            witness = Some((before, state.next_object_id));
            break;
        }
    }
    let (before_beat, after_beat) =
        witness.expect("the production sampler must grow the ring within the drive's cap");
    let sample = state
        .loop_detect_ring
        .back()
        .expect("the ring just grew, so it has a newest sample");

    assert!(
        before_beat > 0,
        "reach-guard: the allocator cursor must be non-zero before the sampled beat, else the \
         inequality below is `0 != 0`"
    );
    assert_eq!(
        sample.normalized.next_object_id, 0,
        "CR 104.4b: the COMPARAND half is normalized — `normalize_for_loop` zeroes the object \
         allocator so two structurally identical boards compare equal"
    );
    assert!(
        sample.live.next_object_id >= before_beat && sample.live.next_object_id <= after_beat,
        "CR 732.2a: the EVALUATION half carries the LIVE allocator cursor, inside the beat's \
         own bracket [{before_beat}, {after_beat}]; got {}",
        sample.live.next_object_id
    );
    assert_ne!(
        sample.live, sample.normalized,
        "the two halves must be genuinely different boards — an equal pair would make the \
         split a distinction without a difference"
    );
}

// ─────────────────────────────────────────────────────────────────────────────────────────
// C1 — the CR 603.5 "may"-answer journal
//
// TIER, stated so no row here is read as covering more than it does: C1 ships the journal
// (record + read) and nothing that CONSUMES it. `build_bounded_declaration` and the offer's
// published `declaration` arrive with C2, so every row below asserts at the JOURNAL, never
// at a minted-or-refused declaration.
// ─────────────────────────────────────────────────────────────────────────────────────────

/// The CR 603.5 "may" SLOT the journal keys on. The source half is built the way
/// `game::engine::object_decision_source` builds it (CR 400.7: `ThisObject` bound to the
/// object's CURRENT incarnation, `trigger_description` held `None`) and is reconstructed
/// here rather than called because the engine's helper is `pub(crate)`; every row that uses
/// it asserts the reconstruction is faithful by requiring the production write site to have
/// stored something under it. The SUB-INDEX half is not reconstructed at all — it comes from
/// the engine's own `DecisionSlot::may`, the same constructor the publisher and the
/// `DecideOptionalEffect` writer use, so this key cannot drift from theirs.
fn may_source_key(
    state: &GameState,
    source_id: ObjectId,
) -> engine::analysis::decision_template::DecisionSlot {
    engine::analysis::decision_template::DecisionSlot::may(
        engine::types::game_state::YieldTarget::ThisObject {
            source_id,
            incarnation: Some(state.objects[&source_id].incarnation),
            trigger_description: None,
        },
    )
}

/// How the drive answers CR 603.5 "may" prompts. Typed rather than a pair of `bool`s: the
/// three rows below need three genuinely different drive shapes, and each is named.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MayPolicy {
    /// Take every prompt and drive on to the bounded offer — the shipped F4 policy.
    TakeAll,
    /// Take every prompt, and STOP at the first prompt that repeats a (source, seat) pair.
    TakeUntilRepeat,
    /// Take every prompt, then DECLINE the first prompt that repeats a (source, seat) pair,
    /// and stop there.
    DeclineOnRepeat,
}

/// One answered "may" prompt, as the drive saw it.
struct MayBeat {
    key: engine::analysis::decision_template::DecisionSlot,
    seat: PlayerId,
    take: bool,
    /// The journal entry for this (source, seat) pair BEFORE this beat was answered — the
    /// evidence that a "repeat" beat really is a repeat.
    before: Option<engine::analysis::decision_template::LoopAnswer>,
}

/// Drive the F4 dump under `policy`, answering "may" prompts directly (so the row controls
/// the answer) and delegating every other beat to [`f4_drive_one_beat`].
///
/// The repeat-stopping policies stop AT the beat that lands, deliberately: a later
/// deliberate action or non-forced window would clear the ring, and the journal follows it.
fn drive_f4_may_beats(state: &mut GameState, cap: u32, policy: MayPolicy) -> Vec<MayBeat> {
    let mut beats: Vec<MayBeat> = Vec::new();
    for _ in 0..cap {
        if matches!(state.waiting_for, WaitingFor::LoopShortcut { .. }) {
            return beats;
        }
        let prompt = match &state.waiting_for {
            WaitingFor::OptionalEffectChoice {
                player, source_id, ..
            } => Some((*player, *source_id)),
            _ => None,
        };
        let Some((seat, source_id)) = prompt else {
            if f4_drive_one_beat(state).is_err() {
                return beats;
            }
            continue;
        };
        let key = may_source_key(state, source_id);
        let repeat = beats.iter().any(|b| b.key == key && b.seat == seat);
        let take = !(repeat && policy == MayPolicy::DeclineOnRepeat);
        let before = state.loop_answer(&key, seat);
        if apply(
            state,
            seat,
            GameAction::DecideOptionalEffect { accept: take },
        )
        .is_err()
        {
            return beats;
        }
        beats.push(MayBeat {
            key,
            seat,
            take,
            before,
        });
        if repeat && policy != MayPolicy::TakeAll {
            return beats;
        }
    }
    beats
}

/// **Row 1′.** CR 603.5 + CR 732.2a: at the real F4 bounded offer, every published
/// `MayChoice` point's source has a journal entry UNDER THE PROPOSER'S OWN KEY.
///
/// `proposer` is bound from the minted `WaitingFor::LoopShortcut`, never hard-coded: the
/// publisher filters the published may slot on `gate.prompt_player == proposer`, so
/// `(source, proposer)` is precisely the key that is supposed to exist, and a hard-coded
/// seat would read `None` and red this row for the wrong reason.
///
/// # Discrimination
///
/// Delete the `record_loop_answer` call from the `DecideOptionalEffect` reducer arm ⇒ the
/// journal stays empty ⇒ `loop_answers_recorded() > 0` fails and every lookup returns
/// `None`. Weaken the gate the other way (record under a fixed seat) ⇒ the per-point
/// lookups fail for any board whose prompt seat is not the proposer.
///
/// # Reach-guards
///
/// * the restored dump starts with an EMPTY journal, so every entry is one this drive wrote;
/// * the drive really answered at least one "may" prompt;
/// * the offer really published at least one `MayChoice` point — without this the `for` loop
///   below is empty and the row would pass on a board it never tested.
#[test]
fn c1_row1_the_may_journal_is_populated_at_the_f4_offer_under_the_proposers_own_key() {
    use engine::analysis::decision_template::{LoopAnswer, LoopAnswerValue, MayChoiceOption};

    let mut state = load_f4();
    assert_eq!(
        state.loop_answers_recorded(),
        0,
        "reach-guard: the restored dump starts with an EMPTY journal"
    );

    let beats = drive_f4_may_beats(&mut state, 400, MayPolicy::TakeAll);
    assert!(
        !beats.is_empty(),
        "reach-guard: the drive must have answered at least one CR 603.5 `may` prompt, else \
         there is no write for this row to observe"
    );

    let (proposer, _certificate, schema) = offer_parts(&state);
    // The WHOLE published slot, sub-index included — the journal is keyed on it, so
    // projecting it down to `slot.source` here would test a coarser identity than the one
    // production writes and reads.
    let may_slots: Vec<_> = schema
        .points
        .iter()
        .filter(|p| matches!(p.kind, DecisionPointKind::MayChoice))
        .map(|p| p.slot.clone())
        .collect();
    assert!(
        !may_slots.is_empty(),
        "reach-guard: the offer must publish at least one MayChoice point (r1b measures \
         three points on this board), else the per-point assertions below are vacuous"
    );
    assert!(
        state.loop_answers_recorded() > 0,
        "CR 603.5: the offer beat must carry the answers the drive gave"
    );
    for slot in &may_slots {
        assert_eq!(
            state.loop_answer(slot, proposer),
            Some(LoopAnswer::Uniform(LoopAnswerValue::May(
                MayChoiceOption::Take
            ))),
            "every published may point's slot must be journalled under the PROPOSER's own \
             key; slot {slot:?}, proposer {proposer:?}, journal holds {} entries",
            state.loop_answers_recorded()
        );
    }
}

/// **Row T1 — WIRE / JOURNAL TIER.** CR 608.2b + CR 601.2c (reached via CR 603.3d) +
/// CR 732.2a: at the real F4 bounded offer, the published `Targets` point's SLOT carries the
/// announcement the proposer actually made, under the proposer's own key.
///
/// Every beat crosses the public `apply()` boundary; the slot is bound from `schema.points`
/// and the pinned seat from the drive policy's own aim, so a re-dump that renumbers objects
/// flows through without edit.
///
/// # Discrimination
///
/// Delete the `record_trigger_target_answer(..)` call from `apply_action`'s
/// `(TriggerTargetSelection, ChooseTarget)` arm ⇒ the `Targets` slot is never journalled and
/// the value assertion reads `None`. The helper and its `SelectTargets` caller survive, so
/// the mutation COMPILES and reds on the assert. The `SelectTargets` arm is covered at a
/// DIFFERENT TIER by `loop_shortcut.rs`'s
/// `c2a_row_t1b_both_trigger_target_selection_arms_route_through_the_single_writer`, which is
/// a SOURCE CENSUS: it asserts that both reducer arms are WIRED to the single writer, and
/// structurally cannot observe an announced seat (no fixture in this repo reaches the
/// `SelectTargets` arm — that row's own doc records the per-dump measurement and the backlog
/// item). The two deletions are ASYMMETRIC, and the asymmetry is the usable part: deleting the
/// `SelectTargets` call reds ONLY the census, while deleting the `ChooseTarget` call reds BOTH —
/// so a red census names the arm, and this row disambiguates which one moved. The census cannot
/// be blind to either arm: it asserts `unwired.is_empty()` across both.
///
/// # Sibling (T1-sib), asserted in this same body
///
/// After that mutation the two `MayChoice` points still read `Uniform(May(Take))`, so the
/// deletion is TARGET-SPECIFIC and cannot be confused with a journal that stopped working.
///
/// # Reach-guards, all asserted BEFORE the claim
///
/// * the restored dump starts with an EMPTY journal, so every entry is one this drive wrote;
/// * the drive really reaches the CR 732.2a offer beat (searched, never hardcoded);
/// * the offer really publishes a `Targets` point — without this the loop below is empty;
/// * the drive's aimed seat is NOT the proposer's own seat, so a writer that journalled the
///   proposer instead of the announcement could not pass.
///
/// # What this row does NOT claim
///
/// It is a WRITER row. C2a ships no declaration consumer, so nothing here asserts that a
/// declaration is built from these entries.
#[test]
fn c2a_row_t1_the_announced_target_is_journalled_at_the_f4_offers_published_slot() {
    use engine::analysis::decision_template::{
        AnnouncementSubject, LoopAnswer, LoopAnswerValue, MayChoiceOption, Ranking, TargetPin,
        TargetSchedule,
    };

    let mut state = load_f4();
    assert_eq!(
        state.loop_answers_recorded(),
        0,
        "reach-guard: the restored dump starts with an EMPTY journal"
    );

    let beat = drive_f4_to_offer(&mut state, 400)
        .expect("reach-guard: the F4 drive must reach the CR 732.2a bounded offer");
    let (proposer, _certificate, schema) = offer_parts(&state);

    let target_slots: Vec<_> = schema
        .points
        .iter()
        .filter(|p| matches!(p.kind, DecisionPointKind::Targets { .. }))
        .map(|p| p.slot.clone())
        .collect();
    assert!(
        !target_slots.is_empty(),
        "reach-guard: the offer must publish at least one CR 601.2c Targets point at beat \
         {beat}, else the per-point assertion below is vacuous"
    );
    // `P1` is the seat `f4_drive_one_beat` aims Torch's "target opponent" at. It must not be
    // the proposer, or a writer that journalled the PROMPT'S OWN SEAT rather than the
    // ANNOUNCED target would satisfy this row.
    assert_ne!(
        P1, proposer,
        "reach-guard: the drive's aimed seat must differ from the proposer's own seat"
    );

    for slot in &target_slots {
        assert_eq!(
            state.loop_answer(slot, proposer),
            Some(LoopAnswer::Uniform(LoopAnswerValue::Targets(vec![
                TargetPin::Scheduled(TargetSchedule::Constant(Ranking::one(
                    AnnouncementSubject::Seat(P1)
                )))
            ]))),
            "CR 608.2b: the published Targets slot must hold the announcement the drive made \
             (a constant CR 115.2 player target, in the CR 601.2c TARGET-class spelling), \
             under the PROPOSER's own key; slot \
             {slot:?}, proposer {proposer:?}, journal holds {} entries",
            state.loop_answers_recorded()
        );
    }

    // ── T1-sib: the CR 603.5 axis is untouched by the target axis's write ──
    let may_slots: Vec<_> = schema
        .points
        .iter()
        .filter(|p| matches!(p.kind, DecisionPointKind::MayChoice))
        .map(|p| p.slot.clone())
        .collect();
    assert!(
        !may_slots.is_empty(),
        "reach-guard: this board publishes MayChoice points too, else the sibling assertion \
         below is vacuous"
    );
    for slot in &may_slots {
        assert_eq!(
            state.loop_answer(slot, proposer),
            Some(LoopAnswer::Uniform(LoopAnswerValue::May(
                MayChoiceOption::Take
            ))),
            "T1-sib: deleting the target write must leave C1's CR 603.5 axis green — the two \
             axes share one journal but not one entry"
        );
    }
}

/// **Row T1-P — WIRE / PROVENANCE.** The journalled pin FOLLOWS THE ANNOUNCEMENT, not a
/// constant: driving the SAME dump with the SAME policy but a different aimed seat produces a
/// different journal value at the same published slot.
///
/// # Why this row exists at all — the vacuity it closes
///
/// [`c2a_row_t1_the_announced_target_is_journalled_at_the_f4_offers_published_slot`] drives
/// the shipped policy, which aims at P1. A writer that IGNORED the announcement and stored
/// the constant seat P1 would satisfy it exactly. Only a second seat discriminates that, and it
/// must be a REAL drive: the seat is announced through production `apply()` at Torch's
/// CR 601.2c choice, never injected.
///
/// # Discrimination
///
/// In `record_trigger_target_answer`, replace the mapped `targets` with
/// `vec![TargetPin::Scheduled(TargetSchedule::Constant(Ranking::one(AnnouncementSubject::Seat(PlayerId(1)))))]`
/// ⇒ this row reds on the value while T1 stays GREEN. That asymmetry is the point: T1 alone
/// cannot see this mutation. The mutant is spelled in the CURRENT producer spelling on purpose:
/// the discrimination is seat-vs-seat and survives any re-spelling, but a recipe naming a
/// spelling the producer no longer emits is a recipe that no longer compiles.
///
/// # Reach-guards
///
/// The P2 drive must reach the offer (MEASURED: constant P1, P2 and P3 all certify — it is
/// the variation between iterations, not the seat, that blocks certification), the offer must
/// publish a `Targets` point, and the aimed seat must differ from T1's.
#[test]
fn c2a_row_t1p_the_journalled_pin_follows_the_announced_seat_not_a_constant() {
    use engine::analysis::decision_template::{
        AnnouncementSubject, LoopAnswer, LoopAnswerValue, Ranking, TargetPin, TargetSchedule,
    };

    const AIMED: PlayerId = PlayerId(2);
    assert_ne!(
        AIMED, P1,
        "reach-guard: this row's aimed seat must differ from the shipped policy's, else it \
         re-runs T1 and discriminates nothing"
    );

    let mut state = load_f4();
    assert_eq!(
        state.loop_answers_recorded(),
        0,
        "reach-guard: the restored dump starts with an EMPTY journal"
    );
    let beat = drive_f4_to_offer_at(&mut state, 400, AIMED).expect(
        "reach-guard: a CONSTANT non-P1 target still certifies — it is the VARIATION between \
         iterations, not the seat, that blocks the CR 732.2a offer",
    );
    let (proposer, _certificate, schema) = offer_parts(&state);
    assert_ne!(
        AIMED, proposer,
        "reach-guard: the aimed seat must not be the proposer's own"
    );

    let target_slots: Vec<_> = schema
        .points
        .iter()
        .filter(|p| matches!(p.kind, DecisionPointKind::Targets { .. }))
        .map(|p| p.slot.clone())
        .collect();
    assert!(
        !target_slots.is_empty(),
        "reach-guard: the offer at beat {beat} must publish a CR 601.2c Targets point"
    );
    for slot in &target_slots {
        assert_eq!(
            state.loop_answer(slot, proposer),
            Some(LoopAnswer::Uniform(LoopAnswerValue::Targets(vec![
                TargetPin::Scheduled(TargetSchedule::Constant(Ranking::one(
                    AnnouncementSubject::Seat(AIMED)
                )))
            ]))),
            "PROVENANCE: the journal must hold the seat this drive ANNOUNCED ({AIMED:?}), not \
             the seat the shipped policy happens to aim at; slot {slot:?}"
        );
    }
}

/// The declaration the live offer PUBLISHES. A separate accessor rather than a fourth element
/// on [`offer_parts`], so the ~20 existing callers of that helper are untouched.
fn offer_declaration(
    state: &GameState,
) -> Option<engine::analysis::decision_template::DecisionTemplate> {
    match &state.waiting_for {
        WaitingFor::LoopShortcut { declaration, .. } => declaration.clone(),
        other => panic!("expected the CR 732.2a bounded offer, got {other:?}"),
    }
}

/// **Row D1 — WIRE / CONFORMANCE.** The bounded offer publishes a `Some` declaration that
/// CONFORMS to the reference shape this suite already accepts, on all three tracked dumps.
///
/// ⚠ **THIS IS A CONFORMANCE ORACLE, NEVER A PROVENANCE ONE.** [`f4_pin_template`] is a pure
/// function of `(schema, owner, count)` — it hard-codes `MayChoiceOption::Take` and the seat P1
/// (as `Scheduled(Constant(Ranking::one(AnnouncementSubject::Seat(P1))))`, the CR 601.2c
/// TARGET-class spelling the publisher emits) and never reads the journal — so a consumer that
/// ignored the journal entirely and emitted those same constants passes this row. That is exactly what
/// [`d1p_the_published_pin_follows_the_journal_not_a_constant`] and its P3 sibling are for.
///
/// # The count trap, measured
///
/// The reference must be built with `count = schema.max_iterations`, NOT the `1` every other
/// declare row in this file passes: `build_bounded_declaration` sets
/// `replay: Scheduled { count: schema.iteration_count }`, and `certified_bounded_cycle_offer`
/// builds the schema with `IterationCount::Fixed(max_iterations)`. Measured on all three boards:
/// `REAL == f4_pin_template(count = 1)` is FALSE and `REAL == f4_pin_template(count = max)` is
/// TRUE.
///
/// # Reach-guards, asserted BEFORE the claim
///
/// The journal holds at least one answer per published point (else `declaration.is_some()`
/// could only ever be the empty-schema path), the point count is the board's known one, and the
/// bound is the board's measured `max_iterations` — which is also the reference's count.
///
/// REVERT-PROBE: make `build_bounded_declaration`'s `(Targets, Targets)` arm `return None` ⇒
/// `is_some()` flips on all three boards.
#[test]
fn d1_the_bounded_offer_publishes_a_conformant_declaration_on_every_tracked_dump() {
    use engine::analysis::decision_template::{predictability_gate, validate_pins};

    for (label, mut state, expected_points, expected_max) in [
        ("F4", load_f4(), 3usize, 18u32),
        ("MODE1", load_mode1(), 2, 17),
        ("MODE2", load_mode2(), 3, 16),
    ] {
        let beat = drive_f4_to_offer(&mut state, 400)
            .unwrap_or_else(|| panic!("[{label}] REACH-GUARD: the bounded offer must FIRE"));
        let (proposer, _certificate, schema) = offer_parts(&state);
        let schema = schema.clone();

        assert!(
            state.loop_answers_recorded() >= schema.points.len(),
            "[{label}] REACH-GUARD: every published point must have an answer in the journal, \
             else a `Some` declaration below could not be about this schema at all. recorded={} \
             points={}",
            state.loop_answers_recorded(),
            schema.points.len()
        );
        assert_eq!(
            schema.points.len(),
            expected_points,
            "[{label}] REACH-GUARD: the published point count at beat {beat}"
        );
        assert_eq!(
            schema.max_iterations, expected_max,
            "[{label}] REACH-GUARD: the CR 704.5a-derived bound — and the count the reference \
             below must be built with"
        );

        let declaration = offer_declaration(&state)
            .unwrap_or_else(|| panic!("[{label}] the offer publishes a declaration"));
        assert_eq!(
            declaration,
            f4_pin_template(&schema, proposer, schema.max_iterations),
            "[{label}] CR 732.2a: the published declaration must CONFORM to the shape this \
             suite's accepted declarations take — one pin per published point, owner == \
             proposer, `replay.count` == the offer's own suggestion"
        );

        let required: Vec<_> = schema.points.iter().map(|p| p.slot.clone()).collect();
        assert!(
            predictability_gate(&declaration, &required).is_ok(),
            "[{label}] the published declaration covers every published slot — the coverage half \
             of the declare-time firewall"
        );
        assert!(
            validate_pins(&schema, &declaration, 1, &state).is_ok(),
            "[{label}] and its pin VALUES are legal at iteration 1"
        );
        assert!(
            validate_pins(&schema, &declaration, schema.max_iterations, &state).is_ok(),
            "[{label}] and at the full declared range — the count the AI's candidate carries"
        );
    }
}

/// **Row D1-P — WIRE / PROVENANCE.** The declaration's pinned target FOLLOWS THE JOURNAL, not a
/// constant: driving the SAME dump with the SAME policy but a different aimed seat publishes a
/// different pin at the same published slot.
///
/// This is the CONSUMER-tier sibling of
/// [`c2a_row_t1p_the_journalled_pin_follows_the_announced_seat_not_a_constant`] (the WRITER-tier
/// row) and reuses its two helpers, so the drive is production `apply()` and the seat is
/// ANNOUNCED at Torch's CR 601.2c choice, never injected.
///
/// # The asymmetry IS the row
///
/// On the shipped P1 board, replacing the journalled targets with the constant seat P1
/// (`vec![TargetPin::Scheduled(TargetSchedule::Constant(Ranking::one(AnnouncementSubject::Seat(PlayerId(1)))))]`)
/// is GREEN — that mutant is indistinguishable there. At a second seat it is RED. Only a second
/// seat discriminates a journal-blind consumer.
///
/// # Reach-guards, asserted BEFORE the claim
///
/// The offer fires at the aimed seat, the point set is the known one, the aimed seat is not the
/// proposer's own (or a writer that stored the PROMPT's seat would satisfy the row), and the
/// `Targets` point's journal entry already reads the aimed seat before the consumer is called.
///
/// REVERT-PROBE: in `build_bounded_declaration`'s `(Targets, Targets)` arm, replace the
/// journalled `targets` with the same constant-P1 vector named above ⇒ this row flips on the pin
/// VALUE while D1 stays green.
///
/// *What wrong implementation would still pass this row?* One that reads the journal but ignores
/// `point.slot` — there is one `Targets` point here, so the slot axis is D1-P-may's and D3's.
#[test]
fn d1p_the_published_pin_follows_the_journal_not_a_constant() {
    d1p_provenance_at_seat(PlayerId(2));
}

/// **Row D1-P-sib** — the same claim at a THIRD seat, so the provenance cannot be a coincidence
/// of one seat's numbering.
#[test]
fn d1p_sib_the_published_pin_provenance_is_not_specific_to_one_second_seat() {
    d1p_provenance_at_seat(PlayerId(3));
}

fn d1p_provenance_at_seat(aimed: PlayerId) {
    use engine::analysis::decision_template::{
        validate_pins, AnnouncementSubject, LoopAnswer, LoopAnswerValue, PinnedDecision, Ranking,
        TargetPin, TargetSchedule,
    };
    // CR 601.2c: the one spelling this row expects at BOTH tiers — the journal's own write and
    // the declaration the publisher derives from it. Built once so the two `assert_eq!`s below
    // cannot drift apart; it is still a fully-determined VALUE, not a pattern.
    let announced_seat = TargetPin::Scheduled(TargetSchedule::Constant(Ranking::one(
        AnnouncementSubject::Seat(aimed),
    )));

    assert_ne!(
        aimed, P1,
        "reach-guard: the aimed seat must differ from the shipped policy's, else this re-runs D1 \
         and discriminates nothing"
    );
    let mut state = load_f4();
    let beat = drive_f4_to_offer_at(&mut state, 400, aimed).expect(
        "reach-guard: a CONSTANT non-P1 target still certifies — it is the VARIATION between \
         iterations, not the seat, that blocks the CR 732.2a offer",
    );
    let (proposer, _certificate, schema) = offer_parts(&state);
    let schema = schema.clone();
    assert_ne!(
        aimed, proposer,
        "reach-guard: the aimed seat must not be the proposer's own"
    );
    assert_eq!(
        schema.points.len(),
        3,
        "reach-guard: the published point set at beat {beat}"
    );

    let target_slot = schema
        .points
        .iter()
        .find(|p| matches!(p.kind, DecisionPointKind::Targets { .. }))
        .map(|p| p.slot.clone())
        .expect("reach-guard: the offer publishes a CR 601.2c Targets point");
    // The WRITER's own output, asserted BEFORE the consumer runs: without this the row could not
    // tell "the consumer ignored the journal" from "the journal never held the aimed seat".
    assert_eq!(
        state.loop_answer(&target_slot, proposer),
        Some(LoopAnswer::Uniform(LoopAnswerValue::Targets(vec![
            announced_seat.clone()
        ]))),
        "reach-guard: the journal holds the ANNOUNCED seat {aimed:?} at the published slot"
    );

    let declaration = offer_declaration(&state).expect("the offer publishes a declaration");
    let pinned = declaration
        .decisions
        .iter()
        .find_map(|pin| match pin {
            PinnedDecision::Targets { slot, targets } if *slot == target_slot => Some(targets),
            _ => None,
        })
        .expect("the declaration pins the published Targets slot");
    assert_eq!(
        *pinned,
        vec![announced_seat],
        "PROVENANCE: the declaration must pin the seat this drive ANNOUNCED ({aimed:?}), not the \
         seat the shipped policy happens to aim at"
    );
    assert!(
        validate_pins(&schema, &declaration, 1, &state).is_ok(),
        "and the journal-derived pin is LEGAL against the offer's own schema — otherwise a \
         provenance-correct consumer could still be publishing an unusable declaration"
    );
}

/// **Row 2b — JOURNAL TIER.** CR 603.5: ONE seat answering ONE source two different ways
/// inside one detection window latches [`LoopAnswer::Conflicted`].
///
/// ⚠ TIER LIMIT, stated rather than implied: C1 ships no declaration consumer, so this row
/// asserts the LATCH, not a refused declaration. The declaration-tier half — that a
/// `Conflicted` entry makes `build_bounded_declaration` return `None` on this same board —
/// belongs to C2 and is NOT covered here.
///
/// The same-seat constraint is asserted in the body, not assumed: under the pair key two
/// DIFFERENT seats answering one source land in two entries and the `Entry::Occupied` arm is
/// never entered at all, which would make this row vacuous.
///
/// # Discrimination
///
/// Delete `record_loop_answer`'s `Entry::Occupied` conflict arm (let a second write be
/// ignored, or overwrite) ⇒ the entry stays `Uniform { take: Take }` ⇒ the final assertion
/// flips. MEASURED, not predicted — see this row's companion probe in the implementation
/// report.
///
/// # Paired positive / reach-guard
///
/// `before` on the conflicting beat must already be `Uniform { Take }`: that proves the beat
/// really was a REPEAT of an already-journalled pair, so a drive that never repeated cannot
/// satisfy this row.
#[test]
fn c1_row2b_one_seat_answering_one_source_two_ways_latches_conflicted() {
    use engine::analysis::decision_template::{LoopAnswer, LoopAnswerValue, MayChoiceOption};

    let mut state = load_f4();
    let beats = drive_f4_may_beats(&mut state, 400, MayPolicy::DeclineOnRepeat);
    let last = beats
        .last()
        .expect("the drive must have answered at least one `may` prompt");
    assert!(
        !last.take,
        "reach-guard: the drive must have REACHED a repeated (source, seat) prompt and \
         declined it; it answered {} prompts and the last was a Take",
        beats.len()
    );

    let first = beats
        .iter()
        .find(|b| b.key == last.key && b.seat == last.seat && b.take)
        .expect("the repeat's own first answer must be in the drive's record");
    assert_eq!(
        first.seat, last.seat,
        "SAME-SEAT CONSTRAINT: both answers must come from one seat. Two seats occupy two \
         keys, never enter the conflict arm, and would make this row vacuous"
    );
    assert_eq!(
        last.before,
        Some(LoopAnswer::Uniform(LoopAnswerValue::May(
            MayChoiceOption::Take
        ))),
        "paired positive: the FIRST answer was journalled as Uniform(May(Take)) before the \
         differing one landed"
    );
    assert_eq!(
        state.loop_answer(&last.key, last.seat),
        Some(LoopAnswer::Conflicted),
        "CR 603.5: a second, DIFFERENT answer from the same seat for the same source latches \
         Conflicted (an engine-capability refusal, not a CR 732.2a mandate)"
    );
}

/// **Row 2b sibling — idempotence.** The latch fires on DISAGREEMENT, not on repetition: the
/// same seat answering the same source the same way twice stays `Uniform`.
///
/// Without this sibling, a `record_loop_answer` that latched `Conflicted` on EVERY repeat
/// would pass row 2b and destroy every real board — the F4 drive answers each may source
/// once per iteration.
///
/// Discrimination: replace the conflict arm's `if *o.get() != answer` with an unconditional
/// `o.insert(LoopAnswer::Conflicted)` ⇒ this row reds while row 2b stays green.
#[test]
fn c1_row2b_sibling_an_identical_second_answer_stays_uniform() {
    use engine::analysis::decision_template::{LoopAnswer, LoopAnswerValue, MayChoiceOption};

    let mut state = load_f4();
    let beats = drive_f4_may_beats(&mut state, 400, MayPolicy::TakeUntilRepeat);
    let last = beats
        .last()
        .expect("the drive must have answered at least one `may` prompt");
    assert_eq!(
        last.before,
        Some(LoopAnswer::Uniform(LoopAnswerValue::May(
            MayChoiceOption::Take
        ))),
        "reach-guard: the last beat must be a REPEAT of an already-journalled pair, else this \
         row asserts idempotence over a single write"
    );
    assert_eq!(
        state.loop_answer(&last.key, last.seat),
        Some(LoopAnswer::Uniform(LoopAnswerValue::May(
            MayChoiceOption::Take
        ))),
        "an identical second answer must not latch Conflicted"
    );
}

/// **Row 7b″.** The journal is invalidated with `loop_detect_ring`, ON THE SAME RECEIVER.
///
/// Three of the eight ring-clear sites act on a `clone`/`self` rather than on `state`, so a
/// journal clear applied to the wrong receiver would leave a stored sample carrying the live
/// window's answers. Sites 6 and 7 are only observable downstream, through
/// `LoopDetectSample`'s `pub normalized` / `pub live` halves on the ring — this row asserts
/// there, simultaneously with the LIVE state being non-empty, so no single-receiver bug
/// satisfies both halves.
///
/// Site 5 (`apply_action`'s pre-action clear, a `state` receiver) is driven directly.
/// Sites 1–4 and 8 are covered structurally instead, by
/// [`c1_every_ring_clear_site_also_clears_the_loop_answer_journal`] — stated here so the coverage of
/// this row is not read as more than it is.
///
/// # Discrimination
///
/// Delete `clone.loop_answer_journal = None;` from `normalize_for_loop` or from
/// `loop_detect_live_sample` ⇒ the corresponding per-sample assertion flips. Delete it from
/// `apply_action`'s clear block ⇒ the final assertion flips.
#[test]
fn c1_row7b_the_may_journal_follows_the_ring_on_the_same_receiver() {
    let mut state = load_f4();
    drive_f4_may_beats(&mut state, 400, MayPolicy::TakeAll);
    let (proposer, _certificate, _schema) = offer_parts(&state);

    assert!(
        state.loop_answers_recorded() > 0,
        "paired positive: the LIVE state must carry answers at the offer beat, else every \
         zero below is satisfied by a journal that was never written"
    );
    assert!(
        !state.loop_detect_ring.is_empty(),
        "reach-guard: there must be stored samples to inspect"
    );
    for (i, sample) in state.loop_detect_ring.iter().enumerate() {
        assert_eq!(
            sample.normalized.loop_answers_recorded(),
            0,
            "site 6 (`normalize_for_loop`, CLONE receiver): stored sample {i}'s normalized \
             half must not carry the live window's answers"
        );
        assert_eq!(
            sample.live.loop_answers_recorded(),
            0,
            "site 7 (`loop_detect_live_sample`, CLONE receiver): stored sample {i}'s live \
             half must not carry the live window's answers"
        );
    }

    apply(&mut state, proposer, GameAction::DeclineShortcut)
        .expect("declining the offer is always legal for the proposer");
    assert!(
        state.loop_detect_ring.is_empty(),
        "reach-guard: site 5's ring clear must actually have fired on this action, else the \
         journal zero below is not evidence about that clear"
    );
    assert_eq!(
        state.loop_answers_recorded(),
        0,
        "site 5 (`apply_action`, STATE receiver): the journal follows the ring"
    );
}

/// **Row 7c.** The journal never crosses save/load as stale data.
///
/// `last_loop_action_sequence` fell into exactly this trap once; `#[serde(skip, default)]`
/// is the bar, and this row asserts BOTH halves of it — the field is absent from the encoded
/// payload, and a decode of a populated board restores an empty journal.
///
/// Discrimination: drop `skip` from the field's serde attribute ⇒ the key appears in the
/// encoded value ⇒ the first assertion flips (and NEITHER `LoopAnswer` NOR `LoopAnswerValue`
/// derives `Serialize`, so that edit does not even compile — which is the point of the note
/// on the field; the compile-time bar had to be re-checked when the value type grew a second
/// axis, and this row is the runtime half of it).
#[test]
fn c1_row7c_the_may_journal_does_not_cross_save_load() {
    let mut state = load_f4();
    drive_f4_may_beats(&mut state, 400, MayPolicy::TakeAll);
    assert!(
        state.loop_answers_recorded() > 0,
        "reach-guard: the board being serialized must have a POPULATED journal, else the \
         empty restore below proves nothing"
    );

    let encoded = serde_json::to_value(&state).expect("a live GameState serializes");
    assert!(
        encoded.get("loop_answer_journal").is_none(),
        "`#[serde(skip)]`: the journal must be absent from the encoded payload entirely"
    );
    let restored = serde_json::from_value::<PersistedGameState>(encoded)
        .expect("the encoded board decodes through the production decoder")
        .into_game_state();
    assert_eq!(
        restored.loop_answers_recorded(),
        0,
        "a restored board must start its own window with no inherited answers"
    );
}

/// **Row 7b″, structural half.** EVERY production `loop_detect_ring.clear()` is paired with
/// a `loop_answer_journal = None` on the same receiver, at all eight sites.
///
/// The driven row above reaches sites 5, 6 and 7 on the F4 board; sites 1–4 and 8 need
/// materialize / until-lethal / pipeline / unobserved-life-move boards that this fixture does
/// not produce. A source-level census covers the whole set at the only tier that can, and
/// fails loudly if a NINTH clear site is added without the journal, which is the actual
/// regression this guards.
///
/// THE WALK IS THE WHOLE CRATE, not a named pair of files. A hard-coded
/// `["game/engine.rs", "types/game_state.rs"]` cannot see a ninth site in any THIRD file: such
/// a site is neither paired nor reported, so `paired == 8` still passes while the regression is
/// live. MEASURED on this tree: the recursive walk finds exactly the 8 sites the named pair did
/// (5 in `game/engine.rs`, 3 in `types/game_state.rs`), so THE COUNT ASSERTION IS BLIND TO THE
/// WIDENING — the planted-third-file probe below is the only thing that measures it.
///
/// Discrimination, BOTH DIRECTIONS, RUN:
/// * delete any one `loop_answer_journal = None;` that follows a ring clear ⇒ the pairing count
///   drops and this row reds naming the file and line;
/// * add an unpaired `state.loop_detect_ring.clear();` to a THIRD file under
///   `crates/engine/src` ⇒ `unpaired` names that file and this row reds. Under the named-pair
///   walk the identical plant left the row GREEN.
#[test]
fn c1_every_ring_clear_site_also_clears_the_loop_answer_journal() {
    use std::path::Path;

    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut unpaired: Vec<String> = Vec::new();
    let mut paired = 0usize;
    // The walker is the sibling census's, not a second copy: one home for "every `.rs` file
    // under a root", already shared by the census rows in this binary.
    for path in super::loop_shortcut_offer_writer_census::rs_files(&src) {
        let rel = path
            .strip_prefix(&src)
            .expect("walked path is under src")
            .to_string_lossy()
            .replace('\\', "/");
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
        let lines: Vec<&str> = text.lines().collect();
        // Both halves read the CODE of a line, never its comment: prose neither clears the ring
        // nor clears the journal. Whole-line-only exclusion is not enough here and the failure is
        // two-sided — a comment naming the clear would be counted as a site, and a comment naming
        // `loop_answer_journal = None` inside a window would mark a genuinely UNPAIRED site as
        // paired, which is the direction that hides the regression. Shared rule, one home:
        // `src/source_census.rs`, the same file the crate's own unit-test censuses use.
        use super::source_census::code;
        for (i, line) in lines.iter().enumerate() {
            if !code(line).contains("loop_detect_ring.clear()") {
                continue;
            }
            // The journal assignment sits within the same block, immediately after the ring
            // clear (a comment line may separate them).
            let window = lines[i + 1..(i + 5).min(lines.len())]
                .iter()
                .map(|l| code(l))
                .collect::<Vec<_>>()
                .join("\n");
            if window.contains("loop_answer_journal = None") {
                paired += 1;
            } else {
                unpaired.push(format!("{rel}:{}", i + 1));
            }
        }
    }
    assert!(
        unpaired.is_empty(),
        "every ring-clear site must also clear the CR 603.5 + CR 608.2b loop-answer journal; \
         unpaired: \
         {unpaired:?}"
    );
    assert_eq!(
        paired, 8,
        "the ring has EIGHT production clear sites across the whole of `crates/engine/src` \
         (5 in game/engine.rs, 3 in types/game_state.rs; MEASURED by this recursive walk). A \
         different count means a site was added or removed and this census must be re-derived, \
         not re-numbered"
    );
}

// ─────────────────────────────────────────────────────────────────────────────────────────
// helpers used by more than one row
// ─────────────────────────────────────────────────────────────────────────────────────────

/// Count the stack entries whose triggered ability is CR 603.5 `optional`. Used as a reach
/// guard where a row's claim is about the optional gate.
fn optional_entries(state: &GameState) -> usize {
    state
        .stack
        .iter()
        .filter(|e| match &e.kind {
            StackEntryKind::TriggeredAbility { ability, .. } => ability.optional,
            _ => false,
        })
        .count()
}

// ─────────────────────────────────────────────────────────────────────────────────────────
// U6 — the AI's candidate set at the real F4 offer, and what the engine does with it
//
// Reachability of the seam under test: `phase_ai::search::choose_action` dispatches
// `WaitingFor::LoopShortcut { .. } => engine::ai_support::legal_actions(state)`
// (`crates/phase-ai/src/search.rs`), and `legal_actions` funnels into the
// `WaitingFor::LoopShortcut` arm of `engine::ai_support::candidates`. These rows drive the
// REAL dump to the REAL offer and measure that arm's output there, plus what
// `handle_declare_shortcut` does with each member of it.
//
// ⚠ MEASURED SCOPE. §5 U6 as planned expects a declare candidate "whose template pins all
// three F4 slots (or declines)". F4 does publish all THREE slots — `r1b` pins
// `[Sue MayChoice, Reed MayChoice, Torch Targets]` — and the measured answer is still the
// SECOND branch: the AI DECLINES, because the only declaration it can emit is one the engine
// refuses outright. The generator builds no pinning template at ALL (its only `Fixed` candidate
// carries `template: None`), so a published set of three is exactly as unreachable for it as a
// set of one would have been — the count is not what excludes it, its emptiness gate is. These
// rows pin that, name the two independent reasons, and pin the accepted shape the generator
// never emits — they do not assert the planned prediction.
// ─────────────────────────────────────────────────────────────────────────────────────────

/// **Row D6 — WIRE / POSITIVE.** At the real F4 bounded offer the AI candidate generator now
/// emits `DeclareShortcut { Fixed(max_iterations), Some(declaration) }` beside the decline, and
/// the `template` it carries IS THE OFFER'S OWN published declaration — not one the AI built.
///
/// ⚠ **THIS ROW'S PREVIOUS CLAIM WAS THE OPPOSITE, AND IT IS SUPERSEDED, NOT BROKEN.** As
/// `u6_the_ai_candidate_set_at_the_f4_offer_is_decline_only` it asserted
/// `assert_eq!(actions, vec![GameAction::DeclineShortcut])` — that the generator could offer no
/// declaration at all, because its only `Fixed` candidate carried `template: None` and a
/// published pin set fail-closes on that. Publishing the offer's own declaration is exactly the
/// capability item-4 C2b adds, so the old assertion asserted the ABSENCE of this commit's
/// subject. The name had to change with it: "decline only" is now false on this board.
///
/// # What is kept, and why
///
/// Both reach-guards survive verbatim and have flipped from exclusion conjuncts to POSITIVE
/// ones: `is_bounded()` is the count gate, and a NON-empty `points` set is what makes the
/// declaration (rather than the empty-schema `None`) the reason the candidate appears. The
/// `predicted_winner == None` guard stays as a measured property of this board.
///
/// # Non-vacuity
///
/// The template is asserted EQUAL to `offer_declaration(&state)`, never merely `Some(_)`: a
/// generator that fabricated its own conformant-looking template would satisfy `is_some()` and
/// fail this. `d6n_a_points_carrying_offer_without_a_declaration_enumerates_only_decline`
/// (in-crate, `ai_support/candidates.rs`) is the paired negative — with `declaration: None` the
/// candidate must NOT appear.
///
/// REVERT-PROBE: drop the `|| declaration.is_some()` disjunct from the generator's gate ⇒ the
/// candidate disappears against this points-carrying offer ⇒ the equality flips.
#[test]
fn d6_the_ai_declare_candidate_carries_the_offers_own_published_declaration() {
    let mut state = load_f4();
    drive_f4_to_offer(&mut state, 400).expect("the bounded offer fires (see R1)");
    let (proposer, _certificate, schema) = offer_parts(&state);
    let schema = schema.clone();
    let WaitingFor::LoopShortcut {
        predicted_winner, ..
    } = &state.waiting_for
    else {
        unreachable!("offer_parts would have panicked")
    };

    assert!(
        schema.is_bounded() && schema.max_iterations < MAX_SHORTCUT_CYCLES_MIRROR,
        "reach-guard: the generator's `Fixed` candidate is gated on `is_bounded()`, so an \
         unbounded offer would decide this row for the wrong reason. bounded={} max_it={}",
        schema.is_bounded(),
        schema.max_iterations
    );
    assert!(
        !schema.points.is_empty(),
        "reach-guard: a NON-empty published pin set is the conjunct this row is about — with \
         `points` empty the candidate appears regardless of the declaration"
    );
    assert_eq!(
        *predicted_winner, None,
        "reach-guard: the F4 offer latches NO predicted winner (a measured property of this \
         board, recorded so a future board swap is visible)"
    );
    let declaration = offer_declaration(&state).expect(
        "reach-guard: the offer PUBLISHES a declaration — that is the generator's new input",
    );

    // ── the seam: `phase-ai/src/search.rs` `WaitingFor::LoopShortcut { .. } =>` calls this ──
    let actions = engine::ai_support::legal_actions(&state);
    assert_eq!(
        actions,
        vec![
            GameAction::DeclareShortcut {
                count: IterationCount::Fixed(schema.max_iterations),
                template: Some(declaration.clone()),
            },
            GameAction::DeclineShortcut,
        ],
        "CR 732.2a: exactly two candidates. No `UntilLethal` declaration (gated on \
         `!schema.is_bounded()`, and this offer narrowed its bound to {}), and the `Fixed` \
         declaration carries the ENGINE'S OWN pin set for the {} published point(s)",
        schema.max_iterations,
        schema.points.len()
    );

    // Stated separately from the equality above so a future generator change that adds an
    // unrelated candidate reports the interesting fact rather than a diff of two long vectors.
    assert!(
        actions.iter().any(|a| matches!(
            a,
            GameAction::DeclareShortcut {
                count: IterationCount::Fixed(n),
                template: Some(t),
            } if *n == schema.max_iterations && *t == declaration
        )),
        "the candidate's template is the offer's own declaration, VALUE-EQUAL — a fabricated \
         template of the same shape would fail here and pass an `is_some()` check"
    );
    assert_eq!(
        proposer, P0,
        "every candidate is the proposer's own action (`ActionMetadata.actor`)"
    );
}

/// §5 U6 (ii) — the generator's OWN candidate now opens the CR 732.2b window, and the four
/// one-axis declare drives that say why.
///
/// ⚠ **THIS ROW'S PREVIOUS CLAIM WAS THAT THE CAPABILITY WAS ABSENT, AND IT IS SUPERSEDED, NOT
/// BROKEN.** As `u6_no_declaration_the_generator_can_emit_opens_the_window_while_the_accepted_
/// shape_is_one_it_never_builds` its candidate loop asserted that EVERY AI candidate lands on
/// `WaitingFor::Priority` — *"a `RespondToShortcut` here would mean the AI CAN open the
/// CR 732.2b window, which is the capability this row measures absent"*. That capability is
/// exactly what item-4 C2b adds, so the loop now asserts the complementary fact, still
/// RE-DERIVED from the generator rather than hand-named: the declare candidate opens the window
/// and the decline hands priority back.
///
/// **The four one-axis drives below still measure the engine-side guards the generator's gate
/// depends on, but one of them has FLIPPED, deliberately.** `Fixed(max) + None` used to be a
/// live fail-closed guard, on the stated grounds that resolving a `template: None` declaration
/// against the offer's own published declaration was a declare-handler change deferred out of
/// that commit's partition. Item-4 C2 IS that change: `handle_declare_shortcut` now resolves a
/// `None` template against `offer.declaration` before the `template.owner` firewall, so on this
/// board — which publishes a declaration — that arm is ACCEPTED and the `None if
/// …loop_period_controller() != Some(proposer)` arm is bypassed rather than reached. The arm is
/// kept, flipped, because it is the one row here that measures the manual ingress agreeing with
/// the AI ingress on one and the same offer. Its fail-closed sibling did not disappear — it
/// moved to the offer shape that still reaches it, which is
/// [`a_template_free_declaration_is_admitted_only_by_the_proposers_own_period`] (offer with
/// `declaration: None`).
///
/// Four declarations are driven through `apply()` on the SAME real offer board, differing one
/// axis at a time:
///
/// | declaration | measured |
/// |---|---|
/// | `UntilLethal` + `None` — **the shape the generator emitted before the bounded gate** | REFUSED ⇒ `Priority` |
/// | `UntilLethal` + a conformant template | REFUSED ⇒ `Priority` (so the refusal is keyed on the COUNT, not on the pins) |
/// | `Fixed(max)` + `None` | **ACCEPTED** ⇒ item-4 C2 resolves the `None` against the declaration this offer published, so the browser payload reaches the same window the AI's does |
/// | `Fixed(max)` + a conformant template | **ACCEPTED** ⇒ the CR 732.2b APNAP window opens |
///
/// The last row is the ANTI-VACUITY control: without it, "everything reaches `Priority`" would
/// be satisfied by a board that refuses every declaration for some unrelated reason. With it,
/// the two `UntilLethal` refusals are proved to be refusals of *those* declarations.
///
/// ⚠ This row deliberately does NOT assert what the accepted declaration then accomplishes —
/// that is [`r2a_an_accepted_declaration_commits_exactly_n_cycles_because_reeds_may_is_announced`]'s
/// job, and it now measures an exact `n`-repetition commit (it measured a zero commit while
/// Reed's `may` was unpublished). Splitting the two keeps this row a DECLARE-time matrix.
///
/// The `UntilLethal` rows are what justifies the generator's `!schema.is_bounded()` gate
/// ([`d6_the_ai_declare_candidate_carries_the_offers_own_published_declaration`]): the engine refuses that count
/// against a narrowed bound on a real board, so emitting it was offering the search layer an
/// action that is accepted-then-discarded. These rows keep measuring the ENGINE guard directly,
/// which is the fact the generator gate depends on and must not be allowed to rot.
///
/// REVERT-PROBES, both RUN, and the measured result CORRECTS the obvious prediction — the
/// count-free declaration is refused by TWO INDEPENDENT guards, so disabling either alone
/// leaves it refused:
///
/// * disable `IterationCount::UntilLethal if offer.schema.is_bounded()` in
///   `handle_declare_shortcut` ⇒ the *`UntilLethal` + conformant template* arm flips
///   (`Priority` → `RespondToShortcut`), while the AI's own `template: None` candidate stays
///   refused by the `None if last_loop_action_sequence.is_empty()` arm;
/// * disable BOTH ⇒ the AI-candidate loop itself flips — `UntilLethal` + `None` builds a
///   proposal and opens APNAP for `PlayerId(1)`.
///
/// The row asserts both arms for exactly that reason: a single-guard probe would report the
/// AI's candidate as still-refused and hide the change.
#[test]
fn u6_the_generators_own_candidate_opens_the_window_and_the_accepted_shape_is_measured() {
    let mut state = load_f4();
    drive_f4_to_offer(&mut state, 400).expect("the bounded offer fires (see R1)");
    let (proposer, _certificate, schema) = offer_parts(&state);
    let schema = schema.clone();
    let max = schema.max_iterations;

    assert!(
        state.last_loop_action_sequence.is_empty(),
        "the measured precondition that makes the `Fixed` + `None` arm below ATTRIBUTABLE: with \
         no recorded period at all, the `None if …loop_period_controller() != Some(proposer)` \
         arm would refuse this declaration on the pre-C2 engine, so that arm's acceptance is \
         attributable to item-4 C2's `or_else` and to nothing else on this board. len={}",
        state.last_loop_action_sequence.len()
    );
    assert!(
        offer_declaration(&state).is_some(),
        "and the other half of that attribution: the `or_else` can only accept because THIS \
         offer published a declaration to fall back to. An offer publishing `None` still \
         fail-closes — `a_template_free_declaration_is_admitted_only_by_the_proposers_own_period`"
    );

    // Every AI candidate, driven through the public boundary and dispatched on its own SHAPE,
    // so the expectation is re-derived from the generator rather than named by hand: a future
    // generator change at this node has to survive it.
    let candidates = engine::ai_support::legal_actions(&state);
    assert!(
        !candidates.is_empty(),
        "positive control: an EMPTY candidate set would satisfy the loop below vacuously"
    );
    let mut opened_the_window = 0usize;
    for action in candidates {
        let mut probe = state.clone();
        apply(&mut probe, proposer, action.clone()).expect("dispatched — refusal is a HANDBACK");
        match &action {
            GameAction::DeclareShortcut { .. } => {
                opened_the_window += 1;
                assert!(
                    matches!(probe.waiting_for, WaitingFor::RespondToShortcut { .. }),
                    "CR 732.2b: the generator's own declare candidate {action:?} must OPEN the \
                     accept-or-shorten window — it carries the engine's published declaration, \
                     which is the shape the accepted-control arm below proves the engine takes. \
                     A `Priority` here means the AI is enumerating an action the engine refuses. \
                     got {:?}",
                    probe.waiting_for
                );
            }
            _ => assert!(
                matches!(probe.waiting_for, WaitingFor::Priority { .. }),
                // CR 732.2a: a shortcut is a SUGGESTION made by the player who already has
                // priority, so refusing it takes no game action and that player still has
                // priority — `handle_decline_shortcut` re-seats `WaitingFor::Priority` and
                // cites the same rule. (Not CR 800.4a, which is player-elimination.)
                "CR 732.2a: the decline candidate {action:?} hands priority back, got {:?}",
                probe.waiting_for
            ),
        }
    }
    assert_eq!(
        opened_the_window, 1,
        "reach-guard for the loop above: EXACTLY ONE candidate is a declaration, so neither arm \
         of the match is vacuous"
    );

    let outcome = |count: IterationCount, template: Option<_>| {
        let mut probe = state.clone();
        apply(
            &mut probe,
            proposer,
            GameAction::DeclareShortcut { count, template },
        )
        .expect("dispatched — refusal is a HANDBACK");
        probe.waiting_for.variant_name()
    };

    assert_eq!(
        outcome(
            IterationCount::UntilLethal,
            Some(f4_pin_template(&schema, proposer, 1))
        ),
        "Priority",
        "CR 732.2a: the refusal of the AI's candidate is keyed on the COUNT — `UntilLethal` \
         against a narrowed bound — not on its missing pins. Carrying the very template the \
         positive control below has accepted changes nothing"
    );
    assert_eq!(
        outcome(IterationCount::Fixed(max), None),
        "RespondToShortcut",
        "item-4 C2, and this arm FLIPPED with it: `Fixed` + `template: None` is the browser's \
         own payload, and `handle_declare_shortcut` now resolves that `None` against the \
         declaration THIS offer published rather than discarding it. Both reach-guards above \
         are what make the flip attributable — no recorded period (so the pre-C2 engine refused \
         here) and a published declaration (so there is something to resolve against). Revert \
         the `or_else` ⇒ `Priority`"
    );
    // ── ANTI-VACUITY CONTROL: this board DOES accept a declaration ──
    assert_eq!(
        outcome(
            IterationCount::Fixed(max),
            Some(f4_pin_template(&schema, proposer, max))
        ),
        "RespondToShortcut",
        "the accepted shape is `Fixed(n)` + a template pinning every published point, owner == \
         proposer. Without this arm the three refusals above would be vacuous"
    );
}

/// §5 U6 (iii) — the declare-time `template.owner` firewall, exercised on the REAL F4 offer.
///
/// `loop_shortcut.rs`'s `r28_a_declared_template_owning_another_seat_is_refused_at_declare`
/// already covers this seam on a STAGED offer; this is the real-dump arm — a 4-player board
/// whose schema, pin slots and proposer all come from a captured game rather than from a
/// scenario built to reach the guard. The matched pair differs in exactly one field.
///
/// Reach-guards: the published pin set is non-empty, so `predictability_gate` and
/// `validate_pins` really run and the accepting arm proves they PASS (a refusal on both arms
/// would otherwise be reported as a firewall hit); and the hostile owner names a LIVING seat
/// that is not the proposer, which is the only shape the guard can distinguish.
///
/// REVERT-PROBE (shared with `r28_a`, and recorded as shared): delete
/// `if template.as_ref().is_some_and(|t| t.owner != offer.proposer)` from
/// `handle_declare_shortcut` ⇒ the hostile arm opens APNAP ⇒ this row FLIPS.
#[test]
fn u6_the_declare_owner_firewall_holds_on_the_real_f4_offer() {
    let mut state = load_f4();
    drive_f4_to_offer(&mut state, 400).expect("the bounded offer fires (see R1)");
    let (proposer, _certificate, schema) = offer_parts(&state);
    let schema = schema.clone();

    assert!(
        !schema.points.is_empty(),
        "reach-guard: a non-empty schema means `predictability_gate` / `validate_pins` really \
         run, so the accepting arm below proves the pair is keyed to `owner`"
    );
    let hostile = state
        .players
        .iter()
        .find(|p| p.id != proposer && !p.is_eliminated)
        .map(|p| p.id)
        .expect("reach-guard: a living seat other than the proposer must exist on a 4p board");

    let mut outcomes = vec![];
    for owner in [proposer, hostile] {
        let template = f4_pin_template(&schema, owner, 1);
        assert_eq!(
            template.owner, owner,
            "the two arms differ in exactly one field"
        );
        let mut probe = state.clone();
        let result = apply(
            &mut probe,
            proposer,
            GameAction::DeclareShortcut {
                count: IterationCount::Fixed(1),
                template: Some(template),
            },
        )
        .expect("dispatched either way — refusal is a HANDBACK");
        outcomes.push((probe.waiting_for.variant_name(), result.events.len()));
    }

    assert_eq!(
        outcomes,
        vec![("RespondToShortcut", 0), ("Priority", 0)],
        "CR 732.2a + CR 603.5: the declaration owned by the engine-issued proposer opens the \
         APNAP window; the byte-identical declaration owned by {hostile:?} is refused into the \
         manual handback. `handle_declare_shortcut` pushes no events on either path, \
         so the event counts are exact rather than wildcards"
    );
}

// ─────────────────────────────────────────────────────────────────────────────────────────
// B5f — the DECLARED term is load-bearing on a real board, in both directions
// ─────────────────────────────────────────────────────────────────────────────────────────

/// §4 B5f — **`elimination_bounds`'s `declared_life_magnitude` can suppress an offer that is
/// otherwise legal, and the suppression is measured ONE LIFE POINT WIDE on the user's own
/// board.**
///
/// CR 704.5a (a seat at 0 or less life has lost) + CR 732.2a (a shortcut describes a
/// PREDICTABLE sequence, so a repetition that could eliminate a seat mid-proposal is not
/// describable). Once the answer-beat sampling site announces Torch's CR 608.2b `Targets`
/// entry, `victim_slot` is non-empty and every declarable victim is charged
/// `observed.max(0) + S` rather than `observed` alone. On MODE1 that is `1 + 1 = 2`, so P1's
/// headroom must be at least 2 for a single legal repetition to exist.
///
/// ARM (α), the matched positive: P1 seeded at **7** and at **6** — headroom 3 and 2 at the
/// offer beat — both OFFER, with `max_iterations == 1`.
/// ARM (β), the typed refusal: P1 seeded at **5** and at **4** — headroom 1 and 0 — the drive
/// reaches the SAME beat and raises NO window, and the typed verdict on that very board is
/// `NoNarrowedLegalCount`. Asserted BY REASON, never as a bare absence: a row that only
/// observes "no offer" stops testing its own conjunct the moment an earlier one refuses first.
///
/// The two arms are **one life point apart** (6 offers, 5 refuses), which is what makes the
/// row about the divisor and not about the board.
///
/// REVERT-PROBE (DROP): delete `declared_life_magnitude` from `elimination_bounds`'s additive
/// form ⇒ the divisor falls 2 → 1 ⇒ headroom 1 at P1=5 yields `1 / 1 == 1` ⇒ (β) OFFERS ⇒
/// FLIPS. REVERT-PROBE (TRIVIALIZE): make the term unconditional (charge it to every seat, not
/// only to declarable victims) ⇒ P0/P2/P3 are charged 0 + 1 with 39 headroom, which does not
/// narrow below 1, so (α) survives — and the arm that flips is the reach-guard below, which
/// asserts P1 is the ONLY declarable victim on this board.
#[test]
fn b5f_the_declared_term_can_suppress_an_otherwise_legal_offer() {
    use engine::game::engine::{
        try_offer_bounded_cycle_shortcut_metered, BoundedOfferRefusal, ProbeCap,
    };

    /// The MODE1 board with P1's life REPLACED. Every other field — including the stored
    /// auto-choice guard (b) reads — is the user's own capture, so the only axis that moves
    /// between the arms below is the headroom `elimination_bounds` divides.
    fn seeded(life: i32) -> GameState {
        let mut state = load_mode1();
        let p1 = state
            .players
            .iter_mut()
            .find(|p| p.id == P1)
            .expect("MODE1 is a 4-player board");
        p1.life = life;
        state
    }

    // ── ARM (α) — the matched positive, asserted FIRST ──────────────────────────────────
    let mut alpha = seeded(7);
    let alpha_beat = drive_f4_to_offer(&mut alpha, 400).expect(
        "REACH-GUARD (α): MODE1 with P1 at 7 must raise the bounded offer, else every \
         refusal below is asserted over a board that was refusing anyway",
    );
    let (proposer, certificate, schema) = offer_parts(&alpha);
    let per_cycle = certificate
        .per_cycle
        .clone()
        .expect("a bounded offer publishes its per-period signature");

    // ── REACH-GUARD: the DECLARED term is what this row is about, so it must be non-zero,
    //    and P1 must be the only seat it is charged to. ──
    let declared: i64 = per_cycle
        .victim_slot
        .iter()
        .map(|(_, m)| *m)
        .filter(|m| *m > 0)
        .sum();
    assert!(
        declared > 0,
        "REACH-GUARD: `victim_slot` must publish a strictly positive magnitude, else the \
         additive term is 0 and (β) below would be about the observed drain alone; \
         victim_slot = {:?}",
        per_cycle.victim_slot
    );
    let declarable: std::collections::BTreeSet<PlayerId> = schema
        .points
        .iter()
        .filter_map(|p| match &p.kind {
            DecisionPointKind::Targets { legal_targets, .. } => Some(legal_targets),
            _ => None,
        })
        .flatten()
        .filter_map(|t| match t {
            TargetRef::Player(p) => Some(*p),
            _ => None,
        })
        .collect();
    assert!(
        declarable.contains(&P1),
        "REACH-GUARD: P1 — the seat this row starves — must be a DECLARABLE victim of the \
         published `Targets` slot, or the extra term is never charged to it; declarable = \
         {declarable:?}"
    );
    let observed_p1 = -per_cycle.delta.life.get(&P1).copied().unwrap_or(0);
    let life_at_offer = alpha
        .players
        .iter()
        .find(|p| p.id == P1)
        .expect("P1 is seated")
        .life as i64;
    assert_eq!(
        i64::from(schema.max_iterations),
        (life_at_offer - 1) / (observed_p1.max(0) + declared),
        "(α) CR 704.5a: the published bound is P1's headroom divided by the ADDITIVE \
         magnitude — observed {observed_p1} plus declared {declared} — at P1 life \
         {life_at_offer}. Under the `max` form this divisor would be \
         {} and the bound would be {}",
        observed_p1.max(declared),
        (life_at_offer - 1) / observed_p1.max(declared).max(1)
    );
    assert_eq!(
        schema.max_iterations, 1,
        "(α) the seeded headroom admits exactly ONE legal repetition; a larger bound would \
         mean (β) is one point further away than this row claims"
    );

    let mut alpha6 = seeded(6);
    assert_eq!(
        drive_f4_to_offer(&mut alpha6, 400),
        Some(alpha_beat),
        "(α) the SECOND positive, one point down: P1 at 6 still offers, at the same beat. \
         This is the arm (β) is one life point away from"
    );

    // ── ARM (β) — the TYPED refusal, on the same beat the positive offered at ───────────
    for life in [5, 4] {
        let mut beta = seeded(life);
        assert_eq!(
            drive_f4_to_offer(&mut beta, alpha_beat + 1),
            None,
            "(β) P1 at {life}: no window may be raised through beat {alpha_beat} — the beat \
             the (α) arms both offered at"
        );
        let at_priority = replay_at_priority(&beta, proposer);
        let (outcome, meter) =
            try_offer_bounded_cycle_shortcut_metered(&at_priority, false, ProbeCap::Shipped);
        assert!(
            matches!(outcome, Err(BoundedOfferRefusal::NoNarrowedLegalCount)),
            "(β) P1 at {life}: the refusal must be TYPED at the elimination bound — \
             `observed {observed_p1} + declared {declared}` exceeds P1's remaining headroom, \
             so no legal repetition count exists (CR 704.5a + CR 732.2a). A different variant \
             here means an EARLIER conjunct refused and this row stopped testing its own. \
             got {outcome:?}, meter {meter:?}"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────────
// M1 / A1 — THE USER'S OWN TWO CAPTURES, DRIVEN TO AN ACCEPTED GRANT THAT COMMITS
// ─────────────────────────────────────────────────────────────────────────────────────────

/// The published point set as `(source card name, kind)` — the offer's OWN data, read off
/// `state.waiting_for` rather than re-derived, so a row asserting a cause asserts the thing
/// the engine published.
fn published_point_names(state: &GameState) -> Vec<(String, &'static str)> {
    let (_, _, schema) = offer_parts(state);
    schema
        .points
        .iter()
        .map(|p| {
            let source = match &p.slot.source {
                engine::types::game_state::YieldTarget::ThisObject { source_id, .. } => state
                    .objects
                    .get(source_id)
                    .map(|o| o.name.clone())
                    // NOT a synthetic `obj<id>`: every caller compares this string against the
                    // SUE / REED / TORCH constants, so an unresolvable source would read as
                    // "not that card" and silently SATISFY the by-name ABSENCE assertions this
                    // helper feeds (m1's owner-firewall row). Same class of failure as the
                    // `other =>` arm below, so the same treatment.
                    .unwrap_or_else(|| {
                        panic!(
                            "a published point names {source_id:?}, absent from `objects` — an \
                             unresolvable name would silently satisfy the by-name ABSENCE \
                             assertions this helper feeds"
                        )
                    }),
                other => panic!("unexpected decision source {other:?}"),
            };
            let kind = match &p.kind {
                DecisionPointKind::MayChoice => "MayChoice",
                DecisionPointKind::Targets { .. } => "Targets",
                other => panic!("unexpected point kind {other:?}"),
            };
            (source, kind)
        })
        .collect()
}

/// THE GUARD ABOVE MUST BE ABLE TO FIRE. A guard that cannot is worse than none: it reads as
/// protection while the hole it names stays open, which is the exact defect the synthetic
/// `obj<id>` fallback was. Drive the real capture to its offer, then delete the first
/// published point's source object — the one state the fallback used to paper over — and
/// require the typed panic. `expected` is a substring match, so a panic from any OTHER cause
/// (an empty point set, a non-`ThisObject` source) fails this row instead of passing it.
#[test]
#[should_panic(expected = "absent from `objects`")]
fn published_point_names_panics_when_a_points_source_is_absent() {
    let mut state = load_f4();
    drive_f4_to_offer(&mut state, 400).expect("the bounded offer fires (see R1)");
    let (_, _, schema) = offer_parts(&state);
    let source_id = match &schema
        .points
        .first()
        .expect("the offer publishes at least one point")
        .slot
        .source
    {
        engine::types::game_state::YieldTarget::ThisObject { source_id, .. } => *source_id,
        other => panic!("unexpected decision source {other:?}"),
    };
    state.objects.remove(&source_id);
    published_point_names(&state);
}

/// Drive one user capture to its offer, declare a CONFORMANT `Fixed(n)`, have every living
/// opponent Accept, and measure what the grant actually committed.
///
/// The life, library and COUNTER axes are asserted EXACTLY and every expectation is DERIVED
/// FROM THE OFFER'S OWN published `per_cycle.delta` — `n` repetitions of the signature the
/// certificate itself carries — so no repetition rate is hard-coded and a re-dump flows
/// through unedited. Each of the three carries an ANTI-VACUITY guard on the published rate,
/// because `x == rate * n` is satisfied by any `x` when `rate` is zero.
///
/// ⚠ THE COUNTER AXIS WAS WEAKENED FOR A REASON THAT WAS FALSE. The note here used to say the
/// counter axis is event-fed and left at zero by `ResourceVector::snapshot`. MEASURED, the
/// published vector carries `counters: {(Plus1Plus1, Creature): 2}` — non-zero, and
/// state-readable (`snapshot` walks the battlefield for it). The real obstacle was the
/// ACCESSOR: [`commit_axes`] reads ONE named object's counters (The Thing) while the published
/// key `(CounterClass, ObjectClass)` is an AGGREGATE over every battlefield object of that
/// class, so the two are not comparable quantities. Asserted here against the aggregate
/// accessor the certificate is minted from, and still returned for the caller's per-object
/// `n`-scaling arm. MEASURED on both captures: aggregate `(Plus1Plus1, Creature)` moves `2`
/// at `n = 1` and `6` at `n = 3`, i.e. exactly `2n`, which is the assertion this note's false
/// predecessor had waved off as underivable.
///
/// The TOKEN axis genuinely cannot be asserted against the certificate: `tokens_created` IS
/// event-fed, and the published vector carries `tokens_created: 0` on both captures, so an
/// exact expectation derived from it would be the vacuous `0 == 0 * n`. It keeps the
/// `n`-scaling arm alone.
///
/// Returns `(offer beat, published points, Thing-counter delta, token delta)`.
fn accept_a_fixed_grant(
    mut state: GameState,
    n: u32,
    label: &str,
) -> (u32, Vec<(String, &'static str)>, i64, i64) {
    let beat = drive_f4_to_offer(&mut state, 400).unwrap_or_else(|| {
        panic!("[{label} n={n}] REACH-GUARD: the CR 732.2a bounded offer must FIRE on this capture")
    });
    let (proposer, certificate, schema) = offer_parts(&state);
    let per_cycle = certificate
        .per_cycle
        .clone()
        .expect("a bounded offer publishes its per-period signature");
    let schema = schema.clone();
    assert!(
        schema.max_iterations >= n,
        "[{label} n={n}] REACH-GUARD: the published bound {} must admit this count, else the \
         declaration is refused for a reason that has nothing to do with the drive",
        schema.max_iterations
    );
    let points = published_point_names(&state);
    let before = commit_axes(&state);
    let before_rv = ResourceVector::snapshot(&state);

    let template = f4_pin_template(&schema, proposer, n);
    apply(
        &mut state,
        proposer,
        GameAction::DeclareShortcut {
            count: IterationCount::Fixed(n),
            template: Some(template),
        },
    )
    .expect("the conformant declaration is dispatched");
    assert!(
        matches!(state.waiting_for, WaitingFor::RespondToShortcut { .. }),
        "[{label} n={n}] the declaration must be ACCEPTED and open the CR 732.2b APNAP window; \
         a `Priority` here is a DECLARE-time refusal, a different defect entirely. got {:?}",
        state.waiting_for
    );
    let responders = accept_all_opponents(&mut state);
    assert!(
        responders > 0,
        "[{label} n={n}] REACH-GUARD: at least one living opponent must have answered the \
         CR 732.2c window, else the grant was never put to the table"
    );

    let after = commit_axes(&state);
    let measured = ResourceVector::delta(&before_rv, &ResourceVector::snapshot(&state));
    // ── ANTI-VACUITY on the published RATES (F3) ─────────────────────────────────────────
    // Every equality below has the shape `moved == rate * n`, which an all-zero certificate
    // satisfies with a board that never moved. The counters/tokens half already guards this
    // in `assert_axis_scales`; these are the life and library halves' matching guards.
    assert!(
        per_cycle.delta.life.values().any(|&rate| rate != 0),
        "[{label} n={n}] ANTI-VACUITY: the published per-cycle LIFE delta must move some \
         seat, else every life equality below is `0 == 0 * {n}` and asserts nothing. \
         published life = {:?}",
        per_cycle.delta.life
    );
    assert!(
        per_cycle
            .delta
            .library_delta
            .values()
            .any(|&rate| rate != 0),
        "[{label} n={n}] ANTI-VACUITY: the published per-cycle LIBRARY delta must move some \
         seat, else every library equality below is `0 == 0 * {n}` and asserts nothing. \
         published library = {:?}",
        per_cycle.delta.library_delta
    );

    for (i, player) in state.players.iter().enumerate() {
        let life_rate = per_cycle.delta.life.get(&player.id).copied().unwrap_or(0);
        assert_eq!(
            i64::from(after.0[i]) - i64::from(before.0[i]),
            life_rate * i64::from(n),
            "[{label} n={n}] CR 732.2a: seat {:?}'s life must move by EXACTLY {n} repetitions \
             of the offer's own published per-cycle life delta ({life_rate}). \
             before={:?} after={:?}",
            player.id,
            before.0,
            after.0
        );
        let lib_rate = per_cycle
            .delta
            .library_delta
            .get(&player.id)
            .copied()
            .unwrap_or(0);
        assert_eq!(
            after.1[i] as i64 - before.1[i] as i64,
            lib_rate * i64::from(n),
            "[{label} n={n}] CR 732.2a: seat {:?}'s library must move by EXACTLY {n} \
             repetitions of the published per-cycle library delta ({lib_rate}). \
             before={:?} after={:?}",
            player.id,
            before.1,
            after.1
        );
    }
    // ── THE COUNTER AXIS, EXACTLY (F4) ───────────────────────────────────────────────────
    // CR 122.1 + CR 732.2a. Against the AGGREGATE accessor the certificate is minted from,
    // not against `commit_axes`'s single named object — that mismatch, not "the axis is
    // event-fed", is why this assertion was previously only a scaling arm.
    assert!(
        per_cycle.delta.counters.values().any(|&rate| rate != 0),
        "[{label} n={n}] ANTI-VACUITY: the published per-cycle COUNTER delta must be \
         non-zero, else the equality below is `0 == 0 * {n}`. published = {:?}",
        per_cycle.delta.counters
    );
    for (key, rate) in &per_cycle.delta.counters {
        assert_eq!(
            measured.counters.get(key).copied().unwrap_or(0),
            rate * i64::from(n),
            "[{label} n={n}] CR 732.2a: the {key:?} counter axis must move by EXACTLY {n} \
             repetitions of the offer's own published per-cycle rate ({rate}). \
             measured = {:?}",
            measured.counters
        );
    }
    // Nothing may move on a counter axis the certificate never published: a commit that
    // pumped an unpublished counter class would satisfy every equality above and still be a
    // cycle the offer did not describe.
    for (key, moved) in &measured.counters {
        if *moved != 0 {
            assert!(
                per_cycle.delta.counters.contains_key(key),
                "[{label} n={n}] CR 732.2a: {key:?} moved by {moved} but is absent from the \
                 published per-cycle signature {:?}",
                per_cycle.delta.counters
            );
        }
    }

    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "[{label} n={n}] CR 732.2a: a taken shortcut's ending point is a place where a player \
         has priority, got {:?}",
        state.waiting_for
    );
    (
        beat,
        points,
        i64::from(after.2) - i64::from(before.2),
        after.3 as i64 - before.3 as i64,
    )
}

/// The `n`-scaling arm shared by both captures: every axis a cycle moves must move `n` times
/// as far at `n = 3` as at `n = 1`, and must move AT ALL at `n = 1`.
///
/// The non-zero guard is the anti-vacuity half and is not decoration: `3 * 0 == 0`, so without
/// it an axis that never moved would satisfy the scaling equality silently. Together the two
/// halves are the discriminator `bounded_fixed_count_commits_exactly_n_periods` uses — a
/// partial commit, a saturating commit and a zero commit each break one of them.
fn assert_axis_scales(label: &str, axis: &str, at_1: i64, at_3: i64) {
    assert_ne!(
        at_1, 0,
        "[{label}] ANTI-VACUITY: the {axis} axis must MOVE on a single committed repetition, \
         else the scaling equality below is `3 * 0 == 0` and asserts nothing"
    );
    assert_eq!(
        at_3,
        at_1 * 3,
        "[{label}] CR 732.2a: three repetitions must move the {axis} axis exactly three times \
         as far as one ({at_1}); a partial or saturating commit separates them"
    );
}

/// **M1 — the user's own capture that raised NO offer at all now offers, and the accepted
/// grant COMMITS on every axis one cycle moves.**
///
/// CR 732.2a + CR 603.5. MODE1's distinguishing field is a stored `may_trigger_auto_choices`
/// entry — the user's "always take" for Sue's `may`. Guard (b) of `entry_publishes_pin_slots`
/// WITHHOLDS a pin slot the CR 603.5 gate can never spend, so Sue's `MayChoice` is deliberately
/// absent from the published set; the gate is discharged instead by the auto-answer relief.
/// That is the whole reason this board raised nothing before: the relief did not exist, so a
/// stored answer looked like an unanswerable choice.
///
/// The row asserts the CAUSE alongside the effect, so a green cannot be read as "some offer
/// appeared":
///
/// * the capture's identity is reach-guarded (`may_trigger_auto_choices` NON-EMPTY) — on a
///   board without one, the relief path is not the mechanism under test;
/// * Sue is asserted ABSENT from the published points while Reed and Torch are PRESENT, which
///   is guard (b) discriminating between a stored answer and an open choice on ONE board;
/// * every axis is asserted exactly, against the offer's own published per-cycle signature.
///
/// REVERT-PROBE: ablate the CR 603.5 auto-answer relief in `auto_may_choice_relief` ⇒ gate (6)
/// can no longer be discharged for Sue's withheld slot ⇒ no offer fires ⇒ the reach-guard in
/// `accept_a_fixed_grant` FLIPS. Positive control: the same drive on MODE2, whose
/// `may_trigger_auto_choices` is EMPTY, reaches its offer through the ordinary publication
/// path (the row below) — so "the drive reaches an offer" is not a property of the harness.
#[test]
fn m1_the_users_stored_auto_choice_board_offers_and_the_grant_commits_on_every_axis() {
    let identity = load_mode1();
    assert!(
        !identity.may_trigger_auto_choices.is_empty(),
        "REACH-GUARD: MODE1 is the capture whose CR 603.5 answer is STORED; without one, guard \
         (b) withholds nothing and this row measures the ordinary publication path instead"
    );

    let (beat1, points, counters_1, tokens_1) = accept_a_fixed_grant(load_mode1(), 1, "MODE1");
    assert!(
        points
            .iter()
            .any(|(src, kind)| src == REED && *kind == "MayChoice")
            && points
                .iter()
                .any(|(src, kind)| src == TORCH && *kind == "Targets"),
        "MODE1: the two choices with NO stored answer must be PUBLISHED — that is the paired \
         positive that makes Sue's absence below an attribution rather than an empty set. \
         published = {points:?}"
    );
    assert!(
        !points.iter().any(|(src, _)| src == SUE),
        "MODE1 THE CAUSE: Sue's `may` is answered by the user's stored auto-choice, so guard \
         (b) withholds a pin slot the CR 603.5 gate could never spend and the relief discharges \
         gate (6) instead. published = {points:?}"
    );

    let (beat3, _, counters_3, tokens_3) = accept_a_fixed_grant(load_mode1(), 3, "MODE1");
    assert_eq!(
        beat1, beat3,
        "the two arms must offer at the SAME beat — they are one declared count apart and \
         nothing else"
    );
    assert_axis_scales("MODE1", "The Thing's counters", counters_1, counters_3);
    assert_axis_scales("MODE1", "token", tokens_1, tokens_3);
}

/// **A1 — the user's own capture where the accepted grant committed NOTHING now commits on
/// every axis, and the declared count scales it.**
///
/// CR 732.2a. This is the capture the user took after clearing the stored auto-choice as a
/// workaround: the offer fired, the declaration was accepted, and the drive then rolled the
/// whole cycle back and re-offered — because Reed's `may` resolves across a forced
/// pre-priority window that the ring sampler could not see, so the offer published a pin set
/// that did not cover every per-iteration choice and cycle 0 aborted on the first uncovered
/// one.
///
/// With the answer-beat sampling site the announced set contains all three choices, so the
/// published set covers the cycle and the grant commits. The row is the fix bar for this
/// change: it asserts a commit on ALL FOUR axes and `n = 1` vs `n = 3` DISTINGUISHABLE.
///
/// * the capture's identity is reach-guarded (`may_trigger_auto_choices` EMPTY), which is
///   MODE1's field inverted — the two captures are one axis apart;
/// * all three sources are asserted PUBLISHED, naming the cause of the commit;
/// * every axis is asserted exactly, against the offer's own published per-cycle signature.
///
/// REVERT-PROBE: ablate the answer-beat sampling site ⇒ Reed's and Torch's entries are never
/// announced ⇒ the published set shrinks ⇒ cycle 0 aborts on the uncovered `may` ⇒ every axis
/// delta collapses to 0 ⇒ both the exact-axis assertions and the scaling arm FLIP.
#[test]
fn a1_the_users_accept_committed_nothing_board_now_commits_on_every_axis() {
    let identity = load_mode2();
    assert!(
        identity.may_trigger_auto_choices.is_empty(),
        "REACH-GUARD: MODE2 is the POST-workaround capture — the user cleared the stored \
         answer, so this board reaches its offer through the ordinary CR 603.5 publication \
         path and not through the relief MODE1 exercises"
    );

    let (beat1, points, counters_1, tokens_1) = accept_a_fixed_grant(load_mode2(), 1, "MODE2");
    for expected in [(SUE, "MayChoice"), (REED, "MayChoice"), (TORCH, "Targets")] {
        assert!(
            points
                .iter()
                .any(|(src, kind)| src == expected.0 && *kind == expected.1),
            "MODE2 THE CAUSE: every per-iteration choice this cycle opens must be PUBLISHED, \
             or the drive aborts on the first uncovered one and commits nothing — which is \
             exactly what the user captured. missing {expected:?}; published = {points:?}"
        );
    }

    let (beat3, _, counters_3, tokens_3) = accept_a_fixed_grant(load_mode2(), 3, "MODE2");
    assert_eq!(
        beat1, beat3,
        "the two arms must offer at the SAME beat — they are one declared count apart and \
         nothing else"
    );
    assert_axis_scales("MODE2", "The Thing's counters", counters_1, counters_3);
    assert_axis_scales("MODE2", "token", tokens_1, tokens_3);
}

/// ITEM 2 (CR 732.2a) — the DECLARE seam: **on an offer that published no declaration of its
/// own**, a `template: None` declaration is admitted only when the recorded period belongs to
/// the offer's own proposer. The qualifier is item-4 C2's and is load-bearing — see the arm
/// table below.
///
/// **WHY THIS FIXTURE AND NOT `loop_shortcut.rs`.** Site F sits under
/// `if !offer.schema.points.is_empty()`. The dina bounded offer publishes an EMPTY point set
/// (asserted green by that module's acceptance row), so this row would be structurally VACUOUS
/// there. The F4 offer publishes all three of this cycle's per-iteration choices, so the arm is
/// live here and only here. That fixture choice is load-bearing, not incidental.
///
/// **WHY IT IS A DIFFERENT ROW FROM THE MINT ARMS.** The mint-seam instrument
/// (`try_offer_bounded_cycle_shortcut`) cannot observe `handle_declare_shortcut` at all —
/// different seam, different instrument. Any future change to this routing discriminant needs
/// BOTH a mint-seam row and a declare-seam row; neither covers the other.
///
/// **THE HAZARD, and it is the one direction in which relaxing step (1b) makes the engine LESS
/// safe than before.** A `template: None` declaration against a non-empty schema skips pin
/// validation entirely — legitimate for exactly one drive shape, the object-growth route, which
/// re-derives its template from `last_loop_action_sequence`. Once (1b) went seat-relative, a
/// bounded offer can be minted with a FOREIGN period in state; under a merely-non-empty test that
/// foreign period would take the unvalidated sibling arm and open the CR 732.2b APNAP window on a
/// client-supplied declaration. The arm therefore asks whose period it is.
///
/// **ALL THREE ARMS RUN ON AN OFFER WHOSE OWN `declaration` IS CLEARED (item-4 C2).** That is
/// the offer shape site F still decides — `handle_declare_shortcut` resolves a `template: None`
/// declaration against `offer.declaration` above the pin block, so an offer that published one
/// bypasses site F entirely. The clearing keeps this row on its own subject instead of silently
/// converting it into a `declaration_conforms` row; the fourth arm below is the paired positive
/// that proves the clearing is the operative axis. See the closure's own comment for why a
/// declaration-free offer is a reachable production shape rather than a contrivance.
///
/// | arm | offer `declaration` | sequence | expected `waiting_for` |
/// |---|---|---|---|
/// | EMPTY-seq | cleared | empty | `Priority` (fail-closed) — must-not-flip |
/// | OWN-seq | cleared | proposer's | `RespondToShortcut` (the legitimate object-growth route) — must-not-flip |
/// | FOREIGN-seq | cleared | an opponent's | `Priority` — **the remedy** |
/// | RETAINED | **retained** | empty | `RespondToShortcut` — **the C2 paired positive**: one field apart from EMPTY-seq, and it flips |
///
/// **TWO-SIDED CONTROL, PER ASSERTION** — no constant implementation passes:
/// * **DROP** the proposer test (restore `state.last_loop_action_sequence.is_empty()`) ⇒
///   FOREIGN-seq returns `RespondToShortcut` ⇒ THAT assertion fails, while EMPTY/OWN still pass.
/// * **TRIVIALIZE** to always-reject ⇒ OWN-seq returns `Priority` ⇒ **that** assertion fails
///   instead (the shipped object-growth declarations break — the tree's own doc above this arm
///   says keying on `template.is_none()` alone does exactly this). TRIVIALIZE to never-reject ⇒
///   EMPTY-seq returns `RespondToShortcut` ⇒ that assertion fails.
/// * **REVERT item-4 C2** (drop `let template = template.or_else(|| offer.declaration.cloned())`
///   from `handle_declare_shortcut`) ⇒ the RETAINED arm returns `Priority` ⇒ **that** assertion
///   fails, while the three cleared-offer arms are untouched (they have no declaration to
///   resolve against, so the `or_else` was already a no-op for them).
///
/// ⚠ **WHAT THIS ROW DELIBERATELY DOES NOT ASSERT — a realized negative, recorded rather than
/// re-keyed.** Continuing each ACCEPTED arm through `accept_all_opponents` was measured, and both
/// the legitimate OWN-seq route and the illegitimate FOREIGN-seq one commit `dlife = 0`: a
/// `template: None` declaration carries no pins, so the drive fail-closes on the first uncovered
/// per-iteration choice either way. (The conformant `template: Some(..)` declarations DO commit —
/// that is `r2a`'s subject — but they never reach this arm.) The board's own zero therefore
/// DOMINATES any life-axis discriminator here, so the downstream harm is structurally
/// unobservable on this fixture and is NOT claimed. This row asserts the GATE VERDICT, which is
/// the property that actually fails closed.
#[test]
fn a_template_free_declaration_is_admitted_only_by_the_proposers_own_period() {
    use engine::types::game_state::{BuybackUsage, LoopAction, LoopActionContext};

    let mut state = load_f4();
    let beat = drive_f4_to_offer(&mut state, 400)
        .expect("REACH-GUARD: every arm below is vacuous without the engine's own bounded offer");
    let (proposer, _, schema) = offer_parts(&state);
    assert!(
        !schema.points.is_empty(),
        "REACH-GUARD: site F sits under `!offer.schema.points.is_empty()`, so an empty point \
         set makes this whole row unreachable — which is exactly why it is not on the dina \
         fixture (beat {beat})"
    );
    let max = schema.max_iterations;
    assert!(
        max >= 1,
        "REACH-GUARD: the published bound must admit `Fixed(1)`, else the arms are refused for \
         a reason that has nothing to do with the period"
    );
    assert!(
        offer_declaration(&state).is_some(),
        "REACH-GUARD for the `declaration = None` mutation the closure below applies: the \
         UNTOUCHED offer really does publish a declaration, so that clearing is a genuine \
         one-field mutation rather than a no-op restating the fixture. Paired with the \
         `declaration retained` positive at the end of this row"
    );

    let opp = state
        .players
        .iter()
        .map(|p| p.id)
        .find(|p| *p != proposer)
        .expect("REACH-GUARD: the FOREIGN arm needs a second seat to attribute a period to");
    let card_id = state
        .objects
        .values()
        .next()
        .map(|o| o.card_id)
        .expect("the dump has objects");

    // One offer state, one field reassigned per arm, one action applied — nothing else differs.
    //
    // ⚠ THE OFFER'S OWN `declaration` IS CLEARED, and that is what keeps this row LIVE rather
    // than what weakens it (item-4 C2). `handle_declare_shortcut` now resolves a `template:
    // None` declaration against `offer.declaration` ABOVE the pin block, so on an offer that
    // published one, `&template` takes the `Some(t)` arm and site F is never reached — all
    // three arms below would read `RespondToShortcut` and the row would be measuring
    // `declaration_conforms` instead of the period test it is named for. Clearing the
    // declaration puts the row back on the offer shape site F still decides, which is a
    // REACHABLE production shape and not a contrivance: `build_bounded_declaration` returns
    // `None` on a journal miss or a kind/value mismatch even with a non-empty schema, both
    // non-bounded mints hard-code `declaration: None`, and a restored save may carry `None`.
    // Measured across the tracked suite at this tip: 34 distinct tests still reach site F on a
    // point-carrying offer that published no declaration.
    let declare_with = |seq: Vec<LoopActionContext>| {
        let mut probe = state.clone();
        probe.last_loop_action_sequence = seq;
        match &mut probe.waiting_for {
            WaitingFor::LoopShortcut { declaration, .. } => *declaration = None,
            other => panic!("expected the CR 732.2a bounded offer, got {other:?}"),
        }
        apply(
            &mut probe,
            proposer,
            GameAction::DeclareShortcut {
                count: IterationCount::Fixed(1),
                template: None,
            },
        )
        .expect("dispatched — a refusal is a HANDBACK, not an error");
        probe.waiting_for.variant_name()
    };
    // The SAME EMPTY-seq call with the declaration RETAINED — one field apart from the first
    // assertion below, and the axis is the offer's own `declaration`.
    let declare_empty_seq_with_declaration_retained = || {
        let mut probe = state.clone();
        probe.last_loop_action_sequence = Vec::new();
        apply(
            &mut probe,
            proposer,
            GameAction::DeclareShortcut {
                count: IterationCount::Fixed(1),
                template: None,
            },
        )
        .expect("dispatched — a refusal is a HANDBACK, not an error");
        probe.waiting_for.variant_name()
    };
    let step = |controller: PlayerId| LoopActionContext {
        card_id,
        controller,
        action: LoopAction::Recast {
            from_zone: engine::types::zones::Zone::Hand,
            uses_buyback: BuybackUsage::NotUsed,
        },
        convoke: None,
        pins: Vec::new(),
    };

    assert_eq!(
        declare_with(Vec::new()),
        "Priority",
        "EMPTY-seq must-not-flip — CR 732.2a: with no period at all there is nothing to \
         re-derive a template from, so a pin-consuming drive would run with no pins. Fail closed \
         into the manual-play handback"
    );
    assert_eq!(
        declare_with(vec![step(proposer)]),
        "RespondToShortcut",
        "OWN-seq must-not-flip: the proposer's own recorded period IS the object-growth route's \
         re-derivation source, so this is the shipped legitimate acceptance. An always-reject \
         remedy breaks it"
    );
    assert_eq!(
        declare_with(vec![step(opp)]),
        "Priority",
        "FOREIGN-seq — THE REMEDY. CR 732.2a: an opponent's independent activation is not a \
         template this proposer's drive can re-derive from, so admitting it would open the \
         CR 732.2b window on a client-supplied declaration that received ZERO pin validation. \
         NOTE the paired assertion below: this seat-relative refusal is what site F decides on a \
         declaration-free offer, NOT a blanket refusal of `template: None` \
         against a schema with published points"
    );
    // ── PAIRED POSITIVE, and it is what makes the two refusals above ATTRIBUTABLE ──
    assert_eq!(
        declare_empty_seq_with_declaration_retained(),
        "RespondToShortcut",
        "item-4 C2: byte-identical to the EMPTY-seq arm above except that the offer's own \
         `declaration` is RETAINED, and it flips. Two things follow, and neither is provable \
         from the refusals alone. (1) Those refusals are site F's seat-relative period verdict, \
         not this fixture refusing every `template: None` declaration for some unrelated reason \
         — an always-reject engine fails HERE. (2) Site F is REACHED at all on the cleared \
         offer, because the only difference between reaching it and bypassing it is the field \
         this assertion restores. Revert C2's `or_else` ⇒ this arm reads `Priority` and the \
         whole row degenerates into three copies of one verdict"
    );
}

// ─────────────────────────────────────────────────────────────────────────────────────────
// item-4 C2 — the engine-issued declaration is HONOURED on the manual declare path
//
// The defect these rows close is an ACTOR DIVERGENCE on one and the same offer: the engine
// mints a bounded offer carrying its own `declaration` (the proposer's journalled answers),
// `ai_support::candidates` reads that field and declares with `template: Some(declaration)` and
// is accepted, while a browser — which structurally sends `template: null`, because the client
// never constructs a template — was refused. The repair is one `Option::or_else` in
// `handle_declare_shortcut`, placed ABOVE the `template.owner` firewall.
// ─────────────────────────────────────────────────────────────────────────────────────────

/// The accepted proposal behind a `RespondToShortcut` window. Panics loudly on any other state
/// so a row that meant to assert on a proposal can never silently assert on its absence.
fn accepted_proposal(state: &GameState) -> &engine::analysis::loop_check::ShortcutProposal {
    match &state.waiting_for {
        WaitingFor::RespondToShortcut { proposal, .. } => proposal,
        other => panic!("expected the `RespondToShortcut` accept-or-shorten window, got {other:?}"),
    }
}

/// Declare `Fixed(k)` with the browser's own payload (`template: None`) against the live F4
/// offer, returning the post-state.
fn declare_template_free(state: &GameState, proposer: PlayerId, k: u32) -> GameState {
    let mut probe = state.clone();
    apply(
        &mut probe,
        proposer,
        GameAction::DeclareShortcut {
            count: IterationCount::Fixed(k),
            template: None,
        },
    )
    .expect("dispatched — a refusal is a HANDBACK, not an error");
    probe
}

/// **Rows R1 + R1b — THE REPAIR.** A browser `template: null` declaration against the real
/// point-carrying bounded offer reaches the ACCEPTED declaration, at every count the picker
/// makes selectable rather than only at the suggested one.
///
/// # Why this row exists at all
///
/// `ai_support::candidates` gates its declare candidate on `declaration.is_some()` and sends
/// that very template, so the AI path was already green
/// ([`d6_the_ai_declare_candidate_carries_the_offers_own_published_declaration`]). The manual
/// arm bound `declaration: _` and threw the field away, so the identical offer answered the two
/// ingresses differently. `template: null` is not "no pins" — it is "no OVERRIDE of the pins you
/// already published", and this row is the measurement of that reading.
///
/// # The revert-failing assertion, named
///
/// `proposal.template == Some(offer_declaration(&state))` — VALUE-equal against the field the
/// offer published, never `is_some()`. Delete `let template = template.or_else(|| ...)` from
/// `handle_declare_shortcut` and every arm here lands `Priority`, so `accepted_proposal` panics
/// before any assertion is reached.
///
/// # R1b: the counts are not the suggested one
///
/// The picker's whole point is that any count in `[min, max]` may be declared, so a repair that
/// only worked at `suggested` would be no repair. `k = 1` is the window's lower edge and
/// `k = 5` is neither edge nor the suggestion — no implementation that special-cases
/// `max_iterations` (which this board publishes as `suggested`) satisfies the `k = 5` arm.
/// `proposal.count` is asserted per arm, so an engine that accepted the declaration but drove
/// the suggested count anyway fails here rather than silently overriding the player.
///
/// # Reach-guards, asserted BEFORE the claim
///
/// The schema publishes points (else the pin block is skipped and the row measures the empty
/// path — that is [`c2_r4b_a_points_empty_offer_is_gated_by_the_owner_firewall_alone`]'s
/// subject), the schema is bounded, the offer really published a declaration (else the
/// `or_else` has nothing to resolve against and every arm would be measuring site F), and the
/// window is wide enough that `k = 5` is genuinely interior. The bound is read from the schema
/// rather than pinned to a literal.
#[test]
fn c2_r1_the_browsers_template_free_declaration_reaches_the_accepted_declaration() {
    let mut state = load_f4();
    drive_f4_to_offer(&mut state, 400).expect("the bounded offer fires (see R1)");
    let (proposer, _certificate, schema) = offer_parts(&state);
    let (points, bounded, max) = (
        schema.points.len(),
        schema.is_bounded(),
        schema.max_iterations,
    );

    assert!(
        points > 0,
        "REACH-GUARD: with an empty point set `handle_declare_shortcut` skips the pin block \
         entirely and this row would measure the owner firewall instead of the repair"
    );
    assert!(
        bounded,
        "REACH-GUARD: an unbounded schema takes the `UntilLethal` arms, not this one"
    );
    let published = offer_declaration(&state).expect(
        "REACH-GUARD: the `or_else` resolves against THIS field — without it every arm \
                 below would be measuring site F's period test, not the repair",
    );
    assert!(
        max >= 5,
        "REACH-GUARD: `k = 5` must be INTERIOR to the declarable window, else R1b's \
         non-suggested arm is refused by the `Fixed(n) > max_iterations` cap for a reason that \
         has nothing to do with the repair. max_iterations={max}"
    );

    // R1 — the suggested count, which is `max` on this board.
    let at_max = declare_template_free(&state, proposer, max);
    assert_eq!(
        accepted_proposal(&at_max).template.as_ref(),
        Some(&published),
        "item-4 C2: the accepted proposal carries the offer's OWN published declaration, \
         value-equal. `is_some()` would also pass on an engine that fabricated an empty \
         template, which is precisely the wrong implementation \
         `a_template_free_declaration_is_admitted_only_by_the_proposers_own_period` kills"
    );
    assert_eq!(
        accepted_proposal(&at_max).count,
        IterationCount::Fixed(max),
        "and the count the player named is the count the proposal carries"
    );

    // R1b — a lower-edge count and a strictly interior one. Neither is `suggested`.
    for k in [1u32, 5] {
        let post = declare_template_free(&state, proposer, k);
        assert_eq!(
            accepted_proposal(&post).template.as_ref(),
            Some(&published),
            "R1b at k={k}: the picker may name ANY count in the window, and the resolved \
             declaration is the same published one at every count — the offer publishes one \
             declaration, not one per count"
        );
        assert_eq!(
            accepted_proposal(&post).count,
            IterationCount::Fixed(k),
            "R1b at k={k}: the proposal drives the count the player NAMED. An engine that \
             accepted the declaration and then substituted `suggested` fails here. k=5 is \
             neither window edge (1/{max}) nor the suggestion, so no hard-coded value \
             satisfies this arm"
        );
    }
}

/// **Row R3 — PLACEMENT.** A restored offer whose published declaration carries a FOREIGN owner
/// is refused, because the `or_else` resolves the `None` template ABOVE the `template.owner`
/// firewall rather than below it.
///
/// # ⚠ What this row does and does not discriminate — read before trusting it
///
/// **It does NOT discriminate the C2 repair itself: it passes both ways.** Pre-repair the
/// `template: None` never resolves, reaches site F and lands `Priority`; post-repair the
/// resolved `Some(hostile)` reaches the owner firewall and lands `Priority`. Same verdict by two
/// different paths, and the paths are indistinguishable from outside — all six refusal arms call
/// the same `reject_shortcut_declaration`, which writes a byte-identical `WaitingFor::Priority`
/// and pushes zero events (`game/engine.rs`, on the count `match`: *"no row can observe which
/// block refused first"*). No assertion can recover which arm fired, so none is attempted here;
/// an arm-exclusion assert would read as verification while proving nothing.
/// [`c2_r1_the_browsers_template_free_declaration_reaches_the_accepted_declaration`] is what
/// covers the repair.
///
/// **What it DOES discriminate is the `or_else`'s PLACEMENT**, which is the one thing about C2
/// that is not self-evident from the diff. Move that statement one line down, below the
/// firewall, and this row flips to `RespondToShortcut`: the firewall would see the unresolved
/// `None` and pass it, then the `Some(t)` arm would judge the hostile template by
/// `declaration_conforms` alone — and `declaration_conforms` is `predictability_gate &&
/// validate_pins`, neither of which reads `owner`. The firewall is therefore the SOLE refuser of
/// a foreign-owner declaration, and below it there is nothing left to refuse one.
/// MEASURED, by physically relocating the statement: refused above, ACCEPTED below.
///
/// # Fixture guard, labelled honestly
///
/// `offer_declaration(..).is_some()` after the mutation is a FIXTURE guard — it proves the owner
/// rewrite did not erase the declaration — and not a path discriminator. It is true pre-repair
/// as well.
///
/// # The matched positive is what makes "refused" mean anything
///
/// The untampered offer, same call, same count, must open APNAP. Without it, `Priority` here is
/// indistinguishable from a fixture that refuses everything. The two differ in exactly one
/// field: `declaration.owner`.
#[test]
fn r3_placement_a_restored_foreign_owner_declaration_is_refused() {
    let mut state = load_f4();
    drive_f4_to_offer(&mut state, 400).expect("the bounded offer fires (see R1)");
    let (proposer, _certificate, schema) = offer_parts(&state);
    assert!(
        !schema.points.is_empty(),
        "REACH-GUARD: an empty point set would make the two arms differ for a different reason"
    );
    let hostile = state
        .players
        .iter()
        .find(|p| p.id != proposer && !p.is_eliminated)
        .map(|p| p.id)
        .expect("REACH-GUARD: a living seat other than the proposer must exist on a 4p board");

    // The RESTORE ingress image: a persisted offer whose published declaration names another
    // seat. One field differs from the untampered board.
    let mut tampered = state.clone();
    match &mut tampered.waiting_for {
        WaitingFor::LoopShortcut { declaration, .. } => {
            declaration
                .as_mut()
                .expect("the untampered offer publishes a declaration")
                .owner = hostile;
        }
        other => panic!("expected the CR 732.2a bounded offer, got {other:?}"),
    }
    assert_eq!(
        offer_declaration(&tampered).map(|d| d.owner),
        Some(hostile),
        "FIXTURE GUARD (not a path discriminator): the owner rewrite landed and did not erase \
         the declaration. This is equally true before the repair"
    );

    assert_eq!(
        declare_template_free(&tampered, proposer, 1)
            .waiting_for
            .variant_name(),
        "Priority",
        "PLACEMENT: the resolved declaration meets the `template.owner` firewall BEFORE anything \
         else looks at it. Relocate the `or_else` below that firewall and this reads \
         `RespondToShortcut`, because `declaration_conforms` accepts a template that differs \
         only in `owner`"
    );
    // ── MATCHED POSITIVE, one field apart ──
    assert_eq!(
        declare_template_free(&state, proposer, 1)
            .waiting_for
            .variant_name(),
        "RespondToShortcut",
        "the byte-identical offer whose declaration is owned by the PROPOSER opens the APNAP \
         window. Without this arm the refusal above would be indistinguishable from a fixture \
         that refuses every declaration"
    );
}

/// **Rows R4b + R5 — the points-EMPTY offer, where the owner firewall is the only gate.**
///
/// `handle_declare_shortcut` runs the pin block only under `!offer.schema.points.is_empty()`, so
/// on a point-free offer neither `declaration_conforms` nor site F ever runs and the resolved
/// template meets the firewall alone. Three arms on one F4-derived fixture, `schema.points`
/// emptied:
///
/// | arm | offer `declaration` | expected |
/// |---|---|---|
/// | **R5** point-free control | cleared | `RespondToShortcut` — accepts pre- AND post-repair |
/// | **R4b/A** | retained, `owner == proposer` | `RespondToShortcut`, and `proposal.template` carries it |
/// | **R4b/B** | retained, `owner == hostile` | `Priority` — the firewall, alone |
///
/// # Per-arm discrimination, stated rather than assumed
///
/// **R5 passes both ways and is labelled a CONTROL.** Its job is to prove this fixture accepts
/// declarations at all once the point set is gone, so R4b/B's refusal is attributable to the
/// owner rather than to the emptied schema. It also pins that the `or_else` is a genuine no-op
/// on the shape §4.3 calls row 4: every production mint publishes `declaration: None` for an
/// empty schema, because `build_bounded_declaration` returns `None` on
/// `schema.points.is_empty()` before doing anything else.
///
/// **R4b/A discriminates the repair** — pre-repair `proposal.template` is `None` here, so the
/// `Some(..)` assertion fails. **R4b/B discriminates in the OPPOSITE direction** — pre-repair
/// the firewall sees an unresolved `None` and ACCEPTS, so `Priority` is the post-repair verdict
/// only. The pair is the row; neither half alone shows both directions.
///
/// # The capability R4b/B does not create, recorded because it looks like one
///
/// A points-empty offer carrying a restored declaration is reachable only through the restore
/// ingress — no production mint emits that pair. The `or_else` is deliberately NOT guarded with
/// `!points.is_empty()`: a live client can already send `template: Some(anything owned by the
/// proposer)` against a points-empty offer today and reach `proposal.template` with the pin
/// block skipped, so the firewall is the only gate on this shape both before and after. A guard
/// for an unreachable case is a special case; the behaviour is pinned here instead.
#[test]
fn c2_r4b_a_points_empty_offer_is_gated_by_the_owner_firewall_alone() {
    let mut state = load_f4();
    drive_f4_to_offer(&mut state, 400).expect("the bounded offer fires (see R1)");
    let (proposer, _certificate, _schema) = offer_parts(&state);
    let hostile = state
        .players
        .iter()
        .find(|p| p.id != proposer && !p.is_eliminated)
        .map(|p| p.id)
        .expect("REACH-GUARD: a living seat other than the proposer must exist on a 4p board");
    let published =
        offer_declaration(&state).expect("the untampered offer publishes a declaration");

    // One F4 offer, `schema.points` emptied, `declaration` set per arm. Nothing else differs.
    let point_free_offer = |declaration: Option<PlayerId>| {
        let mut probe = state.clone();
        match &mut probe.waiting_for {
            WaitingFor::LoopShortcut {
                schema,
                declaration: decl,
                ..
            } => {
                schema.points.clear();
                *decl = declaration.map(|owner| {
                    let mut d = published.clone();
                    d.owner = owner;
                    d
                });
            }
            other => panic!("expected the CR 732.2a bounded offer, got {other:?}"),
        }
        assert!(
            match &probe.waiting_for {
                WaitingFor::LoopShortcut { schema, .. } => schema.points.is_empty(),
                _ => false,
            },
            "REACH-GUARD: the row is about the SKIPPED pin block, so the point set must really \
             be empty — otherwise `declaration_conforms` runs and the firewall is not alone"
        );
        probe
    };

    // ── R5, the point-free CONTROL: passes pre- and post-repair ──
    assert_eq!(
        declare_template_free(&point_free_offer(None), proposer, 1)
            .waiting_for
            .variant_name(),
        "RespondToShortcut",
        "R5 CONTROL: a point-free offer publishing no declaration drains exactly as before — \
         the `or_else` resolves `None` to `None` and is a no-op. This arm is what makes R4b/B's \
         refusal below attributable to the OWNER rather than to the emptied schema"
    );

    // ── R4b/A: retained declaration, owner == proposer ──
    let honest = declare_template_free(&point_free_offer(Some(proposer)), proposer, 1);
    assert_eq!(
        honest.waiting_for.variant_name(),
        "RespondToShortcut",
        "R4b/A: the firewall passes a declaration owned by the proposer"
    );
    assert_eq!(
        accepted_proposal(&honest)
            .template
            .as_ref()
            .map(|t| t.owner),
        Some(proposer),
        "R4b/A discriminates the repair: PRE-repair `proposal.template` is `None` here, because \
         the offer's declaration was discarded and the pin block never ran. The resolved \
         template reaching the proposal is the change"
    );

    // ── R4b/B: retained declaration, foreign owner — the OPPOSITE direction ──
    assert_eq!(
        declare_template_free(&point_free_offer(Some(hostile)), proposer, 1)
            .waiting_for
            .variant_name(),
        "Priority",
        "R4b/B discriminates in the opposite direction from R4b/A: PRE-repair this ACCEPTS, \
         because the firewall inspects an unresolved `None` and passes it. Post-repair the \
         resolved foreign-owner declaration meets the firewall and is refused. A row asserting \
         only R4b/A would miss that the repair WIDENS what the firewall inspects"
    );
}
