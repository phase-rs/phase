use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{BufReader, Write};
use std::time::{Duration, Instant};

use engine::database::CardDatabase;
use engine::game::deck_loading::{
    load_and_hydrate_decks, resolve_deck_list, DeckList, PlayerDeckList,
};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::visibility::{filter_state_for_unseated_viewer, filter_state_for_viewer};
use engine::types::ability::TargetRef;
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::format::FormatConfig;
use engine::types::game_state::CastPaymentMode;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use phase_ai::auto_play::run_driver_loop;
use phase_ai::config::{create_config_for_players, AiDifficulty, Platform};
use rand::{rngs::StdRng, SeedableRng};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use state_codec::{decode, encode, state_hash, Frame, FrameKind, WireState};

fn database() -> CardDatabase {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../engine/tests/fixtures/integration_cards.json.gz");
    CardDatabase::from_export_reader(flate2::read::GzDecoder::new(BufReader::new(
        File::open(path).expect("committed card fixture opens"),
    )))
    .expect("committed card fixture loads")
}

fn deck(db: &CardDatabase, green: bool) -> PlayerDeckList {
    let cards: &[(&str, usize)] = if green {
        &[
            ("Forest", 22),
            ("Llanowar Elves", 4),
            ("Elvish Mystic", 4),
            ("Grizzly Bears", 8),
            ("Centaur Courser", 8),
            ("Leatherback Baloth", 2),
            ("Giant Growth", 8),
            ("Rancor", 4),
        ]
    } else {
        &[
            ("Mountain", 20),
            ("Goblin Guide", 4),
            ("Monastery Swiftspear", 4),
            ("Raging Goblin", 8),
            ("Jackal Pup", 4),
            ("Lightning Bolt", 4),
            ("Shock", 4),
            ("Lava Spike", 4),
            ("Searing Spear", 4),
            ("Skullcrack", 4),
        ]
    };
    let mut main_deck = Vec::with_capacity(60);
    for &(name, count) in cards {
        assert!(
            db.get_face_by_name(name).is_some(),
            "missing fixture card: {name}"
        );
        main_deck.extend(std::iter::repeat_n(name.to_owned(), count));
    }
    assert_eq!(main_deck.len(), 60);
    PlayerDeckList {
        main_deck,
        ..Default::default()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Audience {
    Seat(PlayerId),
    Spectator,
    Unseated,
}

impl Audience {
    fn project(self, state: &GameState) -> GameState {
        match self {
            Self::Seat(player) => filter_state_for_viewer(state, player),
            Self::Spectator => filter_state_for_viewer(state, PlayerId(u8::MAX)),
            Self::Unseated => filter_state_for_unseated_viewer(state),
        }
    }
}

#[derive(Default)]
struct Baselines {
    sender: Option<WireState>,
    receiver: Option<WireState>,
}

#[derive(Clone, Copy)]
enum SampleClass {
    Cold,
    Warm,
}

#[derive(Default)]
struct Samples {
    cold: Vec<u64>,
    warm: Vec<u64>,
}

impl Samples {
    fn record(&mut self, class: SampleClass, elapsed: Duration) {
        let samples = match class {
            SampleClass::Cold => &mut self.cold,
            SampleClass::Warm => &mut self.warm,
        };
        samples.push(elapsed.as_nanos().try_into().expect("duration fits u64"));
    }

    fn report(&self) -> serde_json::Value {
        fn summary(samples: &[u64]) -> serde_json::Value {
            let mut ordered = samples.to_vec();
            ordered.sort_unstable();
            let at = |index| ordered.get(index).copied().unwrap_or(0);
            serde_json::json!({
                "samples": ordered.len(), "total_ns": ordered.iter().sum::<u64>(),
                "median_ns": at(ordered.len() / 2),
                "p95_ns": at((ordered.len() * 95).div_ceil(100).saturating_sub(1)),
                "max_ns": ordered.last().copied().unwrap_or(0)
            })
        }
        serde_json::json!({"cold": summary(&self.cold), "warm": summary(&self.warm)})
    }
}

#[derive(Clone, Copy, Default, Serialize)]
struct Memory {
    json: usize,
    index: usize,
    hashes: usize,
}

impl Memory {
    fn add(self, rhs: Self) -> Self {
        Self {
            json: self.json + rhs.json,
            index: self.index + rhs.index,
            hashes: self.hashes + rhs.hashes,
        }
    }
}

#[derive(Default)]
struct MemorySamples {
    current: Memory,
    peak: Memory,
    sum: Memory,
    peak_total_bytes: usize,
    samples: usize,
}

impl MemorySamples {
    fn record(&mut self, memory: Memory) {
        self.current = memory;
        self.peak = Memory {
            json: self.peak.json.max(memory.json),
            index: self.peak.index.max(memory.index),
            hashes: self.peak.hashes.max(memory.hashes),
        };
        self.sum = self.sum.add(memory);
        self.samples += 1;
        self.peak_total_bytes = self
            .peak_total_bytes
            .max(memory.json + memory.index + memory.hashes);
    }
    fn report(&self) -> serde_json::Value {
        let n = self.samples.max(1);
        serde_json::json!({
            "current_bytes": self.current, "component_peak_bytes": self.peak,
            "peak_total_bytes": self.peak_total_bytes,
            "mean_bytes": Memory { json: self.sum.json/n, index: self.sum.index/n, hashes: self.sum.hashes/n },
            "samples": self.samples
        })
    }
}

#[derive(Default)]
struct Recipient {
    // The uninterrupted measurement lane and reconnect delivery lane each keep
    // independent sender and decoded receiver authorities.
    continuous: Baselines,
    delivered: Baselines,
    updates: usize,
    full: usize,
    delta: usize,
    fallback: usize,
    distinct: usize,
    original_bytes: usize,
    full_bytes: usize,
    selected_bytes: usize,
    delta_bytes: usize,
    original_gzip: usize,
    full_gzip: usize,
    selected_gzip: usize,
    order_bytes: usize,
    operation_bytes: usize,
    hash_bytes: usize,
    delivered_bytes: usize,
    delivered_gzip: usize,
    reconnect_full: usize,
    reconnect_full_bytes: usize,
    reconnect_full_gzip: usize,
    last_kind: Option<FrameKind>,
    memory: MemorySamples,
    timings: BTreeMap<&'static str, Samples>,
}

impl Recipient {
    fn time(&mut self, name: &'static str, class: SampleClass, start: Instant) {
        self.timings
            .entry(name)
            .or_default()
            .record(class, start.elapsed());
    }
}

#[derive(Default)]
struct Coverage {
    states: usize,
    frames: usize,
    bytes: usize,
    largest: usize,
    codec_filter: Duration,
    recipients: BTreeMap<Audience, Recipient>,
    aggregate_memory: MemorySamples,
    reconnect_at: Option<usize>,
    seat_memory: MemorySamples,
    spectator_memory: MemorySamples,
    transitions: BTreeMap<&'static str, usize>,
    resets: usize,
    last_authoritative: Option<state_codec::StateHash>,
}

#[derive(Deserialize)]
struct EnvelopeMeasure<'a> {
    #[serde(borrow)]
    data: &'a RawValue,
}

#[derive(Default, Deserialize)]
struct OperationsMeasure<'a> {
    #[serde(borrow)]
    set: Option<&'a RawValue>,
    #[serde(borrow)]
    upsert: Option<&'a RawValue>,
    #[serde(borrow)]
    remove: Option<&'a RawValue>,
    #[serde(borrow)]
    order: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct PatchMeasure<'a> {
    #[serde(borrow)]
    message: Option<OperationsMeasure<'a>>,
    #[serde(borrow)]
    data: Option<OperationsMeasure<'a>>,
    #[serde(borrow)]
    state: Option<OperationsMeasure<'a>>,
    #[serde(borrow)]
    objects: Option<OperationsMeasure<'a>>,
}

fn operation_sizes(kind: FrameKind, envelope: &EnvelopeMeasure<'_>) -> (usize, usize, usize) {
    match kind {
        FrameKind::Full => (0, 0, 0),
        FrameKind::Delta => {
            #[derive(Deserialize)]
            struct Data<'a> {
                #[serde(borrow)]
                patch: &'a RawValue,
            }
            let data: Data<'_> = serde_json::from_str(envelope.data.get()).unwrap();
            let patch: PatchMeasure<'_> = serde_json::from_str(data.patch.get()).unwrap();
            let mut order = 0;
            let mut operations = 0;
            for scope in [patch.message, patch.data, patch.state, patch.objects]
                .into_iter()
                .flatten()
            {
                order += scope.order.map_or(0, |raw| raw.get().len());
                operations += [scope.set, scope.upsert, scope.remove]
                    .into_iter()
                    .flatten()
                    .map(|raw| raw.get().len())
                    .sum::<usize>();
            }
            // Raw string contents, excluding quotes/field names. Other framing
            // overhead is delta_bytes - order - operations - hash bytes.
            (order, operations, 128)
        }
    }
}

fn gzip_len(bytes: &[u8]) -> usize {
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(3));
    gzip.write_all(bytes).unwrap();
    gzip.finish().unwrap().len()
}

struct DeliveryContext<'a> {
    seed: u64,
    action: usize,
    audience: Audience,
    driver: &'a str,
}

fn check_delivery(
    result: Result<WireState, state_codec::CodecError>,
    baseline: Option<&WireState>,
    bytes: &[u8],
    expected: &WireState,
    observation: DeliveryContext<'_>,
) -> WireState {
    let DeliveryContext {
        seed,
        action,
        audience,
        driver: context,
    } = observation;
    match result {
        Ok(decoded)
            if decoded.as_bytes() == expected.as_bytes()
                && state_hash(&decoded) == state_hash(expected)
                && decoded.revision() == expected.revision() =>
        {
            decoded
        }
        other => {
            let dir = std::env::temp_dir().join(format!(
                "state-codec-{}-{seed}-{action}-{audience:?}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("expected.json"), expected.as_bytes()).unwrap();
            std::fs::write(dir.join("frame.json"), bytes).unwrap();
            if let Ok(decoded) = &other {
                std::fs::write(dir.join("decoded.json"), decoded.as_bytes()).unwrap();
            }
            std::fs::write(dir.join("context.txt"), format!(
                "seed={seed} audience={audience:?} action={action} context={context} base_revision={:?} base_hash={:?} result_revision={:?} result_hash={} error={other:?}",
                baseline.and_then(WireState::revision), baseline.map(state_hash), expected.revision(), state_hash(expected)
            )).unwrap();
            panic!("codec delivery mismatch; artifacts: {}", dir.display());
        }
    }
}

impl Coverage {
    fn observe(&mut self, state: &GameState, seed: u64, action: usize, context: &str) {
        let start = Instant::now();
        if self.recipients.is_empty() {
            for player in &state.players {
                self.recipients
                    .insert(Audience::Seat(player.id), Recipient::default());
            }
            self.recipients
                .insert(Audience::Spectator, Recipient::default());
            self.recipients
                .insert(Audience::Unseated, Recipient::default());
        }
        let mut aggregate = Memory::default();
        let mut seats = Memory::default();
        let mut spectators = Memory::default();
        for (&audience, recipient) in &mut self.recipients {
            let class = if recipient.updates == 0 {
                SampleClass::Cold
            } else {
                SampleClass::Warm
            };
            let production = Instant::now();
            let stage = Instant::now();
            let projected = audience.project(state);
            recipient.time("projection", class, stage);
            let stage = Instant::now();
            let wire = WireState::from_game_state(&projected).unwrap();
            recipient.time("serialization", class, stage);
            let stage = Instant::now();
            // Do not prepare the index before encode: its lazy work belongs to
            // this production-path duration, not an artificially warmed cache.
            let frame = encode(recipient.continuous.sender.as_ref(), &wire).unwrap();
            recipient.time("encode_including_lazy_index_hash", class, stage);
            let stage = Instant::now();
            let bytes = frame.to_bytes().unwrap();
            recipient.time("to_bytes", class, stage);
            let stage = Instant::now();
            let parsed = Frame::from_bytes(&bytes).unwrap();
            recipient.time("from_bytes", class, stage);
            let stage = Instant::now();
            let decoded = decode(recipient.continuous.receiver.as_ref(), &parsed);
            recipient.time("decode", class, stage);
            recipient.time("production_update_total", class, production);
            let stage = Instant::now();
            let decoded = check_delivery(
                decoded,
                recipient.continuous.receiver.as_ref(),
                &bytes,
                &wire,
                DeliveryContext {
                    seed,
                    action,
                    audience,
                    driver: context,
                },
            );
            recipient.distinct += usize::from(
                recipient
                    .continuous
                    .sender
                    .as_ref()
                    .is_some_and(|old| old.as_bytes() != wire.as_bytes()),
            );
            let envelope: EnvelopeMeasure<'_> = serde_json::from_slice(&bytes).unwrap();
            let kind = frame.kind();
            let (order, operations, hashes) = operation_sizes(kind, &envelope);
            recipient.order_bytes += order;
            recipient.operation_bytes += operations;
            recipient.hash_bytes += hashes;
            match kind {
                FrameKind::Full => {
                    recipient.full += 1;
                    recipient.fallback += usize::from(recipient.continuous.sender.is_some());
                }
                FrameKind::Delta => {
                    recipient.delta += 1;
                    recipient.delta_bytes += bytes.len();
                }
            }
            recipient.time("correctness_and_measurement_parsing", class, stage);
            let stage = Instant::now();
            let full = encode(None, &wire).unwrap().to_bytes().unwrap();
            assert!(
                bytes.len() <= full.len(),
                "selected frame must not exceed FULL"
            );
            recipient.time("forced_full_comparator", class, stage);
            let stage = Instant::now();
            recipient.original_gzip += gzip_len(wire.as_bytes());
            recipient.full_gzip += gzip_len(&full);
            recipient.selected_gzip += gzip_len(&bytes);
            recipient.time("gzip_three_lanes", class, stage);
            recipient.original_bytes += wire.as_bytes().len();
            recipient.full_bytes += full.len();
            recipient.selected_bytes += bytes.len();

            let stage = Instant::now();
            let reset =
                audience == Audience::Seat(PlayerId(0)) && self.reconnect_at == Some(self.states);
            if reset {
                recipient.delivered = Baselines::default();
                self.resets += 1;
            }
            let delivered = encode(recipient.delivered.sender.as_ref(), &wire)
                .unwrap()
                .to_bytes()
                .unwrap();
            let delivered_frame = Frame::from_bytes(&delivered).unwrap();
            let received = check_delivery(
                decode(recipient.delivered.receiver.as_ref(), &delivered_frame),
                recipient.delivered.receiver.as_ref(),
                &delivered,
                &wire,
                DeliveryContext {
                    seed,
                    action,
                    audience,
                    driver: context,
                },
            );
            let delivered_kind = delivered_frame.kind();
            if reset {
                assert_eq!(
                    delivered_kind,
                    FrameKind::Full,
                    "reset must explicitly request FULL"
                );
                recipient.reconnect_full += 1;
                recipient.reconnect_full_bytes += delivered.len();
                recipient.reconnect_full_gzip += gzip_len(&delivered);
            } else if self
                .reconnect_at
                .is_some_and(|at| self.states == at || self.states == at + 1)
            {
                assert_eq!(
                    delivered_kind,
                    FrameKind::Delta,
                    "other recipients continue; reset recipient resumes DELTA"
                );
            }
            recipient.last_kind = Some(delivered_kind);
            recipient.delivered_bytes += delivered.len();
            recipient.delivered_gzip += gzip_len(&delivered);
            recipient.delivered.receiver = Some(received);
            recipient.delivered.sender = Some(wire.clone());
            recipient.continuous.receiver = Some(decoded);
            recipient.continuous.sender = Some(wire.clone());
            recipient.time("reconnect_lane_correctness", class, stage);

            // Independent cold indexing/hash probe, outside encode and its
            // production-path timings. Reuses the projection bytes, not AI or
            // filtering; setup allocation is reported separately.
            let stage = Instant::now();
            let probe = WireState::from_json(wire.as_bytes()).unwrap();
            recipient.time("index_probe_setup", class, stage);
            let stage = Instant::now();
            let usage = probe.baseline_memory_usage();
            recipient.time("cold_baseline_index_hash_probe", class, stage);
            let memory = Memory {
                json: usage.json_bytes,
                index: usage.index_key_bytes,
                hashes: usage.object_hash_bytes,
            };
            recipient.memory.record(memory);
            aggregate = aggregate.add(memory);
            match audience {
                Audience::Seat(_) => seats = seats.add(memory),
                Audience::Spectator | Audience::Unseated => {
                    spectators = spectators.add(memory);
                }
            }
            recipient.updates += 1;
            self.frames += 1;
            self.bytes += bytes.len();
            self.largest = self.largest.max(bytes.len());
        }
        self.aggregate_memory.record(aggregate);
        self.seat_memory.record(seats);
        self.spectator_memory.record(spectators);
        self.last_authoritative = Some(state_hash(&WireState::from_game_state(state).unwrap()));
        self.states += 1;
        self.codec_filter += start.elapsed();
    }

    fn require_delta(&self, audiences: &[Audience]) {
        for audience in audiences {
            assert_eq!(
                self.recipients[audience].last_kind,
                Some(FrameKind::Delta),
                "transition DELTA reach guard: {audience:?}"
            );
        }
    }

    fn report(&self, seed: u64, actions: usize) {
        let profile = if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        };
        for (audience, r) in &self.recipients {
            assert!(
                r.full > 0 && r.delta > 0 && r.distinct > 0,
                "FULL/DELTA/distinct reach guard: {audience:?}"
            );
            let timings: BTreeMap<_, _> = r
                .timings
                .iter()
                .map(|(name, values)| (*name, values.report()))
                .collect();
            println!(
                "CODEC_MEASURE {}",
                serde_json::json!({
                    "seed": seed, "audience": format!("{audience:?}"), "actions": actions,
                    "profile": profile, "arch": std::env::consts::ARCH, "os": std::env::consts::OS,
                    "updates": r.updates, "full": r.full, "delta": r.delta, "fallback": r.fallback,
                    "distinct": r.distinct, "original_bytes": r.original_bytes,
                    "full_bytes": r.full_bytes, "selected_bytes": r.selected_bytes,
                    "original_gzip": r.original_gzip, "full_gzip": r.full_gzip, "selected_gzip": r.selected_gzip,
                    "selected_full_ratio": r.selected_bytes as f64 / r.full_bytes as f64,
                    "selected_gzip_original_ratio": r.selected_gzip as f64 / r.original_gzip as f64,
                    "selected_gzip_full_ratio": r.selected_gzip as f64 / r.full_gzip as f64,
                    "order_value_bytes": r.order_bytes, "operation_value_bytes": r.operation_bytes,
                    "hash_content_bytes": r.hash_bytes,
                    "delta_framing_bytes": r.delta_bytes - r.order_bytes - r.operation_bytes - r.hash_bytes,
                    "delivered_bytes": r.delivered_bytes, "reconnect_full": r.reconnect_full,
                    "delivered_gzip": r.delivered_gzip,
                    "reconnect_full_bytes": r.reconnect_full_bytes,
                    "reconnect_full_gzip": r.reconnect_full_gzip,
                    "reconnect_extra_bytes": r.delivered_bytes as i64 - r.selected_bytes as i64,
                    "reconnect_extra_gzip": r.delivered_gzip as i64 - r.selected_gzip as i64,
                    "memory": r.memory.report(), "timings": timings
                })
            );
        }
        println!(
            "CODEC_AGGREGATE {}",
            serde_json::json!({
                "seed": seed, "actions": actions, "updates": self.states, "viewer_frames": self.frames,
                "memory": self.aggregate_memory.report(), "resets": self.resets,
                "seat_memory": self.seat_memory.report(),
                "spectator_memory": self.spectator_memory.report(),
                "transitions": self.transitions,
                "original_bytes": self.recipients.values().map(|r|r.original_bytes).sum::<usize>(),
                "full_bytes": self.recipients.values().map(|r|r.full_bytes).sum::<usize>(),
                "selected_bytes": self.bytes,
                "original_gzip": self.recipients.values().map(|r|r.original_gzip).sum::<usize>(),
                "full_gzip": self.recipients.values().map(|r|r.full_gzip).sum::<usize>(),
                "selected_gzip": self.recipients.values().map(|r|r.selected_gzip).sum::<usize>()
            })
        );
    }
}

fn run(seed: u64, players: u8, cap: usize, require_terminal: bool) -> String {
    let start = Instant::now();
    let db = database();
    let list = DeckList {
        player: deck(&db, false),
        opponent: deck(&db, true),
        ai_decks: (2..players).map(|seat| deck(&db, seat % 2 == 1)).collect(),
        ..Default::default()
    };
    let payload = resolve_deck_list(&db, &list);
    let mut state = GameState::new(FormatConfig::free_for_all(), players, seed);
    load_and_hydrate_decks(&mut state, &payload, Some(&db));
    engine::game::engine::start_game(&mut state);
    let seats: HashSet<_> = (0..players).map(PlayerId).collect();
    let config = create_config_for_players(AiDifficulty::Easy, Platform::Native, players)
        .into_measurement(seed);
    let configs: HashMap<_, _> = seats.iter().map(|&seat| (seat, config.clone())).collect();
    let session = phase_ai::session::AiSession::arc_from_game(&state);
    let mut rng = StdRng::seed_from_u64(seed);
    let mut coverage = Coverage {
        reconnect_at: Some(16),
        ..Default::default()
    };
    coverage.observe(&state, seed, 0, "initial");
    let mut ai = Duration::ZERO;
    let mut driver_mark = Instant::now();
    let outcome = run_driver_loop(
        &mut state,
        &seats,
        &configs,
        &mut rng,
        &session,
        cap,
        &mut |batch, post, total| {
            ai += driver_mark.elapsed();
            let context = format!("{:?}", batch.stop);
            for (offset, result) in batch.results.iter().enumerate() {
                coverage.observe(&result.state, seed, total + offset + 1, &context);
            }
            let final_hash = state_hash(&WireState::from_game_state(post).unwrap());
            if coverage.last_authoritative != Some(final_hash) {
                coverage.observe(post, seed, total + batch.results.len(), &context);
            }
            driver_mark = Instant::now();
        },
    );
    ai += driver_mark.elapsed();
    let final_hash = state_hash(&WireState::from_game_state(&state).unwrap());
    if coverage.last_authoritative != Some(final_hash) {
        coverage.observe(
            &state,
            seed,
            outcome.total_actions,
            &format!("{:?}", outcome.exit),
        );
    }
    let terminal = matches!(state.waiting_for, WaitingFor::GameOver { .. });
    let report = format!("seed={seed} players={players} cap={cap} actions={} states={} viewer_frames={} bytes={} largest={} elapsed_s={:.3} codec_filter_s={:.3} ai_s={:.3} terminal={terminal} exit={:?} final_wait={:?}",
        outcome.total_actions, coverage.states, coverage.frames, coverage.bytes, coverage.largest,
        start.elapsed().as_secs_f64(), coverage.codec_filter.as_secs_f64(), ai.as_secs_f64(), outcome.exit, state.waiting_for);
    println!("{report}");
    coverage.report(seed, outcome.total_actions);
    assert!(
        coverage.frames >= 64,
        "insufficient self-play coverage: {report}"
    );
    assert!(
        outcome.total_actions > 0,
        "insufficient accepted states: {report}"
    );
    assert_eq!(coverage.resets, 1, "single-seat reconnect must be reached");
    assert_eq!(
        coverage.frames,
        coverage.states * (usize::from(players) + 2),
        "no audience sampling"
    );
    if require_terminal {
        assert!(
            terminal,
            "self-play coverage/setup failure, not a codec failure: {report}"
        );
    } else {
        assert!(
            terminal || outcome.total_actions == cap,
            "prefix coverage/setup failure: {report}"
        );
    }
    report
}

// Seed-88 measurement: 2p 26.308 s, 4p 225.722 s on the development Mac.
// CI shards can be slower, and previously completed games can later stall
// (#4878; phase-ai/src/duel_suite/perf.rs). Routine coverage therefore reaches
// 32 actions independently of full-game completion; no frames are sampled.
// Six per-case >=64-frame guards also establish the aggregate >=384 guard.
#[test]
fn prefix_2p_seed_88() {
    run(88, 2, 32, false);
}
#[test]
fn prefix_4p_seed_88() {
    run(88, 4, 32, false);
}
#[test]
fn prefix_2p_seed_188() {
    run(188, 2, 32, false);
}
#[test]
fn prefix_4p_seed_188() {
    run(188, 4, 32, false);
}
#[test]
fn prefix_2p_seed_288() {
    run(288, 2, 32, false);
}
#[test]
fn prefix_4p_seed_288() {
    run(288, 4, 32, false);
}

#[test]
#[ignore = "full-game acceptance; routine CI exercises the every-frame prefix"]
fn full_2p_seed_88() {
    run(88, 2, 10_000, true);
}
#[test]
#[ignore = "full-game acceptance; routine CI exercises the every-frame prefix"]
fn full_4p_seed_88() {
    run(88, 4, 10_000, true);
}
#[test]
#[ignore = "full-game acceptance; routine CI exercises the every-frame prefix"]
fn full_2p_seed_188() {
    run(188, 2, 10_000, true);
}
#[test]
#[ignore = "full-game acceptance; routine CI exercises the every-frame prefix"]
fn full_4p_seed_188() {
    run(188, 4, 10_000, true);
}
#[test]
#[ignore = "full-game acceptance; routine CI exercises the every-frame prefix"]
fn full_2p_seed_288() {
    run(288, 2, 10_000, true);
}
#[test]
#[ignore = "full-game acceptance; routine CI exercises the every-frame prefix"]
fn full_4p_seed_288() {
    run(288, 4, 10_000, true);
}

fn submit(runner: &mut GameRunner, coverage: &mut Coverage, action: GameAction) {
    let context = format!("{action:?}");
    runner
        .act(action)
        .unwrap_or_else(|error| panic!("{context}: {error:?}"));
    coverage.observe(runner.state(), 89, coverage.states, &context);
}

fn settle(runner: &mut GameRunner, coverage: &mut Coverage) {
    for _ in 0..40 {
        match runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => submit(runner, coverage, GameAction::PassPriority),
            ref other => panic!("unexpected scripted prompt: {other:?}"),
        }
    }
    panic!("script did not reach empty stack AND Priority");
}

fn cast_action(runner: &GameRunner, object_id: ObjectId) -> GameAction {
    GameAction::CastSpell {
        object_id,
        card_id: runner.state().objects[&object_id].card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    }
}

fn stable_board(scenario: &mut GameScenario, players: u8) {
    for seat in 0..players {
        for n in 0..12 {
            scenario.add_creature(
                PlayerId(seat),
                &format!("Stable seat {seat} permanent {n}"),
                2,
                3,
            );
        }
        scenario.add_card_to_hand(PlayerId(seat), &format!("Private seat {seat} card"));
    }
}

fn identity(state: &GameState, viewer: PlayerId, object: ObjectId) -> (String, usize, usize) {
    let projected = filter_state_for_viewer(state, viewer);
    let object = &projected.objects[&object];
    (
        object.name.clone(),
        object.abilities.len(),
        object.keywords.len(),
    )
}

#[test]
fn d10_individual_morph_prompts_reveal_and_return_to_hidden_hand() {
    // Oracle verified against Scryfall's named-card API; the codec reference is
    // the production viewer projection after each individual accepted action.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stable_board(&mut scenario, 2);
    let morph = scenario.add_creature_to_hand_from_oracle(P0, "Battering Craghorn", 3, 1,
        "First strike\nMorph {1}{R}{R} (You may cast this card face down as a 2/2 creature for {3}. Turn it face up any time for its morph cost.)")
        .with_mana_cost(ManaCost::Cost { shards: vec![ManaCostShard::Red, ManaCostShard::Red], generic: 2 }).id();
    let bounce = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Unsummon",
            true,
            "Return target creature to its owner's hand.",
        )
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        std::iter::repeat_n(ManaType::Red, 6)
            .chain(std::iter::repeat_n(ManaType::Blue, 6))
            .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    let mut coverage = Coverage::default();
    coverage.observe(runner.state(), 89, 0, "script initial");
    assert_eq!(identity(runner.state(), P0, morph).0, "Battering Craghorn");
    assert_eq!(
        identity(runner.state(), P1, morph),
        ("Hidden Card".to_owned(), 0, 0)
    );
    let action = cast_action(&runner, morph);
    submit(&mut runner, &mut coverage, action);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::AlternativeCastChoice { .. }
        ),
        "the alternative-choice projection must be observed"
    );
    coverage.require_delta(&[Audience::Seat(P0), Audience::Seat(P1)]);
    *coverage
        .transitions
        .entry("alternative_cast_prompt")
        .or_default() += 1;
    submit(
        &mut runner,
        &mut coverage,
        GameAction::ChooseAlternativeCast {
            choice: AlternativeCastDecision::Alternative,
        },
    );
    settle(&mut runner, &mut coverage);
    let down = runner
        .state()
        .objects
        .values()
        .find(|object| object.zone == Zone::Battlefield && object.face_down)
        .expect("face-down battlefield object")
        .id;
    assert_eq!(identity(runner.state(), P0, down).0, "Battering Craghorn");
    assert_ne!(identity(runner.state(), P1, down).0, "Battering Craghorn");
    assert_eq!(identity(runner.state(), P1, down).1, 0);
    coverage.require_delta(&[Audience::Seat(P0), Audience::Seat(P1)]);
    *coverage.transitions.entry("face_down").or_default() += 1;
    submit(
        &mut runner,
        &mut coverage,
        GameAction::TurnFaceUp {
            object_id: down,
            x: 0,
        },
    );
    settle(&mut runner, &mut coverage);
    assert!(!runner.state().objects[&down].face_down);
    let public = identity(runner.state(), P1, down);
    assert_eq!(public.0, "Battering Craghorn");
    assert!(
        public.2 > 0,
        "the observer really saw the public printed keywords"
    );
    coverage.require_delta(&[Audience::Seat(P0), Audience::Seat(P1)]);
    *coverage.transitions.entry("face_up").or_default() += 1;
    let action = cast_action(&runner, bounce);
    submit(&mut runner, &mut coverage, action);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "the target-choice projection must be observed"
    );
    coverage.require_delta(&[Audience::Seat(P0), Audience::Seat(P1)]);
    *coverage
        .transitions
        .entry("target_selection_prompt")
        .or_default() += 1;
    submit(
        &mut runner,
        &mut coverage,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(down)),
        },
    );
    settle(&mut runner, &mut coverage);
    let hand = runner
        .state()
        .objects
        .values()
        .find(|object| object.zone == Zone::Hand && object.name == "Battering Craghorn")
        .expect("the resolved bounce returned the creature")
        .id;
    assert_eq!(identity(runner.state(), P0, hand).0, "Battering Craghorn");
    assert_eq!(
        identity(runner.state(), P1, hand),
        ("Hidden Card".to_owned(), 0, 0)
    );
    // Inspect the *current object* in the decoded receiver state, not historical logs.
    let received = coverage.recipients[&Audience::Seat(P1)]
        .delivered
        .receiver
        .as_ref()
        .unwrap();
    let value: serde_json::Value = serde_json::from_str(received.as_json()).unwrap();
    assert_eq!(value["objects"][hand.0.to_string()]["name"], "Hidden Card");
    assert_eq!(
        value["objects"][hand.0.to_string()]["abilities"],
        serde_json::json!([])
    );
    assert_eq!(
        value["objects"][hand.0.to_string()]["keywords"],
        serde_json::json!([])
    );
    coverage.require_delta(&[Audience::Seat(P0), Audience::Seat(P1)]);
    *coverage.transitions.entry("returned_hidden").or_default() += 1;
    assert_eq!(coverage.frames, coverage.states * 4);
    coverage.report(89, coverage.states - 1);
}

#[test]
fn d11_concession_keeps_surviving_audience_authorities() {
    let mut scenario = GameScenario::new_with_format(FormatConfig::free_for_all(), 4, 89);
    scenario.at_phase(Phase::PreCombatMain);
    stable_board(&mut scenario, 4);
    let mut runner = scenario.build();
    let departing = PlayerId(2);
    let owned: Vec<_> = runner
        .state()
        .objects
        .values()
        .filter(|object| object.owner == departing)
        .map(|object| (object.id, object.zone))
        .collect();
    assert!(owned.iter().any(|(_, zone)| *zone == Zone::Battlefield));
    assert!(owned.iter().any(|(_, zone)| *zone == Zone::Hand));
    let survivor = runner
        .state()
        .objects
        .values()
        .find(|object| object.owner == PlayerId(3) && object.zone == Zone::Battlefield)
        .unwrap()
        .id;
    assert!(!runner.state().players[2].is_eliminated);
    let mut coverage = Coverage::default();
    coverage.observe(runner.state(), 89, 0, "four-seat initial");
    submit(&mut runner, &mut coverage, GameAction::PassPriority);
    coverage.require_delta(&[
        Audience::Seat(P0),
        Audience::Seat(P1),
        Audience::Seat(PlayerId(3)),
    ]);
    submit(
        &mut runner,
        &mut coverage,
        GameAction::Concede {
            player_id: departing,
        },
    );
    assert!(runner.state().players[2].is_eliminated);
    assert_eq!(
        runner
            .state()
            .players
            .iter()
            .filter(|player| !player.is_eliminated)
            .count(),
        3
    );
    assert!(owned.iter().all(|(id, old_zone)| runner
        .state()
        .objects
        .get(id)
        .is_none_or(|object| object.zone != *old_zone)));
    assert_eq!(runner.state().objects[&survivor].zone, Zone::Battlefield);
    coverage.require_delta(&[
        Audience::Seat(P0),
        Audience::Seat(P1),
        Audience::Seat(PlayerId(3)),
        Audience::Spectator,
        Audience::Unseated,
    ]);
    *coverage.transitions.entry("elimination").or_default() += 1;
    for _ in 0..2 {
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        submit(&mut runner, &mut coverage, GameAction::PassPriority);
        coverage.require_delta(&[
            Audience::Seat(P0),
            Audience::Seat(P1),
            Audience::Seat(PlayerId(3)),
            Audience::Spectator,
            Audience::Unseated,
        ]);
    }
    assert_eq!(
        coverage.recipients.len(),
        6,
        "audience keys are never reindexed after elimination"
    );
    assert_eq!(coverage.frames, coverage.states * 6);
    coverage.report(89, coverage.states - 1);
}
