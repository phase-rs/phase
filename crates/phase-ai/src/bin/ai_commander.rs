//! Sanity-check runner: a commander game (four seats by default) driven
//! entirely by the AI.
//!
//! Loads the first N commander decks from the `--feed` file (default
//! `feeds/mtggoldfish-commander.json`) whose commander resolves in the card
//! database, sets up an N-player commander GameState, and drives every player
//! with the native AI until the game ends (or an action budget is hit).
//! Reports per-turn life totals and the final outcome. `--players N` picks N
//! (default 4) within `FormatConfig::commander()`'s seat range;
//! `--difficulty-pK` overrides seat K's difficulty for any K below N.
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
//!   cargo run --profile server-release --bin ai-commander -- client/public --players 5 --difficulty-p4 VeryHard
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
//!
//! Optional engagement telemetry: repeat `--watch-card "Exact Name, With Comma"`
//! for exact, case-sensitive names. Legacy `--watch-cards "Name One,Name Two"`
//! still splits on commas. Both options trim surrounding whitespace. The last
//! legacy list plus all exact names are watched.
//! `PODLAB-TELEM` retains the sorted global draw/cast union `cards_seen` and adds
//! `card_counts`: rows sorted by numeric `seat`, then `name`, with numeric `draws`
//! and `casts`. Only observed, name-resolved draw/cast events produce rows; these
//! are event counts, not evidence of resolution or a win-condition activation.

// pod-lab loop-3 Q5: native-binary throughput lever, gated in Cargo.toml so
// wasm32 builds of this crate's lib (pulled in by engine-wasm/draft-wasm)
// never see it.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::any::Any;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Write as _;
use std::panic::PanicHookInfo;
use std::path::PathBuf;
use std::time::Instant;

use engine::database::CardDatabase;
use engine::game::deck_loading::{
    load_and_hydrate_decks, resolve_deck_list, DeckList, DeckPayload, PlayerDeckList,
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

/// Seat count when `--players` is absent: the four-seat table this binary
/// always played before `--players` existed, so every unflagged invocation —
/// and its stdout — is unchanged.
const DEFAULT_PLAYERS: u8 = 4;

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
    players: u8,
    /// One slot per seat (`len == players`); `None` = the table-wide
    /// `difficulty`.
    seat_difficulty: Vec<Option<AiDifficulty>>,
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

/// Error for a missing, blank, or flag-shaped `--watch-cards` value.
const WATCH_CARDS_REQUIRES: &str = "error: --watch-cards requires a comma-separated card name list";

/// Pure argument parser: consumes the raw process args plus the one
/// env-derived flag `main` reads once (`measurement_env`, from
/// `PHASE_AI_MEASUREMENT` -- presence-based, matching `PHASE_DUMP_*`
/// convention, not `== "1"`) and returns either a fully resolved `CliArgs`
/// or an error message ready for the caller to print verbatim before exiting
/// with status 1. Contains the entire pre-`run` pre-pass (positional
/// `cards_path` resolution, the flag loop, seat-override resolution against
/// `--players`, and `--games-file` validation) that used to live directly in
/// `main`, so `main` itself does nothing but read the env var, call this, and
/// report success/failure. Error paths return an `Err` instead, exactly
/// mirroring the direct-exit message each replaces. Every value-taking flag
/// rejects a missing or unparseable value with an error naming that flag; none
/// falls back to its default.
fn parse_cli(args: &[String], measurement_env: bool) -> Result<CliArgs, String> {
    let mut cards_path: Option<String> = None;

    let mut seed: u64 = 42;
    let mut difficulty = AiDifficulty::Easy;
    let mut players: u8 = DEFAULT_PLAYERS;
    // Per-seat `--difficulty-pK` overrides (pod-lab gauntlet mixed-skill tables)
    // as raw `(flag as typed, label)` pairs in command-line order. Resolved after
    // the loop, once `--players` is known, so an override is validated against the
    // table's seat count whichever flag comes first.
    let mut seat_overrides: Vec<(&str, Option<&str>)> = Vec::new();
    let mut action_cap: usize = DEFAULT_ACTION_CAP;
    let mut feed: String = "feeds/mtggoldfish-commander.json".to_string();
    let mut games_file: Option<String> = None;
    // pod-lab swap-liveness telemetry (loop-3 Q3(b)): empty means the
    // per-event scan in `play_one_game` is skipped entirely, not merely a
    // no-op HashSet lookup — a run that doesn't pass this flag pays nothing.
    let mut watch_cards: HashSet<String> = HashSet::new();
    let mut exact_watch_cards = HashSet::new();
    let mut args_iter = args.iter().skip(1).peekable();
    while let Some(arg) = args_iter.next() {
        match arg.as_str() {
            "--seed" => match args_iter.next() {
                Some(v) => seed = parse_seed(v)?,
                None => return Err("error: --seed requires an integer".to_string()),
            },
            "--difficulty" => match args_iter.next() {
                Some(v) => difficulty = parse_difficulty_checked("--difficulty", v)?,
                None => return Err("error: --difficulty requires a difficulty label".to_string()),
            },
            "--action-cap" => match args_iter.next() {
                Some(v) => action_cap = parse_action_cap_checked(v)?,
                None => return Err("error: --action-cap requires a positive integer".to_string()),
            },
            "--players" => match args_iter.next() {
                Some(v) => players = parse_player_count(v)?,
                None => return Err("error: --players requires a seat count".to_string()),
            },
            "--feed" => match args_iter.next() {
                Some(v) => feed = v.clone(),
                None => return Err("error: --feed requires a path".to_string()),
            },
            "--games-file" => match args_iter.next() {
                Some(v) => games_file = Some(v.clone()),
                None => return Err("error: --games-file requires a path".to_string()),
            },
            "--watch-cards" => match args_iter.next() {
                Some(v) => {
                    let names: HashSet<String> = v
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect();
                    // Parity with `--watch-card`: an all-blank list or a
                    // following flag taken as the value is a mistake, not an
                    // empty set (which would silently switch telemetry off).
                    if names.is_empty() || v.trim_start().starts_with("--") {
                        return Err(WATCH_CARDS_REQUIRES.to_string());
                    }
                    watch_cards = names;
                }
                None => return Err(WATCH_CARDS_REQUIRES.to_string()),
            },
            "--watch-card" => match args_iter.next() {
                Some(v) => {
                    let name = v.trim();
                    if name.is_empty() || name.starts_with("--") {
                        return Err("error: --watch-card requires an exact card name".to_string());
                    }
                    exact_watch_cards.insert(name.to_string());
                }
                None => return Err("error: --watch-card requires an exact card name".to_string()),
            },
            other => {
                // `--difficulty-p0` .. `--difficulty-p{N-1}`: single-seat
                // override, parameterized on seat index rather than one bespoke
                // flag per seat. The value arg is consumed unconditionally so a
                // bad index can't leak its label into the catch-all; the pair
                // is checked after the loop by `parse_seat_override`, which
                // hard-fails (like the sibling parsers) instead of silently
                // dropping a mistyped flag and running a mislabeled seat.
                if other.starts_with("--difficulty-p") {
                    seat_overrides.push((other, args_iter.next().map(String::as_str)));
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

    // `None` means "use the table-wide --difficulty for this seat". Applied in
    // command-line order, so a repeated override of one seat keeps the last.
    let mut seat_difficulty: Vec<Option<AiDifficulty>> = vec![None; usize::from(players)];
    for (flag, value) in seat_overrides {
        let (idx, label) = parse_seat_override(flag, value, players)?;
        seat_difficulty[idx] = Some(parse_difficulty_checked(flag, label)?);
    }

    watch_cards.extend(exact_watch_cards);
    let cards_path = cards_path.unwrap_or_else(|| "client/public".to_string());

    // `--games-file` batch entries are validated up front — same hard-fail-at-
    // startup discipline as `parse_difficulty_checked`/`parse_action_cap_checked`/
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
        players,
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
    players: u8,
    seat_difficulty: &'a [Option<AiDifficulty>],
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
        players,
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

    println!("=== {players}-player Commander AI test ===");
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

    let deck_list = match select_seat_decks(&db, decks_json, players) {
        Ok(deck_list) => deck_list,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
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
        players,
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

/// Selects the table's decks: the first `players` feed decks whose primary
/// commander resolves in `db`, in feed order, printing one SKIP line (commander
/// not in the card db) or one accepted-deck line per deck examined, exactly as
/// before `--players` existed. Seat layout: deck 0 is `player` (seat 0), deck 1
/// is `opponent` (seat 1), and the rest are `ai_decks`, which
/// `load_deck_into_state` seats at `PlayerId(2 + i)` — empty at two seats.
/// Fewer than `players` resolvable decks is an `Err` carrying the
/// "need at least N precons" message the caller prints before exiting 1.
fn select_seat_decks(
    db: &CardDatabase,
    decks_json: &[serde_json::Value],
    players: u8,
) -> Result<DeckList, String> {
    let seats = usize::from(players);
    let mut deck_lists: Vec<PlayerDeckList> = Vec::new();
    // Commander names are populated in PlayerDeckList.commander and resolved
    // by the pipeline — no manual tracking needed.
    for deck in decks_json.iter() {
        if deck_lists.len() == seats {
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

    match deck_lists.as_slice() {
        [player, opponent, ai_decks @ ..] if deck_lists.len() == seats => Ok(DeckList {
            player: player.clone(),
            opponent: opponent.clone(),
            ai_decks: ai_decks.to_vec(),
            ..Default::default()
        }),
        _ => Err(format!(
            "need at least {players} precons, found {}",
            deck_lists.len()
        )),
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
        players,
        seat_difficulty,
        action_cap,
        dump_log_path,
        dump_actions_path,
        watch_cards,
        run_context,
    } = *context;
    let mut state = build_game_state(db, payload, players, seed);

    engine::game::engine::start_game(&mut state);
    println!();
    println!("Game started. {} players.", state.players.len());
    println!();

    let ai_players: HashSet<PlayerId> = (0..players).map(PlayerId).collect();
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
            build_seat_config(seat_diff, players, seed, run_context),
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
    let mut watched_cards = WatchedCards::default();
    let mut last_turn_reported: u32 = 0;
    let mut ai_rng = StdRng::seed_from_u64(seed);
    let ai_session = phase_ai::session::AiSession::arc_from_game(&state);
    // Tracks each seat's is_eliminated (the engine already flips this per
    // CR 800.4 when a player leaves the game) so we print exactly one line
    // per elimination event instead of re-announcing an already-eliminated
    // seat every batch.
    let mut was_eliminated = vec![false; usize::from(players)];

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
                record_watched_cards(results, watch_cards, &mut watched_cards);
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
    // either watch option was given, so a harness that doesn't ask for this pays
    // nothing and every existing consumer's line-by-line parse is untouched.
    // `PODLAB-TELEM ` is a prefix no other line in this binary's stdout uses,
    // and this line does not begin with "Turn ", match `^--- GAME`, or
    // contain "Winner: " / "Difficulty: " / "ABORT: hit " / "did NOT reach
    // GameOver" (pod-lab's `runner.py`/`mechanisms.py` scan for exactly those
    // literals). Cards are sorted for a deterministic, diff-friendly line.
    if !watch_cards.is_empty() {
        println!("PODLAB-TELEM {}", watched_cards.to_json());
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

#[derive(Default)]
struct WatchedCards {
    counts: BTreeMap<(PlayerId, String), WatchedCardCounts>,
}

#[derive(Default)]
struct WatchedCardCounts {
    draws: u64,
    casts: u64,
}

#[derive(serde::Serialize)]
struct WatchedCardCountRow<'a> {
    seat: PlayerId,
    name: &'a str,
    draws: u64,
    casts: u64,
}

impl WatchedCards {
    fn to_json(&self) -> serde_json::Value {
        let seen: BTreeSet<&str> = self.counts.keys().map(|(_, name)| name.as_str()).collect();
        let card_counts: Vec<_> = self
            .counts
            .iter()
            .map(|((seat, name), counts)| WatchedCardCountRow {
                seat: *seat,
                name,
                draws: counts.draws,
                casts: counts.casts,
            })
            .collect();
        serde_json::json!({ "cards_seen": seen, "card_counts": card_counts })
    }
}

/// pod-lab swap-liveness telemetry (loop-3 Q3(b)): scans one driver-loop
/// batch's `AiActionResult`s for `SpellCast`/`CardDrawn` events naming an
/// object whose CURRENT name (resolved via that action's own `r.state`, not
/// a stale/outer snapshot) is in `watch`, counting each event by its own
/// player field and deriving the legacy union from those counts, never
/// the object's owner/current controller or the AI actor. Matches on name,
/// not `CardId`: `CardId` is assigned per
/// physical-card-object at deck load (`deck_loading.rs`), not a stable
/// per-name identity, while every `GameObject` already carries its own
/// resolved `name` — no extra database lookup needed. Pure and unit-tested
/// separately from `play_one_game`'s full game-driving loop; the caller
/// skips calling this entirely when `watch` is empty, so a run that doesn't
/// pass either watch option pays nothing beyond the `is_empty()` check.
/// Unresolvable object names are skipped, as in the legacy union.
fn record_watched_cards(
    results: &[phase_ai::auto_play::AiActionResult],
    watch: &HashSet<String>,
    watched: &mut WatchedCards,
) {
    for r in results {
        for event in &r.events {
            let (object_id, seat, draws, casts) = match event {
                GameEvent::SpellCast {
                    object_id,
                    controller,
                    ..
                } => (*object_id, *controller, 0, 1),
                GameEvent::CardDrawn {
                    object_id,
                    player_id,
                    ..
                } => (*object_id, *player_id, 1, 0),
                _ => continue,
            };
            if let Some(obj) = r.state.objects.get(&object_id) {
                if watch.contains(&obj.name) {
                    let counts = watched.counts.entry((seat, obj.name.clone())).or_default();
                    counts.draws += draws;
                    counts.casts += casts;
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

/// Parses a `--difficulty` / `--difficulty-pK` value (`flag` names which, for
/// the error message). Delegates the actual label→enum mapping to
/// `AiDifficulty::from_label` (the crate's single authority — see its doc
/// comment) so this binary's understanding of each label can never drift from
/// every other transport that parses one. Unlike `from_label` itself, which
/// silently downgrades an unrecognized label to `Medium`, an unrecognized
/// label here is a hard startup error: silently running a garbled
/// `--difficulty` value would poison an entire batch of games with a
/// mislabeled skill tier with no indication anything went wrong (phase#6080
/// diagnosis; pod-lab gauntlet plan §3.8/§4.5, whose local tier-echo guard
/// exists precisely because this class of silent downgrade was possible).
fn parse_difficulty_checked(flag: &str, s: &str) -> Result<AiDifficulty, String> {
    if !ACCEPTED_DIFFICULTY_LABELS
        .iter()
        .any(|label| label.eq_ignore_ascii_case(s.trim()))
    {
        return Err(format!(
            "error: unrecognized {flag} {s:?}; accepted values: {}",
            ACCEPTED_DIFFICULTY_LABELS.join(", ")
        ));
    }
    Ok(AiDifficulty::from_label(s))
}

/// Parses a `--seed` value. A non-numeric seed is an `Err`, never a silent
/// fallback to the default seed (which would run a different game than asked).
fn parse_seed(s: &str) -> Result<u64, String> {
    s.trim()
        .parse::<u64>()
        .map_err(|_| format!("error: --seed {s:?} must be a non-negative integer"))
}

/// Validates an `--action-cap` value: a positive integer (whitespace trimmed).
/// A garbled cap (non-numeric, or zero) would either silently keep the built-in
/// default or abort every game on the first batch — both the same silent-poison
/// failure class `parse_difficulty_checked` guards against — so it is returned as an
/// `Err` that `parse_cli` propagates as a hard startup error rather than falling back.
fn parse_action_cap_checked(s: &str) -> Result<usize, String> {
    match s.trim().parse::<usize>() {
        Ok(n) if n > 0 => Ok(n),
        _ => Err(format!(
            "error: --action-cap {s:?} must be a positive integer"
        )),
    }
}

/// Parses a `--players` value: a seat count the Commander preset admits.
/// CR 903.2: a Commander game may be a two-player game or a multiplayer game
/// (default multiplayer setup: Free-for-All). The admitted range is the engine
/// registry's (`FormatConfig::commander()`), and the bound itself is
/// `FormatConfig::validate_for_player_count`'s — this only turns the text into
/// a number. Same hard-fail discipline as `parse_action_cap_checked`: a
/// malformed or out-of-range value is an `Err`, never a fallback to the default.
fn parse_player_count(s: &str) -> Result<u8, String> {
    let commander = FormatConfig::commander();
    let players = s.trim().parse::<u8>().map_err(|_| {
        format!(
            "error: --players {s:?} must be a seat count in {}..={}",
            commander.min_players, commander.max_players
        )
    })?;
    commander
        .validate_for_player_count(players)
        .map_err(|e| format!("error: --players {s:?}: {e}"))?;
    Ok(players)
}

/// Resolves a `--difficulty-pK <label>` seat override. `flag` is the flag text
/// exactly as typed (errors echo it, not a normalized index); `value` is the following CLI arg (the label), if
/// present. Mirrors the hard-fail discipline of `parse_difficulty_checked` /
/// `parse_action_cap_checked`: a non-numeric or out-of-range (`>= players`) seat index,
/// or a missing label, is a startup error rather than a silently dropped flag —
/// which previously also swallowed the label arg, cascading it into the
/// catch-all. Label validity is delegated to `parse_difficulty_checked` by the caller.
/// `players` is a validated seat count (`parse_player_count` /
/// `DEFAULT_PLAYERS`, so ≥ 2). `parse_cli` calls this after the argument loop
/// so the check doesn't depend on argument order.
fn parse_seat_override<'a>(
    flag: &str,
    value: Option<&'a str>,
    players: u8,
) -> Result<(usize, &'a str), String> {
    let suffix = flag.strip_prefix("--difficulty-p").unwrap_or(flag);
    let idx = suffix
        .parse::<usize>()
        .ok()
        .filter(|&i| i < usize::from(players))
        .ok_or_else(|| format!("error: {flag}: seat index must be 0..={}", players - 1))?;
    let label = value.ok_or_else(|| format!("error: {flag} requires a difficulty label"))?;
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

/// Builds the `players`-seat commander `GameState` this driver plays, from a
/// resolved deck payload. Single authority for this bin's setup sequence —
/// `main()` and the setup regression test below both call this, so the two
/// can never drift apart.
///
/// Loads through the canonical `load_and_hydrate_decks` init path shared with
/// `engine-wasm/src/lib.rs`, `replay.rs` and `server-core/src/session.rs`, so
/// dual-faced cards get their back faces and `state.all_card_names` (a
/// `#[serde(skip)]` field, so it is never restored by deserialization) is
/// populated. Without the pool, `NamedChoice { choice_type: CardName, .. }`
/// candidate generation (`ai_support::candidate_actions` ->
/// `card_name_choice_candidates`) sees an empty `all_card_names` and returns
/// zero legal actions — a permanent AI stall the first time an opponent's
/// card triggers a "name a card" choice.
fn build_game_state(db: &CardDatabase, payload: &DeckPayload, players: u8, seed: u64) -> GameState {
    let mut state = GameState::new(FormatConfig::commander(), players, seed);
    load_and_hydrate_decks(&mut state, payload, Some(db));
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
///
/// `players` is the table's seat count, which `create_config_for_players`
/// scales search by.
fn build_seat_config(
    difficulty: AiDifficulty,
    players: u8,
    seed: u64,
    run_context: RunContext,
) -> AiConfig {
    let config = create_config_for_players(difficulty, Platform::Native, players);
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
    /// (called by `main()`) builds the commander `GameState` (four seats here)
    /// from a resolved deck payload and must set `state.all_card_names`, the
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

        // Calls the same setup function `main()` uses — if `build_game_state`
        // ever drops back to a loader that skips the card-name pool, this test
        // fails instead of silently passing against a duplicated copy.
        let mut state = build_game_state(&db, &payload, 4, 42);

        assert!(
            !state.all_card_names.is_empty(),
            "setup must populate all_card_names right after deck loading"
        );

        state.waiting_for = WaitingFor::NamedChoice {
            free_entry: None,
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
            let config = build_seat_config(
                difficulty,
                DEFAULT_PLAYERS,
                95_000_004,
                RunContext::Measurement,
            );
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
            let config = build_seat_config(
                difficulty,
                DEFAULT_PLAYERS,
                95_000_004,
                RunContext::Interactive,
            );
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
        let mut watched = WatchedCards::default();

        let results = vec![
            AiActionResult {
                action: GameAction::PassPriority,
                state: state.clone(),
                events: vec![GameEvent::SpellCast {
                    card_id: CardId(100),
                    controller: PlayerId(0),
                    object_id: cast_obj,
                    cast_mana_value: None,
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
                    GameEvent::CardDrawn {
                        player_id: PlayerId(0),
                        object_id: ObjectId(999),
                        nth_in_turn: 3,
                        nth_in_step: 3,
                    },
                ],
                log_entries: Vec::new(),
            },
        ];

        record_watched_cards(&results, &watch, &mut watched);

        assert_eq!(
            watched.to_json(),
            serde_json::json!({
                "cards_seen": ["Lightning Bolt", "Sol Ring"],
                "card_counts": [
                    {"seat": 0, "name": "Lightning Bolt", "draws": 0, "casts": 1},
                    {"seat": 0, "name": "Sol Ring", "draws": 1, "casts": 0}
                ]
            })
        );
    }

    #[test]
    fn record_watched_cards_is_a_noop_on_empty_results() {
        let watch: HashSet<String> = ["Lightning Bolt".to_string()].into_iter().collect();
        let mut watched = WatchedCards::default();
        record_watched_cards(&[], &watch, &mut watched);
        assert_eq!(
            watched.to_json(),
            serde_json::json!({"cards_seen": [], "card_counts": []})
        );
    }

    #[test]
    fn watched_counts_use_event_seats_and_accumulate_across_batches() {
        let name = "Xira, the Golden Sting";
        let args: Vec<String> = ["ai-commander", "--watch-card", name]
            .into_iter()
            .map(str::to_string)
            .collect();
        let watch = parse_cli(&args, true).unwrap().watch_cards;
        let mut state = GameState::new(FormatConfig::commander(), 4, 1);
        let object_id = ObjectId(100);
        let mut object = GameObject::new(
            object_id,
            CardId(100),
            PlayerId(0),
            name.to_string(),
            Zone::Stack,
        );
        // Neither the owner nor the current controller determines the event seat.
        object.controller = PlayerId(1);
        state.objects.insert(object_id, object);
        let result = AiActionResult {
            action: GameAction::PassPriority,
            state,
            events: vec![
                GameEvent::SpellCast {
                    card_id: CardId(100),
                    controller: PlayerId(3),
                    object_id,
                    cast_mana_value: None,
                },
                GameEvent::CardDrawn {
                    player_id: PlayerId(2),
                    object_id,
                    nth_in_turn: 1,
                    nth_in_step: 1,
                },
                GameEvent::SpellCast {
                    card_id: CardId(100),
                    controller: PlayerId(0),
                    object_id,
                    cast_mana_value: None,
                },
                GameEvent::CardDrawn {
                    player_id: PlayerId(1),
                    object_id,
                    nth_in_turn: 1,
                    nth_in_step: 1,
                },
            ],
            log_entries: Vec::new(),
        };
        let mut watched = WatchedCards::default();
        for _ in 0..2 {
            record_watched_cards(std::slice::from_ref(&result), &watch, &mut watched);
        }
        assert_eq!(
            watched.to_json(),
            serde_json::json!({
                "cards_seen": [name],
                "card_counts": [
                    {"seat": 0, "name": name, "draws": 0, "casts": 2},
                    {"seat": 1, "name": name, "draws": 2, "casts": 0},
                    {"seat": 2, "name": name, "draws": 2, "casts": 0},
                    {"seat": 3, "name": name, "draws": 0, "casts": 2}
                ]
            })
        );

        // A new game's accumulator and an empty watch set cannot inherit counts.
        let mut next_game = WatchedCards::default();
        record_watched_cards(&[result], &HashSet::new(), &mut next_game);
        assert_eq!(
            next_game.to_json(),
            serde_json::json!({"cards_seen": [], "card_counts": []})
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
        assert!(parse_seat_override("--difficulty-px", Some("Hard"), 4).is_err());
    }

    #[test]
    fn parse_seat_override_rejects_out_of_range_index() {
        assert!(parse_seat_override("--difficulty-p4", Some("Hard"), 4).is_err());
        assert!(parse_seat_override("--difficulty-p9", Some("Hard"), 4).is_err());
        assert!(parse_seat_override("--difficulty-p5", Some("Hard"), 5).is_err());
    }

    #[test]
    fn parse_seat_override_requires_a_label_value() {
        assert!(parse_seat_override("--difficulty-p2", None, 4).is_err());
    }

    #[test]
    fn parse_seat_override_accepts_valid_seat_and_label() {
        assert_eq!(
            parse_seat_override("--difficulty-p2", Some("Hard"), 4),
            Ok((2, "Hard"))
        );
        assert_eq!(
            parse_seat_override("--difficulty-p0", Some("VeryHard"), 4),
            Ok((0, "VeryHard"))
        );
        // The bound is the table's seat count, not a fixed four.
        assert_eq!(
            parse_seat_override("--difficulty-p4", Some("Hard"), 5),
            Ok((4, "Hard"))
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
    /// validates the batch up front), `--watch-cards`, `--players`, and the
    /// seat-override form `--difficulty-p<K>`.
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
            &["--watch-card", "Xira, the Golden Sting", "some/path"],
            &["--difficulty-p0", "Easy", "some/path"],
            &["--players", "5", "some/path"],
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

    #[test]
    fn parse_cli_watch_card_trims_whitespace_without_splitting_names() {
        let args: Vec<String> = [
            "ai-commander",
            "--watch-card",
            "Xira, the Golden Sting",
            "--watch-card",
            "Éomer, Marshal of Rohan",
            "--watch-card",
            "Xira, the Golden Sting",
            "--watch-card",
            " Sol Ring ",
            "some/path",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        let cli = parse_cli(&args, false).unwrap();
        assert_eq!(
            cli.watch_cards,
            [
                "Xira, the Golden Sting",
                "Éomer, Marshal of Rohan",
                "Sol Ring"
            ]
            .into_iter()
            .map(str::to_string)
            .collect()
        );
        assert_eq!(cli.cards_path, "some/path");
    }

    #[test]
    fn parse_cli_watch_options_combine_with_last_legacy_list() {
        let args: Vec<String> = [
            "ai-commander",
            "--watch-card",
            "Xira, the Golden Sting",
            "--watch-cards",
            "Forest",
            "--watch-cards",
            "Sol Ring, Arcane Signet,,",
            "--watch-card",
            "Sol Ring",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        let cli = parse_cli(&args, false).unwrap();
        assert_eq!(
            cli.watch_cards,
            ["Xira, the Golden Sting", "Sol Ring", "Arcane Signet"]
                .into_iter()
                .map(str::to_string)
                .collect()
        );
    }

    #[test]
    fn parse_cli_watch_card_rejects_missing_or_blank_name() {
        for value in [None, Some(""), Some("  "), Some("--seed"), Some(" --seed")] {
            let mut args = vec!["ai-commander".to_string(), "--watch-card".to_string()];
            args.extend(value.map(str::to_string));
            assert!(matches!(
                parse_cli(&args, false),
                Err(message) if message == "error: --watch-card requires an exact card name"
            ));
        }
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

    /// `parse_cli` over `ai-commander <rest…>` with no measurement override.
    fn parse_cli_args(rest: &[&str]) -> Result<CliArgs, String> {
        let args: Vec<String> = std::iter::once("ai-commander")
            .chain(rest.iter().copied())
            .map(str::to_string)
            .collect();
        parse_cli(&args, false)
    }

    /// Precon-style feed decks over `fixture_db()`'s two cards, one per entry of
    /// `sizes`: deck `i` lists `sizes[i]` copies of "Test Land" plus its
    /// commander "Test Commander" (which `select_seat_decks` drops from the main
    /// deck). Distinct sizes make each seat's deck identifiable downstream.
    fn fixture_feed_decks(sizes: &[u64]) -> Vec<serde_json::Value> {
        sizes
            .iter()
            .enumerate()
            .map(|(i, &size)| {
                serde_json::json!({
                    "name": format!("Fixture Deck {i}"),
                    "commander": ["Test Commander"],
                    "main": [
                        { "name": "Test Land", "count": size },
                        { "name": "Test Commander", "count": 1 }
                    ]
                })
            })
            .collect()
    }

    /// A feed deck whose commander is absent from `fixture_db()`, so
    /// `select_seat_decks` must skip it rather than seat it.
    fn ghost_feed_deck() -> serde_json::Value {
        serde_json::json!({
            "name": "Ghost Deck",
            "commander": ["Ghost Commander"],
            "main": [{ "name": "Test Land", "count": 50 }]
        })
    }

    #[test]
    fn parse_cli_defaults_to_four_players() {
        let cli = parse_cli_args(&[]).expect("bare invocation must parse");
        assert_eq!(cli.players, 4);
        assert_eq!(cli.seat_difficulty.len(), 4);
    }

    #[test]
    fn default_player_count_is_within_the_commander_preset() {
        assert_eq!(DEFAULT_PLAYERS, 4);
        assert!(FormatConfig::commander()
            .validate_for_player_count(DEFAULT_PLAYERS)
            .is_ok());
    }

    #[test]
    fn parse_player_count_accepts_the_commander_seat_range() {
        let commander = FormatConfig::commander();
        for n in commander.min_players..=commander.max_players {
            assert_eq!(parse_player_count(&n.to_string()), Ok(n));
        }
        assert_eq!(parse_player_count(" 5 "), Ok(5));
    }

    #[test]
    fn parse_player_count_rejects_out_of_range_and_malformed_values() {
        let commander = FormatConfig::commander();
        let below_min = (commander.min_players - 1).to_string();
        let above_max = (commander.max_players + 1).to_string();
        for value in [
            below_min.as_str(),
            above_max.as_str(),
            "0",
            "-1",
            "abc",
            "",
            "4.0",
            "256",
        ] {
            let message = parse_player_count(value)
                .expect_err(&format!("--players {value:?} must be rejected"));
            assert!(
                message.contains("--players"),
                "{value:?}: the error must name the flag: {message}"
            );
        }
    }

    #[test]
    fn parse_cli_players_value_sizes_the_table() {
        for (value, players) in [("2", 2u8), ("5", 5)] {
            let cli = parse_cli_args(&["--players", value]).expect("--players must parse");
            assert_eq!(cli.players, players);
            assert_eq!(cli.seat_difficulty.len(), usize::from(players));
        }
    }

    #[test]
    fn parse_cli_rejects_missing_or_malformed_players_value() {
        // Reach guard: a well-formed value gets past the arm.
        assert!(parse_cli_args(&["--players", "3"]).is_ok());

        assert_eq!(
            parse_cli_args(&["--players"]).err().as_deref(),
            Some("error: --players requires a seat count")
        );
        let rows: &[&[&str]] = &[
            &["--players", "7"],
            &["--players", "1"],
            &["--players", "abc"],
            // The value is consumed (and rejected), never skipped.
            &["--players", "--seed", "7"],
        ];
        for row in rows {
            assert!(parse_cli_args(row).is_err(), "row {row:?} must be rejected");
        }
    }

    #[test]
    fn parse_cli_rejects_missing_or_malformed_value_naming_the_flag() {
        // Reach guard: well-formed values still parse and reach the option.
        let cli = parse_cli_args(&[
            "--seed",
            "7",
            "--difficulty",
            "Hard",
            "--action-cap",
            "9",
            "--feed",
            "f.json",
            "--difficulty-p1",
            "VeryHard",
        ])
        .expect("well-formed values must parse");
        assert_eq!(cli.seed, 7);
        assert_eq!(cli.difficulty, AiDifficulty::Hard);
        assert_eq!(cli.action_cap, 9);
        assert_eq!(cli.feed, "f.json");
        assert_eq!(cli.seat_difficulty[1], Some(AiDifficulty::VeryHard));
        assert_eq!(parse_cli_args(&["--seed", " 7"]).unwrap().seed, 7);
        let cli = parse_cli_args(&["--watch-cards", "Sol Ring, ,Arcane Signet"]).unwrap();
        assert_eq!(cli.watch_cards.len(), 2);

        // (args, flag the error must name, flags the error must NOT name)
        let rows: &[(&[&str], &str, &[&str])] = &[
            (&["--seed"], "--seed", &[]),
            (&["--seed", "abc"], "--seed", &[]),
            (&["--seed", "-1"], "--seed", &[]),
            (&["--difficulty"], "--difficulty", &[]),
            (&["--difficulty", "Bogus"], "--difficulty", &[]),
            (&["--action-cap"], "--action-cap", &[]),
            (&["--action-cap", "abc"], "--action-cap", &[]),
            (&["--seed", " 7x"], "--seed", &[]),
            (&["--feed"], "--feed", &[]),
            (&["--watch-cards"], "--watch-cards", &[]),
            (&["--watch-cards", ""], "--watch-cards", &[]),
            (&["--watch-cards", "  "], "--watch-cards", &[]),
            (&["--watch-cards", ","], "--watch-cards", &[]),
            // A following flag must not be swallowed as the value.
            (&["--watch-cards", "--seed", "7"], "--watch-cards", &[]),
            (
                &["--difficulty-p01", "Bogus"],
                "--difficulty-p01",
                &["--difficulty-p1", "unrecognized --difficulty "],
            ),
            (
                &["--difficulty-p+1"],
                "--difficulty-p+1",
                &["--difficulty-p1 "],
            ),
            (&["--difficulty-p1"], "--difficulty-p1", &[]),
            (
                &["--difficulty-p1", "Bogus"],
                "--difficulty-p1",
                &["unrecognized --difficulty "],
            ),
            (
                &["--players", "3", "--difficulty-p2", "Bogus"],
                "--difficulty-p2",
                &["unrecognized --difficulty "],
            ),
        ];
        for (row, flag, absent) in rows {
            let message = parse_cli_args(row)
                .err()
                .unwrap_or_else(|| panic!("row {row:?} must be rejected"));
            assert!(
                message.contains(flag),
                "row {row:?}: error {message:?} must name {flag}"
            );
            for bad in *absent {
                assert!(
                    !message.contains(bad),
                    "row {row:?}: error {message:?} must not contain {bad:?}"
                );
            }
        }
    }

    #[test]
    fn parse_cli_seat_override_is_validated_against_players_in_either_order() {
        // K < N is accepted whichever of the two flags comes first.
        let accepted: &[&[&str]] = &[
            &["--players", "5", "--difficulty-p4", "Hard"],
            &["--difficulty-p4", "Hard", "--players", "5"],
        ];
        for row in accepted {
            let cli = parse_cli_args(row).expect("row must parse");
            assert_eq!(cli.seat_difficulty.len(), 5, "row {row:?}");
            assert_eq!(
                cli.seat_difficulty[4],
                Some(AiDifficulty::Hard),
                "row {row:?}"
            );
        }
        let cli = parse_cli_args(&["--players", "2", "--difficulty-p1", "Hard"])
            .expect("the last seat of a two-seat table must parse");
        assert_eq!(cli.seat_difficulty, vec![None, Some(AiDifficulty::Hard)]);
        let cli = parse_cli_args(&["--difficulty-p5", "VeryHard", "--players", "6"])
            .expect("the last seat of a six-seat table must parse");
        assert_eq!(cli.seat_difficulty.len(), 6);
        assert_eq!(cli.seat_difficulty[5], Some(AiDifficulty::VeryHard));

        // K >= N is rejected whichever of the two flags comes first.
        let rejected: &[&[&str]] = &[
            &["--players", "4", "--difficulty-p4", "Hard"],
            &["--difficulty-p4", "Hard", "--players", "4"],
            &["--difficulty-p4", "Hard"],
            &["--players", "2", "--difficulty-p2", "Hard"],
            &["--difficulty-p2", "Hard", "--players", "2"],
            &["--difficulty-p6", "Hard", "--players", "6"],
        ];
        for row in rejected {
            assert!(parse_cli_args(row).is_err(), "row {row:?} must be rejected");
        }

        // The default table keeps today's exact message.
        assert_eq!(
            parse_cli_args(&["--difficulty-p4", "Hard"])
                .err()
                .as_deref(),
            Some("error: --difficulty-p4: seat index must be 0..=3")
        );
        assert_eq!(
            parse_cli_args(&["--players", "2", "--difficulty-p2", "Hard"])
                .err()
                .as_deref(),
            Some("error: --difficulty-p2: seat index must be 0..=1")
        );

        // A repeated override of one seat keeps the last, across `--players`.
        let cli = parse_cli_args(&[
            "--difficulty-p1",
            "Easy",
            "--players",
            "3",
            "--difficulty-p1",
            "Hard",
        ])
        .expect("repeated override must parse");
        assert_eq!(
            cli.seat_difficulty,
            vec![None, Some(AiDifficulty::Hard), None]
        );
    }

    #[test]
    fn select_seat_decks_takes_the_first_n_passing_decks_in_seat_order() {
        let db = fixture_db();
        let mut feed = vec![ghost_feed_deck()];
        feed.extend(fixture_feed_decks(&[11, 12, 13, 14, 15, 16, 17]));
        for players in [2u8, 4, 5, 6] {
            let deck_list =
                select_seat_decks(&db, &feed, players).expect("enough decks must be seated");
            assert_eq!(
                deck_list.ai_decks.len(),
                usize::from(players) - 2,
                "{players} seats"
            );
            let seats: Vec<&PlayerDeckList> = [&deck_list.player, &deck_list.opponent]
                .into_iter()
                .chain(deck_list.ai_decks.iter())
                .collect();
            let sizes: Vec<usize> = seats.iter().map(|seat| seat.main_deck.len()).collect();
            let expected: Vec<usize> = (11..11 + usize::from(players)).collect();
            assert_eq!(
                sizes, expected,
                "{players} seats: the ghost deck is skipped and decks are seated in feed order"
            );
            for seat in &seats {
                assert!(
                    seat.main_deck.iter().all(|name| name == "Test Land"),
                    "{players} seats: the commander is not left in the main deck"
                );
                assert_eq!(seat.commander, vec!["Test Commander".to_string()]);
            }
        }
    }

    #[test]
    fn select_seat_decks_reports_too_few_decks_with_the_requested_count() {
        let db = fixture_db();
        let mut feed = vec![ghost_feed_deck()];
        feed.extend(fixture_feed_decks(&[11, 12, 13, 14]));
        // Reach guard: the same feed seats a four-seat table.
        assert!(select_seat_decks(&db, &feed, 4).is_ok());
        assert_eq!(
            select_seat_decks(&db, &feed, 5).err().as_deref(),
            Some("need at least 5 precons, found 4")
        );
        let feed = fixture_feed_decks(&[11, 12, 13]);
        assert_eq!(
            select_seat_decks(&db, &feed, 4).err().as_deref(),
            Some("need at least 4 precons, found 3")
        );
    }

    #[test]
    fn build_game_state_seats_exactly_n_players_with_their_own_decks() {
        let db = fixture_db();
        let feed = fixture_feed_decks(&[11, 12, 13, 14, 15, 16]);
        for players in [2u8, 5, 6] {
            let deck_list =
                select_seat_decks(&db, &feed, players).expect("enough decks must be seated");
            let payload = resolve_deck_list(&db, &deck_list);
            let state = build_game_state(&db, &payload, players, 42);
            assert_eq!(state.players.len(), usize::from(players));
            for (k, player) in state.players.iter().enumerate() {
                assert_eq!(
                    player.library.len(),
                    11 + k,
                    "{players} seats: seat {k} must hold its own deck"
                );
            }
        }
    }

    #[test]
    fn seat_config_scales_to_the_table_player_count() {
        for players in [2u8, 4, 5] {
            let config =
                build_seat_config(AiDifficulty::Medium, players, 1, RunContext::Measurement);
            assert_eq!(config.player_count, players);
        }
    }

    /// A short action-capped game at a non-default seat count: every seat must
    /// be in the AI set and hold an AI config, or the driver stalls on that
    /// seat's opening decision (`NoEligibleAiActor` / `MissingAiConfig`); and
    /// the elimination tracker must not be longer than the table, or its scan
    /// indexes past `state.players` (the two-seat leg). No seat is eliminated
    /// within the cap, so a tracker shorter than the table is not caught here.
    /// Runs on a `GAME_THREAD_STACK_SIZE` thread, as `main` drives games.
    #[test]
    fn play_one_game_drives_every_seat_at_non_default_player_counts() {
        let handle = std::thread::Builder::new()
            .stack_size(GAME_THREAD_STACK_SIZE)
            .spawn(|| {
                let db = fixture_db();
                let watch_cards = HashSet::new();
                [2u8, 5]
                    .into_iter()
                    .map(|players| {
                        let feed = fixture_feed_decks(&vec![20; usize::from(players)]);
                        let deck_list = select_seat_decks(&db, &feed, players)
                            .expect("enough decks must be seated");
                        let payload = resolve_deck_list(&db, &deck_list);
                        let seat_difficulty = vec![None; usize::from(players)];
                        let context = GameRunContext {
                            db: &db,
                            payload: &payload,
                            players,
                            seat_difficulty: &seat_difficulty,
                            action_cap: 200,
                            dump_log_path: None,
                            dump_actions_path: None,
                            watch_cards: &watch_cards,
                            run_context: RunContext::Measurement,
                        };
                        (players, play_one_game(&context, 7, AiDifficulty::Easy))
                    })
                    .collect::<Vec<_>>()
            })
            .expect("spawn game thread");
        let outcomes = handle.join().expect("no game may panic");
        assert_eq!(outcomes.len(), 2);
        for (players, outcome) in outcomes {
            assert!(
                matches!(outcome, RunOutcome::Completed | RunOutcome::Aborted),
                "{players} seats: the driver must act for every seat, got {outcome:?}"
            );
        }
    }

    #[test]
    fn parse_cli_games_file_batch_carries_the_players_count() {
        let path = temp_games_file_path("games_file_players");
        let _guard = TempFileGuard(path.clone());
        std::fs::write(&path, "1009,Easy\n").unwrap();
        let cli = parse_cli_args(&["--players", "2", "--games-file", path.to_str().unwrap()])
            .expect("games-file invocation must parse");
        assert_eq!(cli.players, 2);
        assert_eq!(cli.seat_difficulty.len(), 2);
        assert_eq!(cli.batch_games, Some(vec![(1009, AiDifficulty::Easy)]));
    }
}
