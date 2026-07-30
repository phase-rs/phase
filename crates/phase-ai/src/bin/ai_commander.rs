//! Sanity-check runner: four-player commander game driven entirely by the AI.
//!
//! Loads four commander precons from `feeds/commander-precons.json`, sets up
//! a 4-player commander GameState, and drives every player with the native
//! AI until the game ends (or an action budget is hit). Reports per-turn
//! life totals and the final outcome.
//!
//! Usage (pod-lab loop-3 Q5): `--profile server-release`, not plain
//! `--release` -- `[profile.release]` (workspace `Cargo.toml`) sets `panic =
//! 'abort'` (it exists to keep the WASM build small), which silently defeats
//! `run_batch_isolated`'s `catch_unwind`-based per-game panic isolation
//! below: under `abort`, one game's panic takes the whole batch process down
//! instead of being caught and reported. `server-release` inherits `release`
//! but overrides `panic = 'unwind'` for exactly this reason.
//!   cargo run --profile server-release --bin ai-commander -- client/public
//!   cargo run --profile server-release --bin ai-commander -- client/public --seed 7 --difficulty Easy
//!   cargo run --profile server-release --bin ai-commander -- client/public --difficulty Easy \
//!       --difficulty-p2 VeryHard --action-cap 50000
//!
//! Batch mode (pod-lab simulation-acceleration plan, Tier 1 item 1): play many
//! games in one process instead of one process per game, amortizing the card
//! database load (~1.8s/game measured) across the whole batch. `--games-file`
//! points at a file of `seed,difficulty` lines (one game per line); when it is
//! given, `--seed`/`--difficulty` are ignored and every line is played in turn
//! against the SAME loaded database and feed. Each game's play-through is
//! panic-isolated (`run_batch_isolated`) and its result is flushed to stdout
//! immediately, so a batch survives an individual game panicking or the whole
//! process being killed mid-batch (pod-lab enforces an external wall-clock
//! timeout on the process) -- but only under a `panic = 'unwind'` profile;
//! see the note above.
//!   cargo run --profile server-release --bin ai-commander -- client/public --games-file games.txt

// pod-lab loop-3 Q5: native-binary throughput lever, gated in Cargo.toml so
// wasm32 builds of this crate's lib (pulled in by engine-wasm/draft-wasm)
// never see it.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::panic::PanicHookInfo;
use std::path::PathBuf;
use std::time::Instant;

use engine::database::CardDatabase;
use engine::game::deck_loading::{
    load_deck_into_state, resolve_deck_list, DeckList, DeckPayload, PlayerDeckList,
};
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::player::PlayerId;
use phase_ai::auto_play::{run_driver_loop, DriverExit};
use phase_ai::config::{
    create_config_for_players, AiConfig, AiDifficulty, Platform, ACCEPTED_DIFFICULTY_LABELS,
};
use rand::rngs::StdRng;
use rand::SeedableRng;

const DEFAULT_ACTION_CAP: usize = 200_000;

/// Stack size for the thread that actually drives games. Deep AI search
/// recursion (even at Easy difficulty — directly reproduced this session:
/// every seed tried overflowed the plain main thread's default stack
/// immediately after game start, under this crate's normal release profile)
/// exceeds the platform default. Mirrors the identical fix already
/// established in `duel_suite::run` for the same reason. Batching (this
/// file's `--games-file` mode) makes this *more* important, not less: more
/// cumulative execution per process is more chances to hit a deep-recursion
/// case, even though no single game's peak depth changes.
const GAME_THREAD_STACK_SIZE: usize = 32 << 20;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // Harness escape hatch (pod-lab): forces the run into measurement mode
    // even on the single-game route. Read exactly once, here, and threaded
    // through as a plain `bool` — this repo's "no `set_var`" rule.
    // `parse_cli` decides what to do with it.
    // Contract: presence-based, matching PHASE_DUMP_*'s convention -- var
    // set to any value (including "") counts as true, not `== "1"`. Presence
    // only, so `var_os` (no Unicode validation needed for a bool check).
    let measurement_env = std::env::var_os("PHASE_AI_MEASUREMENT").is_some();

    let cli = match parse_cli(&args, measurement_env) {
        Ok(cli) => cli,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

    // Argument parsing and validation above needs no special stack; the actual
    // game-driving work (both the single-game path and `--games-file` batch
    // mode) does — see `GAME_THREAD_STACK_SIZE`. Spawning unconditionally
    // (rather than only under `--games-file`) fixes the pre-existing
    // single-game overflow too, not just the batch case.
    let handle = std::thread::Builder::new()
        .name("ai-commander-driver".to_string())
        .stack_size(GAME_THREAD_STACK_SIZE)
        .spawn(move || run(cli))
        .expect("failed to spawn game-driving thread");

    // A non-batch (`games_file.is_none()`) game that panics is NOT caught (see
    // `run`'s doc) — it unwinds only the spawned thread, which by itself would
    // leave the process silently exiting 0. Detect that here and exit 101, the
    // same code an uncaught panic on the plain main thread would have produced
    // before this thread split existed, so a caller keying off exit status sees
    // an unmistakable failure either way.
    let exit_code = handle.join().unwrap_or(101);
    std::process::exit(exit_code);
}

/// Bundles the parsed CLI arguments `run` needs. A plain struct (not more
/// positional params on `run`) so the seam between "parse args" (plain main
/// thread) and "drive games" (large-stack spawned thread) stays one value to
/// move across the `thread::spawn` boundary, not eight.
struct CliArgs {
    cards_path: String,
    feed: String,
    seed: u64,
    difficulty: AiDifficulty,
    seat_difficulty: [Option<AiDifficulty>; 4],
    action_cap: usize,
    games_file: Option<String>,
    batch_games: Option<Vec<(u64, AiDifficulty)>>,
    watch_cards: HashSet<String>,
    run_context: RunContext,
}

/// Which execution mode every seat's `AiConfig` should run under for a given
/// process (see `build_seat_config`). `Measurement` disables the wall-clock
/// search deadline so every decision is a pure function of the game's inputs
/// (see `build_seat_config`'s doc for why that matters); `Interactive` leaves
/// the deadline in place.
///
/// Route table (`parse_cli`): `--games-file` (batch) always resolves to
/// `Measurement` (the cross-game wall-clock-deadline leak fix, see
/// `build_seat_config`'s doc, applies regardless of the env override). A
/// solo invocation resolves to `Interactive` by default, unless the
/// `PHASE_AI_MEASUREMENT` harness escape hatch forces it to `Measurement`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunContext {
    Interactive,
    Measurement,
}

/// Pure argument parser: consumes the raw process args plus the one
/// env-derived flag `main` reads once (`measurement_env`, from
/// `PHASE_AI_MEASUREMENT` -- presence-based, matching `PHASE_DUMP_*`
/// convention, not `== "1"`) and returns either a fully resolved `CliArgs`
/// or an error message ready for the caller to print verbatim before exiting
/// with status 1. Contains the entire pre-`run` pre-pass (positional
/// `cards_path` resolution, the flag loop, and `--games-file` validation)
/// that used to live directly in `main`, so `main` itself does nothing but
/// read the env var, call this, and report success/failure. Never calls
/// `std::process::exit` itself -- every error path here becomes an `Err`
/// instead, exactly mirroring the direct-exit message each replaces.
fn parse_cli(args: &[String], measurement_env: bool) -> Result<CliArgs, String> {
    let mut cards_path: Option<String> = None;

    let mut seed: u64 = 42;
    let mut difficulty = AiDifficulty::Easy;
    // Per-seat override of `difficulty` (pod-lab gauntlet mixed-skill tables).
    // `None` means "use the table-wide --difficulty for this seat".
    let mut seat_difficulty: [Option<AiDifficulty>; 4] = [None; 4];
    let mut action_cap: usize = DEFAULT_ACTION_CAP;
    let mut feed: String = "feeds/mtggoldfish-commander.json".to_string();
    let mut games_file: Option<String> = None;
    // pod-lab swap-liveness telemetry (loop-3 Q3(b)): empty means the
    // per-event scan in `play_one_game` is skipped entirely, not merely a
    // no-op HashSet lookup — a run that doesn't pass this flag pays nothing.
    let mut watch_cards: HashSet<String> = HashSet::new();
    let mut args_iter = args.iter().skip(1).peekable();
    while let Some(arg) = args_iter.next() {
        match arg.as_str() {
            "--seed" => {
                if let Some(v) = args_iter.next() {
                    if let Ok(n) = v.parse::<u64>() {
                        seed = n;
                    }
                }
            }
            "--difficulty" => {
                if let Some(v) = args_iter.next() {
                    difficulty = parse_difficulty(v);
                }
            }
            "--action-cap" => {
                if let Some(v) = args_iter.next() {
                    action_cap = parse_action_cap(v);
                }
            }
            "--feed" => {
                if let Some(v) = args_iter.next() {
                    feed = v.clone();
                }
            }
            "--games-file" => match args_iter.next() {
                Some(v) => games_file = Some(v.clone()),
                None => return Err("error: --games-file requires a path".to_string()),
            },
            "--watch-cards" => match args_iter.next() {
                Some(v) => {
                    watch_cards = v
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect();
                }
                None => {
                    return Err(
                        "error: --watch-cards requires a comma-separated card name list"
                            .to_string(),
                    );
                }
            },
            other => {
                // `--difficulty-p0` .. `--difficulty-p3`: single-seat override,
                // parameterized on seat index rather than four bespoke flags.
                // The value arg is consumed unconditionally so a bad index can't
                // leak its label into the catch-all; `parse_seat_override`
                // hard-fails (like the sibling parsers) instead of silently
                // dropping a mistyped flag and running a mislabeled seat.
                if let Some(suffix) = other.strip_prefix("--difficulty-p") {
                    let value = args_iter.next().map(String::as_str);
                    match parse_seat_override(suffix, value) {
                        Ok((idx, label)) => seat_difficulty[idx] = Some(parse_difficulty(label)),
                        Err(e) => return Err(e),
                    }
                } else if !other.starts_with("--") {
                    // F4: first non-`--`-prefixed token is the positional
                    // `cards_path`. This arm only sees tokens that were NOT
                    // consumed as a value by one of the flag arms above, so a
                    // preceding `--flag value` pair's value can never land
                    // here mistaken for the positional path. A bare unknown
                    // `--flag` (still starts with `--`) falls through this
                    // `else if` and is silently ignored without consuming the
                    // next token, so it can't eat the real positional either.
                    if cards_path.is_none() {
                        cards_path = Some(other.to_string());
                    }
                }
            }
        }
    }

    let cards_path = cards_path.unwrap_or_else(|| "client/public".to_string());

    // `--games-file` batch entries are validated up front — same hard-fail-at-
    // startup discipline as `parse_difficulty`/`parse_action_cap`/
    // `parse_seat_override` above: a garbled batch file should fail before the
    // ~1.8s database load, not silently skip or misinterpret one line mid-batch.
    let batch_games: Option<Vec<(u64, AiDifficulty)>> = match games_file.as_deref() {
        None => None,
        Some(path) => match parse_games_file(path) {
            Ok(games) if games.is_empty() => {
                return Err(format!("error: --games-file {path:?} contains no games"));
            }
            Ok(games) => Some(games),
            Err(e) => return Err(format!("error: {e}")),
        },
    };

    // F3 route table: `--games-file` (batch) always runs every seat in
    // Measurement mode (cross-game wall-clock-deadline leak fix, see
    // `build_seat_config`'s doc). A solo invocation defaults to Interactive
    // (real wall-clock deadline preserved) unless the `PHASE_AI_MEASUREMENT`
    // harness escape hatch forces it to Measurement too.
    let run_context = if batch_games.is_some() || measurement_env {
        RunContext::Measurement
    } else {
        RunContext::Interactive
    };

    Ok(CliArgs {
        cards_path,
        feed,
        seed,
        difficulty,
        seat_difficulty,
        action_cap,
        games_file,
        batch_games,
        watch_cards,
        run_context,
    })
}

/// Shared immutable inputs for one or more AI-commander game runs.
#[derive(Clone, Copy)]
struct GameRunContext<'a> {
    db: &'a CardDatabase,
    payload: &'a DeckPayload,
    seat_difficulty: &'a [Option<AiDifficulty>; 4],
    action_cap: usize,
    dump_log_path: Option<&'a str>,
    dump_actions_path: Option<&'a str>,
    watch_cards: &'a HashSet<String>,
    run_context: RunContext,
}

/// Everything that isn't argument parsing: loads the card database and feed
/// once, resolves the shared deck payload once, then either plays one game
/// (`batch_games.is_none()`, the pre-existing single-game path — its stdout is
/// byte-identical to before this function existed) or drives `--games-file`
/// batch mode. Runs entirely on the large-stack thread `main` spawns. Returns
/// the process exit code `main` should propagate.
fn run(cli: CliArgs) -> i32 {
    let CliArgs {
        cards_path,
        feed,
        seed,
        difficulty,
        seat_difficulty,
        action_cap,
        games_file,
        batch_games,
        watch_cards,
        run_context,
    } = cli;

    let export_path = PathBuf::from(&cards_path).join("card-data.json");
    let db = match CardDatabase::from_export(&export_path) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("failed to load {}: {e}", export_path.display());
            std::process::exit(1);
        }
    };

    let feed_path = PathBuf::from(&cards_path).join(&feed);
    let feed_file = match std::fs::File::open(&feed_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("failed to open {}: {e}", feed_path.display());
            std::process::exit(1);
        }
    };
    let feed_json: serde_json::Value =
        serde_json::from_reader(feed_file).expect("feed is not valid JSON");

    let decks_json = feed_json["decks"].as_array().expect("feed.decks missing");

    println!("=== 4-player Commander AI test ===");
    println!("Feed: {feed}");
    match &batch_games {
        // `--games-file` ignores --seed/--difficulty (per-game values come
        // from the file instead), so echoing the unused top-level seed/
        // difficulty here would be misleading. Single-game mode's line below
        // is unchanged from before batch mode existed.
        Some(games) => println!(
            "Batch mode: {} games from {}",
            games.len(),
            games_file.as_deref().unwrap_or("<unknown>")
        ),
        None => println!("Seed: {seed}   Difficulty: {difficulty:?}"),
    }
    println!();
    // D1 must-pass gate for B3: pod-lab's harness greps for this exact
    // lowercase literal (not `{run_context:?}`) to distinguish a solo
    // Interactive-route game from a Measurement-route one. Appended after
    // the blank line that closes the pre-existing pinned preamble (see
    // `single_game_stdout_is_deterministic_and_preamble_is_pinned`'s
    // `starts_with` assertion), so it never conflicts with that pin.
    match run_context {
        RunContext::Interactive => println!("ExecutionMode: interactive"),
        RunContext::Measurement => println!("ExecutionMode: measurement"),
    }

    let mut deck_lists: Vec<PlayerDeckList> = Vec::new();
    // Commander names are populated in PlayerDeckList.commander and resolved
    // by the pipeline — no manual tracking needed.
    for deck in decks_json.iter() {
        if deck_lists.len() == 4 {
            break;
        }
        let deck_name = deck["name"].as_str().unwrap_or("<unnamed>");
        // Two feed conventions:
        //  • Precon-style: `commander: ["Card Name"]` is an array of commander names.
        //  • MTGGoldfish-style: `commander` is null and the deck `name` IS the
        //    commander card name (included in `main`).
        let cmd_names: Vec<String> = match deck["commander"].as_array() {
            Some(arr) if !arr.is_empty() => arr
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect(),
            _ => vec![deck_name.to_string()],
        };
        let primary_cmd = cmd_names[0].clone();

        if db.get_face_by_name(&primary_cmd).is_none() {
            println!("  SKIP {deck_name}: commander '{primary_cmd}' not in card db");
            continue;
        }

        let mut main: Vec<String> = Vec::new();
        for entry in deck["main"].as_array().unwrap() {
            let n = entry["name"].as_str().unwrap();
            let count = entry["count"].as_u64().unwrap() as usize;
            if cmd_names.iter().any(|c| c == n) {
                continue;
            }
            for _ in 0..count {
                main.push(n.to_string());
            }
        }

        println!(
            "  {deck_name}  |  commander: {primary_cmd}  |  main: {} cards",
            main.len()
        );
        deck_lists.push(PlayerDeckList {
            main_deck: main,
            sideboard: vec![],
            commander: cmd_names,
            ..Default::default()
        });
    }

    if deck_lists.len() < 4 {
        eprintln!("need at least 4 precons, found {}", deck_lists.len());
        std::process::exit(1);
    }

    let deck_list = DeckList {
        player: deck_lists[0].clone(),
        opponent: deck_lists[1].clone(),
        ai_decks: vec![deck_lists[2].clone(), deck_lists[3].clone()],
        ..Default::default()
    };
    let payload: DeckPayload = resolve_deck_list(&db, &deck_list);

    // Post-resolution deck-count line (plan §3.9): `resolve_deck_list` silently
    // skips any name the card database doesn't recognize, so the pre-resolution
    // "main: N cards" print above can't prove a deck actually loaded full-size.
    // Print the resolved per-seat counts too, so a harness parsing stdout can
    // detect silent-skip drift (a resolved count short of the pre-resolution
    // one) without needing engine internals.
    println!("Resolved deck sizes (post name-resolution, 0-indexed by seat):");
    for (i, seat) in [&payload.player, &payload.opponent]
        .into_iter()
        .chain(payload.ai_decks.iter())
        .enumerate()
    {
        let main_count: u32 = seat.main_deck.iter().map(|e| e.count).sum();
        let commander_count: u32 = seat.commander.iter().map(|e| e.count).sum();
        println!("  P{i}  main={main_count:>3}  commander={commander_count}");
    }
    println!();

    // Read once — shared for the whole process (including every game in a
    // batch). NOTE: in `--games-file` mode, if these are set, each game's dump
    // overwrites the previous game's file at the same path (last game wins);
    // pod-lab's harness does not set these env vars for batch runs, and fixing
    // per-game dump paths is out of scope for the batching change itself.
    let dump_log_path = read_dump_env("PHASE_DUMP_LOG");
    let dump_actions_path = read_dump_env("PHASE_DUMP_ACTIONS");
    let game_context = GameRunContext {
        db: &db,
        payload: &payload,
        seat_difficulty: &seat_difficulty,
        action_cap,
        dump_log_path: dump_log_path.as_deref(),
        dump_actions_path: dump_actions_path.as_deref(),
        watch_cards: &watch_cards,
        run_context,
    };

    match batch_games {
        None => {
            let outcome = play_one_game(&game_context, seed, difficulty);
            match outcome {
                RunOutcome::Completed => 0,
                RunOutcome::Aborted => 2,
                RunOutcome::Stalled => 3,
            }
        }
        Some(games) => {
            run_batch_isolated(
                &games,
                |(seed, _)| seed.to_string(),
                |&(seed, seat_diff)| {
                    println!("--- GAME seed={seed} difficulty={seat_diff:?} ---");
                    play_one_game(&game_context, seed, seat_diff);
                    // Immediate per-game flush (Tier 1 item 3): if the process
                    // is killed mid-batch (e.g. pod-lab's external wall-clock
                    // timeout on a hung game), every already-completed game's
                    // result must already be on stdout, not buffered.
                    let _ = std::io::stdout().flush();
                },
            );
            // The whole point of batch mode is that one bad game must not take
            // the process down; individual game outcomes are already on stdout
            // (RESULT/ABORT/STALL/PANICKED lines) for the harness to parse per
            // line, so the process exit code just reports "the batch loop
            // itself ran to completion", not any one game's outcome.
            0
        }
    }
}

/// Drives one complete game: builds a fresh `GameState` from `payload` at
/// `seed`, resolves per-seat difficulty, runs the AI driver loop to
/// completion/cap/stall, and prints the exact `=== RESULT ===` epilogue the
/// single-game path always printed (before batch mode existed, this was the
/// tail of `main` — the code here is unchanged from that except for returning
/// `RunOutcome` instead of calling `std::process::exit` directly, so a batch
/// caller can keep looping instead of exiting on this game's outcome). Called
/// once for the single-game path and once per line for `--games-file` batch
/// mode — the SAME function drives both, so batching can never change what one
/// game's play-through does or prints.
fn play_one_game(context: &GameRunContext<'_>, seed: u64, difficulty: AiDifficulty) -> RunOutcome {
    let GameRunContext {
        db,
        payload,
        seat_difficulty,
        action_cap,
        dump_log_path,
        dump_actions_path,
        watch_cards,
        run_context,
    } = *context;
    let mut state = build_game_state(db, payload, seed);

    engine::game::engine::start_game(&mut state);
    println!();
    println!("Game started. {} players.", state.players.len());
    println!();

    let ai_players: HashSet<PlayerId> = (0..4).map(|i| PlayerId(i as u8)).collect();
    // Effective per-seat difficulty is echoed (not just the table-wide
    // --difficulty) so the resolved tier each seat actually plays at is an
    // operator-visible artifact of the run — silent drift onto the wrong
    // tier is the phase#6080 failure class.
    println!("Per-seat difficulty (0-indexed by seat):");
    let mut ai_configs: HashMap<PlayerId, AiConfig> = HashMap::new();
    for (i, override_diff) in seat_difficulty.iter().enumerate() {
        let seat_diff = override_diff.unwrap_or(difficulty);
        println!("  P{i}  difficulty={seat_diff:?}");
        ai_configs.insert(
            PlayerId(i as u8),
            build_seat_config(seat_diff, seed, run_context),
        );
    }
    println!();

    let start = Instant::now();
    let mut game_log: Vec<engine::types::log::GameLogEntry> = Vec::new();
    let mut actions_log: Vec<String> = Vec::new();
    // pod-lab swap-liveness telemetry (loop-3 Q3(b)): which of `watch_cards`'
    // names were ever drawn to hand or cast this game. Names, not `CardId`s —
    // `CardId` is assigned per physical-card-object at deck load
    // (`deck_loading.rs`), not a stable per-name identity, and every
    // `GameObject` already carries its own resolved `name`, so matching on
    // name needs no extra database lookup.
    let mut cards_seen: HashSet<String> = HashSet::new();
    let mut last_turn_reported: u32 = 0;
    let mut ai_rng = StdRng::seed_from_u64(seed);
    let ai_session = phase_ai::session::AiSession::arc_from_game(&state);
    // Tracks each seat's is_eliminated (the engine already flips this per
    // CR 800.4 when a player leaves the game) so we print exactly one line
    // per elimination event instead of re-announcing an already-eliminated
    // seat every batch.
    let mut was_eliminated = [false; 4];

    // The batch / remaining-budget boundary is owned by `run_driver_loop` (the
    // single authority — see its doc). This observer carries every per-batch
    // side effect 1:1; it must NOT read the outer `state`/total (both are owned
    // by the helper for the duration of the call). It reads the observer's
    // `state` arg (post-batch) and `total_before` (the PRE-batch running total,
    // which the turn line and ELIMINATED lines both printed before).
    let outcome = run_driver_loop(
        &mut state,
        &ai_players,
        &ai_configs,
        &mut ai_rng,
        &ai_session,
        action_cap,
        &mut |results, state, total_before| {
            if dump_log_path.is_some() {
                for r in &mut *results {
                    game_log.extend(std::mem::take(&mut r.log_entries));
                }
            }
            if dump_actions_path.is_some() {
                for r in &*results {
                    actions_log.push(format!("{:?}", r.action));
                }
            }

            if !watch_cards.is_empty() {
                record_watched_cards(results, watch_cards, &mut cards_seen);
            }

            if state.turn_number != last_turn_reported {
                last_turn_reported = state.turn_number;
                let snapshot: Vec<String> = state
                    .players
                    .iter()
                    .enumerate()
                    .map(|(i, p)| format!("P{i}:{}", p.life))
                    .collect();
                println!(
                    "Turn {:>2} (active P{})  actions={:>6}  elapsed={:>6.1}s  {}",
                    state.turn_number,
                    state.active_player.0,
                    total_before,
                    start.elapsed().as_secs_f64(),
                    snapshot.join(" ")
                );
                let _ = std::io::stdout().flush();
            }

            // Print one line the moment a seat's is_eliminated first flips, with
            // turn + wall-clock context so a gauntlet harness can distinguish an
            // expected early elimination from a stall or a perf regression
            // without re-deriving it from the per-turn life snapshots above.
            for (i, was) in was_eliminated.iter_mut().enumerate() {
                if !*was && state.players[i].is_eliminated {
                    *was = true;
                    println!(
                        "ELIMINATED: P{i}  turn={}  actions={:>6}  elapsed={:.1}s",
                        state.turn_number,
                        total_before,
                        start.elapsed().as_secs_f64()
                    );
                    let _ = std::io::stdout().flush();
                }
            }
        },
    );

    let total_actions = outcome.total_actions;
    let aborted = matches!(outcome.exit, DriverExit::CapReached);
    // phase#6080: the reason the driver broke early (one of the batch break
    // doors), so a stall can be diagnosed from the game output alone instead of
    // a `tracing::error` no harness captures. A cap abort carries no break door.
    let last_break_reason = match outcome.exit {
        DriverExit::BatchBreak(reason) => Some(reason),
        DriverExit::CapReached => None,
    };
    // STDOUT PARITY: the abort path prints a blank line + the ABORT line here,
    // immediately after the driver returns and BEFORE the unconditional blank +
    // "=== RESULT ===" epilogue below — so an abort prints two blank lines
    // (this one and the epilogue's), exactly as the pre-refactor in-loop abort did.
    if aborted {
        println!();
        println!("ABORT: hit action cap={action_cap}");
    }

    let elapsed = start.elapsed();
    println!();
    println!("=== RESULT ===");
    println!("Elapsed: {:.1}s", elapsed.as_secs_f64());
    println!("Total actions: {total_actions}");
    println!("Turns played: {}", state.turn_number);
    // pod-lab swap-liveness telemetry (loop-3 Q3(b)): one line, only when
    // `--watch-cards` was given, so a harness that doesn't ask for this pays
    // nothing and every existing consumer's line-by-line parse is untouched.
    // `PODLAB-TELEM ` is a prefix no other line in this binary's stdout uses,
    // and this line does not begin with "Turn ", match `^--- GAME`, or
    // contain "Winner: " / "Difficulty: " / "ABORT: hit " / "did NOT reach
    // GameOver" (pod-lab's `runner.py`/`mechanisms.py` scan for exactly those
    // literals). Cards are sorted for a deterministic, diff-friendly line.
    if !watch_cards.is_empty() {
        let mut seen: Vec<&str> = cards_seen.iter().map(String::as_str).collect();
        seen.sort_unstable();
        println!("PODLAB-TELEM {}", serde_json::json!({ "cards_seen": seen }));
    }
    println!();

    let outcome = classify_run_outcome(aborted, &state.waiting_for);
    match outcome {
        RunOutcome::Completed => {
            // `classify_run_outcome` only returns `Completed` for `GameOver`;
            // the fallthrough arm is unreachable and never prints.
            let winner = match &state.waiting_for {
                WaitingFor::GameOver { winner } => *winner,
                _ => None,
            };
            println!(
                "Game ended cleanly. Winner: {}",
                winner.map_or("draw".to_string(), |p| format!("P{}", p.0))
            );
        }
        // An action-cap abort already printed its own ABORT line above and is a
        // distinct, already-diagnosed outcome — it deliberately skips the
        // softlock STALL block (which asserts `last_break_reason` is unknown,
        // never set on the abort door) so an abort is never misreported as an
        // unexplained stall. Only the shared "did NOT reach GameOver" line prints.
        RunOutcome::Aborted => {
            println!(
                "Game did NOT reach GameOver. waiting_for = {:?}",
                state.waiting_for
            );
        }
        RunOutcome::Stalled => {
            // phase#6080: an empty AI-action batch while parked on anything but
            // GameOver is a driver stall, not a normal game end (the family of
            // p0-softlock issues #5250/#4345/#5958/#6172/#3886/#3919/#3233).
            // Print a machine-readable line with enough context to reproduce
            // (waiting_for variant, turn, active/priority player, pending-cast
            // summary) plus which break door fired, then exit with a distinct
            // code — the caller must not silently treat this as a completed game.
            println!(
                "STALL: waiting_for={} turn={} active=P{} priority=P{} pending_cast={}",
                state.waiting_for.variant_name(),
                state.turn_number,
                state.active_player.0,
                state.priority_player.0,
                state
                    .pending_cast
                    .as_ref()
                    .map(|pc| format!(
                        "object={:?} card={:?} variant={:?}",
                        pc.object_id, pc.card_id, pc.casting_variant
                    ))
                    .unwrap_or_else(|| "none".to_string()),
            );
            match &last_break_reason {
                Some(reason) => println!("STALL: break_reason={reason:?}"),
                None => println!(
                    "STALL: break_reason=unknown (run_ai_actions batch was non-empty; \
                     stall detected on a later empty batch this process did not observe)"
                ),
            }
            // Preserved verbatim: pod-lab's runner.py classifies outcomes by
            // matching this exact substring in stdout — do not reword it.
            // Printed in both the abort and stall cases (kept identical
            // deliberately, see comment above).
            println!(
                "Game did NOT reach GameOver. waiting_for = {:?}",
                state.waiting_for
            );
        }
    }

    println!();
    for (i, p) in state.players.iter().enumerate() {
        let bf_count = state
            .battlefield
            .iter()
            .filter(|oid| {
                state
                    .objects
                    .get(oid)
                    .map(|o| o.owner == PlayerId(i as u8))
                    .unwrap_or(false)
            })
            .count();
        println!(
            "  P{i}  life={:>4}  hand={:>2}  library={:>3}  graveyard={:>3}  battlefield={:>3}",
            p.life,
            p.hand.len(),
            p.library.len(),
            p.graveyard.len(),
            bf_count
        );
    }

    if let Some(path) = dump_actions_path {
        std::fs::write(path, actions_log.join("\n")).expect("write actions dump");
        println!("Dumped {} actions to {path}", actions_log.len());
    }
    if let Some(path) = dump_log_path {
        let json = serde_json::to_string(&game_log).expect("serialize game log");
        std::fs::write(path, json).expect("write game log dump");
        println!("Dumped {} game-log entries to {path}", game_log.len());
    }

    outcome
}

/// pod-lab swap-liveness telemetry (loop-3 Q3(b)): scans one driver-loop
/// batch's `AiActionResult`s for `SpellCast`/`CardDrawn` events naming an
/// object whose CURRENT name (resolved via that action's own `r.state`, not
/// a stale/outer snapshot) is in `watch`, inserting the resolved name into
/// `seen`. Matches on name, not `CardId`: `CardId` is assigned per
/// physical-card-object at deck load (`deck_loading.rs`), not a stable
/// per-name identity, while every `GameObject` already carries its own
/// resolved `name` — no extra database lookup needed. Pure and unit-tested
/// separately from `play_one_game`'s full game-driving loop; the caller
/// skips calling this entirely when `watch` is empty, so a run that doesn't
/// pass `--watch-cards` pays nothing beyond the `is_empty()` check.
fn record_watched_cards(
    results: &[phase_ai::auto_play::AiActionResult],
    watch: &HashSet<String>,
    seen: &mut HashSet<String>,
) {
    for r in results {
        for event in &r.events {
            let object_id = match event {
                GameEvent::SpellCast { object_id, .. } => *object_id,
                GameEvent::CardDrawn { object_id, .. } => *object_id,
                _ => continue,
            };
            if let Some(obj) = r.state.objects.get(&object_id) {
                if watch.contains(&obj.name) {
                    seen.insert(obj.name.clone());
                }
            }
        }
    }
}

/// Runs `play` once per entry in `games`, isolating panics per game (Tier 1
/// item 2) so one bad game (this binary has real `unwrap()`/`expect()` calls,
/// including deep in the AI search path) can't take down the rest of the
/// batch. `play` is responsible for printing and flushing that game's own
/// output; this function supplies panic isolation and, on panic, prints (and
/// flushes) the `GAME <label> PANICKED: <message>` line pod-lab's harness can
/// key off. `label` extracts a short, printable identifier from `T` for that
/// line without requiring `T: Display`. A quiet panic hook is installed for
/// the duration of the batch so an in-batch panic doesn't dump a (potentially
/// large, Debug-formatted game state) default panic report to stderr — it's
/// restored afterward, so single-game mode (which never calls this) and
/// anything the process does after the batch are unaffected.
fn run_batch_isolated<T>(games: &[T], label: impl Fn(&T) -> String, mut play: impl FnMut(&T)) {
    let _panic_hook_guard = PanicHookGuard::install(Box::new(|info| {
        eprintln!("panic (isolated to one batch game): {info}");
    }));

    for game in games {
        // The whole per-game step — including reporting the panic itself — is
        // inside this outer boundary. `println!`/`flush` panic on a genuine
        // I/O failure (e.g. a broken stdout pipe, plausible under an external
        // process harness), and without this wrapper that would break out of
        // the loop before the next game or the hook restore below ran,
        // silently defeating the isolation this function exists to provide.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| play(game)));
            if let Err(payload) = result {
                println!(
                    "GAME {} PANICKED: {}",
                    label(game),
                    panic_message(&*payload)
                );
                let _ = std::io::stdout().flush();
            }
        }));
    }
}

type PanicHook = Box<dyn Fn(&PanicHookInfo<'_>) + Send + Sync + 'static>;

/// Restores the process-wide panic hook even if batch-loop plumbing unwinds.
struct PanicHookGuard(Option<PanicHook>);

impl PanicHookGuard {
    fn install(temporary_hook: PanicHook) -> Self {
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(temporary_hook);
        Self(Some(previous_hook))
    }
}

impl Drop for PanicHookGuard {
    fn drop(&mut self) {
        if let Some(previous_hook) = self.0.take() {
            std::panic::set_hook(previous_hook);
        }
    }
}

/// Best-effort extraction of a human-readable message from a `catch_unwind`
/// payload. `panic!`/`unwrap`/`expect` payloads are `&str` or `String` in
/// every case this codebase produces; anything else (a custom `panic_any`
/// payload) falls back to a fixed placeholder rather than failing to report.
fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

/// Reads an opt-in dump-destination env var once at startup. Absence is a
/// valid "not capturing" state; any other error (e.g. invalid Unicode) is a
/// misconfiguration and must not silently disable capture.
fn read_dump_env(key: &str) -> Option<String> {
    std::env::var(key)
        .map(Some)
        .or_else(|e| match e {
            std::env::VarError::NotPresent => Ok(None),
            e => Err(e),
        })
        .expect("invalid Unicode in dump-destination env var")
}

/// Parses a `--difficulty` value. Delegates the actual label→enum mapping to
/// `AiDifficulty::from_label` (the crate's single authority — see its doc
/// comment) so this binary's understanding of each label can never drift from
/// every other transport that parses one. Unlike `from_label` itself, which
/// silently downgrades an unrecognized label to `Medium`, an unrecognized
/// label here is a hard startup error: silently running a garbled
/// `--difficulty` value would poison an entire batch of games with a
/// mislabeled skill tier with no indication anything went wrong (phase#6080
/// diagnosis; pod-lab gauntlet plan §3.8/§4.5, whose local tier-echo guard
/// exists precisely because this class of silent downgrade was possible).
fn parse_difficulty(s: &str) -> AiDifficulty {
    if !ACCEPTED_DIFFICULTY_LABELS
        .iter()
        .any(|label| label.eq_ignore_ascii_case(s.trim()))
    {
        eprintln!(
            "error: unrecognized --difficulty {s:?}; accepted values: {}",
            ACCEPTED_DIFFICULTY_LABELS.join(", ")
        );
        std::process::exit(1);
    }
    AiDifficulty::from_label(s)
}

/// Parses an `--action-cap` value, aborting startup on a garbled cap. Thin
/// exiting wrapper over [`parse_action_cap_checked`] — see it for the rule.
fn parse_action_cap(s: &str) -> usize {
    parse_action_cap_checked(s).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1);
    })
}

/// Validates an `--action-cap` value: a positive integer (whitespace trimmed).
/// A garbled cap (non-numeric, or zero) would either silently keep the built-in
/// default or abort every game on the first batch — both the same silent-poison
/// failure class `parse_difficulty` guards against — so it is returned as an
/// `Err` the wrapper turns into a hard startup error rather than falling back.
fn parse_action_cap_checked(s: &str) -> Result<usize, String> {
    match s.trim().parse::<usize>() {
        Ok(n) if n > 0 => Ok(n),
        _ => Err(format!(
            "error: --action-cap {s:?} must be a positive integer"
        )),
    }
}

/// Resolves a `--difficulty-pN <label>` seat override. `suffix` is the text
/// after `--difficulty-p`; `value` is the following CLI arg (the label), if
/// present. Mirrors the hard-fail discipline of `parse_difficulty` /
/// `parse_action_cap`: a non-numeric or out-of-range (`>= 4`) seat index, or a
/// missing label, is a startup error rather than a silently dropped flag —
/// which previously also swallowed the label arg, cascading it into the
/// catch-all. Label validity is delegated to `parse_difficulty` by the caller.
fn parse_seat_override<'a>(
    suffix: &str,
    value: Option<&'a str>,
) -> Result<(usize, &'a str), String> {
    let idx = suffix
        .parse::<usize>()
        .ok()
        .filter(|&i| i < 4)
        .ok_or_else(|| format!("error: --difficulty-p{suffix}: seat index must be 0..=3"))?;
    let label =
        value.ok_or_else(|| format!("error: --difficulty-p{idx} requires a difficulty label"))?;
    Ok((idx, label))
}

/// Parses one non-blank `--games-file` line: `<seed>,<difficulty>`. Pure and
/// unit-tested; the caller decides how to report a failure. Difficulty labels
/// are validated against the same `ACCEPTED_DIFFICULTY_LABELS` table
/// `--difficulty` uses (via `AiDifficulty::from_label`), so a games-file typo
/// hard-fails exactly like a CLI typo — never silently downgrades to Medium.
fn parse_games_file_line(line: &str) -> Result<(u64, AiDifficulty), String> {
    let (seed_str, label_str) = line.split_once(',').ok_or_else(|| {
        format!("malformed games-file line (expected `seed,difficulty`): {line:?}")
    })?;
    let seed = seed_str.trim().parse::<u64>().map_err(|_| {
        format!("games-file line {line:?}: seed {seed_str:?} is not a valid non-negative integer")
    })?;
    let label = label_str.trim();
    if !ACCEPTED_DIFFICULTY_LABELS
        .iter()
        .any(|l| l.eq_ignore_ascii_case(label))
    {
        return Err(format!(
            "games-file line {line:?}: unrecognized difficulty {label:?}; accepted values: {}",
            ACCEPTED_DIFFICULTY_LABELS.join(", ")
        ));
    }
    Ok((seed, AiDifficulty::from_label(label)))
}

/// Reads and parses a whole `--games-file`: one `seed,difficulty` pair per
/// non-blank line. Any malformed line fails the whole file (see
/// `parse_games_file_line`'s doc) rather than skipping it, so a typo can't
/// silently shrink or corrupt a batch.
fn parse_games_file(path: &str) -> Result<Vec<(u64, AiDifficulty)>, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read games-file {path:?}: {e}"))?;
    content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(parse_games_file_line)
        .collect()
}

/// End-of-run disposition, derived purely from whether the action cap was hit
/// and where `waiting_for` parked. Drives both the epilogue text and the exit
/// code. Abort takes precedence: `(aborted, GameOver)` is narrowly reachable —
/// if the game-ending action is exactly the last of the remaining budget, the
/// batch fills `remaining` by count with no break reason and `run_driver_loop`
/// returns `DriverExit::CapReached` on a `GameOver` state — and it is
/// deliberately folded into `Aborted` rather than given a fourth state (exit
/// code 2 either way; reporting the abort is more self-consistent than claiming
/// a clean finish for a run the cap cut short).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunOutcome {
    Completed,
    Aborted,
    Stalled,
}

fn classify_run_outcome(aborted: bool, waiting_for: &WaitingFor) -> RunOutcome {
    match (aborted, waiting_for) {
        (true, _) => RunOutcome::Aborted,
        (false, WaitingFor::GameOver { .. }) => RunOutcome::Completed,
        (false, _) => RunOutcome::Stalled,
    }
}

/// Builds the 4-player commander `GameState` this driver plays, from a
/// resolved deck payload. Single authority for this bin's setup sequence —
/// `main()` and the setup regression test below both call this, so the two
/// can never drift apart.
///
/// Populates `state.all_card_names` (a `#[serde(skip)]` field, so it is never
/// restored by deserialization) right after deck loading, mirroring every
/// other game-construction site (`engine-wasm/src/lib.rs`, `replay.rs`,
/// `server-core/src/session.rs`). Without it, `NamedChoice { choice_type:
/// CardName, .. }` candidate generation (`ai_support::candidate_actions` ->
/// `card_name_choice_candidates`) sees an empty `all_card_names` and returns
/// zero legal actions — a permanent AI stall the first time an opponent's
/// card triggers a "name a card" choice.
fn build_game_state(db: &CardDatabase, payload: &DeckPayload, seed: u64) -> GameState {
    let mut state = GameState::new(FormatConfig::commander(), 4, seed);
    load_deck_into_state(&mut state, payload);
    state.all_card_names = db.card_names().into();
    state
}

/// Builds one seat's `AiConfig` for a single game. Single authority for this
/// bin's per-seat AI configuration — `play_one_game` calls it once per seat and
/// the regression test below asserts its one load-bearing invariant.
///
/// EVERY seat runs in MEASUREMENT mode (`AiConfig::into_measurement`), which
/// disables the wall-clock search deadline (`AI_SEARCH_TIME_BUDGET_MS`, default
/// 1500ms) so search is bounded SOLELY by `max_nodes`/`max_depth`. This is
/// required for reproducibility, not a benchmarking nicety: an interactive
/// (wall-clock-bounded) search truncates to a degraded best-so-far result the
/// moment `Deadline::expired()` fires, and whether it fires on a given decision
/// depends on how fast the process happens to be running at that instant — NOT
/// on `(seed, difficulty, feed)`. A `--games-file` batch process is measurably
/// slower on its Nth game (warmer allocator, more resident state) than a fresh
/// single-game process, so the SAME game played 3rd in a batch could expire the
/// deadline on a mid-game decision that the solo run completed in full, pick a
/// different move, and diverge — the exact cross-game non-determinism the
/// pod-lab equivalence gate caught (a game that wins solo stalling at turn 60 in
/// batch). Measurement mode makes every decision a pure function of the game's
/// inputs, so batch game N is bit-identical to the same game run solo. This
/// mirrors the established `duel_suite::run` batch harness, which builds its
/// config with `.into_measurement(seed)` for the same "eliminate wall-clock
/// flake" reason. `seed` is the per-game seed, itself fully determined by the
/// game's inputs; the value inside `ExecutionMode::Measurement { seed }` only
/// tags the mode — the search's determinization entropy is derived from game
/// state (`search.rs`), not from this seed.
///
/// `run_context` (see its doc) is a required parameter, not an implicit
/// always-on: `RunContext::Measurement` calls `.into_measurement(seed)` as
/// before; `RunContext::Interactive` leaves the wall-clock deadline in
/// place. `parse_cli` constructs `RunContext::Interactive` for a solo
/// invocation with no measurement override, and `RunContext::Measurement`
/// for `--games-file` batches or the `PHASE_AI_MEASUREMENT` override.
fn build_seat_config(difficulty: AiDifficulty, seed: u64, run_context: RunContext) -> AiConfig {
    let config = create_config_for_players(difficulty, Platform::Native, 4);
    match run_context {
        RunContext::Measurement => config.into_measurement(seed),
        RunContext::Interactive => config,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::ai_support::candidate_actions;
    use engine::game::game_object::GameObject;
    use engine::types::ability::ChoiceType;
    use engine::types::actions::GameAction;
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::zones::Zone;
    use phase_ai::auto_play::AiActionResult;
    use std::sync::{Arc, Mutex};

    struct TempFileGuard(PathBuf);

    impl Drop for TempFileGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn temp_games_file_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ai_commander_{name}_{}_{:?}.txt",
            std::process::id(),
            std::thread::current().id()
        ))
    }

    /// Minimal two-card fixture (one legendary creature, one basic land) so the
    /// setup-path regression test below doesn't depend on the full card-data.json
    /// corpus. Mirrors the schema used by `deck_loading`'s own `snow_basics_db`
    /// test fixture.
    fn fixture_db() -> CardDatabase {
        let json = serde_json::json!({
            "test commander": {
                "name": "Test Commander",
                "mana_cost": { "type": "NoCost" },
                "card_type": {
                    "supertypes": ["Legendary"],
                    "core_types": ["Creature"],
                    "subtypes": ["Human"]
                },
                "power": { "type": "Fixed", "value": 2 },
                "toughness": { "type": "Fixed", "value": 2 },
                "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [],
                "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null
            },
            "test land": {
                "name": "Test Land",
                "mana_cost": { "type": "NoCost" },
                "card_type": {
                    "supertypes": ["Basic"],
                    "core_types": ["Land"],
                    "subtypes": ["Plains"]
                },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [],
                "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null
            }
        })
        .to_string();
        CardDatabase::from_json_str(&json).expect("ai_commander test fixture parses")
    }

    /// Regression test for the gap fixed alongside this test: `build_game_state`
    /// (called by `main()`) builds a 4-player commander `GameState` from a
    /// resolved deck payload and must set `state.all_card_names`, the
    /// `#[serde(skip)]` field `NamedChoice { choice_type: CardName, .. }`
    /// candidate generation reads (see
    /// `ai_support::candidates::card_name_choice_candidates`, which returns an
    /// empty candidate list — a permanent AI stall — whenever `all_card_names`
    /// is empty). This calls the bin's actual setup function (not a duplicated
    /// copy of it) and asserts a `NamedChoice{CardName}` prompt yields a
    /// non-empty candidate set afterward, mirroring `candidates.rs`'s
    /// `named_card_choice_uses_bounded_in_game_names` test and the restore-path
    /// guard in `printed_cards.rs`.
    #[test]
    fn setup_populates_all_card_names_for_named_choice_candidates() {
        let db = fixture_db();
        let seat = PlayerDeckList {
            main_deck: vec!["Test Land".to_string(); 10],
            commander: vec!["Test Commander".to_string()],
            ..Default::default()
        };
        let deck_list = DeckList {
            player: seat.clone(),
            opponent: seat.clone(),
            ai_decks: vec![seat.clone(), seat],
            ..Default::default()
        };
        let payload: DeckPayload = resolve_deck_list(&db, &deck_list);

        // Calls the same setup function `main()` uses — if the
        // `all_card_names` assignment is ever removed from `build_game_state`,
        // this test fails instead of silently passing against a duplicated copy.
        let mut state = build_game_state(&db, &payload, 42);

        assert!(
            !state.all_card_names.is_empty(),
            "setup must populate all_card_names right after deck loading"
        );

        state.waiting_for = WaitingFor::NamedChoice {
            player: PlayerId(1),
            choice_type: ChoiceType::CardName,
            options: Vec::new(),
            source: None,
            persist_player: None,
        };
        let actions = candidate_actions(&state);
        assert!(
            !actions.is_empty(),
            "NamedChoice{{CardName}} must yield candidates once all_card_names is populated"
        );
    }

    /// Regression for the cross-game state-leakage defect (pod-lab equivalence
    /// gate; PR phase-rs/phase#6252): a game that won solo stalled at turn 60
    /// when played 3rd in a `--games-file` batch. Root cause was NOT a leaked
    /// counter/cache but the interactive wall-clock search deadline
    /// (`AI_SEARCH_TIME_BUDGET_MS`): a slower Nth-in-batch process expired it on
    /// a mid-game decision the fresh solo process completed, diverging the game.
    /// `build_seat_config` fixes this by running every seat in MEASUREMENT mode,
    /// which disables the wall-clock deadline (search bounded solely by
    /// node/depth — see `search.rs` / `planner::PlannerServices::with_deadline`,
    /// both gated on `execution_mode.is_measurement()`), making each decision a
    /// pure function of the game's inputs. This asserts the invariant
    /// deterministically (no card-data / no real game needed): against the
    /// unfixed code (`ExecutionMode::Interactive`) it fails, catching any future
    /// regression that drops measurement mode and reintroduces wall-clock flake.
    #[test]
    fn seat_config_runs_in_measurement_mode_for_batch_reproducibility() {
        for difficulty in [
            AiDifficulty::Easy,
            AiDifficulty::Medium,
            AiDifficulty::Hard,
            AiDifficulty::VeryHard,
        ] {
            let config = build_seat_config(difficulty, 95_000_004, RunContext::Measurement);
            assert!(
                config.execution_mode.is_measurement(),
                "{difficulty:?} seat must run in measurement mode so a batched \
                 game is bit-identical to the same game run solo; interactive \
                 mode makes search wall-clock-dependent and non-reproducible \
                 under batch load"
            );
        }
    }

    /// Companion to the assertion above: `Interactive` must leave the
    /// wall-clock deadline in place (i.e. NOT call `into_measurement`).
    /// `parse_cli` constructs `RunContext::Interactive` for the default solo
    /// route; pinning both directions here means routing can't silently
    /// collapse `Interactive` into `Measurement` (or vice versa) without a
    /// red test.
    #[test]
    fn seat_config_leaves_interactive_mode_wall_clock_bounded() {
        for difficulty in [
            AiDifficulty::Easy,
            AiDifficulty::Medium,
            AiDifficulty::Hard,
            AiDifficulty::VeryHard,
        ] {
            let config = build_seat_config(difficulty, 95_000_004, RunContext::Interactive);
            assert!(
                !config.execution_mode.is_measurement(),
                "{difficulty:?} seat under RunContext::Interactive must NOT run in \
                 measurement mode; collapsing it into Measurement would defeat the \
                 whole point of having the two variants"
            );
        }
    }

    /// Building-block test for `record_watched_cards` (loop-3 Q3(b)): proves
    /// the matcher (a) recognizes both `SpellCast` and `CardDrawn` events,
    /// (b) resolves the watched name via the object's *current* `r.state`
    /// rather than any fixed name table, and (c) is selective — an event
    /// naming an object outside `watch`, and an event of an unrelated
    /// variant entirely, must both be no-ops rather than getting recorded.
    #[test]
    fn record_watched_cards_matches_spell_cast_and_card_drawn_by_name() {
        let mut state = GameState::new(FormatConfig::commander(), 4, 1);
        let cast_obj = ObjectId(100);
        let drawn_obj = ObjectId(101);
        let unwatched_obj = ObjectId(102);
        state.objects.insert(
            cast_obj,
            GameObject::new(
                cast_obj,
                CardId(100),
                PlayerId(0),
                "Lightning Bolt".to_string(),
                Zone::Stack,
            ),
        );
        state.objects.insert(
            drawn_obj,
            GameObject::new(
                drawn_obj,
                CardId(101),
                PlayerId(0),
                "Sol Ring".to_string(),
                Zone::Hand,
            ),
        );
        state.objects.insert(
            unwatched_obj,
            GameObject::new(
                unwatched_obj,
                CardId(102),
                PlayerId(0),
                "Forest".to_string(),
                Zone::Battlefield,
            ),
        );

        let watch: HashSet<String> = ["Lightning Bolt".to_string(), "Sol Ring".to_string()]
            .into_iter()
            .collect();
        let mut seen: HashSet<String> = HashSet::new();

        let results = vec![
            AiActionResult {
                action: GameAction::PassPriority,
                state: state.clone(),
                events: vec![GameEvent::SpellCast {
                    card_id: CardId(100),
                    controller: PlayerId(0),
                    object_id: cast_obj,
                }],
                log_entries: Vec::new(),
            },
            AiActionResult {
                action: GameAction::PassPriority,
                state: state.clone(),
                events: vec![
                    GameEvent::CardDrawn {
                        player_id: PlayerId(0),
                        object_id: drawn_obj,
                        nth_in_turn: 1,
                        nth_in_step: 1,
                    },
                    // Drawn but not watched, and an unrelated event variant —
                    // both must be ignored, not just the watched ones matched.
                    GameEvent::CardDrawn {
                        player_id: PlayerId(0),
                        object_id: unwatched_obj,
                        nth_in_turn: 2,
                        nth_in_step: 2,
                    },
                    GameEvent::PriorityPassed {
                        player_id: PlayerId(0),
                    },
                ],
                log_entries: Vec::new(),
            },
        ];

        record_watched_cards(&results, &watch, &mut seen);

        assert_eq!(
            seen,
            ["Lightning Bolt".to_string(), "Sol Ring".to_string()]
                .into_iter()
                .collect::<HashSet<_>>()
        );
    }

    #[test]
    fn record_watched_cards_is_a_noop_on_empty_results() {
        let watch: HashSet<String> = ["Lightning Bolt".to_string()].into_iter().collect();
        let mut seen: HashSet<String> = HashSet::new();
        record_watched_cards(&[], &watch, &mut seen);
        assert!(seen.is_empty());
    }

    #[test]
    fn parse_action_cap_accepts_positive_integer() {
        assert_eq!(parse_action_cap_checked("50000"), Ok(50000));
        assert_eq!(parse_action_cap_checked("  7 "), Ok(7));
    }

    #[test]
    fn parse_action_cap_rejects_zero_and_non_numeric() {
        assert!(parse_action_cap_checked("0").is_err());
        assert!(parse_action_cap_checked("-5").is_err());
        assert!(parse_action_cap_checked("abc").is_err());
        assert!(parse_action_cap_checked("").is_err());
    }

    #[test]
    fn parse_seat_override_rejects_non_numeric_index() {
        assert!(parse_seat_override("x", Some("Hard")).is_err());
    }

    #[test]
    fn parse_seat_override_rejects_out_of_range_index() {
        assert!(parse_seat_override("4", Some("Hard")).is_err());
        assert!(parse_seat_override("9", Some("Hard")).is_err());
    }

    #[test]
    fn parse_seat_override_requires_a_label_value() {
        assert!(parse_seat_override("2", None).is_err());
    }

    #[test]
    fn parse_seat_override_accepts_valid_seat_and_label() {
        assert_eq!(parse_seat_override("2", Some("Hard")), Ok((2, "Hard")));
        assert_eq!(
            parse_seat_override("0", Some("VeryHard")),
            Ok((0, "VeryHard"))
        );
    }

    #[test]
    fn classify_run_outcome_completed_only_on_clean_gameover() {
        let over = WaitingFor::GameOver {
            winner: Some(PlayerId(0)),
        };
        assert_eq!(classify_run_outcome(false, &over), RunOutcome::Completed);
    }

    #[test]
    fn classify_run_outcome_aborted_when_cap_hit() {
        // Abort wins over the parked state, and also over the narrowly-reachable
        // `(aborted, GameOver)` corner (game ends on the last action of the
        // remaining budget, then the cap fires) — which is deliberately folded
        // into `Aborted`.
        let parked = WaitingFor::Priority {
            player: PlayerId(0),
        };
        assert_eq!(classify_run_outcome(true, &parked), RunOutcome::Aborted);
        let over = WaitingFor::GameOver {
            winner: Some(PlayerId(0)),
        };
        assert_eq!(classify_run_outcome(true, &over), RunOutcome::Aborted);
    }

    #[test]
    fn classify_run_outcome_stalled_when_parked_off_gameover() {
        let parked = WaitingFor::Priority {
            player: PlayerId(0),
        };
        assert_eq!(classify_run_outcome(false, &parked), RunOutcome::Stalled);
    }

    #[test]
    fn games_file_line_parses_seed_and_difficulty() {
        assert_eq!(
            parse_games_file_line("1009,Easy"),
            Ok((1009, AiDifficulty::Easy))
        );
        // Whitespace around either field is tolerated.
        assert_eq!(
            parse_games_file_line(" 42 , VeryHard "),
            Ok((42, AiDifficulty::VeryHard))
        );
    }

    #[test]
    fn games_file_line_rejects_missing_comma() {
        assert!(parse_games_file_line("1009 Easy").is_err());
    }

    #[test]
    fn games_file_line_rejects_non_numeric_seed() {
        assert!(parse_games_file_line("abc,Easy").is_err());
    }

    #[test]
    fn games_file_line_rejects_unrecognized_difficulty() {
        assert!(parse_games_file_line("1009,Impossible").is_err());
    }

    #[test]
    fn parse_games_file_reads_multiple_lines_and_skips_blanks() {
        let path = temp_games_file_path("games_file_test");
        let _guard = TempFileGuard(path.clone());
        std::fs::write(&path, "1009,Easy\n\n1010,VeryHard\n  \n1011,Medium\n").unwrap();
        let games = parse_games_file(path.to_str().unwrap()).unwrap();
        assert_eq!(
            games,
            vec![
                (1009, AiDifficulty::Easy),
                (1010, AiDifficulty::VeryHard),
                (1011, AiDifficulty::Medium),
            ]
        );
    }

    #[test]
    fn parse_games_file_rejects_a_batch_with_any_malformed_line() {
        let path = temp_games_file_path("games_file_bad_test");
        let _guard = TempFileGuard(path.clone());
        std::fs::write(&path, "1009,Easy\nnotaseed,Easy\n").unwrap();
        let result = parse_games_file(path.to_str().unwrap());
        assert!(result.is_err());
    }

    /// Building-block test for panic isolation (Tier 1 item 2), decoupled from
    /// `GameState`/the engine entirely — a `#[cfg(test)]`-only fake `play`
    /// closure stands in for a real game so this proves the loop mechanics
    /// (order, panic containment, continuation) without the cost of driving
    /// real games. Records every `(seed, difficulty)` `run_batch_isolated`
    /// handed to `play`, in order, and forces the middle game to panic.
    #[test]
    fn run_batch_isolated_continues_past_a_panicking_game() {
        let games = vec![
            (1u64, AiDifficulty::Easy),
            (2u64, AiDifficulty::Medium),
            (3u64, AiDifficulty::Hard),
        ];
        let seen: Arc<Mutex<Vec<(u64, AiDifficulty)>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_in_closure = Arc::clone(&seen);

        run_batch_isolated(
            &games,
            |(seed, _)| seed.to_string(),
            move |&(seed, difficulty)| {
                seen_in_closure.lock().unwrap().push((seed, difficulty));
                if seed == 2 {
                    panic!("forced panic for seed 2 (test isolation seam)");
                }
            },
        );

        // All three games were invoked, in order — the panic on game 2 did not
        // stop game 3 (or anything) from running. This is the direct proof
        // that one bad game can't take down the rest of the batch.
        assert_eq!(*seen.lock().unwrap(), games);
    }

    /// D1 route table: a bare solo invocation with no `--games-file` and no
    /// measurement env override resolves to `RunContext::Interactive`.
    #[test]
    fn parse_cli_solo_invocation_routes_to_interactive() {
        let args = vec!["ai-commander".to_string(), "client/public".to_string()];
        let cli = parse_cli(&args, false).expect("solo invocation must parse");
        assert_eq!(
            cli.run_context,
            RunContext::Interactive,
            "a solo (non-games-file) invocation with no measurement env override must route to RunContext::Interactive"
        );
    }

    /// D1 route table: `--games-file` invocations must resolve to
    /// `RunContext::Measurement`.
    #[test]
    fn parse_cli_games_file_invocation_routes_to_measurement() {
        let path = temp_games_file_path("parse_cli_route_games_file");
        let _guard = TempFileGuard(path.clone());
        std::fs::write(
            &path,
            "1009,Easy
",
        )
        .unwrap();
        let args = vec![
            "ai-commander".to_string(),
            "--games-file".to_string(),
            path.to_str().unwrap().to_string(),
        ];
        let cli = parse_cli(&args, false).expect("games-file invocation must parse");
        assert_eq!(cli.run_context, RunContext::Measurement);
    }

    /// D1 route table: the `PHASE_AI_MEASUREMENT` harness escape hatch forces
    /// a solo (non-games-file) invocation to `RunContext::Measurement`.
    #[test]
    fn parse_cli_measurement_env_forces_measurement_on_solo() {
        let args = vec!["ai-commander".to_string(), "client/public".to_string()];
        let cli = parse_cli(&args, true).expect("solo invocation must parse");
        assert_eq!(cli.run_context, RunContext::Measurement);
    }

    /// F4: the positional `cards_path` scan must not be poisoned by the
    /// *value* of any preceding `--flag value` pair (e.g. `--seed 42
    /// some/path` must resolve `cards_path` to `"some/path"`, not `"42"`).
    /// Table-driven over every flag that takes a value — including
    /// `--games-file` (whose value is a real temp file, since `parse_cli`
    /// validates the batch up front), `--watch-cards`, and the seat-override
    /// form `--difficulty-p<N>`.
    #[test]
    fn parse_cli_positional_cards_path_survives_preceding_flag_value() {
        let games_path = temp_games_file_path("positional_survival_games");
        let _guard = TempFileGuard(games_path.clone());
        std::fs::write(&games_path, "1009,Easy\n").unwrap();
        let games_path = games_path.to_str().unwrap();

        let rows: &[&[&str]] = &[
            &["--seed", "42", "some/path"],
            &["--feed", "x.json", "some/path"],
            &["--action-cap", "5", "some/path"],
            &["--difficulty", "Easy", "some/path"],
            &["--games-file", games_path, "some/path"],
            &["--watch-cards", "Sol Ring,Arcane Signet", "some/path"],
            &["--difficulty-p0", "Easy", "some/path"],
        ];
        for row in rows {
            let mut args = vec!["ai-commander".to_string()];
            args.extend(row.iter().map(|s| s.to_string()));
            let cli = parse_cli(&args, false).expect("row must parse");
            assert_eq!(
                cli.cards_path, "some/path",
                "row {row:?}: positional cards_path must survive a preceding --flag value pair (F4)"
            );
        }
    }

    /// F4 value-consumption pin: the `--watch-cards` value must reach the
    /// consuming set — split on commas, trimmed — and must not disturb the
    /// positional `cards_path`. Guards the arm at the consuming loop directly
    /// (a row in the positional table alone can't catch the value being
    /// dropped or mis-split).
    #[test]
    fn parse_cli_watch_cards_value_reaches_the_consuming_set() {
        let args: Vec<String> = [
            "ai-commander",
            "--watch-cards",
            "Sol Ring, Arcane Signet",
            "some/path",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let cli = parse_cli(&args, false).expect("watch-cards invocation must parse");
        let expected: HashSet<String> = ["Sol Ring", "Arcane Signet"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(cli.watch_cards, expected);
        assert_eq!(cli.cards_path, "some/path");
    }

    /// F4 value-consumption pin: the `--games-file` value must reach
    /// `CliArgs.games_file` and its parsed batch (validated up front), and
    /// must not disturb the positional `cards_path`.
    #[test]
    fn parse_cli_games_file_value_reaches_the_consuming_option() {
        let path = temp_games_file_path("games_file_value_consumption");
        let _guard = TempFileGuard(path.clone());
        std::fs::write(&path, "1009,Easy\n").unwrap();
        let path = path.to_str().unwrap();

        let args: Vec<String> = ["ai-commander", "--games-file", path, "some/path"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let cli = parse_cli(&args, false).expect("games-file invocation must parse");
        assert_eq!(cli.games_file.as_deref(), Some(path));
        assert_eq!(
            cli.batch_games,
            Some(vec![(1009, AiDifficulty::Easy)]),
            "the games-file value must reach the up-front-validated batch"
        );
        assert_eq!(cli.cards_path, "some/path");
    }

    /// F4 companion (green today, and a real pin): an *unknown* bare flag
    /// (no recognized value-consuming arm) must not eat the following
    /// positional arg. Guards against a naive single-loop F4 fix that
    /// assumes every `--flag` consumes the next token.
    #[test]
    fn parse_cli_bare_unknown_flag_does_not_consume_positional_path() {
        let args = vec![
            "ai-commander".to_string(),
            "--foo".to_string(),
            "some/path".to_string(),
        ];
        let cli = parse_cli(&args, false).expect("row must parse");
        assert_eq!(cli.cards_path, "some/path");
    }

    /// F4 companion (green today): with no positional arg at all,
    /// `cards_path` must default to `client/public`.
    #[test]
    fn parse_cli_defaults_cards_path_to_client_public_when_no_positional() {
        let args = vec!["ai-commander".to_string()];
        let cli = parse_cli(&args, false).expect("bare invocation must parse");
        assert_eq!(cli.cards_path, "client/public");
    }

    #[test]
    fn run_batch_isolated_visits_every_game_exactly_once_when_none_panic() {
        let games = vec![(10u64, AiDifficulty::Easy), (20u64, AiDifficulty::VeryHard)];
        let seen: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_in_closure = Arc::clone(&seen);

        run_batch_isolated(
            &games,
            |(seed, _)| seed.to_string(),
            move |&(seed, _)| {
                seen_in_closure.lock().unwrap().push(seed);
            },
        );

        assert_eq!(*seen.lock().unwrap(), vec![10, 20]);
    }
}
