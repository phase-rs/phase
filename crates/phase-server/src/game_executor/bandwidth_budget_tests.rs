//! Per-seat bandwidth budget for `StateUpdate` frames.
//!
//! A deterministic four-seat scripted game (no AI) is driven through the real
//! engine pipeline, one production submission per loop iteration. After every
//! submission each seat's `StateUpdate` is built exactly as the shared server
//! builds it (`build_state_update_message`, which runs
//! `server_core::filter_state_for_player` and the derived/interaction views),
//! wrapped in the `Envelope` the socket writer serializes, and passed through
//! the gzip wire envelope (`wire::encode_json_message_sync`). The exact same
//! serialized messages also feed forced-FULL and selected FULL/DELTA codec lanes,
//! each measured with that production gzip/raw-prefix encoder. Separate sender
//! and receiver baselines per seat advance only after delivery and exact byte
//! reconstruction of the complete original message.
//!
//! What the budget covers: every byte of the per-recipient `StateUpdate` --
//! `GameState`/`GameObject`/`ServerMessage` field growth (including definitions
//! and `base_*` copies), per-viewer filtering and redaction, derived views,
//! legal-action and interaction payloads -- and the scripted engine-transition
//! count times seats. What it does not cover: how often production calls
//! `StateUpdatePublisher::publish`. This test publishes through its own seat
//! loop that mirrors `publish`, so a duplicated or suppressed production send
//! is not caught here.
//!
//! This is a regression budget, not a model of real games. Deliberate
//! differences from production: `rewind_targets` is empty (the shared server
//! sends none), `command_id` is `None`, only seat `StateUpdate`s are measured
//! (no `GameStarted`, no spectator frames), and no deck pools are attached.
//!
//! The pinned codec lanes below remain a test-only model, distinct from the
//! negotiated production encoder measured by the opt-in resource entry.
//! Initial FULL frames count in both codec lanes. Every selected serialized frame
//! must be no larger than forced FULL; every seat must reach DELTA and save wire
//! bytes. A separate completion-envelope regression checks top-level `command_id`
//! removal through DELTA at an unchanged revision; it is outside this scenario.
//!
//! Tolerances are two-sided: `json_bytes` must stay within
//! `tolerance_pct.json_bytes` and `wire_bytes` within `tolerance_pct.wire_bytes`
//! of the budget in either direction, so a large reduction also forces a
//! re-baseline and the budget cannot silently go stale. `updates` is exact.
//!
//! # Updating the budget
//!
//! ```text
//! UPDATE_BANDWIDTH_BUDGET=1 cargo test -p phase-server --bin phase-server \
//!     bandwidth_budget -- --nocapture
//! ```
//!
//! validates the legacy `seats` lane against its existing tolerances first, then
//! rewrites only measured `codec_seats`, preserving schema, scenario, tolerances
//! and legacy pins. Re-run without the variable and with `--locked` to confirm.
//! Policy: a commit updating the budget explains why the size moved, links the
//! responsible field or PR, and includes a measured before/after table for all
//! seats (legacy actual/pin, forced FULL and selected raw/wire totals and ratios).
//! An unexplained increase is a regression, not a reason to re-baseline. Changes
//! to cards, script or seats require a new scenario; do not adjust tolerances to
//! hide a regression. Legacy rebaselines require a separate explained decision.
//!
//! # Schema 2 measurement (#89)
//!
//! Captured on base `b59aeaac1` plus this test-only change, macOS arm64, Cargo
//! test profile (unoptimized + debuginfo), using the update command above.
//! Scenario and 187 publications per seat are unchanged. Each cell below is
//! raw JSON bytes / production gzip-or-raw-prefix wire bytes.
//!
//! | Seat | Legacy pin (unchanged) | Legacy actual | Forced FULL codec | Selected codec | Selected/legacy actual wire |
//! | --- | ---: | ---: | ---: | ---: | ---: |
//! | 0 | 13991377 / 1152328 | 14065400 / 1162680 | 14074268 / 1165899 | 827647 / 244075 | 20.9924% |
//! | 1 | 14149300 / 1175503 | 14227607 / 1184971 | 14236475 / 1188181 | 833127 / 246996 | 20.8441% |
//! | 2 | 14072275 / 1159534 | 14148644 / 1169176 | 14157512 / 1172410 | 842634 / 247130 | 21.1371% |
//! | 3 | 14206232 / 1166140 | 14282550 / 1175318 | 14291418 / 1178566 | 870872 / 250230 | 21.2904% |
//! | Total | 56419184 / 4653505 | 56724201 / 4692145 | 56759673 / 4705056 | 3374280 / 988431 | 21.0657% |
//!
//! Pre-existing legacy drift was admitted explicitly, not refreshed: the
//! unchanged pre-edit run had the same raw totals and wire totals of 1162672,
//! 1184970, 1169191 and 1175304. Those are +0.53--0.55% raw and +0.79--0.90%
//! wire against the old pins, within the unchanged 1%/2% tolerances. Subsequent
//! runs vary slightly; pin only actual measured codec output, never a target.
//!
//! Savings target REFER: 988431 wire bytes is 21.0657% of legacy actual
//! (21.2406% of legacy pinned), materially above the intended 5--6%. Raw codec
//! bytes are 5.9486% of legacy actual; that is not the wire acceptance metric.
//! Every seat emitted one initial FULL and 186 DELTAs, with no later FULL
//! fallback. A subsequent locked run's exact raw-span profile across all seats
//! was: initial FULL 269912, DELTA metadata (including hashes) 169520, set values
//! 2387376, remove values 11618, object upserts 179468, order vectors 310152 and
//! patch syntax 46008 bytes. Whole-field set payloads dominate; order and hashes
//! also contribute. Compression is per frame, not across the stream. These
//! are byte measurements, not optimized CPU benchmarks. The harness reports
//! the profile on every run; no tolerances, script, protocol or codec semantics
//! were changed to force the savings target.
//!
//! # Manual release check
//!
//! This test pins per-update size on one small scripted round. The end-to-end
//! release metric is measured outside this repository by an external stress
//! harness: `StateUpdate` frames and wire bytes per seat-turn on paired fixed
//! games (same seeds, gzip envelope on), baseline versus candidate. Reference
//! values measured at upstream 6d285188f (after #10066): about 42
//! `StateUpdate`s and about 1.7 MB wire per seat-turn in four-player Commander
//! with the gzip envelope on.
//!
//! To compute the figure from that harness's per-stream recordings: each row is
//! one seat-stream and records its `max_turn` and, per message type, the
//! `[frames, wire_bytes, json_bytes]` triple (take the `StateUpdate` entry).
//! Sum `StateUpdate` frames and `StateUpdate` wire bytes over all seat streams,
//! and divide each by the sum of `max_turn` over the same streams. A report
//! that prints "per turn (all seats)" shows that per-seat-turn figure
//! multiplied by the seats per game, so compare like with like.
//!
//! The budget's measuring copies mirror the submission tuple and per-seat
//! projection; the new resource entry instead calls the real ConnectionEncoder,
//! real PendingWrite/write_socket acknowledgement, and real commit. Its script
//! visitor preserves the budget's sequence, assertions, pins and tolerances.
//!
//! # Production resource measurements
//!
//! Explicit optimized command (three repetitions; default suites skip it):
//! `cargo test --locked --profile server-release -p phase-server --bin phase-server
//! production_encoder_resource_acceptance -- --ignored --nocapture --test-threads=1`.
//! `production_encoder_resource_smoke` selects one repetition with the same lanes.
//! Four seats and two independently owned spectator connections replay the
//! existing richer round. Setup first completes a real mixed four-seat game via
//! network_tests; its final projections supply actual post-game idle content.
//!
//! One optimized smoke on Linux x86_64, Intel Core i5-12400T, rustc
//! 1.97.0-nightly (0febdbab2 2026-04-18), 187 publications/recipient, measured:
//! These are single-run smoke observations, distinct from the three-run acceptance
//! report, and are not deployment predictions, tolerances or timing/savings targets.
//!
//! | Lane | Encoding | Six-owner payload bytes | Full / Delta | Mean prepare Full / Delta (us) |
//! | --- | --- | ---: | ---: | ---: |
//! | Legacy | plain | 84794997 | 0 / 0 | n/a (StateUpdate 122.559) |
//! | Legacy | gzip | 6982078 | 0 / 0 | n/a (StateUpdate 408.854) |
//! | Continuous | plain | 4773479 | 6 / 1116 | 171.050 / 1370.203 |
//! | Continuous | gzip | 1406359 | 6 / 1116 | 456.839 / 1432.089 |
//! | Reconnect-heavy | plain | 58171825 | 750 / 372 | 191.688 / 1862.068 |
//! | Reconnect-heavy | gzip | 5137539 | 750 / 372 | 487.730 / 1925.576 |
//!
//! The output reports count/total/mean/median/p95/max for synchronous prepare
//! and commit, first/subsequent and actual frame kind, per connection and
//! seats/spectators/game; enqueue-to-successful-writer-ack wait is separate.
//! CPU-work service elapsed can include OS descheduling: it is NOT OS thread CPU
//! or server utilization. Projection, fixture generation, independent reference
//! serialization/decode and memory observation are outside the CPU timers.
//! A collecting Sink's queue/flush wait is harness cost, not real-network RTT.
//!
//! Separate accounting owners initialize the estimator's lazy caches. Reported
//! JSON/index-key/object-hash storage is fully indexed logical storage and can
//! overstate cache allocation on early-Full paths, not passive heap observation.
//! In the smoke, six-owner simultaneous scripted retained storage peaked at
//! 673648 bytes; precommit old+candidate+final-payload subtotals peaked at
//! 760367..802477 bytes across codec lanes. Terminal idle retained components
//! were [461072, 20737, 8192] bytes for three seats, [462255, 20737, 8192]
//! for the winning seat, and [461074, 20737, 8192] for each spectator.
//! Non-state traffic/yield left all six unchanged (2941193 bytes in total).
//! Legacy retained no codec baseline. All measured owners dropped; a fresh owner
//! had no baseline and emitted independently decodable Full.
//!
//! These subtotals are NOT a heap-peak upper bound or RSS/deallocation claim.
//! Excluded: allocator headers/fragmentation, Arc/owner/key/viewer small structs,
//! capacity beyond measured JSON/payload lengths, temporary Envelope JSON,
//! codec diff/assemble/Frame/serde/compression workspace and framing overlap,
//! server game/session/projection storage, queued Envelopes/socket buffers,
//! Tokio tasks/stacks, receiver/reference/report storage, OS/network buffers
//! and process RSS. Unobserved preparation transients are excluded, not zero.

use std::collections::VecDeque;

use axum::extract::ws::Message;
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use engine::types::InteractionSessionId;
use serde::{Deserialize, Serialize};
use server_core::session::ActionResult;
use server_core::FullSessionKey;
use state_codec::{decode, encode, state_hash, Frame, FrameKind, WireState};

use super::StateUpdateSource;
use crate::socket_mailbox::Envelope;

const UPDATE_ENV_VAR: &str = "UPDATE_BANDWIDTH_BUDGET";
const UPDATE_COMMAND: &str = "UPDATE_BANDWIDTH_BUDGET=1 cargo test -p phase-server --bin phase-server bandwidth_budget -- --nocapture";
const BUDGET_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/game_executor/bandwidth_budget.json"
);
const BUDGET_JSON: &str = include_str!("bandwidth_budget.json");

const BUDGET_SEED: u64 = 42;
const SEATS: usize = 4;
/// The scripted game starts on turn 2 (`at_phase`) and ends when the turn
/// counter first reaches 6: one full round of four turns.
const END_TURN: u32 = 6;
const ITERATION_CAP: usize = 2000;

// Oracle texts, verbatim. Grizzly Bears is a vanilla 2/2 and has none.
const LLANOWAR_ELVES_ORACLE: &str = "{T}: Add {G}.";
const SERRA_ANGEL_ORACLE: &str =
    "Flying\nVigilance (Attacking doesn't cause this creature to tap.)";
const ELVISH_VISIONARY_ORACLE: &str = "When this creature enters, draw a card.";
const LIGHTNING_BOLT_ORACLE: &str = "Lightning Bolt deals 3 damage to any target.";

const LIBRARY_CARDS: [&str; 8] = [
    "Library Card 1",
    "Library Card 2",
    "Library Card 3",
    "Library Card 4",
    "Library Card 5",
    "Library Card 6",
    "Library Card 7",
    "Library Card 8",
];

const fn seat(index: usize) -> PlayerId {
    PlayerId(index as u8)
}

fn mana_cost(generic: u32, shards: &[ManaCostShard]) -> ManaCost {
    ManaCost::Cost {
        shards: shards.to_vec(),
        generic,
    }
}

// ---------------------------------------------------------------------------
// Budget file
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Budget {
    schema: u32,
    scenario: String,
    update_procedure: String,
    tolerance_pct: Tolerances,
    seats: Vec<SeatBudget>,
    codec_seats: Vec<SeatBudget>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Tolerances {
    json_bytes: f64,
    wire_bytes: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SeatBudget {
    seat: usize,
    updates: u64,
    json_bytes: u64,
    wire_bytes: u64,
}

impl SeatBudget {
    fn new(seat: usize) -> Self {
        Self {
            seat,
            updates: 0,
            json_bytes: 0,
            wire_bytes: 0,
        }
    }

    fn record(&mut self, json: String) {
        self.updates += 1;
        self.json_bytes += json.len() as u64;
        self.wire_bytes += match crate::wire::encode_json_message_sync(json, true)
            .expect("wire envelope encodes")
        {
            Message::Binary(bytes) => bytes.len() as u64,
            other => panic!("expected a binary wire envelope, got {other:?}"),
        };
    }
}

fn load_budget() -> Budget {
    serde_json::from_str(BUDGET_JSON).unwrap_or_else(|error| {
        panic!(
            "bandwidth_budget.json is malformed ({error}); fix it by hand: the update command below keeps schema, scenario and tolerances from this file, so it needs a parseable file:\n  {UPDATE_COMMAND}"
        )
    })
}

fn delta_pct(actual: u64, budget: u64) -> f64 {
    if budget == 0 {
        return if actual == 0 { 0.0 } else { f64::INFINITY };
    }
    (actual as f64 - budget as f64) / budget as f64 * 100.0
}

fn measured_table(measured: &[SeatBudget], expected: &[SeatBudget]) -> String {
    let mut table = String::from(
        "seat | updates (actual/budget) | json_bytes (actual/budget, delta%) | wire_bytes (actual/budget, delta%)\n",
    );
    for actual in measured {
        let expected = expected.iter().find(|s| s.seat == actual.seat);
        let (updates, json, wire) = match expected {
            Some(b) => (
                b.updates.to_string(),
                format!(
                    "{} (delta {:+.2}%)",
                    b.json_bytes,
                    delta_pct(actual.json_bytes, b.json_bytes)
                ),
                format!(
                    "{} (delta {:+.2}%)",
                    b.wire_bytes,
                    delta_pct(actual.wire_bytes, b.wire_bytes)
                ),
            ),
            None => ("-".into(), "-".into(), "-".into()),
        };
        table.push_str(&format!(
            "{:>4} | {}/{} | {}/{} | {}/{}\n",
            actual.seat, actual.updates, updates, actual.json_bytes, json, actual.wire_bytes, wire,
        ));
    }
    table
}

fn rewrite_budget(measured: &[SeatBudget], existing: Budget) {
    let rewritten = Budget {
        codec_seats: measured.to_vec(),
        ..existing
    };
    let mut text = serde_json::to_string_pretty(&rewritten).expect("budget serializes");
    text.push('\n');
    std::fs::write(BUDGET_PATH, text).expect("write bandwidth_budget.json");
    eprintln!("codec budget rewritten at {BUDGET_PATH}; legacy pins retained; re-run without {UPDATE_ENV_VAR}");
}

fn assert_within_budget(
    measured: &[SeatBudget],
    expected: &[SeatBudget],
    budget: &Budget,
    lane: &str,
) {
    let mut failures = Vec::new();
    if budget.schema != 2 {
        failures.push(format!("unsupported budget schema {}", budget.schema));
    }
    if expected.len() != measured.len() {
        failures.push(format!(
            "budget has {} seats, scenario has {}",
            expected.len(),
            measured.len()
        ));
    }
    for actual in measured {
        let Some(expected) = expected.iter().find(|s| s.seat == actual.seat) else {
            failures.push(format!("seat {} has no budget entry", actual.seat));
            continue;
        };
        if actual.updates != expected.updates {
            failures.push(format!(
                "seat {}: updates {} != budget {}",
                actual.seat, actual.updates, expected.updates
            ));
        }
        let json_delta = delta_pct(actual.json_bytes, expected.json_bytes);
        if json_delta.abs() > budget.tolerance_pct.json_bytes {
            failures.push(format!(
                "seat {}: json_bytes {} vs budget {} ({:+.2}%, tolerance +/-{}%)",
                actual.seat,
                actual.json_bytes,
                expected.json_bytes,
                json_delta,
                budget.tolerance_pct.json_bytes
            ));
        }
        let wire_delta = delta_pct(actual.wire_bytes, expected.wire_bytes);
        if wire_delta.abs() > budget.tolerance_pct.wire_bytes {
            failures.push(format!(
                "seat {}: wire_bytes {} vs budget {} ({:+.2}%, tolerance +/-{}%)",
                actual.seat,
                actual.wire_bytes,
                expected.wire_bytes,
                wire_delta,
                budget.tolerance_pct.wire_bytes
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{lane} StateUpdate bandwidth is outside the checked-in budget (scenario {}):\n{}\n{}\n\
         If the size change is intended and explained, re-baseline and say why in the commit:\n  {UPDATE_COMMAND}",
        budget.scenario,
        failures.join("\n"),
        measured_table(measured, expected),
    );
}

// ---------------------------------------------------------------------------
// Scripted game
// ---------------------------------------------------------------------------

/// A scripted main-phase intent, consumed when its seat holds priority in its
/// own precombat main phase with an empty stack.
#[derive(Debug, Clone, Copy)]
enum Intent {
    PlayLand {
        object_id: ObjectId,
        card_id: CardId,
    },
    CastSpell {
        object_id: ObjectId,
        card_id: CardId,
    },
}

struct ScriptedGame {
    runner: GameRunner,
    /// One pre-existing attacker per seat; attacks the next seat each turn.
    attackers: [ObjectId; SEATS],
    scripts: [VecDeque<Intent>; SEATS],
    /// Targets chosen in order for the target prompts the script produces.
    targets: VecDeque<TargetRef>,
    hand_lands: [ObjectId; SEATS],
    /// Hand spell each seat casts (Bolt, Elvish Visionary, Grizzly Bears, Llanowar Elves).
    hand_spells: [ObjectId; SEATS],
    /// The Llanowar Elves that seat 0's Lightning Bolt kills.
    bolt_victim: ObjectId,
}

fn build_game() -> ScriptedGame {
    let mut scenario = GameScenario::new_n_player(SEATS as u8, BUDGET_SEED);
    scenario.at_phase(Phase::PreCombatMain);

    let bears_cost = mana_cost(1, &[ManaCostShard::Green]);
    let elves_cost = mana_cost(0, &[ManaCostShard::Green]);
    let angel_cost = mana_cost(3, &[ManaCostShard::White, ManaCostShard::White]);
    let visionary_cost = mana_cost(1, &[ManaCostShard::Green]);
    let bolt_cost = mana_cost(0, &[ManaCostShard::Red]);

    // Battlefield lands (the hand land has no mana ability, so every spell is
    // paid from these).
    for _ in 0..3 {
        scenario.add_basic_land(seat(0), ManaColor::Red);
    }
    scenario.add_basic_land(seat(1), ManaColor::Green);
    scenario.add_basic_land(seat(1), ManaColor::Green);
    scenario.add_basic_land(seat(1), ManaColor::White);
    for index in [2, 3] {
        for _ in 0..3 {
            scenario.add_basic_land(seat(index), ManaColor::Green);
        }
    }

    // Pre-existing attackers.
    let attack0 = scenario
        .add_creature(seat(0), "Grizzly Bears", 2, 2)
        .with_mana_cost(bears_cost.clone())
        .id();
    let attack1 = scenario
        .add_creature(seat(1), "Serra Angel", 4, 4)
        .from_oracle_text_with_keywords(&["Flying", "Vigilance"], SERRA_ANGEL_ORACLE)
        .with_mana_cost(angel_cost)
        .id();
    let attack2 = scenario
        .add_creature(seat(2), "Grizzly Bears", 2, 2)
        .with_mana_cost(bears_cost.clone())
        .id();
    let attack3 = scenario
        .add_creature(seat(3), "Grizzly Bears", 2, 2)
        .with_mana_cost(bears_cost.clone())
        .id();
    // The mana creature on seat 1 that seat 0's Lightning Bolt kills.
    let bolt_victim = scenario
        .add_creature_from_oracle(seat(1), "Llanowar Elves", 1, 1, LLANOWAR_ELVES_ORACLE)
        .with_mana_cost(elves_cost.clone())
        .id();

    // Hand: one land and one spell per seat.
    let hand_lands = [
        scenario.add_land_to_hand(seat(0), "Mountain").id(),
        scenario.add_land_to_hand(seat(1), "Forest").id(),
        scenario.add_land_to_hand(seat(2), "Forest").id(),
        scenario.add_land_to_hand(seat(3), "Forest").id(),
    ];
    let hand_spells = [
        scenario
            .add_spell_to_hand_from_oracle(seat(0), "Lightning Bolt", true, LIGHTNING_BOLT_ORACLE)
            .with_mana_cost(bolt_cost)
            .id(),
        scenario
            .add_creature_to_hand_from_oracle(
                seat(1),
                "Elvish Visionary",
                1,
                1,
                ELVISH_VISIONARY_ORACLE,
            )
            .with_mana_cost(visionary_cost)
            .id(),
        scenario
            .add_creature_to_hand(seat(2), "Grizzly Bears", 2, 2)
            .with_mana_cost(bears_cost)
            .id(),
        scenario
            .add_creature_to_hand_from_oracle(
                seat(3),
                "Llanowar Elves",
                1,
                1,
                LLANOWAR_ELVES_ORACLE,
            )
            .with_mana_cost(elves_cost)
            .id(),
    ];

    for index in 0..SEATS {
        scenario.with_library_top(seat(index), &LIBRARY_CARDS);
    }

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.log_player_names = (0..SEATS).map(|index| format!("Seat {index}")).collect();
        // Without a bound authority `derive_viewer_interaction` short-circuits
        // to `AuthorityUnbound` and the frames are unrealistically small
        // (server_core::session::bind_interaction_session does this for real games).
        engine::game::interaction::bind_interaction_authority(
            state,
            InteractionSessionId("BUDGET".into()),
        )
        .expect("bind interaction authority");
    }

    let state = runner.state();
    let scripts = std::array::from_fn(|index| {
        let land = Intent::PlayLand {
            object_id: hand_lands[index],
            card_id: state.objects[&hand_lands[index]].card_id,
        };
        let spell = Intent::CastSpell {
            object_id: hand_spells[index],
            card_id: state.objects[&hand_spells[index]].card_id,
        };
        VecDeque::from([land, spell])
    });

    ScriptedGame {
        runner,
        attackers: [attack0, attack1, attack2, attack3],
        scripts,
        targets: VecDeque::from([TargetRef::Object(bolt_victim)]),
        hand_lands,
        hand_spells,
        bolt_victim,
    }
}

impl ScriptedGame {
    /// Choose the next production submission from the current prompt. Any
    /// prompt the script does not cover fails loudly rather than defaulting.
    fn next_action(&mut self) -> GameAction {
        let state = self.runner.state();
        match &state.waiting_for {
            WaitingFor::Priority { player } => {
                let seat_index = usize::from(player.0);
                let own_main = state.active_player == *player
                    && state.phase == Phase::PreCombatMain
                    && state.stack.is_empty();
                let intent = if own_main {
                    self.scripts[seat_index].pop_front()
                } else {
                    None
                };
                match intent {
                    Some(Intent::PlayLand { object_id, card_id }) => {
                        GameAction::PlayLand { object_id, card_id }
                    }
                    Some(Intent::CastSpell { object_id, card_id }) => GameAction::CastSpell {
                        object_id,
                        card_id,
                        targets: vec![],
                        payment_mode: CastPaymentMode::Auto,
                    },
                    None => GameAction::PassPriority,
                }
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                let target = self.targets.pop_front().unwrap_or_else(|| {
                    panic!(
                        "unscripted target prompt WaitingFor::{} on turn {}",
                        self.runner.waiting_for_kind(),
                        self.runner.state().turn_number
                    )
                });
                GameAction::ChooseTarget {
                    target: Some(target),
                }
            }
            WaitingFor::DeclareAttackers { player, .. } => {
                let seat_index = usize::from(player.0);
                GameAction::DeclareAttackers {
                    attacks: vec![(
                        self.attackers[seat_index],
                        AttackTarget::Player(seat((seat_index + 1) % SEATS)),
                    )],
                    bands: vec![],
                }
            }
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            WaitingFor::ResolveAllConsent { .. }
            | WaitingFor::ResolveAllReady { .. }
            | WaitingFor::MeldPairChoice { .. }
            | WaitingFor::MeldAttackTargetChoice { .. }
            | WaitingFor::EntryAttackTargetChoice { .. }
            | WaitingFor::MulliganDecision { .. }
            | WaitingFor::OpeningHandBottomCards { .. }
            | WaitingFor::ManaPayment { .. }
            | WaitingFor::ManaSourceSelection { .. }
            | WaitingFor::AssistChoosePlayer { .. }
            | WaitingFor::AssistPayment { .. }
            | WaitingFor::ChooseXValue { .. }
            | WaitingFor::UntapChoice { .. }
            | WaitingFor::ChooseUntapSubset { .. }
            | WaitingFor::ExertChoice { .. }
            | WaitingFor::EnlistChoice { .. }
            | WaitingFor::GameOver { .. }
            | WaitingFor::ReplacementChoice { .. }
            | WaitingFor::EntryControllerChoice { .. }
            | WaitingFor::OrderTriggers { .. }
            | WaitingFor::CopyTargetChoice { .. }
            | WaitingFor::ExploreChoice { .. }
            | WaitingFor::ReturnAsAuraTarget { .. }
            | WaitingFor::ChooseTokenHost { .. }
            | WaitingFor::EquipTarget { .. }
            | WaitingFor::CrewVehicle { .. }
            | WaitingFor::StationTarget { .. }
            | WaitingFor::SaddleMount { .. }
            | WaitingFor::ScryChoice { .. }
            | WaitingFor::RippleRevealChoice { .. }
            | WaitingFor::RippleBottomOrder { .. }
            | WaitingFor::RevealUntilBottomOrder { .. }
            | WaitingFor::ArrangePlanarDeckTopChoice { .. }
            | WaitingFor::RedistributeLifeTotals { .. }
            | WaitingFor::CoinFlipKeepChoice { .. }
            | WaitingFor::DieKeepChoice { .. }
            | WaitingFor::DigChoice { .. }
            | WaitingFor::DigRestSplitChoice { .. }
            | WaitingFor::SurveilChoice { .. }
            | WaitingFor::RevealChoice { .. }
            | WaitingFor::SearchChoice { .. }
            | WaitingFor::SearchPartitionChoice { .. }
            | WaitingFor::OutsideGameChoice { .. }
            | WaitingFor::ChooseFromZoneChoice { .. }
            | WaitingFor::BeholdChoice { .. }
            | WaitingFor::EmpowerJaceChoice { .. }
            | WaitingFor::SpellCopyOrderChoice { .. }
            | WaitingFor::ChooseOneOfBranch { .. }
            | WaitingFor::ConniveDiscard { .. }
            | WaitingFor::DiscardChoice { .. }
            | WaitingFor::EffectZoneChoice { .. }
            | WaitingFor::DrawnThisTurnTopdeckChoice { .. }
            | WaitingFor::LearnChoice { .. }
            | WaitingFor::ManifestDreadChoice { .. }
            | WaitingFor::BetweenGamesSideboard { .. }
            | WaitingFor::BetweenGamesChoosePlayDraw { .. }
            | WaitingFor::NamedChoice { .. }
            | WaitingFor::OpponentGuess { .. }
            | WaitingFor::SpellbookDraft { .. }
            | WaitingFor::DamageSourceChoice { .. }
            | WaitingFor::ModeChoice { .. }
            | WaitingFor::DiscardToHandSize { .. }
            | WaitingFor::OptionalCostChoice { .. }
            | WaitingFor::ChooseGiftRecipient { .. }
            | WaitingFor::SpliceOffer { .. }
            | WaitingFor::DefilerPayment { .. }
            | WaitingFor::OrderCostReductions { .. }
            | WaitingFor::CastOffer { .. }
            | WaitingFor::ModalFaceChoice { .. }
            | WaitingFor::AlternativeCastChoice { .. }
            | WaitingFor::MutateMergeChoice { .. }
            | WaitingFor::CipherEncodeChoice { .. }
            | WaitingFor::CastingVariantChoice { .. }
            | WaitingFor::ChoosePermanentTypeSlot { .. }
            | WaitingFor::MultiTargetSelection { .. }
            | WaitingFor::AbilityModeChoice { .. }
            | WaitingFor::OptionalEffectChoice { .. }
            | WaitingFor::ResolutionOptionalPaymentChoice { .. }
            | WaitingFor::PairChoice { .. }
            | WaitingFor::TributeChoice { .. }
            | WaitingFor::MiracleReveal { .. }
            | WaitingFor::OpponentMayChoice { .. }
            | WaitingFor::LoopShortcut { .. }
            | WaitingFor::RespondToShortcut { .. }
            | WaitingFor::PrecastCopyShortcutOffer { .. }
            | WaitingFor::RespondToPrecastCopyShortcut { .. }
            | WaitingFor::UnlessPayment { .. }
            | WaitingFor::UnlessPaymentChooseCost { .. }
            | WaitingFor::WardDiscardChoice { .. }
            | WaitingFor::WardSacrificeChoice { .. }
            | WaitingFor::UnlessBounceChoice { .. }
            | WaitingFor::ChooseRingBearer { .. }
            | WaitingFor::ChooseRoomDoor { .. }
            | WaitingFor::ChooseDungeon { .. }
            | WaitingFor::ChooseDungeonRoom { .. }
            | WaitingFor::SpecializeColor { .. }
            | WaitingFor::PayCost { .. }
            | WaitingFor::ActivationCostOneOfChoice { .. }
            | WaitingFor::CostTypeChoice { .. }
            | WaitingFor::BlightChoice { .. }
            | WaitingFor::PayManaAbilityMana { .. }
            | WaitingFor::ManaAbilityManaPayment { .. }
            | WaitingFor::ChooseManaColor { .. }
            | WaitingFor::CollectEvidenceChoice { .. }
            | WaitingFor::HarmonizeTapChoice { .. }
            | WaitingFor::RevealUntilKeptChoice { .. }
            | WaitingFor::RepeatDecision { .. }
            | WaitingFor::TopOrBottomChoice { .. }
            | WaitingFor::PopulateChoice { .. }
            | WaitingFor::ClashChooseOpponent { .. }
            | WaitingFor::ChooseFromZoneOpponentChooser { .. }
            | WaitingFor::ChooseAnnouncingOpponent { .. }
            | WaitingFor::ClashCardPlacement { .. }
            | WaitingFor::PhysicalContest { .. }
            | WaitingFor::VoteChoice { .. }
            | WaitingFor::SeparatePilesChooseOpponent { .. }
            | WaitingFor::SeparatePilesPartition { .. }
            | WaitingFor::SeparatePilesChoice { .. }
            | WaitingFor::CompanionReveal { .. }
            | WaitingFor::ChooseLegend { .. }
            | WaitingFor::CommanderZoneChoice { .. }
            | WaitingFor::BattleProtectorChoice { .. }
            | WaitingFor::ProliferateChoice { .. }
            | WaitingFor::TimeTravelChoice { .. }
            | WaitingFor::ChooseObjectsSelection { .. }
            | WaitingFor::CategoryChoice { .. }
            | WaitingFor::EachPlayerCopyChosenSelection { .. }
            | WaitingFor::KeepWithinTotalPowerChoice { .. }
            | WaitingFor::KeepExactPermanentsChoice { .. }
            | WaitingFor::CopyRetarget { .. }
            | WaitingFor::AssignCombatDamage { .. }
            | WaitingFor::AssignBlockerDamage { .. }
            | WaitingFor::DistributeAmong { .. }
            | WaitingFor::MoveCountersDistribution { .. }
            | WaitingFor::RemoveCountersChoice { .. }
            | WaitingFor::PayAmountChoice { .. }
            | WaitingFor::RetargetChoice { .. }
            | WaitingFor::CombatTaxPayment { .. }
            | WaitingFor::PhyrexianPayment { .. } => panic!(
                "unscripted prompt WaitingFor::{} on turn {}; extend the script deliberately",
                self.runner.waiting_for_kind(),
                state.turn_number
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// Measurement
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct Baselines {
    sender: Option<WireState>,
    receiver: Option<WireState>,
}

struct CodecFrames {
    full: Vec<u8>,
    selected: Vec<u8>,
    kind: FrameKind,
}

/// Exact raw JSON value spans; operation names, braces and layout are syntax.
#[derive(Debug, Default)]
struct CodecProfile {
    full_bytes: u64,
    metadata_bytes: u64,
    set_bytes: u64,
    remove_bytes: u64,
    upsert_bytes: u64,
    order_bytes: u64,
    patch_syntax_bytes: u64,
}

impl CodecProfile {
    fn record(&mut self, frames: &CodecFrames) {
        use std::collections::BTreeMap;

        use serde_json::value::RawValue;

        if frames.kind == FrameKind::Full {
            self.full_bytes += frames.selected.len() as u64;
            return;
        }
        type RawMap<'a> = BTreeMap<&'a str, &'a RawValue>;
        let envelope: RawMap<'_> = serde_json::from_slice(&frames.selected).unwrap();
        let data: RawMap<'_> = serde_json::from_str(envelope["data"].get()).unwrap();
        let patch = data["patch"];
        self.metadata_bytes += (frames.selected.len() - patch.get().len()) as u64;
        let sections: RawMap<'_> = serde_json::from_str(patch.get()).unwrap();
        let mut payload_bytes = 0;
        for section in ["message", "data", "state", "objects"] {
            let Some(raw) = sections.get(section) else {
                continue;
            };
            let operations: RawMap<'_> = serde_json::from_str(raw.get()).unwrap();
            for (operation, raw) in operations {
                let bytes = raw.get().len() as u64;
                match operation {
                    "set" => self.set_bytes += bytes,
                    "remove" => self.remove_bytes += bytes,
                    "upsert" => self.upsert_bytes += bytes,
                    "order" => self.order_bytes += bytes,
                    other => panic!("unexpected patch operation {other}"),
                }
                payload_bytes += bytes;
            }
        }
        self.patch_syntax_bytes += patch.get().len() as u64 - payload_bytes;
    }

    fn total_bytes(&self) -> u64 {
        self.full_bytes
            + self.metadata_bytes
            + self.set_bytes
            + self.remove_bytes
            + self.upsert_bytes
            + self.order_bytes
            + self.patch_syntax_bytes
    }
}

impl Baselines {
    fn deliver(&mut self, json: &str, revision: u64) -> CodecFrames {
        let current = WireState::from_json(json.as_bytes())
            .expect("production Envelope is JSON")
            .with_revision(revision);
        let full = encode(None, &current)
            .expect("FULL encodes")
            .to_bytes()
            .expect("FULL serializes");
        let frame = encode(self.sender.as_ref(), &current).expect("codec encodes");
        let kind = frame.kind();
        let selected = frame.to_bytes().expect("codec frame serializes");
        assert!(
            selected.len() <= full.len(),
            "selected frame {} exceeds FULL {} at revision {revision}",
            selected.len(),
            full.len()
        );
        let frame = Frame::from_bytes(&selected).expect("serialized frame parses");
        assert_eq!(frame.kind(), kind, "serialized frame preserves its kind");
        let reconstructed =
            decode(self.receiver.as_ref(), &frame).expect("delivered frame decodes");
        assert_eq!(
            reconstructed.as_bytes(),
            json.as_bytes(),
            "complete production Envelope differs at revision {revision}"
        );
        assert_eq!(reconstructed.revision(), Some(revision));
        assert_eq!(state_hash(&reconstructed), state_hash(&current));
        // Advance independently only after simulated delivery and successful decode.
        self.sender = Some(current);
        self.receiver = Some(reconstructed);
        CodecFrames {
            full,
            selected,
            kind,
        }
    }
}

#[derive(Debug)]
struct SeatMeasurements {
    legacy: SeatBudget,
    forced_full: SeatBudget,
    codec: SeatBudget,
    full_updates: u64,
    delta_updates: u64,
    baselines: Baselines,
    profile: CodecProfile,
}

impl SeatMeasurements {
    fn new(index: usize) -> Self {
        Self {
            legacy: SeatBudget::new(index),
            forced_full: SeatBudget::new(index),
            codec: SeatBudget::new(index),
            full_updates: 0,
            delta_updates: 0,
            baselines: Baselines::default(),
            profile: CodecProfile::default(),
        }
    }
}

/// Mirrors the tail of `GameSession::handle_action_with_card_db_outcome`
/// (server-core `session.rs`): the raw post-action state, the engine's events and
/// log entries, and the legal-action bundle recomputed from that state.
fn server_action_result(
    state: &engine::types::game_state::GameState,
    result: engine::types::game_state::ActionResult,
) -> ActionResult {
    let (legal_actions, spell_costs, by_object) = engine::ai_support::legal_actions_full(state);
    let auto_pass = engine::ai_support::auto_pass_recommended(state, &legal_actions);
    (
        state.clone(),
        result.events,
        legal_actions,
        result.log_entries,
        auto_pass,
        spell_costs,
        by_object,
    )
}

/// Mirrors the per-seat loop of `StateUpdatePublisher::publish` and the frame
/// encode of `socket_io::encode_application`: build the seat's `StateUpdate`,
/// wrap it in the writer's `Envelope`, serialize, then gzip-envelope it.
fn measure_publication(result: &ActionResult, revision: u64, totals: &mut [SeatMeasurements]) {
    let source = StateUpdateSource::from_result(result, revision)
        .guarded("BUDGET")
        .expect("the snapshot guard admits the scripted board");
    let full_key = FullSessionKey {
        game_code: "BUDGET".into(),
        generation: 1,
    };
    for (index, total) in totals.iter_mut().enumerate() {
        let message = crate::build_state_update_message(
            &source,
            &full_key,
            seat(index),
            Vec::new(),
            &source.state.eliminated_players,
        );
        let json = serde_json::to_string(&Envelope {
            message,
            command_id: None,
            quota_refusal: None,
            state_encoding: crate::socket_mailbox::StateEncoding::Automatic,
        })
        .expect("StateUpdate serializes");
        let frames = total.baselines.deliver(&json, revision);
        total.profile.record(&frames);
        match frames.kind {
            FrameKind::Full => total.full_updates += 1,
            FrameKind::Delta => total.delta_updates += 1,
        }
        total.legacy.record(json);
        total
            .forced_full
            .record(String::from_utf8(frames.full).expect("FULL frame is UTF-8 JSON"));
        total
            .codec
            .record(String::from_utf8(frames.selected).expect("selected frame is UTF-8 JSON"));
    }
}

fn play_round() -> Vec<SeatMeasurements> {
    let mut totals: Vec<SeatMeasurements> = (0..SEATS).map(SeatMeasurements::new).collect();
    visit_round(|result, revision| measure_publication(result, revision, &mut totals));
    assert!(
        totals
            .iter()
            .all(|t| t.legacy.updates > 0 && t.legacy.updates == totals[0].legacy.updates),
        "every seat receives the same positive number of updates"
    );
    totals
}

/// Stream the unchanged script once per pass; do not archive the whole stream.
fn visit_round(mut publication: impl FnMut(&ActionResult, u64)) {
    let mut game = build_game();

    let mut revision = 0u64;
    let mut reached_end = false;
    for _ in 0..ITERATION_CAP {
        let action = game.next_action();
        let applied = game
            .runner
            .act(action.clone())
            .unwrap_or_else(|error| panic!("scripted action {action:?} rejected: {error:?}"));
        let result = server_action_result(game.runner.state(), applied);
        revision += 1;
        publication(&result, revision);
        if game.runner.state().turn_number >= END_TURN {
            reached_end = true;
            break;
        }
    }
    assert!(
        reached_end,
        "scripted game did not reach turn {END_TURN} within {ITERATION_CAP} submissions (stuck on WaitingFor::{})",
        game.runner.waiting_for_kind()
    );

    // Guards against a vacuous pass: the script must have run to completion.
    let state = game.runner.state();
    assert_eq!(
        state.turn_number, END_TURN,
        "the round ends on turn {END_TURN}"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "the round ends at a priority window, got WaitingFor::{}",
        game.runner.waiting_for_kind()
    );
    assert!(state.stack.is_empty(), "the stack is empty at the end");
    assert!(
        !matches!(state.waiting_for, WaitingFor::GameOver { .. }),
        "nobody lost the game"
    );
    assert!(
        game.scripts.iter().all(VecDeque::is_empty) && game.targets.is_empty(),
        "every scripted intent was consumed"
    );
    for (index, land) in game.hand_lands.iter().enumerate() {
        assert_eq!(
            state.objects[land].zone,
            Zone::Battlefield,
            "seat {index} played its hand land"
        );
    }
    for (index, spell) in game.hand_spells.iter().enumerate() {
        let expected = if index == 0 {
            Zone::Graveyard
        } else {
            Zone::Battlefield
        };
        assert_eq!(
            state.objects[spell].zone, expected,
            "seat {index}'s hand spell resolved"
        );
    }
    assert_eq!(
        state.objects[&game.bolt_victim].zone,
        Zone::Graveyard,
        "Lightning Bolt killed the Llanowar Elves"
    );
    assert!(
        state.players[1].graveyard.contains(&game.bolt_victim),
        "the Bolted creature is in its owner's graveyard"
    );
}

#[test]
fn state_update_bandwidth_stays_within_budget() {
    let measured = play_round();
    let budget = load_budget();
    let legacy: Vec<_> = measured.iter().map(|s| s.legacy).collect();
    let forced_full: Vec<_> = measured.iter().map(|s| s.forced_full).collect();
    let codec: Vec<_> = measured.iter().map(|s| s.codec).collect();
    eprintln!(
        "measured StateUpdate bandwidth (scenario {}):\nlegacy:\n{}forced FULL codec:\n{}selected codec:\n{}",
        budget.scenario,
        measured_table(&legacy, &budget.seats),
        measured_table(&forced_full, &[]),
        measured_table(&codec, &budget.codec_seats),
    );
    // Always validate legacy, including rebaseline runs; never refresh its pins.
    assert_within_budget(&legacy, &budget.seats, &budget, "legacy");
    for seat in &measured {
        eprintln!(
            "seat {}: FULL={} DELTA={} selected/legacy wire={:.4}% selected/forced-FULL wire={:.4}%",
            seat.legacy.seat,
            seat.full_updates,
            seat.delta_updates,
            seat.codec.wire_bytes as f64 / seat.legacy.wire_bytes as f64 * 100.0,
            seat.codec.wire_bytes as f64 / seat.forced_full.wire_bytes as f64 * 100.0,
        );
        eprintln!(
            "seat {} raw codec profile: {:?}",
            seat.legacy.seat, seat.profile
        );
        assert_eq!(seat.profile.total_bytes(), seat.codec.json_bytes);
        assert!(
            seat.codec.wire_bytes < seat.legacy.wire_bytes
                && seat.codec.wire_bytes < seat.forced_full.wire_bytes,
            "seat {}: codec must save wire bytes against legacy and forced FULL",
            seat.legacy.seat,
        );
        assert!(seat.delta_updates > 0, "each seat must receive DELTA");
        assert!(seat.full_updates > 0, "each seat starts with FULL");
        assert_eq!(seat.codec.updates, seat.legacy.updates);
        assert_eq!(seat.forced_full.updates, seat.legacy.updates);
        assert_eq!(seat.full_updates + seat.delta_updates, seat.legacy.updates);
    }
    if std::env::var_os(UPDATE_ENV_VAR).is_some() {
        rewrite_budget(&codec, budget);
        return;
    }
    assert_within_budget(&codec, &budget.codec_seats, &budget, "codec");
}

#[test]
fn completion_envelope_removes_command_id_through_delta() {
    use server_core::protocol::CommandId;

    let mut game = build_game();
    let action = game.next_action();
    let applied = game.runner.act(action).expect("scripted action succeeds");
    let result = server_action_result(game.runner.state(), applied);
    let revision = 1;
    let source = StateUpdateSource::from_result(&result, revision)
        .guarded("COMPLETION")
        .expect("scripted snapshot is admitted");
    let full_key = FullSessionKey {
        game_code: "COMPLETION".into(),
        generation: 1,
    };
    let mut baselines = Baselines::default();
    for command_id in [Some(CommandId(37)), None] {
        let message = crate::build_state_update_message(
            &source,
            &full_key,
            seat(0),
            Vec::new(),
            &source.state.eliminated_players,
        );
        let json = serde_json::to_string(&Envelope {
            message,
            command_id,
            quota_refusal: None,
            state_encoding: crate::socket_mailbox::StateEncoding::Automatic,
        })
        .expect("completion Envelope serializes");
        let original: serde_json::Value = serde_json::from_str(&json).unwrap();
        let frames = baselines.deliver(&json, revision);
        let decoded: serde_json::Value =
            serde_json::from_str(baselines.receiver.as_ref().unwrap().as_json()).unwrap();
        match command_id {
            Some(id) => {
                assert_eq!(original.get("command_id"), Some(&serde_json::json!(id.0)));
                assert_eq!(decoded.get("command_id"), original.get("command_id"));
                assert_eq!(
                    frames.kind,
                    FrameKind::Full,
                    "completion establishes a FULL baseline"
                );
            }
            None => {
                assert!(original.get("command_id").is_none());
                assert_eq!(
                    frames.kind,
                    FrameKind::Delta,
                    "key removal must reach DELTA"
                );
                assert!(
                    decoded.get("command_id").is_none(),
                    "no stale completion key"
                );
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Traffic {
    Continuous,
    ReconnectHeavy,
}

#[derive(Clone, Copy, Debug)]
enum ResourceLane {
    Legacy,
    Codec(Traffic),
}

impl ResourceLane {
    fn owner(self) -> crate::socket_io::resource_probe::Owner {
        use crate::socket_io::resource_probe::Owner;
        match self {
            Self::Legacy => Owner::legacy(),
            Self::Codec(_) => Owner::codec(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum ResourcePass {
    Cpu,
    Memory,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SamplePosition {
    First,
    Subsequent,
}

struct ResourceSample {
    kind: crate::metrics::WsFrameType,
    position: SamplePosition,
    prepare: std::time::Duration,
    commit: std::time::Duration,
    wait: std::time::Duration,
}

#[derive(Default)]
struct ResourceTotals {
    frames: [usize; 4],
    bytes: [u64; 4],
    reconnect_full: usize,
    continuing_delta: usize,
    samples: Vec<ResourceSample>,
    /// Component maxima need not occur together; total_peak is simultaneous.
    retained_component_peaks: [usize; 3],
    retained_total_peak: usize,
    old_component_peaks: [usize; 3],
    candidate_component_peaks: [usize; 3],
    /// initial-empty, normal Delta, automatic Full, reconnect Full.
    precommit_counts: [usize; 4],
    precommit_subtotal_peaks: [usize; 4],
}

fn memory_components(usage: Option<state_codec::BaselineMemoryUsage>) -> [usize; 3] {
    usage.map_or([0; 3], |usage| {
        [
            usage.json_bytes,
            usage.index_key_bytes,
            usage.object_hash_bytes,
        ]
    })
}

fn component_peaks(peaks: &mut [usize; 3], current: [usize; 3]) {
    for (peak, value) in peaks.iter_mut().zip(current) {
        *peak = (*peak).max(value);
    }
}

fn payload(frame: &Message) -> &[u8] {
    match frame {
        Message::Text(text) => text.as_bytes(),
        Message::Binary(bytes) => bytes,
        other => panic!("resource application payload: {other:?}"),
    }
}

fn resource_slot(kind: crate::metrics::WsFrameType) -> usize {
    use crate::metrics::WsFrameType;
    match kind {
        WsFrameType::Full => 0,
        WsFrameType::Delta => 1,
        WsFrameType::StateUpdate => 2,
        WsFrameType::GameStarted => 3,
        WsFrameType::Other | WsFrameType::Control => panic!("expected state frame"),
    }
}

async fn assert_resource_output(
    frame: Message,
    expected: &str,
    revision: u64,
    lane: ResourceLane,
    encoding: crate::socket_control::Encoding,
    receiver: &mut Option<WireState>,
) {
    match lane {
        ResourceLane::Legacy => {
            let reference = crate::wire::encode_json_message_sync(
                expected.to_owned(),
                encoding == crate::socket_control::Encoding::GzipEnvelope,
            )
            .unwrap();
            assert_eq!(
                payload(&frame),
                payload(&reference),
                "SAME-Envelope legacy framing"
            );
            assert!(
                receiver.is_none(),
                "legacy retains no receiver codec baseline"
            );
        }
        ResourceLane::Codec(_) => {
            let json = match frame {
                Message::Text(text) => text.to_string(),
                Message::Binary(bytes) => {
                    crate::wire::decode_client_envelope(bytes.to_vec(), 16 * 1024 * 1024)
                        .await
                        .unwrap()
                }
                other => panic!("codec application frame: {other:?}"),
            };
            let frame = Frame::from_bytes(json.as_bytes()).unwrap();
            let decoded = decode(receiver.as_ref(), &frame).unwrap();
            assert_eq!(
                decoded.as_bytes(),
                expected.as_bytes(),
                "exact complete Envelope bytes"
            );
            assert_eq!(decoded.revision(), Some(revision));
            *receiver = Some(decoded);
        }
    }
}

fn duration_distribution(values: impl Iterator<Item = std::time::Duration>) -> serde_json::Value {
    let mut ns: Vec<_> = values.map(|value| value.as_nanos()).collect();
    ns.sort_unstable();
    let total: u128 = ns.iter().sum();
    if ns.is_empty() {
        return serde_json::json!({"count": 0});
    }
    serde_json::json!({
        "count": ns.len(), "total_ns": total, "mean_ns": total as f64 / ns.len() as f64,
        "median_ns": ns[ns.len() / 2], "p95_ns": ns[(ns.len() * 95).div_ceil(100) - 1],
        "max_ns": ns.last().unwrap()
    })
}

fn report_samples(label: &str, samples: &[&ResourceSample]) {
    use crate::metrics::WsFrameType;
    for position in [SamplePosition::First, SamplePosition::Subsequent] {
        let selected: Vec<_> = samples
            .iter()
            .copied()
            .filter(|sample| sample.position == position)
            .collect();
        println!(
            "P5 CPU {label} position={position:?} prepare={} commit={} queue_flush={}",
            duration_distribution(selected.iter().map(|sample| sample.prepare)),
            duration_distribution(selected.iter().map(|sample| sample.commit)),
            duration_distribution(selected.iter().map(|sample| sample.wait))
        );
    }
    for kind in [
        WsFrameType::Full,
        WsFrameType::Delta,
        WsFrameType::StateUpdate,
        WsFrameType::GameStarted,
    ] {
        let selected: Vec<_> = samples
            .iter()
            .copied()
            .filter(|sample| sample.kind == kind)
            .collect();
        if !selected.is_empty() {
            println!(
                "P5 CPU {label} kind={kind:?} prepare={} commit={} queue_flush={}",
                duration_distribution(selected.iter().map(|sample| sample.prepare)),
                duration_distribution(selected.iter().map(|sample| sample.commit)),
                duration_distribution(selected.iter().map(|sample| sample.wait))
            );
        }
    }
}

fn resource_envelope(message: server_core::protocol::ServerMessage) -> Envelope {
    Envelope {
        message,
        command_id: None,
        quota_refusal: None,
        state_encoding: crate::socket_mailbox::StateEncoding::Automatic,
    }
}

/// No accounting call touches the CPU owners. Projection, reference serialization,
/// independent decode, writer setup and fixture generation are outside CPU timers.
fn resource_pass(
    runtime: &tokio::runtime::Runtime,
    run: usize,
    lane: ResourceLane,
    encoding: crate::socket_control::Encoding,
    pass: ResourcePass,
    terminal: &[Envelope],
) -> [u64; 6] {
    use crate::metrics::WsFrameType;
    let mut owners: Vec<_> = (0..6).map(|_| lane.owner()).collect();
    let mut receivers: Vec<Option<WireState>> = (0..6).map(|_| None).collect();
    let mut totals: Vec<ResourceTotals> = (0..6).map(|_| ResourceTotals::default()).collect();
    let mut current = [[0_usize; 3]; 6];
    // Seats, spectators, game: aggregate simultaneous observations, not peaks.
    let mut simultaneous_retained_peak = [[0_usize; 3]; 3];
    let mut simultaneous_retained_total_peak = [0_usize; 3];
    let mut simultaneous_precommit_subtotal_peak = [0_usize; 3];
    let mut publication = 0_usize;
    visit_round(|result, revision| {
        let source = StateUpdateSource::from_result(result, revision);
        let source = source.guarded("RESOURCE").unwrap();
        let key = FullSessionKey {
            game_code: "RESOURCE".into(),
            generation: 1,
        };
        for index in 0..6 {
            let reconnect = matches!(lane, ResourceLane::Codec(Traffic::ReconnectHeavy))
                && publication > 0
                && publication % 3 != 2;
            if reconnect {
                owners[index] = lane.owner();
                receivers[index] = None;
                current[index] = [0; 3];
            }
            let fresh = publication == 0 || reconnect;
            let message = if index < SEATS {
                crate::build_state_update_message(
                    &source,
                    &key,
                    seat(index),
                    Vec::new(),
                    &source.state.eliminated_players,
                )
            } else {
                crate::build_spectator_state_update_message(
                    &key,
                    source.state,
                    source.events,
                    source.log_entries,
                    revision,
                )
                .unwrap()
            };
            let envelope = resource_envelope(message);
            // Compute the exact reference from THIS Envelope, before moving it.
            let expected = serde_json::to_string(&envelope).unwrap();
            let old = match pass {
                ResourcePass::Cpu => [0; 3],
                ResourcePass::Memory => memory_components(owners[index].memory()),
            };
            let (prepared, prepare) = owners[index].prepare(envelope, encoding);
            let kind = prepared.frame_type();
            let slot = resource_slot(kind);
            let bytes = payload(prepared.frame()).len();
            if fresh && matches!(lane, ResourceLane::Codec(_)) {
                assert_eq!(
                    kind,
                    WsFrameType::Full,
                    "fresh sender must emit independent Full"
                );
            }
            totals[index].frames[slot] += 1;
            totals[index].bytes[slot] += bytes as u64;
            totals[index].reconnect_full += usize::from(reconnect && kind == WsFrameType::Full);
            totals[index].continuing_delta += usize::from(!fresh && kind == WsFrameType::Delta);
            if let ResourcePass::Memory = pass {
                let candidate = memory_components(prepared.memory());
                component_peaks(&mut totals[index].old_component_peaks, old);
                component_peaks(&mut totals[index].candidate_component_peaks, candidate);
                if matches!(lane, ResourceLane::Codec(_)) {
                    assert!(candidate[0] > 0);
                    let class = if reconnect {
                        3
                    } else if fresh {
                        0
                    } else if kind == WsFrameType::Delta {
                        1
                    } else {
                        2
                    };
                    let subtotal =
                        old.iter().sum::<usize>() + candidate.iter().sum::<usize>() + bytes;
                    totals[index].precommit_counts[class] += 1;
                    totals[index].precommit_subtotal_peaks[class] =
                        totals[index].precommit_subtotal_peaks[class].max(subtotal);
                    for (group, range) in [0..4, 4..6, 0..6].into_iter().enumerate() {
                        let transient = if range.contains(&index) {
                            candidate.iter().sum::<usize>() + bytes
                        } else {
                            0
                        };
                        let simultaneous =
                            current[range].iter().flatten().sum::<usize>() + transient;
                        simultaneous_precommit_subtotal_peak[group] =
                            simultaneous_precommit_subtotal_peak[group].max(simultaneous);
                    }
                } else {
                    assert_eq!(old, [0; 3]);
                    assert_eq!(candidate, [0; 3]);
                }
            }
            let delivery = runtime.block_on(owners[index].deliver(prepared));
            runtime.block_on(assert_resource_output(
                delivery.frame,
                &expected,
                revision,
                lane,
                encoding,
                &mut receivers[index],
            ));
            if let ResourcePass::Cpu = pass {
                totals[index].samples.push(ResourceSample {
                    kind,
                    position: if fresh {
                        SamplePosition::First
                    } else {
                        SamplePosition::Subsequent
                    },
                    prepare,
                    commit: delivery.commit,
                    wait: delivery.queue_flush,
                });
            } else {
                current[index] = memory_components(owners[index].memory());
                component_peaks(&mut totals[index].retained_component_peaks, current[index]);
                totals[index].retained_total_peak = totals[index]
                    .retained_total_peak
                    .max(current[index].iter().sum());
                for (group, range) in [0..4, 4..6, 0..6].into_iter().enumerate() {
                    let aggregate = std::array::from_fn(|component| {
                        current[range.clone()]
                            .iter()
                            .map(|owner| owner[component])
                            .sum()
                    });
                    component_peaks(&mut simultaneous_retained_peak[group], aggregate);
                    simultaneous_retained_total_peak[group] = simultaneous_retained_total_peak
                        [group]
                        .max(aggregate.iter().sum::<usize>());
                }
            }
        }
        publication += 1;
    });
    assert!(publication > 0);
    for (index, total) in totals.iter().enumerate() {
        match lane {
            ResourceLane::Legacy => {
                assert_eq!(&total.frames[..2], &[0, 0]);
                assert_eq!(total.frames[2], publication);
            }
            ResourceLane::Codec(Traffic::Continuous) => {
                assert!(
                    total.frames[0] > 0 && total.frames[1] > total.frames[0],
                    "actual Delta-heavy class"
                );
            }
            ResourceLane::Codec(Traffic::ReconnectHeavy) => {
                assert!(
                    total.frames[0] > total.frames[1]
                        && total.reconnect_full > 0
                        && total.continuing_delta > 0,
                    "actual Full-heavy/reconnect and continuing Delta reach"
                );
            }
        }
        println!("P5 RESOURCE run={run} pass={pass:?} lane={lane:?} encoding={encoding:?} recipient={index} publications={publication} frames={:?} payload_bytes={:?} reconnect_full={} continuing_delta={}",
            total.frames, total.bytes, total.reconnect_full, total.continuing_delta);
        match pass {
            ResourcePass::Cpu => {
                assert_eq!(total.samples.len(), publication);
                report_samples(&format!("run={run} lane={lane:?} encoding={encoding:?} recipient={index}"), &total.samples.iter().collect::<Vec<_>>());
            }
            ResourcePass::Memory => println!("P5 MEMORY run={run} lane={lane:?} encoding={encoding:?} recipient={index} fully_indexed_retained_component_peaks={:?} simultaneous_owner_retained_total_peak={} old_component_peaks={:?} candidate_component_peaks={:?} precommit_class_counts={:?} old_plus_candidate_plus_frame_subtotal_peaks={:?}",
                total.retained_component_peaks, total.retained_total_peak, total.old_component_peaks,
                total.candidate_component_peaks, total.precommit_counts, total.precommit_subtotal_peaks),
        }
    }
    for (audience, range) in [("seats", 0..4), ("spectators", 4..6), ("game", 0..6)] {
        let selected = &totals[range];
        let bytes: u64 = selected.iter().flat_map(|total| total.bytes).sum();
        let frames: usize = selected.iter().flat_map(|total| total.frames).sum();
        println!("P5 RESOURCE aggregate run={run} pass={pass:?} lane={lane:?} encoding={encoding:?} audience={audience} frames={frames} payload_bytes={bytes}");
        if let ResourcePass::Cpu = pass {
            report_samples(
                &format!("run={run} lane={lane:?} encoding={encoding:?} audience={audience}"),
                &selected
                    .iter()
                    .flat_map(|total| &total.samples)
                    .collect::<Vec<_>>(),
            );
        }
    }
    if let ResourcePass::Memory = pass {
        for (group, audience) in ["seats", "spectators", "game"].into_iter().enumerate() {
            println!("P5 MEMORY aggregate run={run} lane={lane:?} encoding={encoding:?} audience={audience} simultaneous_retained_component_peaks={:?} simultaneous_retained_total_peak={} simultaneous_precommit_subtotal_peak={}",
                simultaneous_retained_peak[group], simultaneous_retained_total_peak[group], simultaneous_precommit_subtotal_peak[group]);
        }
        assert_eq!(terminal.len(), owners.len());
        for (index, envelope) in terminal.iter().enumerate() {
            let revision = match &envelope.message {
                server_core::protocol::ServerMessage::StateUpdate {
                    state,
                    state_revision,
                    ..
                } => {
                    assert!(matches!(
                        state.waiting_for,
                        WaitingFor::GameOver { winner: Some(_) }
                    ));
                    *state_revision
                }
                other => panic!("terminal projected StateUpdate: {other:?}"),
            };
            let envelope = resource_envelope(envelope.message.clone());
            let expected = serde_json::to_string(&envelope).unwrap();
            let old = memory_components(owners[index].memory());
            let (prepared, _) = owners[index].prepare(envelope, encoding);
            let candidate = memory_components(prepared.memory());
            let framed = payload(prepared.frame()).len();
            let subtotal = old.iter().sum::<usize>() + candidate.iter().sum::<usize>() + framed;
            println!("P5 MEMORY terminal_precommit run={run} lane={lane:?} encoding={encoding:?} recipient={index} old={old:?} candidate={candidate:?} final_frame_payload={framed} accounted_subtotal={subtotal}");
            let delivery = runtime.block_on(owners[index].deliver(prepared));
            runtime.block_on(assert_resource_output(
                delivery.frame,
                &expected,
                revision,
                lane,
                encoding,
                &mut receivers[index],
            ));
            let before = memory_components(owners[index].memory());
            let nonstate =
                resource_envelope(server_core::protocol::ServerMessage::Pong { timestamp: 717 });
            let expected = serde_json::to_string(&nonstate).unwrap();
            let (prepared, _) = owners[index].prepare(nonstate, encoding);
            assert_eq!(prepared.frame_type(), WsFrameType::Other);
            assert!(prepared.memory().is_none());
            let framed = payload(prepared.frame()).len();
            println!("P5 MEMORY nonstate_precommit run={run} lane={lane:?} encoding={encoding:?} recipient={index} old={before:?} candidate=absent final_frame_payload={framed} accounted_subtotal={}",
                before.iter().sum::<usize>() + framed);
            let delivery = runtime.block_on(owners[index].deliver(prepared));
            // Non-state traffic remains ordinary in both lanes.
            runtime.block_on(assert_resource_output(
                delivery.frame,
                &expected,
                revision,
                ResourceLane::Legacy,
                encoding,
                &mut None,
            ));
            runtime.block_on(tokio::task::yield_now());
            let after = memory_components(owners[index].memory());
            assert_eq!(
                before, after,
                "terminal/non-state/idle must retain the last baseline"
            );
            match lane {
                ResourceLane::Legacy => assert_eq!(after, [0; 3]),
                ResourceLane::Codec(_) => assert!(after[0] > 0),
            }
            println!("P5 IDLE run={run} lane={lane:?} encoding={encoding:?} recipient={index} terminal_before={before:?} after_nonstate_yield={after:?}");
            current[index] = after;
        }
        for (audience, range) in [("seats", 0..4), ("spectators", 4..6), ("game", 0..6)] {
            let components: [usize; 3] = std::array::from_fn(|component| {
                current[range.clone()]
                    .iter()
                    .map(|owner| owner[component])
                    .sum()
            });
            println!("P5 IDLE aggregate run={run} lane={lane:?} encoding={encoding:?} audience={audience} fully_indexed_terminal_components={components:?} simultaneous_total={}",
                components.iter().sum::<usize>());
        }
    }
    let bytes = std::array::from_fn(|index| totals[index].bytes.iter().sum());
    owners.clear();
    assert!(
        owners.is_empty(),
        "all measured server owners and prepared tokens dropped"
    );
    let mut fresh = ResourceLane::Codec(Traffic::Continuous).owner();
    assert!(fresh.memory().is_none());
    let envelope = resource_envelope(terminal[0].message.clone());
    let expected = serde_json::to_string(&envelope).unwrap();
    let revision = match &envelope.message {
        server_core::protocol::ServerMessage::StateUpdate { state_revision, .. } => *state_revision,
        _ => unreachable!("proven terminal fixture"),
    };
    let (prepared, _) = fresh.prepare(envelope, encoding);
    assert_eq!(prepared.frame_type(), WsFrameType::Full);
    let delivery = runtime.block_on(fresh.deliver(prepared));
    runtime.block_on(assert_resource_output(
        delivery.frame,
        &expected,
        revision,
        ResourceLane::Codec(Traffic::Continuous),
        encoding,
        &mut None,
    ));
    drop(fresh);
    runtime.block_on(tokio::task::yield_now());
    println!("P5 TEARDOWN run={run} pass={pass:?} lane={lane:?} encoding={encoding:?} remaining_accounted_server_owners=0; fresh owner absent then independent Full");
    bytes
}

fn production_resource_runs(repetitions: usize) {
    use crate::socket_control::Encoding;
    if cfg!(debug_assertions) {
        panic!("run cargo test --locked --profile server-release -p phase-server --bin phase-server production_encoder_resource_acceptance -- --ignored --nocapture --test-threads=1");
    }
    assert!(
        std::env::var_os(UPDATE_ENV_VAR).is_none(),
        "never rebaseline resource acceptance"
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let terminal = runtime.block_on(super::network_tests::completed_terminal_envelopes());
    let _entered = runtime.enter();
    println!("P5 RESOURCE profile=server-release optimized arch={} os={} repetitions={repetitions}; CPU-work service elapsed, not OS CPU/utilization; prepare/commit exclude projection/reference/decode/accounting; queue_flush is collecting-sink harness wait, not network RTT",
        std::env::consts::ARCH, std::env::consts::OS);
    println!("P5 MEMORY fully indexed logical storage, not passive allocator observation; precommit subtotal=old+candidate+final frame, NOT heap-peak upper bound. Excludes allocator headers/fragmentation, Arc/small handles/key/viewer/owner structs, spare JSON/payload capacity, transient Envelope JSON/diff/assemble/Frame/serde/compression workspaces and overlap, game/session/projection storage, queued Envelopes/socket buffers, Tokio tasks/stacks, receiver/reference/report storage, OS/network buffers and RSS.");
    let mut all_runs = Vec::new();
    for run in 1..=repetitions {
        for encoding in [Encoding::Plain, Encoding::GzipEnvelope] {
            let legacy = resource_pass(
                &runtime,
                run,
                ResourceLane::Legacy,
                encoding,
                ResourcePass::Cpu,
                &terminal,
            );
            resource_pass(
                &runtime,
                run,
                ResourceLane::Legacy,
                encoding,
                ResourcePass::Memory,
                &terminal,
            );
            for traffic in [Traffic::Continuous, Traffic::ReconnectHeavy] {
                let lane = ResourceLane::Codec(traffic);
                let measured =
                    resource_pass(&runtime, run, lane, encoding, ResourcePass::Cpu, &terminal);
                resource_pass(
                    &runtime,
                    run,
                    lane,
                    encoding,
                    ResourcePass::Memory,
                    &terminal,
                );
                let ratios: [f64; 6] =
                    std::array::from_fn(|index| measured[index] as f64 / legacy[index] as f64);
                println!("P5 MATCHED run={run} lane={lane:?} encoding={encoding:?} codec_to_legacy_payload_ratios={ratios:?}; independent map order may vary across replays, no exact cross-run byte pin");
                all_runs.push((run, format!("{encoding:?}/{traffic:?}"), measured));
            }
        }
    }
    println!("P5 RESOURCE repetitions_complete={repetitions} all_run_payload_totals={all_runs:?}; report all repetitions/spread, not fastest only");
}

#[test]
#[ignore = "optimized production encoder measurement; three full repetitions"]
fn production_encoder_resource_acceptance() {
    production_resource_runs(3);
}

#[test]
#[ignore = "establish-first optimized resource smoke; one full repetition"]
fn production_encoder_resource_smoke() {
    production_resource_runs(1);
}
