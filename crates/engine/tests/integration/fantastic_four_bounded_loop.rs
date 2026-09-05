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
const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

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
        .into_game_state()
        .expect("persisted test snapshot satisfies the checked restore contract");
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
/// `jq -c '{gameState}' … | gzip -9 -n` (859,705 B, sha256
/// `31eae665961b3da9161a1ba91907db150d9aa1787b0f3ad667be22055ccbca7e`; the raw envelope is
/// 20.5 MB, of which `turnCheckpoints` alone is 16.4 MB and no loader reads it).
///
/// That `jq` pipeline ALONE is no longer sufficient: this capture predates U5, so its bare
/// `"deck_size": 100` must first become the adjacently-tagged `DeckSizeRule` form
/// `{"type":"Exactly","data":100}` — the variant taken from the sibling `format` field,
/// `Commander` here. Piping the raw member straight through does not merely miss the digest; it
/// yields a fixture `PersistedGameState` cannot deserialize, so `load_dump`'s
/// `.expect("gameState deserializes through the production decoder")` aborts — a red test on a
/// green engine. MODE2 below was captured the same day and needs the same migration.
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
/// `jq -c '{gameState}' … | gzip -9 -n` plus the same U5 `deck_size` migration MODE1
/// documents (968,121 B, sha256
/// `04bb89f9910edf1c828a7639217620481e40609febc0a3801e59a14857c35b9d`).
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

        assert!(
            predictability_gate(&declaration, &schema.points).is_ok(),
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
        .into_game_state()
        .expect("persisted test snapshot satisfies the checked restore contract");
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
/// `validate_pins` have something to check and the accepting arm proves they PASS (against an
/// exposed-nothing schema `predictability_gate` has no required slot, and `f4_pin_template`
/// derives its pins FROM the schema, so `validate_pins` is handed none either — a refusal on
/// both arms would then be reported as a firewall hit); and the hostile owner names a LIVING seat
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
        "reach-guard: a non-empty schema gives `predictability_gate` a required slot, and \
         `f4_pin_template` derives its pins from that schema so `validate_pins` is handed one \
         too — so the accepting arm below proves the pair is keyed to `owner`"
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
/// The schema publishes points (else `declaration_conforms` decides nothing about a pin-free
/// declaration and the row measures the empty path — that is
/// [`c2_r4b_a_points_empty_offer_is_gated_by_the_owner_firewall_alone`]'s
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
        "REACH-GUARD: with an empty point set `declaration_conforms` has no published slot to \
         check and this row would measure the owner firewall instead of the repair"
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
/// On a point-free offer site F never runs and `declaration_conforms` is vacuous for a pin-free
/// declaration, so the resolved template meets the firewall alone. Three arms on one F4-derived
/// fixture, `schema.points` emptied and the declaration's pins stripped with them:
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
/// ingress — no production mint emits that pair. The `or_else` needs no `!points.is_empty()`
/// guard of its own, because the RESOLVED template is validated either way: CR 732.2a lets a
/// declaration pin only choices the offer published, so a SLOT-ADDRESSING pin naming an
/// unexposed slot is refused on that axis whatever its owner, and a pin-free one leaves
/// the owner as the single remaining gate — which is what these arms vary.
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
    // The declaration is stripped of its pins with the points: CR 732.2a lets a declaration pin
    // only choices the offer published, so a SLOT-ADDRESSING pin naming an unexposed slot is
    // refused on the PIN axis and the owner axis this row varies would not be the operative one.
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
                    d.decisions.clear();
                    d.owner = owner;
                    d
                });
            }
            other => panic!("expected the CR 732.2a bounded offer, got {other:?}"),
        }
        assert!(
            match &probe.waiting_for {
                WaitingFor::LoopShortcut {
                    schema,
                    declaration: Some(d),
                    ..
                } => schema.points.is_empty() && d.decisions.is_empty(),
                WaitingFor::LoopShortcut {
                    schema,
                    declaration: None,
                    ..
                } => schema.points.is_empty(),
                _ => false,
            },
            "REACH-GUARD: an empty point set with a pin-free declaration is what leaves \
             `declaration_conforms` vacuous, so the owner firewall is the only gate the arms \
             below can be measuring"
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

/// **A SLOT-ADDRESSING pin may name only choices the offer published — measured on the shape
/// where the published set is EMPTY and the charged set is not.**
///
/// CR 732.2a: a shortcut proposal describes a sequence of choices that may legally be taken.
/// An offer publishing no decision point states no such choice, so a template naming one is not
/// a legal answer to it. The title says SLOT-ADDRESSING because the fourth arm below carries a
/// pin that addresses no slot at all: a CR 603.3b ordering pin, refused by `validate_pins` as
/// `NotALoopDecision` rather than as `UnexposedSlot`.
///
/// The two sets diverge by construction and legitimately so: `victim_slot` is derived from the
/// period's ANNOUNCED targets, which CR 119.3 charges whoever announces them, while
/// `schema.points` publishes only the proposer's own CR 601.2c choices. A period whose announced
/// target is nobody's published choice therefore mints a charged slot with no point beside it,
/// and a fabricated pin naming that slot is what the per-cycle conformance check would size its
/// comparison by.
///
/// Four arms on one staged offer, one axis apart — the declaration's `decisions`:
///
/// | arm | `decisions` | expected |
/// |---|---|---|
/// | **matched positive** | empty | `RespondToShortcut` — the staged offer accepts |
/// | **charged slot** | a `Targets` pin naming a `victim_slot` entry | `Priority` |
/// | **unknown slot** | a `Targets` pin naming neither a point nor a charged slot | `Priority` |
/// | **ordering pin** | an `Order` pin on the charged slot's source | `Priority` |
///
/// The positive is asserted FIRST and is what makes the two refusals attributable to the pin
/// rather than to the staged offer refusing everything. The second refusal is the class end the
/// first does not reach: the guard is keyed to what the offer PUBLISHED, not to what the
/// certificate CHARGES, so a pin naming neither is refused by the same predicate. The fourth
/// arm is the other end — the member the title's SLOT-ADDRESSING qualifier excludes, refused on
/// its own axis so the qualifier stays a measurement rather than a hedge.
///
/// REVERT-PROBE: skip `declaration_conforms` when `offer.schema.points.is_empty()` ⇒ both
/// refusals open APNAP and carry the fabricated pin into `proposal.template`.
#[test]
fn a_slot_addressing_pin_naming_a_slot_the_offer_never_published_is_refused() {
    use engine::analysis::decision_template::{
        AnnouncementSubject, DecisionGroupKey, DecisionSlot, DecisionTemplate, PinnedDecision,
        Ranking, ReplayMode, TargetPin, TargetSchedule,
    };

    let mut state = load_f4();
    drive_f4_to_offer(&mut state, 400).expect("the bounded offer fires (see R1)");
    let (proposer, certificate, _schema) = offer_parts(&state);
    let per_cycle = certificate
        .per_cycle
        .clone()
        .expect("a bounded offer publishes its per-period signature");
    let (charged_slot, magnitude) = per_cycle.victim_slot.first().cloned().expect(
        "REACH-GUARD: the fabricated pin must name a slot the certificate really charges, else \
         it would be inert at the per-cycle check and the refusal below would be about nothing",
    );
    assert!(
        magnitude > 0,
        "REACH-GUARD: the charged slot must carry a strictly positive CR 119.3 magnitude; \
         got {magnitude} from {:?}",
        per_cycle.victim_slot
    );
    let aimed = state
        .players
        .iter()
        .find(|p| p.id != proposer && !p.is_eliminated)
        .map(|p| p.id)
        .expect("REACH-GUARD: a living seat other than the proposer must exist on a 4p board");

    // The staged offer: the F4 offer with its published point set emptied. Nothing else moves —
    // the certificate keeps the `victim_slot` entry the fabricated pin names.
    let mut staged = state.clone();
    match &mut staged.waiting_for {
        WaitingFor::LoopShortcut {
            schema,
            declaration,
            ..
        } => {
            schema.points.clear();
            *declaration = None;
        }
        other => panic!("expected the CR 732.2a bounded offer, got {other:?}"),
    }
    let (staged_proposer, staged_certificate, staged_schema) = offer_parts(&staged);
    assert!(
        staged_schema.points.is_empty() && staged_proposer == proposer,
        "REACH-GUARD: the staged offer must publish nothing while keeping the live proposer, \
         else the arms below measure the owner firewall instead"
    );
    assert!(
        staged_certificate
            .per_cycle
            .as_ref()
            .is_some_and(|pc| pc.victim_slot.iter().any(|(s, _)| *s == charged_slot)),
        "REACH-GUARD: emptying the published set must leave the CHARGED set intact — that \
         divergence is the shape this row is about"
    );

    let declare = |decisions: Vec<PinnedDecision>| {
        let mut probe = staged.clone();
        apply(
            &mut probe,
            proposer,
            GameAction::DeclareShortcut {
                count: IterationCount::Fixed(1),
                template: Some(DecisionTemplate {
                    owner: proposer,
                    decisions,
                    replay: ReplayMode::Scheduled {
                        count: IterationCount::Fixed(1),
                    },
                    key: DecisionGroupKey::from_sources(
                        std::slice::from_ref(&charged_slot.source),
                        DecisionKind::LoopChoice,
                    ),
                }),
            },
        )
        .expect("dispatched — a refusal is a HANDBACK, not an error");
        probe
    };
    let aimed_at = |slot: DecisionSlot| {
        vec![PinnedDecision::Targets {
            slot,
            targets: vec![TargetPin::Scheduled(TargetSchedule::Constant(
                Ranking::one(AnnouncementSubject::Seat(aimed)),
            ))],
        }]
    };

    assert_eq!(
        declare(vec![]).waiting_for.variant_name(),
        "RespondToShortcut",
        "MATCHED POSITIVE: a declaration that pins nothing is a legal answer to an offer that \
         published nothing, so the refusals below are keyed to the PIN and not to the staged \
         offer refusing every declaration"
    );

    assert_eq!(
        declare(aimed_at(charged_slot.clone()))
            .waiting_for
            .variant_name(),
        "Priority",
        "CR 732.2a: a pin naming a slot the offer published no point for is not a legal answer \
         — the declaration hands back to manual play and no `ShortcutProposal` is built, so \
         nothing carries the fabricated pin into the drive"
    );

    let unknown = DecisionSlot {
        source: charged_slot.source.clone(),
        index: charged_slot.index.wrapping_add(1),
    };
    assert!(
        !per_cycle.victim_slot.iter().any(|(s, _)| *s == unknown),
        "REACH-GUARD: the second arm's slot must be absent from the charged set too, else it \
         is the first arm again"
    );
    assert_eq!(
        declare(aimed_at(unknown)).waiting_for.variant_name(),
        "Priority",
        "the refusal is keyed to what the offer PUBLISHED, so a pin the certificate does not \
         charge either is refused by the same predicate"
    );

    assert_eq!(
        declare(vec![PinnedDecision::Order {
            source: charged_slot.source.clone(),
            pos: 0,
        }])
        .waiting_for
        .variant_name(),
        "Priority",
        "CR 603.3b: an ordering pin is a choice about the APNAP order triggered abilities are \
         put on the stack in, not one of the CR 732.2a per-iteration choices a loop \
         declaration answers, so `validate_pins` refuses it as `NotALoopDecision` — on an \
         offer publishing nothing exactly as on one publishing a point"
    );
}

/// **An `Order` pin is not an answer to the F4 offer's published choices — on the LIVE,
/// NON-EMPTY schema.**
///
/// CR 732.2a: a shortcut proposal describes the sequence of choices that will be taken.
/// CR 603.3b: an `Order` pin is a choice about the APNAP order simultaneously triggered
/// abilities are put on the stack in — a different decision kind, which this offer publishes
/// no point for and which the drive never reads.
///
/// Three arms on one live offer, one axis apart — the declaration's `decisions`:
///
/// | arm | `decisions` | expected |
/// |---|---|---|
/// | **paired positive** | the conformant [`f4_pin_template`] set | `RespondToShortcut` |
/// | **swapped** | the same, `Targets` pin replaced by an `Order` pin on that source | `Priority` |
/// | **extra** | the conformant set PLUS an `Order` pin on that source | `Priority` |
///
/// SWAPPED is the whole-defect member: before the repair the ordering pin COVERED the published
/// `Targets` point — `pin_slot` synthesized `{source, index 0}` for it and the gate compared
/// slots without kinds — while `validate_pins` checked nothing, so a declaration answering none
/// of the offer's published choices was accepted. EXTRA is the VALUE half alone, and the
/// multi-authority member coverage cannot reach: the conformant pins beside it already satisfy
/// coverage, so only `validate_pins`' refusal can reject it.
///
/// REVERT-PROBE, MEASURED per half rather than assumed. SWAPPED opens APNAP only under the FULL
/// pre-repair — `pin_slot` fabricating the `Order` slot AND `validate_pins`' `Order { .. } => {}`
/// arm — because either half alone still refuses it. EXTRA opens APNAP under the value half
/// alone. The coverage half alone is isolated in-crate, by
/// `analysis::decision_template::tests::gate_coverage_is_kind_aware`.
#[test]
fn an_order_pin_is_not_an_answer_to_the_f4_offers_published_choices() {
    use engine::analysis::decision_template::PinnedDecision;

    let mut state = load_f4();
    drive_f4_to_offer(&mut state, 400).expect("the bounded offer fires (see R1)");
    let (proposer, _certificate, schema) = offer_parts(&state);
    let schema = schema.clone();

    let target_source = schema
        .points
        .iter()
        .find(|p| matches!(p.kind, DecisionPointKind::Targets { .. }))
        .map(|p| p.slot.source.clone())
        .expect(
            "REACH-GUARD: the live schema must publish a `Targets` point, else the swap below \
             is a refusal on an EMPTY schema rather than on a published choice",
        );

    let conformant = f4_pin_template(&schema, proposer, 1);
    let declare = |decisions: Vec<PinnedDecision>| {
        let mut probe = state.clone();
        let mut template = conformant.clone();
        template.decisions = decisions;
        apply(
            &mut probe,
            proposer,
            GameAction::DeclareShortcut {
                count: IterationCount::Fixed(1),
                template: Some(template),
            },
        )
        .expect("dispatched — a refusal is a HANDBACK, not an error");
        probe
    };

    assert_eq!(
        declare(conformant.decisions.clone())
            .waiting_for
            .variant_name(),
        "RespondToShortcut",
        "PAIRED POSITIVE, same offer and same call: the conformant declaration still opens the \
         CR 732.2b APNAP window, so the two refusals below are keyed to the ordering pin"
    );

    let ordering = PinnedDecision::Order {
        source: target_source,
        pos: 0,
    };
    let swapped: Vec<PinnedDecision> = conformant
        .decisions
        .iter()
        .map(|pin| match pin {
            PinnedDecision::Targets { .. } => ordering.clone(),
            other => other.clone(),
        })
        .collect();
    assert!(
        swapped
            .iter()
            .any(|p| matches!(p, PinnedDecision::Order { .. }))
            && !swapped
                .iter()
                .any(|p| matches!(p, PinnedDecision::Targets { .. })),
        "REACH-GUARD: the swap must have replaced the `Targets` pin, else SWAPPED is the \
         conformant arm again. got {swapped:?}"
    );
    assert_eq!(
        declare(swapped).waiting_for.variant_name(),
        "Priority",
        "CR 732.2a: an ordering pin covers no published targeting point, so that choice is \
         unpinned, the declaration hands back to manual play and no `ShortcutProposal` is \
         built to carry the pin into the drive"
    );

    let mut extra = conformant.decisions.clone();
    extra.push(ordering);
    assert_eq!(
        declare(extra).waiting_for.variant_name(),
        "Priority",
        "CR 603.3b: coverage is already satisfied by the conformant pins beside it, so only \
         `validate_pins`' `NotALoopDecision` refusal can reject this declaration"
    );
}

/// **The reserved victim domain the offer publishes GATES the committed drive — driven on the
/// real 4-player dump.**
///
/// CR 704.5a: `elimination_bounds` reserved elimination headroom only for the seats in
/// `declarable_victims`, and the per-cycle conformance check confines its lift to them. The two
/// arms differ in EXACTLY that field — board, schema, declaration and count are identical.
///
/// | arm | `per_cycle.declarable_victims` | expected |
/// |---|---|---|
/// | **live** | what the mint published | the full declared count of `N` cycles commits |
/// | **staged** | the proposer's own seat, which the live domain EXCLUDES | truncates at the first re-aimed cycle |
///
/// # Why the declaration RE-AIMS, and why that doubles as the reach-guard
///
/// MEASURED on this dump: with the conformant [`f4_pin_template`] the observed cycle is
/// BYTE-IDENTICAL to the published period, so `conforms` decides on its leading equality and
/// never consults the domain — the two arms would then be indistinguishable for a reason that
/// has nothing to do with this check. A CR 601.2c re-aim onto another declarable victim is what
/// makes the lift decide the verdict, and it is the shape
/// [`t1_a_victim_changing_declaration_commits_its_whole_count_on_the_seats_it_declared`]
/// already drives. The live arm asserts the re-aimed seat absorbs a STRICTLY POSITIVE share of
/// the count — an outcome byte-equality cannot produce — so the lift is measured to have run
/// rather than assumed. Only the first cycle charges the published seat: its Torch target was
/// announced before the grant, so the schedule cannot re-aim it.
///
/// The staged value is read off the board rather than invented: the reach-guards assert the
/// live domain is non-empty and EXCLUDES the proposer's own seat, so `[proposer]` is an
/// out-of-domain singleton this offer itself makes constructible. Both arms are asserted
/// ACCEPTED at declare, so what separates them is the DRIVE's per-cycle check.
///
/// REVERT-PROBE: drop `slot_charged_life`'s domain filter ⇒ the staged arm commits the same `N`
/// cycles as the live one and the two arms stop being distinguishable. That the staged arm
/// commits its ONE byte-identical cycle is also what proves the refusal is the domain's and not
/// a blanket one.
#[test]
fn u2_the_published_victim_domain_gates_the_committed_drive() {
    const N: u32 = 3;
    let mut outcomes = vec![];
    for stage_to_proposer in [false, true] {
        let mut state = load_f4();
        drive_f4_to_offer(&mut state, 400).expect("the bounded offer fires (see R1)");
        let (proposer, certificate, schema) = offer_parts(&state);
        let schema = schema.clone();
        let per_cycle = certificate
            .per_cycle
            .clone()
            .expect("a bounded offer publishes its per-period signature");

        assert!(
            !per_cycle.declarable_victims.is_empty()
                && !per_cycle.declarable_victims.contains(&proposer),
            "REACH-GUARD: the LIVE domain must be non-empty and must EXCLUDE the proposer's own \
             seat, else `[proposer]` below is not an out-of-domain value. got {:?}, proposer \
             {proposer:?}",
            per_cycle.declarable_victims
        );
        assert!(
            per_cycle.victim_slot.iter().any(|(_, m)| *m > 0),
            "REACH-GUARD: a charged slot with a strictly positive CR 119.3 magnitude, else the \
             lift is the identity whatever the domain. got {:?}",
            per_cycle.victim_slot
        );

        let (published_seat, life_rate) = per_cycle
            .delta
            .life
            .iter()
            .find(|(_, magnitude)| **magnitude < 0)
            .map(|(seat, magnitude)| (*seat, -*magnitude))
            .expect("REACH-GUARD: the published period must charge some seat a life LOSS");
        let re_aimed = *per_cycle
            .declarable_victims
            .iter()
            .find(|seat| **seat != published_seat)
            .expect(
                "REACH-GUARD: a SECOND declarable victim must exist, else the re-aim below \
                 names the published seat again and the observed cycle stays byte-identical",
            );

        if stage_to_proposer {
            match &mut state.waiting_for {
                WaitingFor::LoopShortcut { certificate, .. } => {
                    certificate
                        .per_cycle
                        .as_mut()
                        .expect("the signature the reach-guards above just read")
                        .declarable_victims = vec![proposer];
                }
                other => panic!("expected the CR 732.2a bounded offer, got {other:?}"),
            }
        }

        let life_before = life_by_seat(&state);
        apply(
            &mut state,
            proposer,
            GameAction::DeclareShortcut {
                count: IterationCount::Fixed(N),
                template: Some(f4_scheduled_template(
                    &schema,
                    proposer,
                    N,
                    &[(0, re_aimed)],
                )),
            },
        )
        .expect("the declaration is dispatched");
        assert!(
            matches!(state.waiting_for, WaitingFor::RespondToShortcut { .. }),
            "staged={stage_to_proposer}: BOTH arms must be ACCEPTED at declare — the domain is \
             read by the DRIVE's per-cycle check, never by the declare firewall — else the \
             zero-commit below is a declare-time refusal. got {:?}",
            state.waiting_for
        );
        assert!(
            accept_all_opponents(&mut state) > 0,
            "the CR 732.2c window must actually take responses"
        );

        let life_after = life_by_seat(&state);
        outcomes.push((
            life_of(&life_before, published_seat) - life_of(&life_after, published_seat),
            life_of(&life_before, re_aimed) - life_of(&life_after, re_aimed),
            life_rate,
        ));
    }

    let (live_published_loss, live_re_aimed_loss, life_rate) = outcomes[0];
    assert!(
        life_rate > 0,
        "ANTI-VACUITY: the published per-cycle life LOSS must be strictly positive, else every \
         relation below degenerates to `0 == 0 * {N}`"
    );
    assert!(
        live_re_aimed_loss > 0,
        "REACH-GUARD: the re-aim must actually MOVE the charge off the published seat, else \
         every observed cycle stays byte-identical to the published period and `conforms` \
         decides on its leading equality without ever consulting the domain. published seat \
         lost {live_published_loss}, re-aimed seat lost {live_re_aimed_loss}"
    );
    assert_eq!(
        live_published_loss + live_re_aimed_loss,
        life_rate * i64::from(N),
        "LIVE: with the domain the mint published, every cycle conforms — the cycle whose \
         target was announced before the grant charges the published seat, the re-aimed \
         remainder is admitted by the domain-confined lift, and the full declared count of {N} \
         commits across the two"
    );
    let (staged_published_loss, staged_re_aimed_loss, _) = outcomes[1];
    assert_eq!(
        (staged_published_loss, staged_re_aimed_loss),
        (life_rate, 0),
        "CR 704.5a: with the domain staged to a seat this cycle's bound reserved nothing for, \
         only the byte-identical first cycle survives — its leading equality needs no lift — \
         and the first RE-AIMED cycle is refused, because neither side's loss is liftable so \
         the residues differ. The drive truncates there and the re-aimed seat is never charged. \
         live was ({live_published_loss}, {live_re_aimed_loss})"
    );
}

// ─────────────────────────────────────────────────────────────────────────────────────────
// U1 / U2 — the per-cycle accounting is victim-invariant and carries the token axis
// ─────────────────────────────────────────────────────────────────────────────────────────

/// [`f4_pin_template`] with its single announced `Targets` pin re-aimed by a SCHEDULE. Every
/// other pin the board publishes is left exactly as the conformant template builds it, so the
/// only axis these rows vary is WHICH SEAT the announced slot names at each iteration
/// (CR 601.2c + CR 732.2a: the choice is fixed by the declaration, not made per iteration).
fn f4_scheduled_template(
    schema: &engine::analysis::decision_template::ShortcutDecisionSchema,
    owner: PlayerId,
    count: u32,
    segments: &[(u32, PlayerId)],
) -> engine::analysis::decision_template::DecisionTemplate {
    use engine::analysis::decision_template::{
        AnnouncementSubject, PinnedDecision, Ranking, TargetPin, TargetSchedule,
    };
    let mut template = f4_pin_template(schema, owner, count);
    let mut aimed = 0;
    for pin in &mut template.decisions {
        if let PinnedDecision::Targets { targets, .. } = pin {
            *targets = vec![TargetPin::Scheduled(TargetSchedule::Piecewise(
                segments
                    .iter()
                    .map(|(start, seat)| (*start, Ranking::one(AnnouncementSubject::Seat(*seat))))
                    .collect(),
            ))];
            aimed += 1;
        }
    }
    assert_eq!(
        aimed, 1,
        "the schedule these rows vary belongs to the board's ONE announced target slot; \
         re-aiming zero pins would leave the declaration identical to the conformant one and \
         re-aiming several would make the seat attribution below ambiguous"
    );
    template
}

/// Per-seat life, read by SEAT ID rather than positionally — these rows declare seats by name
/// and a positional read would silently follow a re-dump's player ordering instead.
fn life_by_seat(state: &GameState) -> Vec<(PlayerId, i64)> {
    state
        .players
        .iter()
        .map(|p| (p.id, p.life as i64))
        .collect()
}

fn life_of(snapshot: &[(PlayerId, i64)], seat: PlayerId) -> i64 {
    snapshot
        .iter()
        .find(|(id, _)| *id == seat)
        .map(|(_, life)| *life)
        .unwrap_or_else(|| panic!("{seat:?} is not seated on this board"))
}

/// The board-side total of ONE published counter class, read through the very
/// [`ResourceVector::snapshot`] projection the certificate's per-cycle counter entry is a rate
/// for — so the growth measured below and the rate it is compared against cannot be different
/// quantities.
fn counter_class_total(
    state: &GameState,
    key: &(
        engine::analysis::resource::CounterClass,
        engine::analysis::resource::ObjectClass,
    ),
) -> i64 {
    ResourceVector::snapshot(state)
        .counters
        .get(key)
        .copied()
        .unwrap_or(0)
}

/// **T1** — a VICTIM-CHANGING declaration commits its whole count, charges only the seats it
/// declared, and moves the board by the published per-cycle magnitude.
///
/// CR 732.2a lets one declaration specify a sequence of choices that aims an announced target
/// slot at a different seat in different iterations. At BASE the drive compared each committed
/// cycle against the published signature by exact equality, so the first cycle whose life
/// landed on a different seat than the certified period's diverged and the drive truncated
/// there. `PeriodicDelta::conforms` lifts the pinned slot's charge off both sides before
/// comparing, so the re-aim conforms and the declared count commits in full.
///
/// # Discrimination
///
/// Revert `conforms` to `self.delta == *observed`: the drive truncates at the first segment
/// boundary, so (a) fails, (b) fails on the two later seats, and (e) fails because the counter
/// growth is the published rate times the TRUNCATED count.
///
/// Both magnitude relations are `rate * count` with the rate READ OFF the certificate, never
/// pinned. The counter leg is the non-degenerate one and its guard demands a published rate
/// STRICTLY ABOVE 1 — a rate-1 axis reduces `rate * n` to the bare committed count and could
/// not tell a per-period magnitude from a degenerate one. A board that publishes rate 1 must
/// RED this row rather than satisfy it.
#[test]
fn t1_a_victim_changing_declaration_commits_its_whole_count_on_the_seats_it_declared() {
    let mut state = load_f4();
    drive_f4_to_offer(&mut state, 400).expect("the bounded offer fires (see R1)");
    let (proposer, certificate, schema) = offer_parts(&state);
    let per_cycle = certificate
        .per_cycle
        .clone()
        .expect("a bounded offer publishes its per-period signature");
    let schema = schema.clone();
    let n = schema.max_iterations;

    let life_rate = -per_cycle.delta.life.values().copied().min().unwrap_or(0);
    assert!(
        life_rate > 0,
        "ANTI-VACUITY: the published per-cycle life LOSS must be strictly positive, else the \
         sum relation below degenerates to `0 == 0 * {n}`. published life={:?}",
        per_cycle.delta.life
    );
    assert_eq!(
        per_cycle.delta.counters.len(),
        1,
        "with more than one published counter class the board-side total is no longer that \
         class's own projection, and this row must RED rather than guess which entry the \
         growth belongs to. published counters={:?}",
        per_cycle.delta.counters
    );
    let (counter_key, counter_rate) = per_cycle
        .delta
        .counters
        .iter()
        .map(|(key, rate)| (*key, *rate))
        .next()
        .expect("the length assertion above already established the single entry");
    assert!(
        counter_rate > 1,
        "ANTI-DEGENERACY: this is the only axis whose published rate can separate a per-period \
         MAGNITUDE from the bare committed count, so it must exceed 1. published \
         {counter_key:?}={counter_rate}"
    );

    // Three segments, one declared seat each, with the boundaries derived from the published
    // count — nothing here pins a figure.
    let declared = [P1, P2, P3];
    let segments: Vec<(u32, PlayerId)> = declared
        .iter()
        .enumerate()
        .map(|(i, seat)| (i as u32 * n / 3, *seat))
        .collect();
    assert!(
        segments.windows(2).all(|w| w[0].0 < w[1].0),
        "the published count {n} must be large enough for three DISTINCT segment starts, else \
         a declared seat absorbs no iteration and (b) below reds for a fixture reason. \
         derived starts={:?}",
        segments.iter().map(|(start, _)| *start).collect::<Vec<_>>()
    );
    let template = f4_scheduled_template(&schema, proposer, n, &segments);

    let life_before = life_by_seat(&state);
    let counters_before = counter_class_total(&state, &counter_key);

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
        "the victim-changing declaration must be ACCEPTED and open the CR 732.2b APNAP window \
         — a `Priority` here would mean the shortfall below is a declare-time refusal rather \
         than the drive's behaviour. got {:?}",
        state.waiting_for
    );
    assert!(
        accept_all_opponents(&mut state) > 0,
        "the CR 732.2c window must actually take responses"
    );

    let life_after = life_by_seat(&state);
    let counters_after = counter_class_total(&state, &counter_key);
    let loss = |seat: PlayerId| life_of(&life_before, seat) - life_of(&life_after, seat);
    let declared_losses: Vec<i64> = declared.iter().map(|seat| loss(*seat)).collect();

    // (a) CR 732.2a: the whole declared count commits, spread across the declared seats.
    assert_eq!(
        declared_losses.iter().sum::<i64>(),
        life_rate * i64::from(n),
        "the declared seats {declared:?} must absorb the published per-cycle life rate \
         {life_rate} times the full declared count {n}. per-seat losses={declared_losses:?}, \
         life {life_before:?} -> {life_after:?}"
    );
    // (b) every declared seat is actually reached — a drive that truncated at the first
    // boundary leaves the later seats untouched while (a) could still be met by one seat.
    assert!(
        declared_losses.iter().all(|l| *l > 0),
        "every declared seat must strictly decrease; a zero belongs to a seat the drive never \
         reached. losses={declared_losses:?} for {declared:?}"
    );
    // (c) the lift may not relocate a charge onto a seat the declaration never named.
    for (seat, before) in &life_before {
        if !declared.contains(seat) {
            assert!(
                life_of(&life_after, *seat) >= *before,
                "{seat:?} was never declared and must not lose life: {before} -> {}",
                life_of(&life_after, *seat)
            );
        }
    }
    // (d) the split is NOT uniform, and the cause is a fact of this board rather than a defect:
    // the first driven cycle resolves the target announced BEFORE the drive begins, so every
    // segment boundary lands one iteration late. Do not "correct" the split back into equal
    // parts — that would make this assertion the thing being worked around.
    assert!(
        declared_losses.windows(2).any(|w| w[0] != w[1]),
        "the segment boundaries land one iteration late because cycle 0 resolves the \
         pre-announced target, so the declared seats CANNOT absorb equal shares. an equal \
         split here means the index map moved. losses={declared_losses:?}"
    );
    // (e) THE COUNTER LEG — the drive moved the BOARD by the published magnitude, not merely
    // the life axis. An implementation that commits the full count on life while the board
    // stops advancing passes (a) and fails here.
    assert_eq!(
        counters_after - counters_before,
        counter_rate * i64::from(n),
        "{counter_key:?} must grow by the published per-cycle rate {counter_rate} times the \
         declared count {n}: {counters_before} -> {counters_after}"
    );
}

/// **T2** — the same-seat control: the SCHEDULE SHAPE is not what was broken.
///
/// A two-segment `Piecewise` naming ONE seat commits its whole count. This row PASSES AT BASE
/// BY DESIGN and is a REACH-GUARD for [`t1_a_victim_changing_declaration_commits_its_whole_count_on_the_seats_it_declared`],
/// not evidence for the phase: it holds the schedule shape fixed and varies only whether the
/// declaration changes victim, so a T1 failure cannot be blamed on `Piecewise` itself. There is
/// no revert that reds it, and the phase must not ACQUIRE its pass.
#[test]
fn t2_reach_guard_a_same_seat_schedule_shape_already_commits_its_whole_count() {
    let mut state = load_f4();
    drive_f4_to_offer(&mut state, 400).expect("the bounded offer fires (see R1)");
    let (proposer, certificate, schema) = offer_parts(&state);
    let per_cycle = certificate
        .per_cycle
        .clone()
        .expect("a bounded offer publishes its per-period signature");
    let schema = schema.clone();
    let n = schema.max_iterations;

    let life_rate = -per_cycle.delta.life.values().copied().min().unwrap_or(0);
    assert!(
        life_rate > 0,
        "ANTI-VACUITY: the published per-cycle life LOSS must be strictly positive. published \
         life={:?}",
        per_cycle.delta.life
    );

    let segments = [(0, P1), (n / 3, P1)];
    assert!(
        segments[0].0 < segments[1].0,
        "the two segment starts must differ, else this is a `Constant` schedule wearing a \
         `Piecewise` name and controls nothing. derived={segments:?}"
    );
    let template = f4_scheduled_template(&schema, proposer, n, &segments);

    let life_before = life_by_seat(&state);
    apply(
        &mut state,
        proposer,
        GameAction::DeclareShortcut {
            count: IterationCount::Fixed(n),
            template: Some(template),
        },
    )
    .expect("the declaration is dispatched");
    assert!(
        matches!(state.waiting_for, WaitingFor::RespondToShortcut { .. }),
        "the same-seat declaration must be ACCEPTED and open the APNAP window, got {:?}",
        state.waiting_for
    );
    assert!(
        accept_all_opponents(&mut state) > 0,
        "the CR 732.2c window must actually take responses"
    );

    let life_after = life_by_seat(&state);
    assert_eq!(
        life_of(&life_before, P1) - life_of(&life_after, P1),
        life_rate * i64::from(n),
        "the one declared seat absorbs the whole count: life {life_before:?} -> {life_after:?}"
    );
    for (seat, before) in &life_before {
        if *seat != P1 {
            assert_eq!(
                life_of(&life_after, *seat),
                *before,
                "{seat:?} is not declared by this schedule and must not move"
            );
        }
    }
}

/// **The count-keyed preview on the real 4p board** — the published sample states this offer's
/// own count window, splits each count over the three announced player candidates, and charges
/// each candidate its own share of the drain.
///
/// This board is the one that gives the split cardinality: `delta.life` names ONE seat and the
/// per-cycle rate is 1, but the announced `Targets` point publishes THREE player candidates, so
/// a multi-seat allocation is real here and an implementation keyed on the life map alone
/// cannot produce it. The candidate ids also sit on the THIRD published point, so an
/// implementation keyed on `points[0]` mints the wrong namespace.
///
/// # Discrimination
///
/// Fold with no split ⇒ (a) publishes ONE life seat where the allocation names three; keep the
/// split but drop the remainder distribution ⇒ (c) is short on the elements whose count the
/// candidates do not divide, while (a) at the suggested count still passes; publish the
/// suggested count's magnitudes on every element ⇒ (c) fails on the second published count;
/// re-attribute the looper's library axis too ⇒ (b) fails.
#[test]
fn the_f4_offer_splits_each_published_count_over_its_announced_candidates() {
    use engine::game::interaction::{bind_interaction_authority, derive_viewer_interaction};
    use engine::game::visibility::filter_state_for_viewer;
    use engine::types::interaction::{
        InteractionOpportunityResponse, InteractionResponseSpec, InteractionSessionId,
        InteractionShortcutCountSpec, InteractionShortcutPointKind, InteractionShortcutPreview,
        InteractionShortcutPreviewFamily,
    };

    let mut state = load_f4();
    drive_f4_to_offer(&mut state, 400).expect("the bounded offer fires (see R1)");
    let (proposer, certificate, schema) = offer_parts(&state);
    let per_cycle = certificate
        .per_cycle
        .clone()
        .expect("a bounded offer publishes its per-period signature");

    // ── The announced slot and its candidate seats, read off the SCHEMA — the same "first
    //    `Targets` point" the producer keys the allocation on.
    let (charged_slot, seats) = schema
        .points
        .iter()
        .find_map(|point| match &point.kind {
            DecisionPointKind::Targets { legal_targets, .. } => Some((
                point.slot.clone(),
                legal_targets
                    .iter()
                    .map(|target| match target {
                        TargetRef::Player(seat) => Some(*seat),
                        TargetRef::Object(_) => None,
                    })
                    .collect::<Option<Vec<_>>>()
                    .expect("this board announces player targets"),
            )),
            _ => None,
        })
        .expect("the F4 offer announces a Targets point");
    let rate = per_cycle
        .victim_slot
        .iter()
        .find(|(slot, _)| *slot == charged_slot)
        .map(|(_, magnitude)| *magnitude)
        .expect("the period charges the announced slot");
    assert!(
        rate > 0 && seats.len() > 1,
        "reach-guard: a positive charge over MORE THAN ONE announced candidate is what makes a \
         per-seat split observable at all; rate={rate} seats={seats:?}"
    );
    assert_eq!(
        per_cycle.delta.life.len(),
        1,
        "reach-guard: this board's life map names ONE seat, so a per-seat split published here \
         cannot have been read off the life map; got {:?}",
        per_cycle.delta.life
    );
    assert!(
        !per_cycle.delta.library_delta.is_empty(),
        "reach-guard: the unattributed seat-keyed control axis must be fed, or (b) below is \
         vacuous; got {:?}",
        per_cycle.delta
    );

    let mut probe = state.clone();
    bind_interaction_authority(
        &mut probe,
        InteractionSessionId("f4-count-keyed-preview".to_string()),
    )
    .expect("bind the interaction authority over the live offer");
    let filtered = filter_state_for_viewer(&probe, proposer);
    let view = derive_viewer_interaction(&probe, &filtered, proposer);
    let InteractionOpportunityResponse::Schema {
        spec:
            InteractionResponseSpec::Shortcut {
                count,
                points,
                preview,
                ..
            },
        ..
    } = &view
        .opportunities
        .first()
        .expect("the live offer publishes an interaction opportunity")
        .response
    else {
        panic!("the live offer publishes a Shortcut response schema");
    };
    let InteractionShortcutCountSpec::Fixed {
        min,
        max,
        suggested,
    } = count
    else {
        panic!("a bounded offer publishes a Fixed count window, got {count:?}");
    };
    let candidate_ids = points
        .iter()
        .find(|point| point.kind == InteractionShortcutPointKind::Targets)
        .map(|point| point.candidate_ids.clone())
        .expect("the offer publishes its announced Targets point");

    // ── THE COUNT AXIS: the window's own endpoints are always stated.
    let published: Vec<u32> = preview.iter().map(|element| element.count).collect();
    for endpoint in [*min, *suggested, *max] {
        assert!(
            published.contains(&endpoint),
            "CR 732.2a: the window's own {endpoint} must be published; got {published:?}"
        );
    }

    // ── REACH-GUARDS on the published list, before any magnitude is read.
    assert!(
        published.len() > 1,
        "reach-guard: more than one count must be published, or a producer that ignores \
         `count` passes every leg below; got {published:?}"
    );
    assert!(
        preview.iter().any(|element| {
            u32::try_from(element.allocation.len())
                .is_ok_and(|parts| parts > 0 && element.count % parts != 0)
        }),
        "reach-guard: some published count must NOT divide its part count, or a split that \
         drops its remainder still totals correctly"
    );
    assert!(
        preview.iter().any(|element| element
            .allocation
            .iter()
            .map(|assignment| &assignment.choice_id)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            > 1),
        "reach-guard: some element must allocate over more than one candidate — the count-1 \
         element necessarily names exactly one"
    );

    let life_of = |element: &InteractionShortcutPreview| {
        let mut entries: Vec<(Option<u8>, i32)> = element
            .entries
            .iter()
            .filter(|entry| entry.family == InteractionShortcutPreviewFamily::Life)
            .map(|entry| (entry.player, entry.amount))
            .collect();
        entries.sort_unstable();
        entries
    };

    for element in preview {
        // ── THE SPLIT: one part per announced candidate, truncated by the count, ids taken
        //    from the point's own published order.
        let parts = usize::try_from(element.count)
            .expect("a published count fits usize")
            .min(candidate_ids.len());
        assert_eq!(element.allocation.len(), parts);
        assert_eq!(
            element
                .allocation
                .iter()
                .map(|assignment| assignment.choice_id.clone())
                .collect::<Vec<_>>(),
            candidate_ids[..parts].to_vec(),
            "CR 601.2c: the split speaks the announced point's OWN candidate ids, in the order \
             it published them"
        );
        let amounts: Vec<u32> = element
            .allocation
            .iter()
            .map(|assignment| assignment.amount)
            .collect();
        assert!(amounts.iter().all(|amount| *amount >= 1));
        assert!(amounts.windows(2).all(|pair| pair[0] >= pair[1]));
        assert_eq!(amounts.iter().sum::<u32>(), element.count);

        // ── (a) THE ENTRIES FOLLOW THE SPLIT: each allocated candidate at its own share.
        let mut expected: Vec<(Option<u8>, i32)> = seats
            .iter()
            .zip(element.allocation.iter())
            .map(|(seat, assignment)| {
                (
                    Some(seat.0),
                    i32::try_from(-rate * i64::from(assignment.amount))
                        .expect("a previewed magnitude fits i32"),
                )
            })
            .collect();
        expected.sort_unstable();
        assert_eq!(
            life_of(element),
            expected,
            "CR 119.3: at count {} the drain is charged to each announced candidate at the \
             published rate {rate} times its own share",
            element.count
        );

        // ── (c) TOTAL INVARIANCE: whatever the split, the seats together absorb the count.
        assert_eq!(
            life_of(element)
                .iter()
                .map(|(_, amount)| i64::from(*amount))
                .sum::<i64>(),
            -rate * i64::from(element.count),
            "the published split totals the element's own count at count {}",
            element.count
        );

        // ── (b) PAIRED CONTROL: the looper's own library axis is seat-keyed too and is NOT
        //    victim-attributed, so it keeps its seat and its unscaled product.
        for (seat, magnitude) in &per_cycle.delta.library_delta {
            assert!(
                element.entries.iter().any(|entry| {
                    entry.family == InteractionShortcutPreviewFamily::Mill
                        && entry.player == Some(seat.0)
                        && i64::from(entry.amount) == magnitude * i64::from(element.count)
                }),
                "an axis the announced slot does not charge keeps the seat `payload_seat` gave \
                 it and the raw count product: {:?}",
                element.entries
            );
        }
    }
}

/// **T3** — the token axis is PUBLISHED and DELIVERED, and the two are coupled.
///
/// CR 111.1: a token creation is an event, so the snapshot pair the per-period signature used
/// to be measured with reported zero on this axis while the board really minted a token every
/// cycle. `ResourceVector::period` derives the axis from the board pair, so the offer's
/// preview states the token product and the accepted drive delivers it.
///
/// The preview is read through the whole published projection
/// (`bind_interaction_authority` + `derive_viewer_interaction`), on a CLONE so nothing here
/// perturbs the drive that (b) then measures.
///
/// # Discrimination
///
/// (i) drop the token term from `period` ⇒ the rate guard and (a) red; (ii) point the
/// conformance site back at the raw snapshot pair while leaving the mint fed ⇒ zero cycles
/// commit, so (b) reds while (a) still passes — that coupling is the assertion; (iii) revert
/// `ring_delta_signature`'s `per_period` binding ⇒ this board certifies on the
/// resource-signature basis, the published rate returns to zero and the guard reds.
#[test]
fn t3_the_published_token_rate_is_delivered_by_the_accepted_drive() {
    use engine::game::interaction::{bind_interaction_authority, derive_viewer_interaction};
    use engine::game::visibility::filter_state_for_viewer;
    use engine::types::interaction::{
        InteractionOpportunityResponse, InteractionResponseSpec, InteractionSessionId,
        InteractionShortcutCountSpec, InteractionShortcutPreviewFamily,
    };

    let mut state = load_f4();
    drive_f4_to_offer(&mut state, 400).expect("the bounded offer fires (see R1)");
    let (proposer, certificate, schema) = offer_parts(&state);
    let per_cycle = certificate
        .per_cycle
        .clone()
        .expect("a bounded offer publishes its per-period signature");
    let schema = schema.clone();

    let token_rate = per_cycle.delta.tokens_created;
    assert!(
        token_rate > 0,
        "the published per-cycle token rate must be strictly positive — a ZERO rate FAILS this \
         row rather than satisfying it, because a zero-token preview is exactly the unfed axis \
         under test. published delta={:?}",
        per_cycle.delta
    );
    let life_rate = -per_cycle.delta.life.values().copied().min().unwrap_or(0);
    assert!(
        life_rate > 0,
        "ANTI-VACUITY: (b) re-derives the committed count as `total life loss / life_rate`, \
         which needs a strictly positive rate. published life={:?}",
        per_cycle.delta.life
    );

    // ── (a) THE PUBLISHED SIDE, read through the real viewer projection on a clone.
    let mut probe = state.clone();
    bind_interaction_authority(
        &mut probe,
        InteractionSessionId("t3-token-preview".to_string()),
    )
    .expect("bind the interaction authority over the live offer");
    let filtered = filter_state_for_viewer(&probe, proposer);
    let view = derive_viewer_interaction(&probe, &filtered, proposer);
    let opportunity = view
        .opportunities
        .first()
        .expect("the live offer publishes an interaction opportunity");
    let InteractionOpportunityResponse::Schema {
        spec: InteractionResponseSpec::Shortcut { count, preview, .. },
        ..
    } = &opportunity.response
    else {
        panic!(
            "the live offer publishes a Shortcut response schema, got {:?}",
            opportunity.response
        );
    };
    let InteractionShortcutCountSpec::Fixed { suggested, .. } = count else {
        panic!("a bounded offer publishes a Fixed count window, got {count:?}");
    };
    let preview = preview
        .iter()
        .find(|element| element.count == *suggested)
        .expect("the published sample always states the offer's suggested count");
    let tokens: Vec<i32> = preview
        .entries
        .iter()
        .filter(|entry| entry.family == InteractionShortcutPreviewFamily::Tokens)
        .map(|entry| entry.amount)
        .collect();
    assert_eq!(
        tokens.len(),
        1,
        "the preview must carry exactly ONE Tokens entry — none means the axis is unfed, \
         several means the projection stopped folding it. entries={:?}",
        preview.entries
    );
    assert_eq!(
        i64::from(tokens[0]),
        token_rate * i64::from(preview.count),
        "the preview states the token product for the count it travels with \
         ({}), at the published rate {token_rate}",
        preview.count
    );

    // ── (b) THE DELIVERED SIDE: the same count, driven, on the untouched board.
    let count = preview.count;
    let template = f4_pin_template(&schema, proposer, count);
    let (_, _, _, tokens_before) = commit_axes(&state);
    let life_before = life_by_seat(&state);

    apply(
        &mut state,
        proposer,
        GameAction::DeclareShortcut {
            count: IterationCount::Fixed(count),
            template: Some(template),
        },
    )
    .expect("the declaration is dispatched");
    assert!(
        matches!(state.waiting_for, WaitingFor::RespondToShortcut { .. }),
        "the declaration of the previewed count must be ACCEPTED, got {:?}",
        state.waiting_for
    );
    assert!(
        accept_all_opponents(&mut state) > 0,
        "the CR 732.2c window must actually take responses"
    );

    let (_, _, _, tokens_after) = commit_axes(&state);
    let life_after = life_by_seat(&state);
    let total_life_loss: i64 = life_before
        .iter()
        .map(|(seat, before)| (*before - life_of(&life_after, *seat)).max(0))
        .sum();
    let committed = total_life_loss / life_rate;
    // The REACH-GUARD on (b): a drive that committed nothing would satisfy a bare
    // "tokens grew by rate times committed" with 0 == 0.
    assert_eq!(
        committed,
        i64::from(count),
        "the accepted drive must commit the very count the preview stated: life \
         {life_before:?} -> {life_after:?} at rate {life_rate}"
    );
    assert_eq!(
        (tokens_after - tokens_before) as i64,
        token_rate * committed,
        "the board must mint the published per-cycle token rate {token_rate} on each of the \
         {committed} committed cycles: {tokens_before} -> {tokens_after} battlefield tokens"
    );
}

/// **T8** — the enumerated consumer MOVES, its channel stays CLOSED, and the admissible-action
/// set at the offer beat is PINNED by driving rather than asserted.
///
/// (a) is U2's: feeding the token axis puts `TokensCreated` into `LoopCertificate.unbounded`,
/// which is the consumption site the period-delta rewiring has to account for. (b) and (c)
/// PASS AT BASE BY DESIGN and are REACH-GUARDS — (b) is the containment this phase must not
/// break (a bounded offer publishes nothing to the unbounded-resource channel), and (c) is the
/// reducer property that containment argument quantifies over.
///
/// `GameState::loop_period_controller` — the predicate guarding the only mark route this
/// phase's new axis could reach — is `pub(crate)` and unnameable here, so (b) asserts its
/// INPUT: `last_loop_action_sequence` is EMPTY, which makes that function's leading
/// `first()?` return `None` outright.
///
/// # Discrimination
///
/// (a) reds if the token term is dropped from `ResourceVector::period`. (b) reds if the
/// accept-side route stops testing the controller predicate.
#[test]
fn t8_the_token_axis_reaches_the_certificate_while_the_unbounded_channel_stays_closed() {
    use engine::analysis::resource::ResourceAxis;

    let mut state = load_f4();
    drive_f4_to_offer(&mut state, 400).expect("the bounded offer fires (see R1)");
    let (proposer, certificate, schema) = offer_parts(&state);
    let unbounded = certificate.unbounded.clone();
    let per_cycle = certificate
        .per_cycle
        .clone()
        .expect("a bounded offer publishes its per-period signature");
    let schema = schema.clone();
    let n = schema.max_iterations;

    // ── (a) the enumerated consumer moves.
    assert!(
        unbounded.contains(&ResourceAxis::TokensCreated),
        "a bounded offer whose period MINTS tokens carries the token axis into the \
         certificate's unbounded set. published axes={unbounded:?}"
    );

    // ── (c) THE FIREWALL LEG, taken first because it must be measured on the OFFER beat.
    let torch = resolve_by_name(&state, TORCH);
    assert!(
        state.battlefield.contains(&torch),
        "the refused action needs a LIVE battlefield source, else the refusal could be about \
         the source rather than about the wait"
    );
    let sequence_at_offer = state.last_loop_action_sequence.clone();
    let mut firewall = state.clone();
    let refusal = apply(
        &mut firewall,
        proposer,
        GameAction::ActivateAbility {
            source_id: torch,
            ability_index: 0,
        },
    );
    assert!(
        matches!(
            refusal,
            Err(engine::game::engine::EngineError::ActionNotAllowed(_))
        ),
        "an action outside the offer beat's admissible set is refused as ActionNotAllowed, \
         got {refusal:?}"
    );
    assert_eq!(
        firewall.last_loop_action_sequence, sequence_at_offer,
        "the refused action must not mint a loop-action step — that sequence is the input to \
         the very predicate guarding the mark route (b) asserts closed"
    );
    assert!(
        matches!(firewall.waiting_for, WaitingFor::LoopShortcut { .. }),
        "the refusal leaves the offer standing, got {:?}",
        firewall.waiting_for
    );

    // ── (b) the guard's input is unset at the offer beat, with its own positive control.
    assert!(
        sequence_at_offer.is_empty(),
        "no seat owns a driving period at the offer beat, so the object-growth mark route is \
         not live for anyone. sequence={sequence_at_offer:?}"
    );
    assert!(
        state.unbounded_resources.is_empty(),
        "the bounded offer publishes nothing to the unbounded-resource channel. got {:?}",
        state.unbounded_resources
    );
    let mut marked = state.clone();
    marked.mark_unbounded_loop(proposer, &[ResourceAxis::TokensCreated]);
    assert!(
        !marked.unbounded_resources.is_empty(),
        "POSITIVE CONTROL: the same reader returns non-empty on a hand-marked state, so the \
         empty result above is a real negative and not a dead instrument"
    );

    // ── (c) PAIRED POSITIVE + (b) across the whole window: the SAME beat accepts a
    //    declaration, which then commits its whole count with the channel still closed.
    let life_rate = -per_cycle.delta.life.values().copied().min().unwrap_or(0);
    assert!(
        life_rate > 0,
        "ANTI-VACUITY: the committed count below is re-derived from the life relation. \
         published life={:?}",
        per_cycle.delta.life
    );
    let template = f4_pin_template(&schema, proposer, n);
    let life_before = life_by_seat(&state);
    apply(
        &mut state,
        proposer,
        GameAction::DeclareShortcut {
            count: IterationCount::Fixed(n),
            template: Some(template),
        },
    )
    .expect("the declaration is dispatched");
    assert!(
        matches!(state.waiting_for, WaitingFor::RespondToShortcut { .. }),
        "PAIRED POSITIVE for the firewall: `DeclareShortcut` on the SAME beat is ADMITTED and \
         moves the wait, so an Err-on-everything reducer cannot satisfy the refusal above. got \
         {:?}",
        state.waiting_for
    );
    assert!(
        accept_all_opponents(&mut state) > 0,
        "the CR 732.2c window must actually take responses"
    );

    let life_after = life_by_seat(&state);
    let committed: i64 = life_before
        .iter()
        .map(|(seat, before)| (*before - life_of(&life_after, *seat)).max(0))
        .sum::<i64>()
        / life_rate;
    assert_eq!(
        committed,
        i64::from(n),
        "the accepted declaration commits its whole count, so the channel assertion below is \
         about a drive that actually ran: life {life_before:?} -> {life_after:?}"
    );
    assert!(
        state.last_loop_action_sequence.is_empty() && state.unbounded_resources.is_empty(),
        "after the bounded drive the object-growth route is STILL not live and nothing was \
         published to the unbounded-resource channel. sequence={:?} marks={:?}",
        state.last_loop_action_sequence,
        state.unbounded_resources
    );
}

// ═════════════════════════════════════════════════════════════════════════════════════════
// PHASE 4 — the per-victim ALLOCATION INGRESS, driven on the real 4p dump
//
// Every row below enters through `resolve_interaction_response` — the wire path a client
// actually takes — rather than by handing `apply` a `DecisionTemplate` the test built itself.
// Restrict the ingress to a per-position pin and the submission is refused, so none of these
// rows can dispatch.
// ═════════════════════════════════════════════════════════════════════════════════════════

/// The published allocation surface of one CR 732.2a offer, read off the offer's own fields.
///
/// Nothing here is spelled: the legal victim set, the candidate ids, the count window, the
/// per-cycle rate and the pre-announced seat all come from published state.
struct F4Allocation {
    interaction_id: engine::types::interaction::InteractionId,
    proposer: PlayerId,
    /// The `Targets` point's index in `schema.points`, which is also its published `group`.
    target_group: u32,
    /// Seat candidates in published order, positionally aligned with `candidate_ids`.
    legal_seats: Vec<PlayerId>,
    candidate_ids: Vec<engine::types::interaction::InteractionChoiceId>,
    /// Pins answering every OTHER non-read-only point, so a submission is complete.
    other_pins: Vec<engine::types::interaction::InteractionShortcutPin>,
    /// Those same points, carrying their own candidate lists — so a row can author a DIFFERENT
    /// answer than `other_pins`' default first pick without re-reading the offer.
    answerable: Vec<engine::types::interaction::InteractionShortcutPoint>,
    /// The offer's own count-keyed published preview list, read at the offer beat because the
    /// interaction id rotates the moment the declaration is dispatched.
    published_preview: Vec<engine::types::interaction::InteractionShortcutPreview>,
    /// The offer beat's published candidates, for the same reason.
    offer_candidates: Vec<engine::types::interaction::InteractionChoice>,
    /// The CR 601.2c announcement journal's answer at the `Targets` slot — the seat the
    /// drive's LEADING cycle resolves, before anything the allocation governs.
    preannounced: PlayerId,
    /// The published `InteractionShortcutCountSpec::Fixed` ceiling.
    max_count: u32,
    /// Magnitude of the published per-cycle life charge at `preannounced`.
    rate: i64,
}

/// Drive the real dump to its bounded offer, bind the interaction authority, and read the
/// allocation surface off the published offer.
fn f4_allocation_offer(state: &mut GameState) -> F4Allocation {
    use engine::analysis::decision_template::{
        AnnouncementSubject, LoopAnswer, LoopAnswerValue, TargetPin, TargetSchedule,
    };
    use engine::game::interaction::{bind_interaction_authority, derive_viewer_interaction};
    use engine::game::visibility::filter_state_for_viewer;
    use engine::types::interaction::{
        InteractionOpportunityResponse, InteractionResponseSpec, InteractionSessionId,
        InteractionShortcutCountSpec, InteractionShortcutPin, InteractionShortcutPointKind,
    };

    drive_f4_to_offer(state, 400).expect("reach-guard: the bounded offer fires (see R1)");
    let (proposer, certificate, schema) = offer_parts(state);
    let per_cycle = certificate
        .per_cycle
        .clone()
        .expect("a bounded offer publishes its per-period signature");
    let schema = schema.clone();

    let target_group = schema
        .points
        .iter()
        .position(|p| matches!(p.kind, DecisionPointKind::Targets { .. }))
        .expect("reach-guard: the offer publishes Torch's CR 601.2c Targets point");
    let DecisionPointKind::Targets { legal_targets, .. } = &schema.points[target_group].kind else {
        unreachable!("the position above selected a Targets point");
    };
    let legal_seats: Vec<PlayerId> = legal_targets
        .iter()
        .map(|t| match t {
            TargetRef::Player(seat) => *seat,
            other => panic!("this board's victims are seats, got {other:?}"),
        })
        .collect();

    // The pre-announced seat is read from the CR 601.2c announcement journal — the authority
    // `c2a_row_t1` reads and `c2a_row_t1p` proves follows the announcement. Identifying it as
    // "the seat that lost one extra cycle" would make every sum leg below unfalsifiable.
    let preannounced = match state.loop_answer(&schema.points[target_group].slot, proposer) {
        Some(LoopAnswer::Uniform(LoopAnswerValue::Targets(pins))) => match pins.as_slice() {
            [TargetPin::Scheduled(TargetSchedule::Constant(ranking))] => match ranking.head() {
                AnnouncementSubject::Seat(seat) => *seat,
                other => panic!("the journalled announcement is a seat, got {other:?}"),
            },
            other => panic!("the journal holds one scheduled constant pin, got {other:?}"),
        },
        other => panic!("the CR 601.2c journal must hold this slot's announcement, got {other:?}"),
    };
    let rate = -per_cycle
        .delta
        .life
        .get(&preannounced)
        .copied()
        .unwrap_or(0);

    bind_interaction_authority(state, InteractionSessionId("p4-allocation".to_string()))
        .expect("valid interaction authority binding");
    let filtered = filter_state_for_viewer(state, proposer);
    let view = derive_viewer_interaction(state, &filtered, proposer);
    let opportunity = view
        .opportunities
        .iter()
        .find(|o| {
            matches!(
                &o.response,
                InteractionOpportunityResponse::Schema {
                    spec: InteractionResponseSpec::Shortcut { .. },
                    ..
                }
            )
        })
        .expect("reach-guard: the offer is published as a shortcut schema");
    let InteractionOpportunityResponse::Schema {
        spec:
            InteractionResponseSpec::Shortcut {
                count,
                points,
                preview,
                ..
            },
        candidates: offer_candidates,
    } = &opportunity.response
    else {
        unreachable!("the find above selected a shortcut schema");
    };
    assert_eq!(
        points.len(),
        schema.points.len(),
        "reach-guard: the projection publishes one point per schema point, which is what makes \
         `target_group` address the SAME point in both"
    );
    let target_point = &points[target_group];
    assert_eq!(
        target_point.kind,
        InteractionShortcutPointKind::Targets,
        "reach-guard: the published point at the schema's Targets index is a Targets point"
    );
    assert_eq!(
        target_point.candidate_ids.len(),
        legal_seats.len(),
        "reach-guard: candidate ids are positionally aligned with the published legal victims, \
         which is how every allocation below names a seat without spelling an id"
    );
    let max_count = match count {
        InteractionShortcutCountSpec::Fixed { max, .. } => *max,
        other => panic!("this board publishes a Fixed count window, got {other:?}"),
    };

    let answerable: Vec<_> = points
        .iter()
        .enumerate()
        .filter(|(group, point)| !point.read_only && *group != target_group)
        .map(|(_, point)| point.clone())
        .collect();
    let other_pins = answerable
        .iter()
        .map(|point| InteractionShortcutPin {
            group: point.group,
            choice_ids: point
                .candidate_ids
                .iter()
                .take(point.min as usize)
                .cloned()
                .collect(),
            amounts: Vec::new(),
        })
        .collect();

    F4Allocation {
        interaction_id: opportunity.interaction_id.clone(),
        proposer,
        target_group: target_group as u32,
        legal_seats,
        candidate_ids: target_point.candidate_ids.clone(),
        other_pins,
        answerable,
        published_preview: preview.clone(),
        offer_candidates: offer_candidates.clone(),
        preannounced,
        max_count,
        rate,
    }
}

/// One `InteractionSubmission` naming `allocation` as a SEQUENCED pin on the published
/// `Targets` point. `allocation` is client-authored input; the published count is the
/// authority it must match, so a re-dump that moved the ceiling fails here loudly instead of
/// quietly testing a different claim.
fn f4_allocation_submission(
    offer: &F4Allocation,
    count: u32,
    allocation: &[(PlayerId, u32)],
) -> engine::types::interaction::InteractionSubmission {
    use engine::types::interaction::{
        AmountAssignment, InteractionResponse, InteractionShortcutDecision, InteractionShortcutPin,
        InteractionSubmission,
    };

    assert_eq!(
        allocation.iter().map(|(_, amount)| amount).sum::<u32>(),
        count,
        "reach-guard: an allocation partitions the DECLARED count; this row's shape does not"
    );
    let choice_ids: Vec<_> = allocation
        .iter()
        .map(|(seat, _)| {
            let index = offer
                .legal_seats
                .iter()
                .position(|candidate| candidate == seat)
                .unwrap_or_else(|| panic!("{seat:?} is not a published legal victim"));
            offer.candidate_ids[index].clone()
        })
        .collect();
    let amounts = choice_ids
        .iter()
        .zip(allocation)
        .map(|(choice_id, (_, amount))| AmountAssignment {
            choice_id: choice_id.clone(),
            amount: *amount,
        })
        .collect();
    let mut pins = offer.other_pins.clone();
    pins.push(InteractionShortcutPin {
        group: offer.target_group,
        choice_ids,
        amounts,
    });
    InteractionSubmission {
        interaction_id: offer.interaction_id.clone(),
        response: InteractionResponse::Shortcut {
            decision: InteractionShortcutDecision::Fixed { iterations: count },
            pins,
        },
    }
}

/// Submit `allocation` through the ingress, dispatch the action it mints, let every living
/// opponent accept, and return the per-seat life LOSSES positionally by `state.players`.
fn f4_drive_allocation(
    state: &mut GameState,
    offer: &F4Allocation,
    count: u32,
    allocation: &[(PlayerId, u32)],
) -> Vec<i64> {
    use engine::game::interaction::resolve_interaction_response;

    let before: Vec<i64> = state.players.iter().map(|p| p.life as i64).collect();
    let action = resolve_interaction_response(
        state,
        offer.proposer,
        &f4_allocation_submission(offer, count, allocation),
    )
    .expect("the allocation ingress accepts a conformant sequenced pin");
    apply(state, offer.proposer, action).expect("the minted declaration is dispatched");
    // THE DISCRIMINATOR between "declare refused it" and "the drive aborted": a refused
    // declaration hands priority straight back and never opens the APNAP window.
    assert!(
        matches!(state.waiting_for, WaitingFor::RespondToShortcut { .. }),
        "the accepted declaration must open the CR 732.2b APNAP window, got {:?}",
        state.waiting_for
    );
    assert!(
        accept_all_opponents(state) > 0,
        "the CR 732.2c window must actually take responses"
    );
    state
        .players
        .iter()
        .enumerate()
        .map(|(seat, p)| before[seat] - p.life as i64)
        .collect()
}

/// The seat each `state.players` position holds, so a loss vector can be read by seat.
fn f4_loss(state: &GameState, losses: &[i64], seat: PlayerId) -> i64 {
    let index = state
        .players
        .iter()
        .position(|p| p.id == seat)
        .unwrap_or_else(|| panic!("{seat:?} is not seated on this board"));
    losses[index]
}

/// The announced target of the trigger still on the stack when a CR 732.2a drive hands back.
fn f4_pending_announcement(state: &GameState) -> Vec<TargetRef> {
    let entry = state
        .stack
        .last()
        .expect("the drive hands back with the next repetition's trigger still on the stack");
    match &entry.kind {
        StackEntryKind::TriggeredAbility { ability, .. } => ability.targets.clone(),
        other => panic!("the pending entry is a triggered ability, got {other:?}"),
    }
}

/// **Row (1)** — an allocation of the published `Fixed` ceiling across all three legal victims
/// decodes to `TargetSchedule::Piecewise` at the prefix sums its amounts imply, and commits its
/// whole declared count.
///
/// # Discrimination
///
/// Restrict the ingress to a per-position pin (drop the `sequenced` gate, or the `Piecewise`
/// build) ⇒ `resolve_interaction_response` returns `ConstraintUnsatisfied` and this row cannot
/// dispatch at all.
///
/// # Shape
///
/// The near-equal split, whose every segment start lands inside the window a drive of `n`
/// cycles actually realizes: the FIRST cycle resolves the target announced before the drive
/// begins, so template indices `0..=n-2` govern the rest. Row (1c) pins that window as its own
/// class property rather than smuggling it into this row's wording.
///
/// # Paired positives
///
/// The published rate is asserted strictly positive, else every product below degenerates to
/// `0 == 0 * n`; the declaration must open the APNAP window, separating "declare refused" from
/// "drive aborted"; and the realized split is asserted NON-uniform, so a uniform-split bug
/// cannot satisfy it.
///
/// # The set the omitted-victim leg quantifies over
///
/// With all three legal victims declared, the only undeclared seat is the PROPOSER, who is not
/// among the offer's published `legal_targets`. The charter's omitted-legal-victim clause has
/// no member in this row; [`p4_row_1b_an_authored_non_canonical_distribution_is_accepted`]'s
/// third arm is where a real one lives.
#[test]
fn p4_row_1_an_allocation_of_the_published_ceiling_commits_its_whole_count() {
    let mut state = load_f4();
    let offer = f4_allocation_offer(&mut state);
    let count = offer.max_count;

    assert!(
        offer.rate > 0,
        "ANTI-VACUITY: the published per-cycle life rate must be strictly positive, else every \
         product below degenerates to `0 == 0 * {count}`"
    );
    assert_eq!(
        offer.legal_seats.len(),
        3,
        "reach-guard: this board publishes three legal victims, which is what makes a \
         three-segment allocation a real member of the composition set"
    );
    assert!(
        !offer.legal_seats.contains(&offer.proposer),
        "the undeclared-seat leg below is stated over the set it walks: the proposer is NOT a \
         published legal victim, so declaring all three victims leaves no omitted victim here"
    );

    let third = count / 3;
    let allocation = [
        (offer.legal_seats[0], third),
        (offer.legal_seats[1], third),
        (offer.legal_seats[2], count - 2 * third),
    ];
    let losses = f4_drive_allocation(&mut state, &offer, count, &allocation);

    for (seat, _) in &allocation {
        assert!(
            f4_loss(&state, &losses, *seat) > 0,
            "CR 732.2a: every seat this allocation declares takes repetitions, so each strictly \
             decreases. {seat:?} losses={losses:?}"
        );
    }
    assert_eq!(
        allocation
            .iter()
            .map(|(seat, _)| f4_loss(&state, &losses, *seat))
            .sum::<i64>(),
        offer.rate * i64::from(count),
        "CR 732.2a: the accepted shortcut commits EXACTLY {count} repetitions of the published \
         per-cycle charge, and with every legal victim declared the whole charge lands on them. \
         losses={losses:?} rate={}",
        offer.rate
    );
    assert_eq!(
        f4_loss(&state, &losses, offer.proposer),
        0,
        "the proposer is not a published victim and loses nothing. losses={losses:?}"
    );
    let realized: Vec<i64> = allocation
        .iter()
        .map(|(seat, _)| f4_loss(&state, &losses, *seat))
        .collect();
    assert!(
        realized.windows(2).any(|pair| pair[0] != pair[1]),
        "ANTI-VACUITY: an EQUAL declared split still realizes a NON-uniform loss map, because \
         the leading cycle shifts every segment boundary one cycle late. A uniform-split bug \
         would satisfy the sum leg above; it cannot satisfy this one. realized={realized:?}"
    );
}

/// **Row (1b)** — an authored, NON-CANONICAL distribution is accepted: shapes the published
/// preview allocation does not offer.
///
/// Paired with row (1), the two show the ingress admits the whole composition set rather than
/// the canonical member. Restrict the ingress to the published allocation ⇒ all three arms fail
/// while row (1) still passes.
///
/// # One rule governs both sum legs, and the arms sit on opposite sides of it
///
/// Total realized loss is always `rate * count`; the pre-announced seat takes the leading cycle
/// ON TOP OF whatever the allocation gives it. So the declared seats sum to `rate * (count - 1)`
/// exactly when the allocation OMITS that seat, and to `rate * count` when it INCLUDES it.
/// Asserting one figure in both arms would be false in one of them.
#[test]
fn p4_row_1b_an_authored_non_canonical_distribution_is_accepted() {
    let count_probe = {
        let mut state = load_f4();
        f4_allocation_offer(&mut state).max_count
    };

    // ── (a) UNEQUAL PARTS over all three victims ──
    {
        let mut state = load_f4();
        let offer = f4_allocation_offer(&mut state);
        let count = offer.max_count;
        assert!(
            offer.rate > 0,
            "ANTI-VACUITY: the published rate is positive"
        );
        let allocation = [
            (offer.legal_seats[0], 2),
            (offer.legal_seats[1], 4),
            (offer.legal_seats[2], count - 6),
        ];
        let losses = f4_drive_allocation(&mut state, &offer, count, &allocation);
        for (seat, _) in &allocation {
            assert!(
                f4_loss(&state, &losses, *seat) > 0,
                "(a) every declared seat takes repetitions. {seat:?} losses={losses:?}"
            );
        }
        assert_eq!(
            allocation
                .iter()
                .map(|(seat, _)| f4_loss(&state, &losses, *seat))
                .sum::<i64>(),
            offer.rate * i64::from(count),
            "(a) an unequal composition of the same count commits the same total. \
             losses={losses:?}"
        );
    }

    // ── (b) A PROPER SUBSET omitting the PRE-ANNOUNCED seat ──
    {
        let mut state = load_f4();
        let offer = f4_allocation_offer(&mut state);
        let count = offer.max_count;
        let declared: Vec<PlayerId> = offer
            .legal_seats
            .iter()
            .copied()
            .filter(|seat| *seat != offer.preannounced)
            .collect();
        assert_eq!(
            declared.len(),
            2,
            "reach-guard: omitting the pre-announced seat must leave a real subset to declare"
        );
        let half = count / 2;
        let allocation = [(declared[0], half), (declared[1], count - half)];
        let losses = f4_drive_allocation(&mut state, &offer, count, &allocation);

        assert!(
            !allocation
                .iter()
                .any(|(seat, _)| *seat == offer.preannounced),
            "(b) the submitted allocation omits the pre-announced seat"
        );
        assert_eq!(
            f4_loss(&state, &losses, offer.preannounced),
            offer.rate,
            "(b) CR 601.2c: the pre-announced seat still takes the LEADING cycle — the one \
             resolving a target announced before the drive begins — and nothing else. Reading \
             that seat from the announcement journal rather than from 'the one that lost rate' \
             is what lets a drive that charged the leading cycle elsewhere red this row. \
             losses={losses:?}"
        );
        assert_eq!(
            allocation
                .iter()
                .map(|(seat, _)| f4_loss(&state, &losses, *seat))
                .sum::<i64>(),
            offer.rate * i64::from(count - 1),
            "(b) with the leading cycle discounted, the declared seats take the REMAINING \
             repetitions. losses={losses:?}"
        );
    }

    // ── (c) A PROPER SUBSET omitting a legal victim that is NOT the pre-announced seat ──
    //
    // This is the arm that gives the omitted-victim clause a member the leading-cycle
    // exception does not excuse. Without it the conjunct ranges over the empty set across the
    // whole matrix: row (1) declares every victim, and arm (b)'s only omission is excused.
    {
        let mut state = load_f4();
        let offer = f4_allocation_offer(&mut state);
        let count = offer.max_count;
        let omitted = *offer
            .legal_seats
            .iter()
            .rev()
            .find(|seat| **seat != offer.preannounced)
            .expect("reach-guard: some legal victim is not the pre-announced seat");
        let declared: Vec<PlayerId> = offer
            .legal_seats
            .iter()
            .copied()
            .filter(|seat| *seat != omitted)
            .collect();
        assert!(
            offer.legal_seats.contains(&omitted) && omitted != offer.preannounced,
            "the omitted seat is a PUBLISHED legal victim and is NOT the pre-announced seat, so \
             its zero below cannot be explained by either exception"
        );
        assert!(
            declared.contains(&offer.preannounced),
            "reach-guard: this arm keeps the pre-announced seat in the allocation, which is \
             what puts its sum leg on the other side of the rule from arm (b)"
        );
        let half = count / 2;
        let allocation = [(declared[0], half), (declared[1], count - half)];
        let losses = f4_drive_allocation(&mut state, &offer, count, &allocation);

        assert_eq!(
            f4_loss(&state, &losses, omitted),
            0,
            "(c) a legal victim left OUT of the allocation loses nothing. losses={losses:?}"
        );
        assert_eq!(
            allocation
                .iter()
                .map(|(seat, _)| f4_loss(&state, &losses, *seat))
                .sum::<i64>(),
            offer.rate * i64::from(count),
            "(c) the pre-announced seat is IN this allocation, so the declared seats take the \
             whole charge including the leading cycle. losses={losses:?}"
        );
    }

    // The omitted seat's zero in (c) is an AXIS, not a dead reading: the same seat reads a
    // strictly positive loss under row (1)'s allocation on the same board.
    let mut control = load_f4();
    let control_offer = f4_allocation_offer(&mut control);
    assert_eq!(
        control_offer.max_count, count_probe,
        "reach-guard: the loaded board is the same one every arm above used"
    );
    let third = control_offer.max_count / 3;
    let control_allocation = [
        (control_offer.legal_seats[0], third),
        (control_offer.legal_seats[1], third),
        (
            control_offer.legal_seats[2],
            control_offer.max_count - 2 * third,
        ),
    ];
    let control_losses = f4_drive_allocation(
        &mut control,
        &control_offer,
        control_offer.max_count,
        &control_allocation,
    );
    let omitted = *control_offer
        .legal_seats
        .iter()
        .rev()
        .find(|seat| **seat != control_offer.preannounced)
        .expect("some legal victim is not the pre-announced seat");
    assert!(
        f4_loss(&control, &control_losses, omitted) > 0,
        "CONTROL for (c): the seat that read zero when omitted reads a positive loss when \
         declared, so that zero is a measurement and not a dead instrument. \
         losses={control_losses:?}"
    );
}

/// **Row (1c)** — THE REALIZED INDEX WINDOW, the class property the ingress now exposes.
///
/// A drive of `n` cycles commits `rate * n` in total. Its FIRST cycle resolves the target
/// announced before the drive begins, so template indices `0..=n-2` govern the remaining
/// `n-1` cycles; index `n-1`'s announcement lands on the trigger left on the stack at the
/// CR 732.2a handback and resolves in manual play.
///
/// # What the pair isolates, exactly
///
/// `8/8/2` (last start `n-2`) gives its trailing seat ONE realized cycle; `8/9/1` (last start
/// `n-1`) gives it NONE. Same arity, same first two boundaries, last start one index apart —
/// so the 1 -> 0 step isolates the index window FROM ARITY. It does NOT isolate it from
/// segment length, and no pair over LAST segments can: at fixed `n` a last segment's start is
/// `n - length`, so the two move together. The middle-segment rows are what separate those —
/// row (1b) arm (a)'s middle seat realizes its full declared 4, not 3. The two-segment form
/// (`16/2` vs `17/1`) reproduces the step independently.
///
/// This is not a revert row for the ingress: it pins a shipped drive property. It reds if the
/// ingress starts refusing trailing segments, or if the drive's leading-cycle offset moves.
#[test]
fn p4_row_1c_a_segment_starting_at_the_last_index_realizes_nothing_but_stays_announced() {
    let drive = |allocation_of: &dyn Fn(&F4Allocation) -> Vec<(PlayerId, u32)>| {
        let mut state = load_f4();
        let offer = f4_allocation_offer(&mut state);
        let count = offer.max_count;
        let allocation = allocation_of(&offer);
        let trailing = allocation
            .last()
            .expect("an allocation has a last segment")
            .0;
        let losses = f4_drive_allocation(&mut state, &offer, count, &allocation);
        let declared_total: i64 = allocation
            .iter()
            .map(|(seat, _)| f4_loss(&state, &losses, *seat))
            .sum();
        (
            offer.rate,
            count,
            f4_loss(&state, &losses, trailing),
            declared_total,
            trailing,
            f4_pending_announcement(&state),
            losses,
        )
    };

    // Arity 3, adjacent last-segment starts.
    let (rate, count, last_len2, total_len2, _, _, losses_len2) = drive(&|o: &F4Allocation| {
        let head = o.max_count / 2 - 1;
        vec![
            (o.legal_seats[0], head),
            (o.legal_seats[1], head),
            (o.legal_seats[2], o.max_count - 2 * head),
        ]
    });
    let (_, _, last_len1, total_len1, trailing_len1, pending_len1, losses_len1) =
        drive(&|o: &F4Allocation| {
            let head = o.max_count / 2 - 1;
            vec![
                (o.legal_seats[0], head),
                (o.legal_seats[1], o.max_count - head - 1),
                (o.legal_seats[2], 1),
            ]
        });

    assert!(rate > 0, "ANTI-VACUITY: the published rate is positive");
    assert_eq!(
        (last_len2, last_len1),
        (rate, 0),
        "the trailing seat realizes ONE cycle when its segment starts at index n-2 and NONE \
         when it starts at n-1. losses were {losses_len2:?} then {losses_len1:?}"
    );
    assert_eq!(
        (total_len2, total_len1),
        (rate * i64::from(count), rate * i64::from(count)),
        "both shapes are ACCEPTED and both commit the whole declared count — the trailing \
         segment is admitted, not refused"
    );
    assert_eq!(
        pending_len1,
        vec![TargetRef::Player(trailing_len1)],
        "the trailing seat loses nothing INSIDE the drive while its announcement is LIVE: it \
         is the announced target of the trigger left on the stack at the CR 732.2a handback"
    );

    // Arity 2 reproduces the step independently, and its pending readout takes a DIFFERENT
    // seat — so the readout above follows index n-1's segment rather than being a constant.
    let (_, _, two_len2, _, _, _, losses_two_len2) =
        drive(&|o: &F4Allocation| vec![(o.legal_seats[0], o.max_count - 2), (o.legal_seats[1], 2)]);
    let (_, _, two_len1, _, two_trailing, two_pending, losses_two_len1) =
        drive(&|o: &F4Allocation| vec![(o.legal_seats[0], o.max_count - 1), (o.legal_seats[1], 1)]);
    assert_eq!(
        (two_len2, two_len1),
        (rate, 0),
        "the two-segment form reproduces the same 1 -> 0 step at the same two starts. \
         losses were {losses_two_len2:?} then {losses_two_len1:?}"
    );
    assert_eq!(
        two_pending,
        vec![TargetRef::Player(two_trailing)],
        "the pending announcement follows index n-1's segment"
    );
    assert_ne!(
        two_trailing, trailing_len1,
        "CONTROL: the two shapes' trailing seats DIFFER, so the pending readout asserted twice \
         above is not a constant this board would print either way"
    );
}

/// Grant player hexproof from a permanent that seat controls, AFTER the offer latched.
///
/// Built with production `zones::create_object` rather than a raw `objects.insert`: a raw
/// insert never joins `state.battlefield`, so the grantor would be invisible to
/// `game_functioning_statics`. The dirty mark is fixture bookkeeping — after a completed drive
/// this board reads `Clean` and `create_object` does not re-dirty it, so a bare `flush_layers`
/// returns immediately and the O(1) `static_mode_presence` gate answers `false` for `Hexproof`
/// no matter what the grantor carries.
fn f4_grant_player_hexproof(state: &mut GameState, seat: PlayerId, card_id: u64) {
    use engine::types::ability::{ControllerRef, StaticDefinition, TargetFilter, TypedFilter};
    use engine::types::game_state::LayersDirty;
    use engine::types::identifiers::CardId;
    use engine::types::statics::StaticMode;
    use engine::types::zones::Zone;

    let grantor = engine::game::zones::create_object(
        state,
        CardId(card_id),
        seat,
        "You Have Hexproof Source".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&grantor)
        .expect("the grantor was just created")
        .static_definitions =
        vec![
            StaticDefinition::new(StaticMode::Hexproof).affected(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            )),
        ]
        .into();
    state.layers_dirty = LayersDirty::Full;
    engine::game::layers::flush_layers(state);
    assert!(
        engine::game::static_abilities::player_has_hexproof(state, seat),
        "reach-guard: the grant must actually land on {seat:?}, else the refusal it is supposed \
         to cause proves nothing"
    );
}

/// **Row (2)** — THE VALIDATED-RANGE REPAIR, its own row rather than a side effect of row (1),
/// and **row (4)'s hexproof leg**: the refusal takes the WHOLE declaration.
///
/// A `Piecewise` pin whose LATER segment names a seat granted hexproof after the offer latched
/// must be REFUSED. Under the `1` literal the human ingress replaced, index 0 lands in the
/// FIRST segment and the later one is never re-resolved, so the same submission is accepted.
///
/// # Discrimination, at the seam itself
///
/// The template the ingress minted is handed to `validate_pins` at range 1 and at the declared
/// count against the same hostile board: `Ok` at 1, `Err` at the count. That IS the literal's
/// verdict beside the repair's, on one value, so "restore the `1`" needs no code edit to read.
///
/// # Paired positive reach-guard — mandatory
///
/// The SAME declaration with every segment legal is ACCEPTED end to end. A bare refusal is
/// satisfiable by any upstream short-circuit.
///
/// # Attribution control
///
/// Hexproof on the FIRST segment's seat is refused too — at range 1 as well as at the count —
/// so the middle-segment result above is attributable to the index window and not to a board
/// that refuses every hexproofed seat only at wide ranges.
#[test]
fn p4_row_2_a_later_segment_that_went_illegal_refuses_the_whole_declaration() {
    use engine::analysis::decision_template::validate_pins;
    use engine::game::interaction::resolve_interaction_response;

    let mut state = load_f4();
    let offer = f4_allocation_offer(&mut state);
    let count = offer.max_count;
    let third = count / 3;
    let allocation = [
        (offer.legal_seats[0], third),
        (offer.legal_seats[1], third),
        (offer.legal_seats[2], count - 2 * third),
    ];
    let submission = f4_allocation_submission(&offer, count, &allocation);

    // ── PAIRED POSITIVE: every segment legal ⇒ accepted end to end ──
    let action = resolve_interaction_response(&state, offer.proposer, &submission).expect(
        "paired positive: with every segment legal the ingress ACCEPTS, so the refusals below \
         are not an ingress that had simply started refusing everything",
    );
    let (declared_count, template) = match action {
        GameAction::DeclareShortcut { count, template } => (
            count,
            template.expect("a shortcut acceptance carrying pins materializes a template"),
        ),
        other => panic!("the allocation ingress mints a declaration, got {other:?}"),
    };
    assert_eq!(declared_count, IterationCount::Fixed(count));
    let schema = offer_parts(&state).2.clone();
    assert!(
        validate_pins(&schema, &template, count, &state).is_ok(),
        "paired positive at the seam: the minted Piecewise validates at the FULL declared range \
         on the un-hexproofed board"
    );

    let torch = resolve_by_name(&state, TORCH);
    let torch_controller = state.objects[&torch].controller;

    // ── THE CLAIM: hexproof on the MIDDLE segment's seat ──
    let middle = offer.legal_seats[1];
    let mut hostile = state.clone();
    assert!(
        engine::game::players::is_opponent(&hostile, middle, torch_controller),
        "reach-guard: CR 702.11c only excludes OPPONENTS' abilities, so {middle:?} must be \
         Torch's controller {torch_controller:?}'s opponent"
    );
    f4_grant_player_hexproof(&mut hostile, middle, 9402);
    assert!(
        !engine::game::static_abilities::player_cannot_be_targeted_by(
            &hostile,
            offer.legal_seats[0],
            torch,
            torch_controller
        ),
        "reach-guard: a DIFFERENT seat on the same hostile board is still targetable, so the \
         refusal below is the hexproof and not a blanket one"
    );
    assert!(
        validate_pins(&schema, &template, 1, &hostile).is_ok(),
        "THE REVERT READING: at range 1 — the literal this phase replaced — index 0 lands in \
         the FIRST segment and the hexproofed MIDDLE seat is never re-resolved, so the same \
         declaration is ACCEPTED"
    );
    assert!(
        validate_pins(&schema, &template, count, &hostile).is_err(),
        "CR 608.2b + CR 702.11c: validated over the range the ACCEPTED COUNT drives, the later \
         segment's seat is re-resolved and is an illegal target"
    );
    assert!(
        resolve_interaction_response(&hostile, offer.proposer, &submission).is_err(),
        "row (4): the refusal takes the WHOLE declaration — no action is minted at all, rather \
         than one segment being dropped"
    );

    // ── ATTRIBUTION CONTROL: hexproof on the FIRST segment's seat ──
    let first = offer.legal_seats[0];
    let mut first_hostile = state.clone();
    assert!(
        engine::game::players::is_opponent(&first_hostile, first, torch_controller),
        "reach-guard: {first:?} must be Torch's controller's opponent too"
    );
    f4_grant_player_hexproof(&mut first_hostile, first, 9403);
    assert!(
        validate_pins(&schema, &template, 1, &first_hostile).is_err()
            && validate_pins(&schema, &template, count, &first_hostile).is_err(),
        "CONTROL: a FIRST-segment seat gone illegal reds at BOTH ranges, so the middle-segment \
         split above is attributable to the index window rather than to a range-shaped board"
    );
}

// ─────────────────────────────────────────────────────────────────────────────────────────
// E — `DerivedViews::bounded_loop_max_repetitions`, the open window's own repetition ceiling
// ─────────────────────────────────────────────────────────────────────────────────────────

/// **Rows E1 / E2 / E3.** CR 732.2a: a narrowed offer publishes the largest number of
/// repetitions its proposal may specify; the `∞` channel beside it is neither admitted nor
/// withheld by that publication; and the channel is absent before the window opens and again
/// after the accepted drive hands priority back.
///
/// # Non-vacuity
///
/// The bound is READ OFF the live offer, never written as a literal, so a re-dump that moves the
/// board's CR 704.5a threshold flows through instead of reddening the row. Every absence leg
/// carries its matched positive in the same test: the `∞` emptiness at the offer beat is paired
/// with a hand-marked clone of that same beat, and both `None` readings are paired with the
/// `Some(..)` this row asserts between them.
///
/// # Discrimination
///
/// Delete the `derive_views` `LoopShortcut` arm ⇒ the offer-beat assertion reads `None`. Emit
/// the channel unconditionally ⇒ the load-time and handback legs read `Some(..)`.
#[test]
fn e1_a_narrowed_offer_publishes_its_bound_beside_an_untouched_infinity_channel() {
    use engine::analysis::resource::ResourceAxis;
    use engine::game::derived_views::derive_views;

    for (label, mut state) in [
        ("F4", load_f4()),
        ("MODE1", load_mode1()),
        ("MODE2", load_mode2()),
    ] {
        // ── E3, load leg: no window is open, so there is no ceiling to state.
        assert_eq!(
            derive_views(&state, None).bounded_loop_max_repetitions,
            None,
            "[{label}] CR 732.2a: a board with no open shortcut window states no ceiling"
        );

        drive_f4_to_offer(&mut state, 400)
            .unwrap_or_else(|| panic!("[{label}] reach-guard: the bounded offer must FIRE"));
        let (proposer, _certificate, schema) = offer_parts(&state);
        let bound = schema.max_iterations;
        let bounded = schema.is_bounded();
        let schema = schema.clone();
        assert!(
            bounded && bound > 1,
            "[{label}] reach-guard: this offer's producer must have NARROWED the bound below the \
             engine cap, and to more than one repetition — an unnarrowed offer takes the other \
             arm and a ceiling of 1 could not discriminate. max_iterations={bound} cap={}",
            MAX_SHORTCUT_CYCLES_MIRROR
        );

        // ── E1.
        let views = derive_views(&state, None);
        assert_eq!(
            views.bounded_loop_max_repetitions,
            Some(bound),
            "[{label}] CR 732.2a: the open window's own ceiling, read off the live offer"
        );

        // ── E2, both directions on the same beat. The marked clone is the matched positive
        //    without which the emptiness below could mean the `∞` channel never fills at all.
        assert!(
            views.unbounded_resources.is_empty(),
            "[{label}] the bounded window marks no unbounded resource; got {:?}",
            views.unbounded_resources
        );
        let mut marked = state.clone();
        marked.mark_unbounded_loop(proposer, &[ResourceAxis::TokensCreated]);
        let marked_views = derive_views(&marked, None);
        assert_eq!(
            marked_views
                .unbounded_resources
                .iter()
                .map(|row| (row.player, row.axis))
                .collect::<Vec<_>>(),
            vec![(proposer, ResourceAxis::TokensCreated)],
            "[{label}] CR 732.2a: a marked `∞` row is published unchanged while the bounded \
             window is open"
        );
        assert_eq!(
            marked_views.bounded_loop_max_repetitions,
            Some(bound),
            "[{label}] and the bound is still stated beside it — neither channel withholds the \
             other"
        );

        // ── E3, handback leg: the window's own lifetime ends the channel.
        let template = f4_pin_template(&schema, proposer, 3);
        apply(
            &mut state,
            proposer,
            GameAction::DeclareShortcut {
                count: IterationCount::Fixed(3),
                template: Some(template),
            },
        )
        .unwrap_or_else(|e| panic!("[{label}] the declaration is dispatched: {e:?}"));
        let responders = accept_all_opponents(&mut state);
        assert!(
            responders > 0,
            "[{label}] reach-guard: the CR 732.2b window must have opened and been answered, \
             else no drive ran and no window was ever closed"
        );
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "[{label}] reach-guard: CR 732.2a's ending point is a place where a player has \
             priority; got {:?}",
            state.waiting_for
        );
        assert_eq!(
            derive_views(&state, None).bounded_loop_max_repetitions,
            None,
            "[{label}] CR 732.2a: the closed window states no ceiling — the channel is WITHDRAWN, \
             not merely never set"
        );
    }
}

/// **Row E4.** The channel is additive on the wire: a `DerivedViews` object serialized before it
/// existed still decodes, and an absent channel is omitted from emitted JSON.
///
/// # Non-vacuity
///
/// Each leg carries the opposite-direction control in the same test, because "decodes to `None`"
/// alone is satisfied by a decoder that never looks at the field, and "the key is absent" alone
/// is satisfied by a serializer that never emits it.
///
/// # Discrimination
///
/// Remove `#[serde(default, ..)]` ⇒ the key-absent decode fails; remove
/// `skip_serializing_if = "Option::is_none"` ⇒ the omitted-key assertion fails.
#[test]
fn e4_the_bounded_repetition_channel_is_additive_in_both_directions() {
    use engine::game::derived_views::DerivedViews;

    const KEY: &str = "bounded_loop_max_repetitions";
    let absent =
        serde_json::to_value(DerivedViews::default()).expect("an empty projection serializes");
    assert!(
        absent.get(KEY).is_none(),
        "an absent channel is OMITTED from emitted JSON; got {absent:?}"
    );
    assert_eq!(
        serde_json::from_value::<DerivedViews>(absent.clone())
            .expect("a projection without the key decodes")
            .bounded_loop_max_repetitions,
        None,
        "a `DerivedViews` object serialized before this channel existed still decodes"
    );

    let mut present = absent;
    present[KEY] = serde_json::json!(7);
    assert_eq!(
        serde_json::from_value::<DerivedViews>(present)
            .expect("a projection carrying the key decodes")
            .bounded_loop_max_repetitions,
        Some(7),
        "control: the decoder does read the field, so the `None` above is the default and not a \
         field the decoder ignores"
    );

    let emitted = serde_json::to_value(DerivedViews {
        bounded_loop_max_repetitions: Some(7),
        ..DerivedViews::default()
    })
    .expect("a populated projection serializes");
    assert_eq!(
        emitted.get(KEY),
        Some(&serde_json::json!(7)),
        "control: the serializer does emit the field, so the omission above is \
         `skip_serializing_if` and not a field that is never written"
    );
}

/// **Rows E5a / E5b / E6.** CR 732.2a: only a producer that NARROWED the bound has a ceiling to
/// state. An unnarrowed offer publishes nothing, a legal `Fixed(n)` declaration made against
/// that unnarrowed offer still publishes nothing, and neither does a respond window on a
/// narrowed offer.
///
/// # Non-vacuity
///
/// E5b's `None` would pass on a board where the declaration was simply refused, so four
/// reach-guards run before it: the dispatch returned `Ok`, the respond window is open, the
/// proposal carries the declared count, and the proposal's axis vector is NON-EMPTY — the vector
/// the accept path marks, which is what makes a published `n` here a false bound rather than a
/// merely missing one. E6 is paired with the `Some(..)` read one beat earlier on the same board.
///
/// # Discrimination
///
/// Drop the `is_bounded()` guard ⇒ E5a reads `Some(cap)`. Add a `RespondToShortcut` arm ⇒ E5b
/// and E6 both read `Some(n)`.
#[test]
fn e5_an_unnarrowed_offer_and_every_respond_window_state_no_ceiling() {
    use engine::game::derived_views::derive_views;

    for (label, mut state) in [
        ("F4", load_f4()),
        ("MODE1", load_mode1()),
        ("MODE2", load_mode2()),
    ] {
        drive_f4_to_offer(&mut state, 400)
            .unwrap_or_else(|| panic!("[{label}] reach-guard: the bounded offer must FIRE"));
        let (proposer, _certificate, schema) = offer_parts(&state);
        let declared = schema.max_iterations;
        let schema = schema.clone();

        // ── E5a: the same board with its bound un-narrowed to the engine cap.
        let mut unnarrowed = state.clone();
        let WaitingFor::LoopShortcut {
            schema: hostile_schema,
            ..
        } = &mut unnarrowed.waiting_for
        else {
            panic!("[{label}] the driven beat is the CR 732.2a offer");
        };
        hostile_schema.max_iterations = MAX_SHORTCUT_CYCLES_MIRROR;
        assert!(
            !hostile_schema.is_bounded(),
            "[{label}] reach-guard: the mutated board really is UNNARROWED, so the absence below \
             is the guard's work and not a failure to build the offer"
        );
        assert_eq!(
            derive_views(&unnarrowed, None).bounded_loop_max_repetitions,
            None,
            "[{label}] CR 732.2a: a producer that never narrowed the bound has no ceiling to state"
        );

        // ── E5b: a LEGAL declaration against that unnarrowed offer still states nothing.
        apply(
            &mut unnarrowed,
            proposer,
            GameAction::DeclareShortcut {
                count: IterationCount::Fixed(declared),
                template: Some(f4_pin_template(&schema, proposer, declared)),
            },
        )
        .unwrap_or_else(|e| {
            panic!(
                "[{label}] reach-guard: a `Fixed({declared})` under the cap is legal here: {e:?}"
            )
        });
        let WaitingFor::RespondToShortcut { proposal, .. } = &unnarrowed.waiting_for else {
            panic!(
                "[{label}] reach-guard: the declaration must open the CR 732.2b window, got {:?}",
                unnarrowed.waiting_for
            );
        };
        assert_eq!(
            proposal.count,
            IterationCount::Fixed(declared),
            "[{label}] reach-guard: the open window carries the count that was declared"
        );
        assert!(
            !proposal.unbounded.is_empty(),
            "[{label}] reach-guard: the proposal carries the axis vector the accept path marks — \
             publishing a count here would be a FALSE bound, not a missing one"
        );
        assert_eq!(
            derive_views(&unnarrowed, None).bounded_loop_max_repetitions,
            None,
            "[{label}] CR 732.2a: a proposal carries no boundedness witness, so no respond window \
             states a ceiling"
        );

        // ── E6: the same refusal on a NARROWED offer, with its matched positive one beat back.
        assert_eq!(
            derive_views(&state, None).bounded_loop_max_repetitions,
            Some(declared),
            "[{label}] matched positive: the narrowed offer DOES state its ceiling one beat \
             before the declaration below"
        );
        apply(
            &mut state,
            proposer,
            GameAction::DeclareShortcut {
                count: IterationCount::Fixed(declared),
                template: Some(f4_pin_template(&schema, proposer, declared)),
            },
        )
        .unwrap_or_else(|e| panic!("[{label}] the declaration is dispatched: {e:?}"));
        assert!(
            matches!(state.waiting_for, WaitingFor::RespondToShortcut { .. }),
            "[{label}] reach-guard: the CR 732.2b window is open, got {:?}",
            state.waiting_for
        );
        assert_eq!(
            derive_views(&state, None).bounded_loop_max_repetitions,
            None,
            "[{label}] CR 732.2a: the respond surface is refused UNIFORMLY — narrowed offer or not"
        );
    }
}

/// **Row E7.** CR 732.2a: the ceiling this channel states IS the ceiling the count picker
/// publishes for the same window — one number in the dialog, not two that could diverge.
///
/// # Non-vacuity
///
/// The comparison is against the OTHER projection's live output, never against a re-derived
/// clamp, so it can red independently of E1. The interaction authority is BOUND on the clone
/// before the read, and the proposer's opportunity list is asserted non-empty first: an unbound
/// probe can answer `AuthorityUnbound` with no ceiling at all, which is a dead instrument rather
/// than a disagreement.
///
/// # Discrimination
///
/// Publish `max_iterations + 1` from the new arm ⇒ the equality reds. The hostile leg is the
/// unnarrowed board, where the picker still publishes a ceiling at the cap while this channel
/// states nothing — so no disagreement is representable there.
#[test]
fn e7_the_published_bound_is_the_count_pickers_own_ceiling() {
    use engine::game::derived_views::derive_views;
    use engine::game::interaction::{bind_interaction_authority, derive_viewer_interaction};
    use engine::game::visibility::filter_state_for_viewer;
    use engine::types::interaction::{
        InteractionOpportunityResponse, InteractionResponseSpec, InteractionSessionId,
        InteractionShortcutCountSpec,
    };

    fn published_ceiling(state: &GameState, proposer: PlayerId, label: &str) -> u32 {
        let mut probe = state.clone();
        bind_interaction_authority(
            &mut probe,
            InteractionSessionId("e7-bounded-channel".to_string()),
        )
        .expect("bind the interaction authority over the live offer");
        let filtered = filter_state_for_viewer(&probe, proposer);
        let view = derive_viewer_interaction(&probe, &filtered, proposer);
        assert!(
            !view.opportunities.is_empty(),
            "[{label}] liveness control: a bound proposer must read a non-empty opportunity list, \
             else the ceiling below is an unbound probe's silence"
        );
        let InteractionOpportunityResponse::Schema {
            spec: InteractionResponseSpec::Shortcut { count, .. },
            ..
        } = &view.opportunities[0].response
        else {
            panic!("[{label}] the live offer publishes a Shortcut response schema");
        };
        let InteractionShortcutCountSpec::Fixed { max, .. } = count else {
            panic!("[{label}] a Fixed count window publishes a ceiling, got {count:?}");
        };
        *max
    }

    for (label, mut state) in [
        ("F4", load_f4()),
        ("MODE1", load_mode1()),
        ("MODE2", load_mode2()),
    ] {
        drive_f4_to_offer(&mut state, 400)
            .unwrap_or_else(|| panic!("[{label}] reach-guard: the bounded offer must FIRE"));
        let (proposer, _certificate, schema) = offer_parts(&state);
        let bound = schema.max_iterations;
        assert!(
            schema.is_bounded() && bound > 1,
            "[{label}] reach-guard: a narrowed window with a ceiling above 1 — a ceiling of 1 \
             could not discriminate a clamp from an identity"
        );

        assert_eq!(
            derive_views(&state, None).bounded_loop_max_repetitions,
            Some(published_ceiling(&state, proposer, label)),
            "[{label}] CR 732.2a: the badge's number and the count picker's ceiling are the same \
             engine value"
        );

        // ── HOSTILE: unnarrowed. The picker still caps at the engine-wide limit while this
        //    channel states nothing, so the two cannot disagree.
        let mut unnarrowed = state.clone();
        let WaitingFor::LoopShortcut {
            schema: hostile_schema,
            ..
        } = &mut unnarrowed.waiting_for
        else {
            panic!("[{label}] the driven beat is the CR 732.2a offer");
        };
        hostile_schema.max_iterations = MAX_SHORTCUT_CYCLES_MIRROR;
        assert_eq!(
            published_ceiling(&unnarrowed, proposer, label),
            MAX_SHORTCUT_CYCLES_MIRROR,
            "[{label}] reach-guard: the picker still publishes a ceiling on the unnarrowed board"
        );
        assert_eq!(
            derive_views(&unnarrowed, None).bounded_loop_max_repetitions,
            None,
            "[{label}] CR 732.2a: and this channel states nothing there, so no number can \
             disagree with the picker beside it"
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════════════════════
// CR 732.2b — WHAT THE RESPONDING OPPONENT ACTUALLY SEES
//
// A responder reads the proposer's declaration here, not only the count. These tests drive the
// tracked dump through the real ingress, open the accept-or-shorten window, and read the
// published `InteractionResponseSpec::ShortcutReply` off `derive_viewer_interaction`.
// ═════════════════════════════════════════════════════════════════════════════════════════

/// What one seat's published accept-or-shorten schema carries.
struct F4Reply {
    points: Vec<engine::types::interaction::InteractionShortcutPoint>,
    declared: Option<engine::types::interaction::InteractionShortcutPreview>,
    candidates: Vec<engine::types::interaction::InteractionChoice>,
}

/// Read `viewer`'s own respond-side projection. Panics unless that seat has exactly one
/// opportunity, so a row cannot silently assert about a seat the projection never addressed.
fn f4_reply_at(state: &GameState, viewer: PlayerId) -> F4Reply {
    use engine::game::interaction::derive_viewer_interaction;
    use engine::game::visibility::filter_state_for_viewer;
    use engine::types::interaction::{InteractionOpportunityResponse, InteractionResponseSpec};

    let filtered = filter_state_for_viewer(state, viewer);
    let view = derive_viewer_interaction(state, &filtered, viewer);
    let [opportunity] = view.opportunities.as_slice() else {
        panic!(
            "{viewer:?} must carry exactly one opportunity at the respond beat, got {}",
            view.opportunities.len()
        );
    };
    let InteractionOpportunityResponse::Schema {
        spec: InteractionResponseSpec::ShortcutReply {
            points, declared, ..
        },
        candidates,
    } = &opportunity.response
    else {
        panic!("the accept-or-shorten window uses a shortcut-reply schema");
    };
    F4Reply {
        points: points.clone(),
        declared: declared.clone(),
        candidates: candidates.clone(),
    }
}

/// How many opportunities `viewer` reads, and the availability it reads them under.
fn f4_opportunity_count(
    state: &GameState,
    viewer: PlayerId,
) -> (usize, engine::types::interaction::InteractionAvailability) {
    use engine::game::interaction::derive_viewer_interaction;
    use engine::game::visibility::filter_state_for_viewer;

    let filtered = filter_state_for_viewer(state, viewer);
    let view = derive_viewer_interaction(state, &filtered, viewer);
    (view.opportunities.len(), view.availability)
}

/// [`f4_allocation_submission`] with each answerable point's candidate chosen by INDEX rather
/// than defaulted to the first, so a row can author a declaration whose optional decisions went
/// different ways.
fn f4_submission_with_answers(
    offer: &F4Allocation,
    count: u32,
    allocation: &[(PlayerId, u32)],
    answers: &[usize],
) -> engine::types::interaction::InteractionSubmission {
    use engine::types::interaction::InteractionResponse;

    assert_eq!(
        answers.len(),
        offer.answerable.len(),
        "fixture guard: one authored answer per answerable point"
    );
    let mut submission = f4_allocation_submission(offer, count, allocation);
    let InteractionResponse::Shortcut { pins, .. } = &mut submission.response else {
        unreachable!("the allocation submission is a shortcut response");
    };
    for (point, answer) in offer.answerable.iter().zip(answers) {
        let pin = pins
            .iter_mut()
            .find(|pin| pin.group == point.group)
            .expect("every answerable point already carries a pin");
        pin.choice_ids = vec![point.candidate_ids[*answer].clone()];
    }
    submission
}

/// Dispatch a declaration through the real ingress and STOP at the CR 732.2b window, rather
/// than draining it the way [`f4_drive_allocation`] does.
fn f4_open_respond_window(
    state: &mut GameState,
    offer: &F4Allocation,
    submission: &engine::types::interaction::InteractionSubmission,
) -> PlayerId {
    use engine::game::interaction::resolve_interaction_response;

    let action = resolve_interaction_response(state, offer.proposer, submission)
        .expect("the allocation ingress accepts a conformant sequenced pin");
    apply(state, offer.proposer, action).expect("the minted declaration is dispatched");
    // THE DISCRIMINATOR between "declare refused it" and "the drive aborted": a refused
    // declaration hands priority straight back and never opens the APNAP window.
    let WaitingFor::RespondToShortcut { player, .. } = state.waiting_for else {
        panic!(
            "the accepted declaration must open the CR 732.2b APNAP window, got {:?}",
            state.waiting_for
        );
    };
    player
}

/// The seat a published candidate names, read off the engine's own player surface.
fn f4_candidate_seat(
    candidates: &[engine::types::interaction::InteractionChoice],
    id: &engine::types::interaction::InteractionChoiceId,
) -> Option<u8> {
    use engine::types::interaction::InteractionPresentationSurface;
    candidates
        .iter()
        .find(|choice| choice.id == *id)?
        .surfaces
        .iter()
        .find_map(|surface| match surface {
            InteractionPresentationSurface::Player { seat, .. } => Some(*seat),
            _ => None,
        })
}

/// The object reference a published candidate names, read off the engine's own object surface.
fn f4_candidate_object(
    candidates: &[engine::types::interaction::InteractionChoice],
    id: &engine::types::interaction::InteractionChoiceId,
) -> Option<String> {
    use engine::types::interaction::InteractionPresentationSurface;
    candidates
        .iter()
        .find(|choice| choice.id == *id)?
        .surfaces
        .iter()
        .find_map(|surface| match surface {
            InteractionPresentationSurface::Object { reference, .. } => Some(reference.clone()),
            _ => None,
        })
}

/// The discriminant a published `mayChoice` answer candidate states.
fn f4_candidate_value(
    candidates: &[engine::types::interaction::InteractionChoice],
    id: &engine::types::interaction::InteractionChoiceId,
) -> Option<String> {
    use engine::types::interaction::InteractionPresentationSurface;
    candidates
        .iter()
        .find(|choice| choice.id == *id)?
        .surfaces
        .iter()
        .find_map(|surface| match surface {
            InteractionPresentationSurface::Value { value, .. } => Some(value.clone()),
            _ => None,
        })
}

/// The per-seat life magnitudes one published element states, keyed by seat.
fn f4_life_entries(
    element: &engine::types::interaction::InteractionShortcutPreview,
) -> Vec<(u8, i32)> {
    use engine::types::interaction::InteractionShortcutPreviewFamily;
    element
        .entries
        .iter()
        .filter(|entry| entry.family == InteractionShortcutPreviewFamily::Life)
        .map(|entry| {
            (
                entry
                    .player
                    .expect("a Life magnitude is keyed by the seat that loses it"),
                entry.amount,
            )
        })
        .collect()
}

/// AGREEMENT MODULO BEAT-LOCAL IDS — the substitute for a byte equality that cannot hold.
///
/// `InteractionChoiceId` embeds the interaction id, and that id ROTATES between the offer beat
/// and the respond beat (`rebind_interaction_slots_after_action`: "Single decisions always
/// rotate, including A→A and A→B→A"), so a whole-struct equality across beats could only ever
/// fail. Each side therefore resolves its OWN beat's candidate list, and the conjunct is that
/// the two allocations name the same seats in the same order.
///
/// Returns the conjunct that refused, so a caller can assert WHICH one did.
fn shortcut_elements_agree_modulo_ids(
    a: (
        &engine::types::interaction::InteractionShortcutPreview,
        &[engine::types::interaction::InteractionChoice],
    ),
    b: (
        &engine::types::interaction::InteractionShortcutPreview,
        &[engine::types::interaction::InteractionChoice],
    ),
) -> Result<(), &'static str> {
    let (left, left_candidates) = a;
    let (right, right_candidates) = b;
    if left.count != right.count {
        return Err("count");
    }
    if left.entries != right.entries {
        return Err("entries");
    }
    if left.allocation.len() != right.allocation.len() {
        return Err("allocation arity");
    }
    if left
        .allocation
        .iter()
        .zip(&right.allocation)
        .any(|(x, y)| x.amount != y.amount)
    {
        return Err("allocation amounts");
    }
    for (x, y) in left.allocation.iter().zip(&right.allocation) {
        let left_seat = f4_candidate_seat(left_candidates, &x.choice_id);
        if left_seat.is_none() || left_seat != f4_candidate_seat(right_candidates, &y.choice_id) {
            return Err("allocation subject");
        }
    }
    Ok(())
}

/// **The responder reads the declared partition, each declared seat's own magnitude, and nothing
/// that belongs to another seat.**
///
/// CR 732.2b's right is to name a place where this player's choice will differ from what was
/// proposed, so the responder is published the proposer's declaration and not only the count.
///
/// # Non-vacuity, on a fixture that is cardinality-degenerate by default
///
/// The tracked dump's per-cycle life rate is 1 and its life map names exactly ONE seat, so a
/// driven row can admit a flag where a count is required. The declaration this row authors is
/// therefore `1 / 2 / 3` at a count of 6: THREE segments, PAIRWISE DISTINCT, and a non-canonical
/// split (the canonical one at 6 over three seats is `2 / 2 / 2`). Every per-seat magnitude is
/// asserted as the published rate times the authored segment, so a uniform re-attribution, a
/// flag standing in for a count, and an equality between two empty lists all fail.
///
/// # The per-seat opportunity claim rides here, because this test's harness proof is its
/// positive control
///
/// The projection is empty for every seat at every beat unless the interaction authority is
/// bound first, and what is uniform under an unbound harness is `opportunities = 0` AT THE OFFER
/// BEAT TOO. So the opportunity COUNT is what the obligation asserts on, and the offer beat's
/// proposer-only opportunity is the control that the binding took.
///
/// # Discrimination
///
/// Leave `declared: None` on the spec ⇒ the responder is back to the two-number shape and the
/// element assertions fail. Publish to any seat but the current responder ⇒ the per-seat
/// opportunity counts fail.
#[test]
fn the_responder_reads_the_declared_partition_and_its_per_seat_magnitudes() {
    use engine::types::interaction::{
        InteractionAvailability, InteractionShortcutPointKind, InteractionShortcutPreviewFamily,
    };

    const COUNT: u32 = 6;
    const SEGMENTS: [u32; 3] = [1, 2, 3];

    let mut state = load_f4();
    let offer = f4_allocation_offer(&mut state);
    let seats: Vec<PlayerId> = state.players.iter().map(|player| player.id).collect();

    // ── HARNESS CONTROL + the per-seat paired positive: at the OFFER beat exactly the proposer
    //    carries an opportunity. Under an unbound authority every seat reads zero, so a
    //    non-zero count here is what proves the binding took.
    for seat in &seats {
        let (count, availability) = f4_opportunity_count(&state, *seat);
        if *seat == offer.proposer {
            assert_eq!(
                count, 1,
                "control: the proposer's own offer opportunity is what proves the interaction \
                 authority is bound — a zero here makes every count below a dead instrument"
            );
        } else {
            assert_eq!(
                (count, availability),
                (0, InteractionAvailability::Waiting),
                "CR 732.2a: the offer addresses the player with priority and nobody else \
                 ({seat:?})"
            );
        }
    }

    assert!(
        offer.rate > 0,
        "ANTI-VACUITY: the published per-cycle life rate is strictly positive, else every \
         magnitude below degenerates to zero"
    );
    assert_eq!(
        offer.legal_seats.len(),
        3,
        "reach-guard: three legal victims, which is what makes a three-segment declaration a \
         real member of the composition set"
    );
    let allocation: Vec<(PlayerId, u32)> =
        offer.legal_seats.iter().copied().zip(SEGMENTS).collect();
    let responder = f4_open_respond_window(
        &mut state,
        &offer,
        &f4_allocation_submission(&offer, COUNT, &allocation),
    );

    // ── PER-SEAT: at the RESPOND beat exactly the current responder carries an opportunity. The
    //    queued opponents read `Waiting`, which is what distinguishes "not yet their turn" from
    //    "published to everyone".
    let WaitingFor::RespondToShortcut {
        ref remaining_players,
        ..
    } = state.waiting_for
    else {
        unreachable!("the window was just asserted open");
    };
    assert!(
        !remaining_players.is_empty(),
        "reach-guard: opponents are still QUEUED behind this responder, so the zero counts \
         below are a routing rule and not an empty table"
    );
    for seat in &seats {
        let (count, availability) = f4_opportunity_count(&state, *seat);
        if *seat == responder {
            assert_eq!(count, 1, "CR 732.2b addresses the current responder");
        } else {
            assert_eq!(
                (count, availability),
                (0, InteractionAvailability::Waiting),
                "CR 732.2b: neither the proposer nor a queued opponent is addressed yet \
                 ({seat:?})"
            );
        }
    }

    // ── THE GAP ITSELF.
    let reply = f4_reply_at(&state, responder);
    let targets: Vec<_> = reply
        .points
        .iter()
        .filter(|point| point.kind == InteractionShortcutPointKind::Targets)
        .collect();
    let [target_point] = targets.as_slice() else {
        panic!("the declaration states exactly one announced-target decision");
    };
    assert_eq!(
        target_point
            .candidate_ids
            .iter()
            .map(|id| f4_candidate_seat(&reply.candidates, id))
            .collect::<Vec<_>>(),
        offer
            .legal_seats
            .iter()
            .map(|seat| Some(seat.0))
            .collect::<Vec<_>>(),
        "the declaration's announced seats reach the responder in the proposer's own order"
    );
    let element = reply
        .declared
        .as_ref()
        .expect("CR 732.2b: the responder is published the declaration they are judging");
    assert_eq!(element.count, COUNT);
    assert_eq!(
        element
            .allocation
            .iter()
            .map(|assignment| assignment.amount)
            .collect::<Vec<_>>(),
        SEGMENTS.to_vec(),
        "the published partition is the one the proposer authored — THREE segments, pairwise \
         distinct, and NOT the canonical split of {COUNT} over three seats"
    );
    assert_eq!(
        element
            .allocation
            .iter()
            .map(|assignment| f4_candidate_seat(&reply.candidates, &assignment.choice_id))
            .collect::<Vec<_>>(),
        offer
            .legal_seats
            .iter()
            .map(|seat| Some(seat.0))
            .collect::<Vec<_>>(),
        "and every segment names its own seat through this beat's published candidates"
    );
    let expected: Vec<(u8, i32)> = allocation
        .iter()
        .map(|(seat, segment)| (seat.0, -(offer.rate * i64::from(*segment)) as i32))
        .collect();
    assert_eq!(
        f4_life_entries(element),
        expected,
        "CR 119.3: each declared seat's magnitude is the published per-cycle rate times ITS \
         OWN segment — {expected:?} — so a producer that re-attributed the drain uniformly, or \
         keyed it on the seat the period was measured on, fails here"
    );
    let magnitudes: Vec<i32> = f4_life_entries(element)
        .into_iter()
        .map(|(_, amount)| amount)
        .collect();
    assert_eq!(
        magnitudes
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        magnitudes.len(),
        "ANTI-VACUITY: the three magnitudes are PAIRWISE DISTINCT, so a flag standing in for a \
         count cannot satisfy the equality above. magnitudes={magnitudes:?}"
    );
    assert!(
        element
            .entries
            .iter()
            .any(|entry| entry.family != InteractionShortcutPreviewFamily::Life),
        "reach-guard: the element states the period's other families too, so the Life filter \
         above is selecting rather than describing the whole list"
    );

    // ── HOSTILE (a): the count-only route. A proposal carrying no declaration — every offer
    //    minted before the declaration ingress existed, and every save written then — publishes
    //    no statement point and no element. First production branch: the decoder's template
    //    `as_ref()`.
    let mut count_only = state.clone();
    let WaitingFor::RespondToShortcut { proposal, .. } = &mut count_only.waiting_for else {
        unreachable!("the clone parks on the same window");
    };
    proposal.template = None;
    let bare = f4_reply_at(&count_only, responder);
    assert!(
        bare.points.is_empty() && bare.declared.is_none(),
        "a count-only proposal states no declaration, so the responder reads the base shape"
    );

    // ── HOSTILE (b): a declaration whose proposal states NO per-period signature. The
    //    statement points and the PARTITION still publish — segment lengths are not magnitudes —
    //    but no magnitude is invented beside them. First production branch:
    //    `shortcut_preview_basis`'s `per_cycle`.
    let mut signatureless = state.clone();
    let WaitingFor::RespondToShortcut { proposal, .. } = &mut signatureless.waiting_for else {
        unreachable!("the clone parks on the same window");
    };
    proposal.per_cycle = None;
    let unmeasured = f4_reply_at(&signatureless, responder);
    assert_eq!(
        unmeasured.points.len(),
        reply.points.len(),
        "the declaration is still published whole — only its magnitudes are unstated"
    );
    let unmeasured_element = unmeasured.declared.as_ref().expect(
        "CR 732.2b: the responder judges the whole declaration, and its partition is the \
         proposer's own whether or not a period was measured",
    );
    assert_eq!(
        (
            unmeasured_element.count,
            unmeasured_element
                .allocation
                .iter()
                .map(|assignment| assignment.amount)
                .collect::<Vec<_>>(),
        ),
        (COUNT, SEGMENTS.to_vec()),
        "and it is the SAME partition the measured board published — three segments, pairwise \
         distinct, so an element emptied wholesale cannot satisfy this"
    );
    assert!(
        unmeasured_element.entries.is_empty(),
        "CR 732.2a: a magnitude is the period times the count, and there is no period to \
         multiply. got {:?}",
        unmeasured_element.entries
    );
    assert!(
        !element.entries.is_empty(),
        "reach-guard: the measured board one field apart DOES state magnitudes, so the emptiness \
         above is this branch rather than a producer that never states any"
    );
}

/// **ONE PRODUCER, AND THE CALL SITES AGREE — the responder's element is minted by the same
/// function the proposer's own preview and the offer's published list are.**
///
/// # A byte equality across the two beats is unsatisfiable, and the substitute is pinned
///
/// `InteractionChoiceId` embeds the interaction id, which rotates between the two beats, so a
/// whole-struct equality could only fail. [`shortcut_elements_agree_modulo_ids`] is the
/// substitute, and the ADMITTED-MEMBER leg below is what shows it is not merely weaker: it
/// constructs a pair that agrees on every conjunct EXCEPT the resolved subject mapping and
/// asserts the predicate refuses it, naming that conjunct.
///
/// # Discrimination
///
/// Re-derive the entries at the respond-side call site instead of calling
/// `shortcut_preview_element` ⇒ the two disagree and the main leg fails. Delete the resolved-
/// subject conjunct ⇒ the admitted-member leg's second half fails.
#[test]
fn the_responders_element_agrees_with_the_producers_other_two_call_sites() {
    use engine::game::interaction::preview_interaction;
    use engine::types::interaction::{
        AmountAssignment, InteractionChoice, InteractionChoiceId, InteractionPresentationSurface,
        InteractionPreviewRequest, InteractionShortcutPreview, InteractionShortcutPreviewEntry,
        InteractionShortcutPreviewFamily, PreviewRequestId,
    };

    const COUNT: u32 = 6;
    const SEGMENTS: [u32; 3] = [1, 2, 3];

    // ── MAIN LEG: the proposer's own preview of this declaration, through `preview_interaction`,
    //    the responder's published element for the same declaration.
    let mut state = load_f4();
    let offer = f4_allocation_offer(&mut state);
    let allocation: Vec<(PlayerId, u32)> =
        offer.legal_seats.iter().copied().zip(SEGMENTS).collect();
    let submission = f4_allocation_submission(&offer, COUNT, &allocation);
    let authored = preview_interaction(
        &state,
        offer.proposer,
        &InteractionPreviewRequest {
            request_id: PreviewRequestId("declared-element-agreement".to_string()),
            interaction_id: submission.interaction_id.clone(),
            response: submission.response.clone(),
        },
    )
    .shortcut_preview
    .expect("the proposer's own declaration previews an element");

    let responder = f4_open_respond_window(&mut state, &offer, &submission);
    let reply = f4_reply_at(&state, responder);
    let published = reply
        .declared
        .as_ref()
        .expect("the responder is published the same declaration");

    // Reach-guards, before the comparison: MORE than one segment, non-empty entries, and
    // PAIRWISE-DISTINCT per-seat magnitudes — so an equality between two empty elements, or
    // between two uniform ones, cannot satisfy the agreement below.
    assert!(published.allocation.len() > 1 && !published.entries.is_empty());
    let magnitudes: Vec<i32> = f4_life_entries(published)
        .into_iter()
        .map(|(_, amount)| amount)
        .collect();
    assert_eq!(
        magnitudes,
        SEGMENTS
            .iter()
            .map(|segment| -(offer.rate * i64::from(*segment)) as i32)
            .collect::<Vec<_>>(),
        "reach-guard: the compared element states three DISTINCT per-seat magnitudes"
    );
    assert_ne!(
        submission.interaction_id,
        reply_interaction_id(&state, responder),
        "reach-guard: the interaction id really DID rotate across the two beats, which is why \
         a byte equality is unsatisfiable and this predicate exists"
    );
    assert_eq!(
        shortcut_elements_agree_modulo_ids(
            (&authored, &offer.offer_candidates),
            (published, &reply.candidates),
        ),
        Ok(()),
        "CR 732.2a: one producer mints both, so the two call sites cannot disagree about what \
         this declaration does"
    );

    // ── SECOND LEG: when the declaration IS the canonical split at a count the offer publishes
    //    an element for, the responder's element agrees with THAT element too.
    let mut canonical_state = load_f4();
    let canonical_offer = f4_allocation_offer(&mut canonical_state);
    let sampled = canonical_offer
        .published_preview
        .iter()
        .find(|element| {
            element.allocation.len() == canonical_offer.legal_seats.len()
                && element.count % 3 != 0
                && element.count > 3
        })
        .cloned()
        .expect(
            "reach-guard: the offer publishes an element whose canonical split has a REMAINDER, \
             so the compared magnitudes are not all equal",
        );
    let canonical_allocation: Vec<(PlayerId, u32)> = sampled
        .allocation
        .iter()
        .map(|assignment| {
            let seat = f4_candidate_seat(&canonical_offer.offer_candidates, &assignment.choice_id)
                .expect("every published allocation position names a seat");
            (PlayerId(seat), assignment.amount)
        })
        .collect();
    let canonical_submission =
        f4_allocation_submission(&canonical_offer, sampled.count, &canonical_allocation);
    let canonical_responder = f4_open_respond_window(
        &mut canonical_state,
        &canonical_offer,
        &canonical_submission,
    );
    let canonical_reply = f4_reply_at(&canonical_state, canonical_responder);
    let canonical_published = canonical_reply
        .declared
        .as_ref()
        .expect("the responder is published the canonical declaration too");
    let canonical_magnitudes: Vec<i32> = f4_life_entries(canonical_published)
        .into_iter()
        .map(|(_, amount)| amount)
        .collect();
    assert!(
        canonical_magnitudes.len() > 1
            && canonical_magnitudes
                .windows(2)
                .any(|pair| pair[0] != pair[1]),
        "reach-guard: the canonical split at {} carries a remainder, so its per-seat magnitudes \
         are NOT all equal and a uniform producer cannot satisfy the agreement below. \
         magnitudes={canonical_magnitudes:?}",
        sampled.count
    );
    assert_eq!(
        shortcut_elements_agree_modulo_ids(
            (&sampled, &canonical_offer.offer_candidates),
            (canonical_published, &canonical_reply.candidates),
        ),
        Ok(()),
        "CR 732.2a: the offer's own published element for that count and the responder's \
         element for the same declaration are the same arithmetic"
    );

    // ── ADMITTED-MEMBER LEG: the substitute must not admit what a byte equality would have
    //    refused aside from ids. Such a member is CONSTRUCTIBLE — two elements agreeing on
    //    count, entries and allocation AMOUNTS while position 0 resolves to a different seat —
    //    so both halves are asserted: that the pair really satisfies every other conjunct, and
    //    that the predicate refuses it naming the subject conjunct.
    let seat_choice = |id: &str, seat: u8| InteractionChoice {
        id: InteractionChoiceId(id.to_string()),
        surfaces: vec![InteractionPresentationSurface::Player {
            role: engine::types::interaction::InteractionRoleCode::Target,
            index: None,
            seat,
        }],
        status: engine::types::interaction::InteractionChoiceStatus::Available,
    };
    let element = |first: &str, second: &str| InteractionShortcutPreview {
        count: 3,
        entries: vec![InteractionShortcutPreviewEntry {
            family: InteractionShortcutPreviewFamily::Tokens,
            player: None,
            amount: 9,
        }],
        allocation: vec![
            AmountAssignment {
                choice_id: InteractionChoiceId(first.to_string()),
                amount: 1,
            },
            AmountAssignment {
                choice_id: InteractionChoiceId(second.to_string()),
                amount: 2,
            },
        ],
    };
    let left = element("beat-a.k0", "beat-a.k1");
    let right = element("beat-b.k0", "beat-b.k1");
    let left_candidates = [seat_choice("beat-a.k0", 1), seat_choice("beat-a.k1", 2)];
    // The SAME ids in the same order, resolving to the OPPOSITE seats — the whole difference.
    let right_candidates = [seat_choice("beat-b.k0", 2), seat_choice("beat-b.k1", 1)];
    assert_eq!(left.count, right.count);
    assert_eq!(left.entries, right.entries);
    assert_eq!(
        left.allocation.iter().map(|a| a.amount).collect::<Vec<_>>(),
        right
            .allocation
            .iter()
            .map(|a| a.amount)
            .collect::<Vec<_>>(),
        "the constructed pair really is a member only the resolved-subject conjunct can refuse: \
         it agrees on count, on entries element-for-element, and on the allocation amounts"
    );
    assert_eq!(
        shortcut_elements_agree_modulo_ids((&left, &left_candidates), (&right, &right_candidates),),
        Err("allocation subject"),
        "the substitute REFUSES a pair whose allocation positions name different seats — delete \
         that conjunct and this leg fails while the two agreements above still pass"
    );
}

/// The interaction id `viewer` is currently answering under, so a row can show that it rotated.
fn reply_interaction_id(
    state: &GameState,
    viewer: PlayerId,
) -> engine::types::interaction::InteractionId {
    use engine::game::interaction::derive_viewer_interaction;
    use engine::game::visibility::filter_state_for_viewer;

    let filtered = filter_state_for_viewer(state, viewer);
    let view = derive_viewer_interaction(state, &filtered, viewer);
    view.opportunities
        .first()
        .expect("the responder carries an opportunity")
        .interaction_id
        .clone()
}

/// **THE ANSWERED OPTIONAL DECISIONS REACH THE RESPONDER, AND WHICH WAY EACH ONE WENT.**
///
/// CR 732.2c makes every choice in the proposal actually taken on acceptance, so the answers are
/// the rule's own object rather than decoration. Each published statement point carries exactly
/// two candidate ids, read in order as its own decision's SUBJECT and that decision's ANSWER.
///
/// # HOSTILE, and it is what makes the row discriminating rather than decorative
///
/// The declaration is authored with the two answers going DIFFERENT ways. A producer that keyed
/// an answer on anything but its own pin's slot pairs the wrong answer to the wrong source, and
/// one that states "taken" for every optional decision passes the uniform case and fails this.
///
/// # Discrimination
///
/// Drop one answered decision ⇒ the point count fails. Publish both answers from the first
/// point ⇒ the difference assertion fails. The uniform sibling below keeps the difference from
/// being a shape the producer always emits.
#[test]
fn both_answered_optional_decisions_reach_the_responder_with_their_own_answers() {
    use engine::types::interaction::InteractionShortcutPointKind;

    const COUNT: u32 = 6;

    let answered = |answers: &[usize]| {
        let mut state = load_f4();
        let offer = f4_allocation_offer(&mut state);
        assert_eq!(
            offer.answerable.len(),
            2,
            "reach-guard: the tracked offer publishes TWO optional decisions, which is what \
             makes a differing pair constructible at all"
        );
        assert!(
            offer
                .answerable
                .iter()
                .all(|point| point.kind == InteractionShortcutPointKind::MayChoice),
            "reach-guard: both answerable points are optional decisions"
        );
        let allocation: Vec<(PlayerId, u32)> = offer
            .legal_seats
            .iter()
            .copied()
            .zip([1u32, 2, 3])
            .collect();
        let submission = f4_submission_with_answers(&offer, COUNT, &allocation, answers);
        let responder = f4_open_respond_window(&mut state, &offer, &submission);
        let reply = f4_reply_at(&state, responder);
        let published: Vec<_> = reply
            .points
            .iter()
            .filter(|point| point.kind == InteractionShortcutPointKind::MayChoice)
            .cloned()
            .collect();
        assert_eq!(
            published.len(),
            2,
            "both answered optional decisions reach the responder"
        );
        let stated: Vec<(String, String)> = published
            .iter()
            .map(|point| {
                let [subject, answer] = point.candidate_ids.as_slice() else {
                    panic!(
                        "a published optional-decision statement point carries EXACTLY two \
                         candidate ids — subject then answer — got {}",
                        point.candidate_ids.len()
                    );
                };
                (
                    f4_candidate_object(&reply.candidates, subject)
                        .expect("the subject candidate names its decision's own source object"),
                    f4_candidate_value(&reply.candidates, answer)
                        .expect("the answer candidate states which way the decision went"),
                )
            })
            .collect();
        (stated, reply.declared)
    };

    // ── HOSTILE: take one, decline the other.
    let (differing, differing_declared) = answered(&[0, 1]);
    assert_ne!(
        differing[0].0, differing[1].0,
        "reach-guard: the two statement points name DIFFERENT source objects, so pairing an \
         answer to the wrong one is visible"
    );
    assert_eq!(
        differing
            .iter()
            .map(|(_, answer)| answer.as_str())
            .collect::<Vec<_>>(),
        vec!["take", "decline"],
        "CR 732.2c: each published answer is its OWN decision's — a producer that stated \
         'taken' for every optional decision passes the uniform sibling below and fails here"
    );
    assert!(
        differing_declared.is_some(),
        "and publishing the answered decisions does not cost the declaration its partition"
    );

    // ── SIBLING: the same board declared with BOTH answers taken, so the difference above is a
    //    branch rather than a shape the producer always emits.
    let (uniform, _) = answered(&[0, 0]);
    assert_eq!(
        uniform
            .iter()
            .map(|(_, answer)| answer.as_str())
            .collect::<Vec<_>>(),
        vec!["take", "take"]
    );
    assert_eq!(
        uniform
            .iter()
            .map(|(subject, _)| subject)
            .collect::<Vec<_>>(),
        differing
            .iter()
            .map(|(subject, _)| subject)
            .collect::<Vec<_>>(),
        "and both boards name the same two decisions, so only the ANSWERS differ between them"
    );
}

/// CR 732.2a + CR 601.2c: whether an authored split is an ADMISSIBLE declaration of `count`
/// over the announced-target decision's own candidates, read in the canonical published order.
///
/// Returns the conjunct that refused — the [`shortcut_elements_agree_modulo_ids`] idiom — so a
/// caller asserts WHICH clause a constructed member trips rather than that something did. This
/// is not the ingress's own condition: the ingress accepts members this refuses. It is what
/// every leg of the composed row needs of the split it drives.
///
/// * `"dropped seat"` — the canonical element's own seats, each exactly once. Stated as a SET,
///   so a REORDERING reaches the final clause instead of being refused here.
/// * `"zero part"` — every part at least 1: the entries producer drops a family netting to
///   zero, so a zero part publishes no magnitude and no per-seat decrease.
/// * `"total"` — the parts total the declared count.
/// * `"unedited"` — the sequence differs from the canonical one, else "edits the allocation
///   away from the canonical split" is satisfied by re-sending it.
/// * `"repeated part"` — pairwise-distinct parts, which is what makes the per-seat magnitudes
///   distinguishable.
/// * `"final segment length"` — a final part of at least 2. With positive parts totalling the
///   count that is exactly "every segment START inside the window a drive of that count
///   realizes"; a segment starting at the last index realizes nothing, which
///   [`p4_row_1c_a_segment_starting_at_the_last_index_realizes_nothing_but_stays_announced`]
///   drives.
/// * `"leading-cycle seat"` — the final segment's seat is NOT the one the leading cycle
///   resolves, which repays exactly the cycle that segment loses.
fn authored_split_is_admissible(
    canonical: &[(PlayerId, u32)],
    authored: &[(PlayerId, u32)],
    count: u32,
    leading: PlayerId,
) -> Result<(), &'static str> {
    use std::collections::{BTreeSet, HashSet};

    let seats_of = |split: &[(PlayerId, u32)]| -> BTreeSet<PlayerId> {
        split.iter().map(|(seat, _)| *seat).collect()
    };
    if authored.len() != canonical.len() || seats_of(authored) != seats_of(canonical) {
        return Err("dropped seat");
    }
    if authored.iter().any(|(_, part)| *part == 0) {
        return Err("zero part");
    }
    if authored.iter().map(|(_, part)| *part).sum::<u32>() != count {
        return Err("total");
    }
    if authored == canonical {
        return Err("unedited");
    }
    let parts: Vec<u32> = authored.iter().map(|(_, part)| *part).collect();
    if parts.iter().collect::<HashSet<_>>().len() != parts.len() {
        return Err("repeated part");
    }
    let Some((final_seat, final_part)) = authored.last() else {
        return Err("dropped seat");
    };
    if *final_part < 2 {
        return Err("final segment length");
    }
    if *final_seat == leading {
        return Err("leading-cycle seat");
    }
    Ok(())
}

/// **ONE CHAIN** — the published offer, the authored edit, the ingress, the responder's own
/// projection and the committed drive are ONE object travelling ONE chain.
///
/// Every leg is already asserted by the row that built it — the ceiling by
/// [`e1_a_narrowed_offer_publishes_its_bound_beside_an_untouched_infinity_channel`] and
/// [`e7_the_published_bound_is_the_count_pickers_own_ceiling`], the token product by
/// [`t3_the_published_token_rate_is_delivered_by_the_accepted_drive`], the full commit across
/// an authored allocation by [`p4_row_1_an_allocation_of_the_published_ceiling_commits_its_whole_count`],
/// the one-producer identity by [`the_responders_element_agrees_with_the_producers_other_two_call_sites`].
/// What none of them holds is that those surfaces are the SAME object, so this is ONE test with
/// ONE `load_f4()` and every leg reading what the previous leg produced.
///
/// No magnitude is transcribed: the board is re-dumpable by design, so a pinned figure would
/// bind this row to one dump, and a re-dump the derivation cannot serve reds LOUDLY at the
/// admissibility predicate rather than quietly at a leg.
///
/// # Discrimination, per leg
///
/// (a) revert `DerivedViews::bounded_loop_max_repetitions`' population ⇒ `None`. (b) drop the
/// token term from `ResourceVector::period` ⇒ this guard reds directly, never as `0 == 0`.
/// (c) stop publishing `allocation` ⇒ there is no split to select. (d) derive a same-total
/// RE-COMPOSITION (two parts exchanged) instead of a transfer ⇒ the prefix-sum leg reds while
/// every predicate clause, the total included, still passes. (e) fall back to the canonical allocation when a pin
/// states `amounts` ⇒ the returned allocation is (c)'s; re-derive the entries at that call site
/// instead of calling the shared producer ⇒ its magnitudes disagree with (f)'s. (f) publish the
/// canonical split on the respond side ⇒ the agreement predicate returns
/// `Err("allocation amounts")`. (g) revert the slot-attributed subtraction ⇒ the declaration
/// truncates strictly short of its declared count. (h) point the conformance site back at the
/// raw snapshot pair while leaving the mint fed ⇒ zero cycles commit, so this leg reds while
/// (b) still passes.
#[test]
fn the_published_offer_the_authored_edit_and_the_committed_drive_are_one_chain() {
    use engine::game::derived_views::derive_views;
    use engine::game::interaction::preview_interaction;
    use engine::types::interaction::{InteractionPreviewRequest, PreviewRequestId};

    let mut state = load_f4();
    let offer = f4_allocation_offer(&mut state);

    // ── (a) THE BADGE. The open window's ceiling is the engine-published bound, and that bound
    //    is the count picker's own — ONE value, which every leg below then runs on.
    let (bound, per_cycle) = {
        let (_, certificate, schema) = offer_parts(&state);
        (
            schema.max_iterations,
            certificate
                .per_cycle
                .clone()
                .expect("a bounded offer publishes its per-period signature"),
        )
    };
    assert_eq!(
        derive_views(&state, None).bounded_loop_max_repetitions,
        Some(bound),
        "CR 732.2a: the open window's ceiling is the schema's own bound {bound}"
    );
    assert_eq!(
        bound, offer.max_count,
        "CR 732.2a: the badge's bound and the count picker's published ceiling are ONE engine \
         value, which is what makes every leg below run on the same count"
    );
    let count = offer.max_count;

    // ── (b) THE RATES, asserted BEFORE anything multiplies by them, so no product below can
    //    meet its clause as `0 == 0 * n`.
    let token_rate = per_cycle.delta.tokens_created;
    assert!(
        token_rate > 0,
        "ANTI-VACUITY: a ZERO published token rate FAILS this row rather than satisfying it. \
         published delta={:?}",
        per_cycle.delta
    );
    assert!(
        offer.rate > 0,
        "ANTI-VACUITY: the published per-cycle life rate is strictly positive, else every \
         per-seat magnitude below degenerates to zero. published life={:?}",
        per_cycle.delta.life
    );

    // ── (c) THE PUBLISHED ELEMENT, selected by its published count rather than by index.
    let element = offer
        .published_preview
        .iter()
        .find(|element| element.count == count)
        .expect("the published preview list always samples the picker's own ceiling");
    let canonical: Vec<(PlayerId, u32)> = element
        .allocation
        .iter()
        .map(|assignment| {
            let seat = f4_candidate_seat(&offer.offer_candidates, &assignment.choice_id)
                .expect("every published allocation position names a seat");
            (PlayerId(seat), assignment.amount)
        })
        .collect();
    // Identity and ORDER against the offer's OWN published victims, so the arity this row runs
    // at is the offer's rather than a transcribed number and a re-dump that moves the victim
    // set flows through.
    assert_eq!(
        canonical.iter().map(|(seat, _)| *seat).collect::<Vec<_>>(),
        offer.legal_seats,
        "CR 601.2c: the count-keyed element allocates over the offer's own published legal \
         victims, in published order. canonical={canonical:?}"
    );
    assert!(
        canonical.len() > 1,
        "reach-guard: more than one segment, which is what a per-seat comparison below needs. \
         canonical={canonical:?}"
    );

    // ── (d) THE EDIT. Repetitions are TRANSFERRED from earlier segments to later ones until
    //    the parts are strictly increasing in the canonical published order; the offsets sum to
    //    zero, so the count is preserved by construction.
    let spread = i64::try_from(canonical.len()).expect("a published arity fits an i64") - 1;
    let authored: Vec<(PlayerId, u32)> = canonical
        .iter()
        .enumerate()
        .map(|(index, (seat, part))| {
            let shifted =
                i64::from(*part) + 2 * i64::try_from(index).expect("an index fits an i64") - spread;
            (*seat, u32::try_from(shifted).unwrap_or(0))
        })
        .collect();
    let prefix_sums = |split: &[(PlayerId, u32)]| -> Vec<u32> {
        split
            .iter()
            .scan(0u32, |running, (_, part)| {
                *running += part;
                Some(*running)
            })
            .collect()
    };
    let (authored_prefix, canonical_prefix) = (prefix_sums(&authored), prefix_sums(&canonical));
    assert!(
        authored_prefix
            .iter()
            .zip(&canonical_prefix)
            .all(|(edited, published)| edited <= published)
            && authored_prefix
                .iter()
                .zip(&canonical_prefix)
                .any(|(edited, published)| edited < published),
        "the edit MOVES repetitions from earlier segments to later ones: every prefix sum is at \
         most the canonical's, and one is strictly less. A same-total re-composition over the \
         same seats — two parts exchanged — preserves the total the predicate checks and fails \
         here. canonical={canonical_prefix:?} authored={authored_prefix:?}"
    );
    assert_eq!(
        authored_split_is_admissible(&canonical, &authored, count, offer.preannounced),
        Ok(()),
        "the derived split must be admissible; a re-dump this derivation cannot serve reds \
         HERE. canonical={canonical:?} authored={authored:?} count={count}"
    );

    // Both refusals are REAL members the ingress accepts and commits, and each falsifies a
    // named conjunct of (g). The admitted split above, run through the IDENTICAL predicate in
    // this same invocation, is their positive control.
    let mut trailing_one = authored.clone();
    let last = trailing_one.len() - 1;
    let moved = trailing_one[last].1 - 1;
    trailing_one[last].1 = 1;
    trailing_one[0].1 += moved;
    assert_eq!(
        authored_split_is_admissible(&canonical, &trailing_one, count, offer.preannounced),
        Err("final segment length"),
        "a split whose FINAL PART is 1 starts its last segment past the window a drive of \
         {count} realizes, so its trailing seat realizes nothing. split={trailing_one:?}"
    );
    let mut swapped_seats: Vec<PlayerId> = authored.iter().map(|(seat, _)| *seat).collect();
    let leading_index = swapped_seats
        .iter()
        .position(|seat| *seat == offer.preannounced)
        .expect("the leading cycle's seat is one of the published legal victims");
    swapped_seats.swap(leading_index, last);
    let leading_last: Vec<(PlayerId, u32)> = swapped_seats
        .into_iter()
        .zip(authored.iter().map(|(_, part)| *part))
        .collect();
    assert_eq!(
        authored_split_is_admissible(&canonical, &leading_last, count, offer.preannounced),
        Err("leading-cycle seat"),
        "a split whose FINAL SEGMENT is the leading cycle's seat has that cycle repay exactly \
         the cycle the segment loses, so it realizes the declared split scaled. \
         split={leading_last:?}"
    );

    // ── (e) THE ROUND-TRIP: what the proposer is shown back for the split just authored.
    let submission = f4_allocation_submission(&offer, count, &authored);
    let returned = preview_interaction(
        &state,
        offer.proposer,
        &InteractionPreviewRequest {
            request_id: PreviewRequestId("h1-one-chain".to_string()),
            interaction_id: submission.interaction_id.clone(),
            response: submission.response.clone(),
        },
    )
    .shortcut_preview
    .expect("the proposer's own authored declaration previews an element");
    assert_eq!(
        returned
            .allocation
            .iter()
            .map(|assignment| (
                f4_candidate_seat(&offer.offer_candidates, &assignment.choice_id).map(PlayerId),
                assignment.amount
            ))
            .collect::<Vec<_>>(),
        authored
            .iter()
            .map(|(seat, part)| (Some(*seat), *part))
            .collect::<Vec<_>>(),
        "CR 732.2a: the returned element states the allocation the player AUTHORED, not the \
         canonical one it was derived from. canonical={canonical:?}"
    );
    assert_eq!(
        f4_life_entries(&returned),
        authored
            .iter()
            .map(|(seat, part)| (seat.0, -(offer.rate * i64::from(*part)) as i32))
            .collect::<Vec<_>>(),
        "CR 732.2a: the per-victim lines are the RETURNED element's own entries — the \
         predictable result of the described sequence, the published rate {} times EACH seat's \
         own segment",
        offer.rate
    );

    // ── (f) SUBMISSION AND THE RESPOND BEAT.
    assert!(
        !offer.other_pins.is_empty(),
        "reach-guard: the split travels ALONGSIDE a pin for every other non-read-only point, \
         else that clause is vacuous on this board"
    );
    let (life_before, _, _, tokens_before) = commit_axes(&state);
    let responder = f4_open_respond_window(&mut state, &offer, &submission);
    let reply = f4_reply_at(&state, responder);
    let published = reply
        .declared
        .as_ref()
        .expect("CR 732.2b: the responding seat is published the declaration it is judging");
    assert!(
        published.allocation.len() > 1 && !published.entries.is_empty(),
        "reach-guard: an equality between two empty or single-segment elements cannot satisfy \
         the agreement below. allocation={:?} entries={:?}",
        published.allocation,
        published.entries
    );
    let magnitudes: Vec<i32> = f4_life_entries(published)
        .into_iter()
        .map(|(_, amount)| amount)
        .collect();
    assert_eq!(
        magnitudes
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        magnitudes.len(),
        "reach-guard: the compared per-seat magnitudes are PAIRWISE DISTINCT, so a uniform \
         re-attribution cannot satisfy the agreement below. magnitudes={magnitudes:?}"
    );
    assert_eq!(
        shortcut_elements_agree_modulo_ids(
            (&returned, &offer.offer_candidates),
            (published, &reply.candidates),
        ),
        Ok(()),
        "CR 732.2c: the choices taken on acceptance are the ones this seat was SHOWN — the \
         round-tripped element and the published one agree modulo the ids that rotate across \
         the beat"
    );

    // ── (g) THE COMMIT.
    assert!(
        accept_all_opponents(&mut state) > 0,
        "the CR 732.2c window must actually take responses"
    );
    let (life_after, _, _, tokens_after) = commit_axes(&state);
    let losses: Vec<i64> = life_before
        .iter()
        .zip(&life_after)
        .map(|(before, after)| i64::from(before - after))
        .collect();
    for (seat, _) in &authored {
        assert!(
            f4_loss(&state, &losses, *seat) > 0,
            "CR 732.2a: every seat the declaration names takes repetitions. {seat:?} \
             losses={losses:?}"
        );
    }
    assert_eq!(
        authored
            .iter()
            .map(|(seat, _)| f4_loss(&state, &losses, *seat))
            .sum::<i64>(),
        offer.rate * i64::from(count),
        "CR 732.2a: the accepted drive commits EXACTLY {count} repetitions of the published \
         per-cycle charge, and every legal victim is declared. losses={losses:?}"
    );
    assert_eq!(
        f4_loss(&state, &losses, offer.proposer),
        0,
        "the proposer is not a published victim and loses nothing. losses={losses:?}"
    );
    let realized: Vec<i64> = authored
        .iter()
        .map(|(seat, _)| f4_loss(&state, &losses, *seat))
        .collect();
    let declared_scaled: Vec<i64> = authored
        .iter()
        .map(|(_, part)| offer.rate * i64::from(*part))
        .collect();
    assert_ne!(
        realized, declared_scaled,
        "CR 601.2c: the leading cycle resolves a target announced BEFORE the drive begins, so \
         every segment boundary lands one cycle late while the total stays exact — the realized \
         map is not the declared split scaled. realized={realized:?}"
    );

    // ── (h) THE TOKENS, at the very count this row committed.
    assert_eq!(
        (tokens_after - tokens_before) as i64,
        token_rate * i64::from(count),
        "CR 732.2a: the board mints the published per-cycle token rate {token_rate} on each of \
         the {count} committed cycles: {tokens_before} -> {tokens_after} battlefield tokens"
    );
}
