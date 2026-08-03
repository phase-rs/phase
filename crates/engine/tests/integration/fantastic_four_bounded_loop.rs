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
//! on_the_real_f4_dump`] is the row). It publishes exactly **one** decision point — Sue's
//! `MayChoice`. Torch's `Targets` point and Reed's `MayChoice` point are **NOT** published, and
//! the mechanism is measured and pinned by
//! [`r1b_the_published_point_set_is_exactly_what_the_retained_window_announces`]: the CR 732.2a
//! ring sampler fires only at `Priority { player == active_player }` after a non-shrinking
//! resolution, so on this board the retained frames alternate strictly between the `404` and
//! `402` stack entries. `certified_period_touch`'s `announced` set is "entries in a frame's
//! stack that were absent from the previous frame's", so the `403` and `401` entries are
//! structurally invisible to conjunct (6) and to `bounded_cycle_pin_slots_for_window`.
//!
//! CONSEQUENCE, also measured and pinned
//! ([`r2_an_accepted_declaration_commits_zero_cycles_because_reeds_may_is_unannounced`]): an
//! accepted `Fixed(n)` declaration carrying the FULL published pin set drives cycle 0, answers
//! Sue's "may" from the pin (U4's arm, on the real dump), and then **aborts** on Reed's
//! unpinned "may" ⇒ whole-cycle rollback, zero commit, manual handback. That is fail-CLOSED and
//! rules-safe, but it is not a grant — so the plan's R2a/R2b/R3/R5 (pass ⇒ grant, respond ⇒
//! no-grant, Sue-Decline rollback, `victim_slot` keyed by Torch) have no non-vacuous form on
//! this tree and are NOT written here. They are handed back with the mechanism above.

use engine::analysis::decision_template::{DecisionKind, DecisionPointKind, IterationCount};
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
/// The dump was captured with the detector OFF; every row here is about the CR 732.2a
/// interactive offer, so the mode is set to `Interactive` at load — the same thing the user's
/// own toggle does.
fn load_f4() -> GameState {
    let json = gunzip(include_bytes!(
        "../fixtures/fantastic_four_bounded_loop_4p.json.gz"
    ));
    let envelope: serde_json::Value =
        serde_json::from_str(&json).expect("dump envelope parses as JSON");
    let mut state = serde_json::from_value::<PersistedGameState>(envelope["gameState"].clone())
        .expect("gameState deserializes through the production decoder")
        .into_game_state();
    state.loop_detection = engine::types::game_state::LoopDetectionMode::Interactive;
    state
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
/// loop — casting here wanders off it. At Torch's CR 608.2b target choice aim **P1** (a
/// CONSTANT seat, so the cycle is board-stable and the detector can certify it); at either
/// CR 603.5 "may" prompt TAKE (declining Sue's token breaks the chain to Reed).
///
/// ⚠ This is deliberately NOT `loop_shortcut.rs`'s shared `dump_drive_one_beat`: that helper's
/// victim preference matches `GameAction::SelectTargets`, and this dump raises
/// `GameAction::ChooseTarget`, so its pin is inert here and its "first legal non-terminal
/// action" fallback answers Sue's "may" with whichever `DecideOptionalEffect` is enumerated
/// first. MEASURED: under that policy this dump reaches no offering beat at all.
fn f4_drive_one_beat(state: &mut GameState) -> Result<(), String> {
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
                    GameAction::ChooseTarget { target: Some(TargetRef::Player(p)) } if *p == P1
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
    for beat in 0..cap {
        if matches!(state.waiting_for, WaitingFor::LoopShortcut { .. }) {
            return Some(beat);
        }
        f4_drive_one_beat(state).ok()?;
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
/// [`u6_no_declaration_the_generator_can_emit_opens_the_window_while_the_accepted_shape_is_one_it_never_builds`]),
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
        DecisionGroupKey, DecisionTemplate, MayChoiceOption, PinnedDecision, ReplayMode, TargetPin,
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
                DecisionPointKind::Targets { .. } => PinnedDecision::Targets {
                    slot: p.slot.clone(),
                    targets: vec![TargetPin::Player(P1)],
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
/// **STATUS: PARTIAL — pending the (A)/(B) ruling.** §6 R1 as planned also expected the offer to
/// publish three decision points and to be TAKEABLE (commit ≥ 1 cycle). Measured on this tree it
/// publishes ONE point and commits ZERO cycles (see `r1b` and `r2` for the pinned measurements,
/// and the module header for the mechanism). This row therefore ships the half of R1 that the
/// measurement supports — the offer fires, and its bound arithmetic is correct — and pins the
/// other half AS MEASURED rather than asserting the falsified prediction. R2a/R2b/R3/R4/R5 and
/// the interruptibility pair stay unwritten until the ruling lands.
///
/// # What the assertion is bound to, and why it is not `f(x) == f(x)`
///
/// The expectation is computed HERE from (i) each living seat's life and library on the
/// offer-beat board and (ii) the per-period delta the ENGINE published on the certificate — it
/// never calls `elimination_bounds`, which is the function under test. Per §6 R1's ROUND-38
/// (F3) ruling the row is anchored to the **in-tree MAX form** (`resource.rs`
/// `observed_life_loss.max(declared_life_magnitude)` under the `declarable_victims` guard);
/// the additive per-victim form is a tracked follow-up (R1-fu), not a prerequisite. Measured on
/// this board `victim_slot` is EMPTY (see `r5`'s handback in the module header), so the two
/// forms coincide here and the row states which one it assumes.
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
    let mut bounds: Vec<i64> = vec![];
    for player in state.players.iter().filter(|p| !p.is_eliminated) {
        let loss = -per_cycle.delta.life.get(&player.id).copied().unwrap_or(0);
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
         published. Re-derived here as {bounds:?} -> {expected}; the offer published {}. \
         (This row assumes the IN-TREE max form; see R1-fu.)",
        schema.max_iterations
    );
    assert!(
        schema.max_iterations < MAX_SHORTCUT_CYCLES_MIRROR,
        "the bound must be NARROWED, else this row is satisfied by the unnarrowed default \
         every pre-bounded offer carries"
    );
}

/// §6 R1, SECOND HALF — **a MEASURED CORRECTION to the plan, pinned so it cannot drift
/// silently. STATUS: PARTIAL — this row pins the CURRENT truth of the published point set, not
/// the planned one, pending the (A)/(B) ruling.**
///
/// R1 as written expects `points ≡ {Targets(403 Torch), MayChoice(401 Reed),
/// MayChoice(402 Sue)}`. That expectation is a HEAD-era SNAPSHOT-mint reading (§2: *"returns 1
/// point when 403 is up"*), and it does not survive U3's WINDOW mint. Measured on this tree:
///
/// * the retained ring frames on this board alternate strictly between the `404` and `402`
///   stack entries — the CR 732.2a sampler fires only at `Priority { player == active_player }`
///   after a non-shrinking resolution, and the `403` / `401` entries only ever sit on the stack
///   across a `TriggerTargetSelection` / `OptionalEffectChoice` window;
/// * `certified_period_touch`'s `announced` set is exactly "entries in a frame's stack absent
///   from the previous frame's", so `403` and `401` are never announced;
/// * therefore `bounded_cycle_pin_slots_for_window` publishes exactly ONE point — Sue's
///   `MayChoice`.
///
/// The row asserts the MEASUREMENT, with the sources named, and the frame census as its own
/// reach-guard. **If a future change widens the announced set this row FAILS LOUDLY and must be
/// re-keyed — which is the point: R2a/R2b/R3/R5 become writable at exactly that moment.**
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
        [thing, sue].into_iter().collect(),
        "MEASURED: every retained sample's stack holds a {THING:?} ({thing:?}) or {SUE:?} \
         ({sue:?}) entry and NEVER a {TORCH:?} ({torch:?}) or {REED:?} ({reed:?}) one, because \
         those two resolve across a prompt window and the sampler only fires at an \
         active-player `Priority` settle. This is the reach-guard for the point-set assertion \
         below"
    );
    assert!(
        !framed_sources.contains(&torch) && !framed_sources.contains(&reed),
        "stated as its own conjunct because it is the load-bearing half: the two sources whose \
         choices go unpublished are exactly the two the sampler never retains"
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
        vec![(sue, "MayChoice")],
        "MEASURED PLAN CORRECTION (§6 R1): the window mint publishes ONE point — Sue's \
         CR 603.5 `may`. Torch's CR 608.2b `Targets` point and Reed's CR 603.5 `may` are NOT \
         published because their stack entries are never ANNOUNCED (see the frame census \
         above). If this assertion fails because the set GREW, the announced-set derivation \
         changed and R2a/R2b/R3/R5 must be written in the same change"
    );
}

/// §6 R2, **as measured** — the consequence of the unannounced choices, driven end to end.
///
/// A `Fixed(n)` declaration carrying the FULL published pin set is ACCEPTED at declare
/// (`predictability_gate` + `validate_pins` both pass — the published set is covered), every
/// living opponent Accepts (CR 732.2c), and then the drive **commits nothing**: cycle 0 answers
/// Sue's `OptionalEffectChoice` from the pin (U4's `inject_pinned_answer` arm, on the real
/// dump), reaches Reed's `OptionalEffectChoice`, finds no pin for it, and returns
/// `CycleOutcome::Abort` ⇒ whole-cycle rollback ⇒ CR 800.4a priority handback.
///
/// This is FAIL-CLOSED and rules-safe; it is also NOT a grant, so §6 R2a's *"exactly N cycles
/// commit"* has no non-vacuous form here and is handed back rather than weakened. The row pins
/// the zero-commit **together with its cause**, so it cannot be read as "the drive works":
///
/// * the same `n` is run at 1 and at 3 and BOTH commit zero (a partial commit would separate
///   them, which is the discriminator `bounded_fixed_count_commits_exactly_n_periods` uses);
/// * the declaration is asserted to have been ACCEPTED (`RespondToShortcut` raised), so the
///   zero is the DRIVE's and not a declare-time refusal — that distinction is the whole row;
/// * Reed's "may" is asserted UNPUBLISHED on the same offer, naming the cause.
#[test]
fn r2_an_accepted_declaration_commits_zero_cycles_because_reeds_may_is_unannounced() {
    use engine::analysis::loop_check::ShortcutResponse;

    let mut committed_per_n = vec![];
    for n in [1u32, 3] {
        let mut state = load_f4();
        let reed = resolve_by_name(&state, REED);
        drive_f4_to_offer(&mut state, 400).expect("the bounded offer fires (see R1)");
        let (proposer, _certificate, schema) = offer_parts(&state);
        let schema = schema.clone();

        assert!(
            !schema.points.iter().any(|p| matches!(&p.slot.source,
                    engine::types::game_state::YieldTarget::ThisObject { source_id, .. }
                        if *source_id == reed)),
            "the CAUSE this row is about: Reed's CR 603.5 `may` is NOT among the published \
             points, so no legal declaration can pin it"
        );

        let template = f4_pin_template(&schema, proposer, n);

        let life_before: Vec<i64> = state.players.iter().map(|p| p.life as i64).collect();
        let libs_before: Vec<usize> = state.players.iter().map(|p| p.library.len()).collect();

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
        assert_eq!(
            (&life_after, &libs_after),
            (&life_before, &libs_before),
            "n={n}: MEASURED — the accepted shortcut commits NOTHING. Cycle 0 answers Sue's \
             `may` from the pin and then aborts on Reed's UNPINNED `may`, and the whole cycle \
             is rolled back (CR 732.2a: an unpinned per-iteration choice is not a describable \
             predictable sequence). If this ever fails because a delta APPEARED, the announced \
             set widened and §6 R2a/R2b/R3/R5 must be written in the same change"
        );
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "n={n}: CR 800.4a — the aborted drive hands back to ordinary priority, got {:?}",
            state.waiting_for
        );
        committed_per_n.push((life_after, libs_after));
    }
    assert_eq!(
        committed_per_n[0], committed_per_n[1],
        "n=1 and n=3 must be INDISTINGUISHABLE: a partial commit would separate them, and a \
         partial commit is the one outcome CR 732.2a forbids outright"
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
/// On the offer-beat board, ONE CR 614.1a replacement definition that the resolver's OWN
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
/// * **(a)** one OPTIONAL `AddCounter` definition ⇒ `UnspecifiedChoiceWindow` (CR 614.1a: an
///   optional replacement is a genuine resolution-time choice ⇒ the period is not choice-free).
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
/// CR 614.1a discharge conjunct refuses instead. Defence in depth is the property; a row that
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
    // CR 614.1a scopes a definition to its controller's events, and The Thing is P0's.
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
        "(a) CR 614.1a + CR 732.2a: an OPTIONAL replacement candidate applicable to an \
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

    // ── (b) the def-NAME discriminator: six optional definitions the resolver never draws ──
    for event in [
        ReplacementEvent::ChangeZone,
        ReplacementEvent::Moved,
        ReplacementEvent::CreateToken,
        ReplacementEvent::Draw,
        ReplacementEvent::DamageDone,
        ReplacementEvent::RemoveCounter,
    ] {
        let (b_out, b_meter) = outcome(&with_def(event.clone(), true));
        assert!(
            b_out.is_ok(),
            "(b) {event:?}: this board's announced resolutions never PROPOSE this event, so \
             an event-derived obligation must ignore the definition entirely and the offer \
             must stand. A scan over `def.event` NAMES — round 2's design — would refuse here \
             exactly as it refuses in (a), which is what makes this arm the discriminator. \
             (`ChangeZone`/`CreateToken` are §6 R9's own stated keying; see this row's doc for \
             why `Effect::Token` derives no token-entry event.) got {b_out:?}, meter {b_meter:?}"
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
// three F4 slots (or declines)". F4 publishes ONE point, not three (see `r1b`), and the
// measured answer to the underlying question is the second branch: the AI DECLINES, because
// the only declaration it can emit is one the engine refuses outright. These rows pin that,
// name the two independent reasons, and pin the accepted shape the generator never emits —
// they do not assert the planned prediction.
// ─────────────────────────────────────────────────────────────────────────────────────────

/// §5 U6 (i) — MEASURED: at the real F4 bounded offer the engine's AI candidate generator
/// emits exactly ONE action, `DeclineShortcut`. It offers no declaration at all.
///
/// Both declare candidates are excluded, each by a different conjunct, and this board trips
/// both at once:
///
/// * `UntilLethal` is gated on `!schema.is_bounded()`. CR 732.2a: a count-free declaration
///   names no legal repetition number against an offer that narrowed the bound, and
///   `handle_declare_shortcut` refuses it — measured in
///   [`u6_no_declaration_the_generator_can_emit_opens_the_window_while_the_accepted_shape_is_one_it_never_builds`].
/// * `Fixed(max_iterations)` is gated on `schema.points.is_empty()` — it carries
///   `template: None`, and a published pin set fail-closes on that. F4 publishes one point.
///
/// So the AI declines because it has nothing else it can legally say, not because it emitted a
/// declaration the engine then accepted-and-discarded.
///
/// # Reach-guards (each excludes a way this could pass degenerately)
///
/// * the offer is BOUNDED (`is_bounded()`, bound narrowed below the ceiling) — that is the
///   `UntilLethal` gate's conjunct; on an unbounded offer that candidate would be PRESENT, so
///   without this guard the row could pass on a board where it was never at issue;
/// * `schema.points` is NON-empty — that is the `Fixed` gate's conjunct, and symmetrically the
///   row would otherwise pass on a board where `Fixed` was never at issue;
/// * `predicted_winner` is `None`. Recorded as a measured property of this board, NOT as
///   reachability for `phase_ai::policies::loop_shortcut::LoopShortcutPolicy`'s
///   `(None, UntilLethal) => reject` arm: since the bounded gate landed, this generator can no
///   longer put that pair in front of the policy from a bounded offer, and
///   `declare_until_lethal_with_no_predicted_winner_is_rejected` covers the arm directly.
///
/// REVERT-PROBE — one per excluded candidate, because a single probe would leave the OTHER
/// exclusion holding the assertion up and report a false pass:
///
/// * drop `!schema.is_bounded()` from the `UntilLethal` push in `ai_support/candidates.rs`
///   ⇒ `DeclareShortcut { UntilLethal, None }` reappears ⇒ this row FLIPS on the equality;
/// * drop `schema.points.is_empty() &&` from the `Fixed` push ⇒ `Fixed(max_iterations)`
///   appears ⇒ this row FLIPS on the equality.
#[test]
fn u6_the_ai_candidate_set_at_the_f4_offer_is_decline_only() {
    let mut state = load_f4();
    drive_f4_to_offer(&mut state, 400).expect("the bounded offer fires (see R1)");
    let (proposer, _certificate, schema) = offer_parts(&state);
    let WaitingFor::LoopShortcut {
        predicted_winner, ..
    } = &state.waiting_for
    else {
        unreachable!("offer_parts would have panicked")
    };

    assert!(
        schema.is_bounded() && schema.max_iterations < MAX_SHORTCUT_CYCLES_MIRROR,
        "reach-guard: the generator's `Fixed` candidate is gated on `is_bounded()` too, so an \
         unbounded offer would exclude it for the wrong reason. bounded={} max_it={}",
        schema.is_bounded(),
        schema.max_iterations
    );
    assert!(
        !schema.points.is_empty(),
        "reach-guard: a NON-empty published pin set is the conjunct this row is about"
    );
    assert_eq!(
        *predicted_winner, None,
        "reach-guard + REACHABILITY for `phase_ai::policies::loop_shortcut`: the F4 offer \
         latches NO predicted winner, which is what routes its `(None, UntilLethal)` reject arm"
    );

    // ── the seam: `phase-ai/src/search.rs` `WaitingFor::LoopShortcut { .. } =>` calls this ──
    let actions = engine::ai_support::legal_actions(&state);
    assert_eq!(
        actions,
        vec![GameAction::DeclineShortcut],
        "MEASURED: exactly one candidate. No `UntilLethal` declaration (gated on \
         `!schema.is_bounded()`, and this offer narrowed its bound to {}), no `Fixed` \
         declaration (gated on `schema.points.is_empty()`, and this schema publishes {} \
         point(s)), and no declaration carrying a template at all — so the AI cannot pin the \
         point the offer DID publish",
        schema.max_iterations,
        schema.points.len()
    );

    // Stated separately from the equality above so a future generator change that adds an
    // unrelated candidate reports the interesting fact rather than a diff of two long vectors.
    assert!(
        !actions.iter().any(|a| matches!(
            a,
            GameAction::DeclareShortcut {
                count: IterationCount::Fixed(_),
                ..
            }
        )),
        "no `Fixed` candidate is generated against a points-carrying offer"
    );
    assert!(
        !actions.iter().any(|a| matches!(
            a,
            GameAction::DeclareShortcut {
                template: Some(_),
                ..
            }
        )),
        "the generator never builds a pin template — that is the capability §5 U6 asks about"
    );
    assert_eq!(
        proposer, P0,
        "every candidate is the proposer's own action (`ActionMetadata.actor`)"
    );
}

/// §5 U6 (ii) — the branch that fires, and the MEASURED reason it fires.
///
/// Every action the AI can take at this offer hands priority straight back; the accepted shape
/// is one the generator never emits. Four declarations are driven through `apply()` on the SAME
/// real offer board, differing one axis at a time:
///
/// | declaration | measured |
/// |---|---|
/// | `UntilLethal` + `None` — **the shape the generator emitted before the bounded gate** | REFUSED ⇒ `Priority` |
/// | `UntilLethal` + a conformant template | REFUSED ⇒ `Priority` (so the refusal is keyed on the COUNT, not on the pins) |
/// | `Fixed(max)` + `None` | REFUSED ⇒ `Priority` (`template: None` against a non-empty schema fail-closes when `last_loop_action_sequence` is empty — measured empty here) |
/// | `Fixed(max)` + a conformant template | **ACCEPTED** ⇒ the CR 732.2b APNAP window opens |
///
/// The last row is the ANTI-VACUITY control: without it, "everything reaches `Priority`" would
/// be satisfied by a board that refuses every declaration for some unrelated reason. With it,
/// the three refusals are proved to be refusals of *those* declarations.
///
/// ⚠ This row deliberately does NOT assert that the accepted declaration accomplishes
/// anything — measured, it commits zero cycles ([`r2_an_accepted_declaration_commits_zero_cycles_because_reeds_may_is_unannounced`]).
/// Closing the generator gap would therefore ride the grant mechanism, which is why U6 reports
/// the gap rather than building the candidate.
///
/// The `UntilLethal` rows are what justifies the generator's `!schema.is_bounded()` gate
/// ([`u6_the_ai_candidate_set_at_the_f4_offer_is_decline_only`]): the engine refuses that count
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
fn u6_no_declaration_the_generator_can_emit_opens_the_window_while_the_accepted_shape_is_one_it_never_builds(
) {
    let mut state = load_f4();
    drive_f4_to_offer(&mut state, 400).expect("the bounded offer fires (see R1)");
    let (proposer, _certificate, schema) = offer_parts(&state);
    let schema = schema.clone();
    let max = schema.max_iterations;

    assert!(
        state.last_loop_action_sequence.is_empty(),
        "the measured precondition for the `Fixed` + `None` arm below: with a NON-empty \
         sequence a template-free declaration is legitimately re-derivable and that arm would \
         be measuring something else. len={}",
        state.last_loop_action_sequence.len()
    );

    // Every AI candidate, driven through the public boundary. Since the bounded gate landed this
    // set is `[DeclineShortcut]` alone, so on its own the loop is a WEAK statement — it is the
    // four one-axis drives below that carry this row. Kept because it is the only assertion here
    // that re-derives the candidate set from the generator rather than naming shapes by hand: a
    // future generator change that reintroduces a declaration at this node has to survive it.
    let candidates = engine::ai_support::legal_actions(&state);
    assert!(
        !candidates.is_empty(),
        "positive control: an EMPTY candidate set would satisfy the loop below vacuously"
    );
    for action in candidates {
        let mut probe = state.clone();
        apply(&mut probe, proposer, action.clone()).expect("dispatched — refusal is a HANDBACK");
        assert!(
            matches!(probe.waiting_for, WaitingFor::Priority { .. }),
            "CR 800.4a: the AI candidate {action:?} hands priority back. A \
             `RespondToShortcut` here would mean the AI CAN open the CR 732.2b window, which \
             is the capability this row measures absent. got {:?}",
            probe.waiting_for
        );
    }

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
        "Priority",
        "and 'just emit `Fixed`' is not a template-free remedy: a `template: None` declaration \
         against a non-empty schema fail-closes when `last_loop_action_sequence` is empty"
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
         CR 800.4a manual handback. `handle_declare_shortcut` pushes no events on either path, \
         so the event counts are exact rather than wildcards"
    );
}
