use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::BufReader;
use std::time::{Duration, Instant};

use engine::database::CardDatabase;
use engine::game::deck_loading::{
    load_and_hydrate_decks, resolve_deck_list, DeckList, PlayerDeckList,
};
use engine::game::visibility::{filter_state_for_unseated_viewer, filter_state_for_viewer};
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::player::PlayerId;
use phase_ai::auto_play::run_driver_loop;
use phase_ai::config::{create_config_for_players, AiDifficulty, Platform};
use rand::{rngs::StdRng, SeedableRng};
use state_codec::{decode, encode, state_hash, Frame, WireState};

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

#[derive(Default)]
struct Coverage {
    states: usize,
    frames: usize,
    bytes: usize,
    largest: usize,
    codec_filter: Duration,
    baselines: Vec<Option<WireState>>,
    first_hash: Option<state_codec::StateHash>,
    changed: bool,
    last_authoritative: Option<state_codec::StateHash>,
}

impl Coverage {
    fn observe(&mut self, state: &GameState, seed: u64, action: usize, context: &str) {
        let start = Instant::now();
        let mut audiences: Vec<_> = state.players.iter().map(|p| Some(p.id)).collect();
        audiences.extend([Some(PlayerId(u8::MAX)), None]);
        self.baselines.resize(audiences.len(), None);
        for (index, viewer) in audiences.into_iter().enumerate() {
            let projected = match viewer {
                Some(player) => filter_state_for_viewer(state, player),
                None => filter_state_for_unseated_viewer(state),
            };
            let wire = WireState::from_game_state(&projected).unwrap();
            let hash = state_hash(&wire);
            if index == 0 {
                self.changed |= self.first_hash.is_some_and(|first| first != hash);
                self.first_hash.get_or_insert(hash);
            }
            let baseline = if self.states.is_multiple_of(2) {
                None
            } else {
                self.baselines[index].as_ref()
            };
            let bytes = encode(baseline, &wire).unwrap().to_bytes().unwrap();
            let decoded = decode(baseline, &Frame::from_bytes(&bytes).unwrap()).unwrap();
            if decoded.as_bytes() != wire.as_bytes() || state_hash(&decoded) != hash {
                let dir = std::env::temp_dir().join(format!(
                    "state-codec-{}-{seed}-{action}-{index}",
                    std::process::id()
                ));
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(dir.join("expected.json"), wire.as_bytes()).unwrap();
                std::fs::write(dir.join("decoded.json"), decoded.as_bytes()).unwrap();
                std::fs::write(
                    dir.join("context.txt"),
                    format!("seed={seed} audience={viewer:?} action={action} driver={context}"),
                )
                .unwrap();
                panic!("codec mismatch; artifacts: {}", dir.display());
            }
            self.frames += 1;
            self.bytes += bytes.len();
            self.largest = self.largest.max(bytes.len());
            self.baselines[index] = Some(wire);
        }
        self.last_authoritative = Some(state_hash(&WireState::from_game_state(state).unwrap()));
        self.states += 1;
        self.codec_filter += start.elapsed();
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
    let mut coverage = Coverage::default();
    coverage.observe(&state, seed, 0, "initial");
    let outcome = run_driver_loop(
        &mut state,
        &seats,
        &configs,
        &mut rng,
        &session,
        cap,
        &mut |batch, post, total| {
            let context = format!("{:?}", batch.stop);
            for (offset, result) in batch.results.iter().enumerate() {
                coverage.observe(&result.state, seed, total + offset + 1, &context);
            }
            let final_hash = state_hash(&WireState::from_game_state(post).unwrap());
            if coverage.last_authoritative != Some(final_hash) {
                coverage.observe(post, seed, total + batch.results.len(), &context);
            }
        },
    );
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
    let report = format!("seed={seed} players={players} cap={cap} actions={} states={} viewer_frames={} bytes={} largest={} elapsed_s={:.3} codec_filter_s={:.3} terminal={terminal} exit={:?} final_wait={:?}",
        outcome.total_actions, coverage.states, coverage.frames, coverage.bytes, coverage.largest,
        start.elapsed().as_secs_f64(), coverage.codec_filter.as_secs_f64(), outcome.exit, state.waiting_for);
    println!("{report}");
    assert!(
        coverage.frames >= 64,
        "insufficient self-play coverage: {report}"
    );
    assert!(
        outcome.total_actions > 0 && coverage.changed,
        "insufficient accepted/distinct states: {report}"
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
