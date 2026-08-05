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
/// correct. `r2b`/`r3`/`r4`/`r5` and the interruptibility pair are still unwritten: no `fn r2b_`,
/// `fn r3_`, `fn r4_` or `fn r5_` row exists in this file.
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
///   `template: None`, and a published pin set fail-closes on that. F4 publishes THREE points
///   (`r1b`), so the gate is closed with room to spare; ONE would already have closed it.
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
/// ⚠ This row deliberately does NOT assert what the accepted declaration then accomplishes —
/// that is [`r2a_an_accepted_declaration_commits_exactly_n_cycles_because_reeds_may_is_announced`]'s
/// job, and it now measures an exact `n`-repetition commit (it measured a zero commit while
/// Reed's `may` was unpublished). Splitting the two keeps this row a DECLARE-time matrix.
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
