//! Sanity-check runner: four-player commander game driven entirely by the AI.
//!
//! Loads four commander precons from `feeds/commander-precons.json`, sets up
//! a 4-player commander GameState, and drives every player with the native
//! AI until the game ends (or an action budget is hit). Reports per-turn
//! life totals and the final outcome.
//!
//! Usage:
//!   cargo run --release --bin ai-commander -- client/public
//!   cargo run --release --bin ai-commander -- client/public --seed 7 --difficulty Easy
//!   cargo run --release --bin ai-commander -- client/public --difficulty Easy \
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
//! timeout on the process).
//!   cargo run --release --bin ai-commander -- client/public --games-file games.txt

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

    let cards_path = args
        .iter()
        .skip(1)
        .find(|a| !a.starts_with("--"))
        .cloned()
        .unwrap_or_else(|| "client/public".to_string());

    let mut seed: u64 = 42;
    let mut difficulty = AiDifficulty::Easy;
    // Per-seat override of `difficulty` (pod-lab gauntlet mixed-skill tables).
    // `None` means "use the table-wide --difficulty for this seat".
    let mut seat_difficulty: [Option<AiDifficulty>; 4] = [None; 4];
    let mut action_cap: usize = DEFAULT_ACTION_CAP;
    let mut feed: String = "feeds/mtggoldfish-commander.json".to_string();
    let mut games_file: Option<String> = None;
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
                None => {
                    eprintln!("error: --games-file requires a path");
                    std::process::exit(1);
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
                        Err(e) => {
                            eprintln!("{e}");
                            std::process::exit(1);
                        }
                    }
                }
            }
        }
    }

    // `--games-file` batch entries are validated up front — same hard-fail-at-
    // startup discipline as `parse_difficulty`/`parse_action_cap`/
    // `parse_seat_override` above: a garbled batch file should fail before the
    // ~1.8s database load, not silently skip or misinterpret one line mid-batch.
    let batch_games: Option<Vec<(u64, AiDifficulty)>> =
        games_file
            .as_deref()
            .map(|path| match parse_games_file(path) {
                Ok(games) if games.is_empty() => {
                    eprintln!("error: --games-file {path:?} contains no games");
                    std::process::exit(1);
                }
                Ok(games) => games,
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            });

    // Argument parsing and validation above needs no special stack; the actual
    // game-driving work (both the single-game path and `--games-file` batch
    // mode) does — see `GAME_THREAD_STACK_SIZE`. Spawning unconditionally
    // (rather than only under `--games-file`) fixes the pre-existing
    // single-game overflow too, not just the batch case.
    let cli = CliArgs {
        cards_path,
        feed,
        seed,
        difficulty,
        seat_difficulty,
        action_cap,
        games_file,
        batch_games,
    };
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
            create_config_for_players(seat_diff, Platform::Native, 4),
        );
    }
    println!();

    let start = Instant::now();
    let mut game_log: Vec<engine::types::log::GameLogEntry> = Vec::new();
    let mut actions_log: Vec<String> = Vec::new();
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

#[cfg(test)]
mod tests {
    use super::*;
    use engine::ai_support::candidate_actions;
    use engine::types::ability::ChoiceType;
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
