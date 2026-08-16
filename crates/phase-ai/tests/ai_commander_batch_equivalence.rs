//! Subprocess-level regression tests for `ai-commander`'s `--games-file` batch
//! mode: a batch invocation's per-game output must match the single-game
//! invocation of the same seed+difficulty, byte-for-byte outside the one
//! inherently nondeterministic field (wall-clock elapsed time). These spawn the
//! real `ai-commander` binary rather than calling `run()` in-process, so what is
//! under test is the binary's actual contract with the pod-lab harness —
//! argument parsing, per-game stdout framing, flush timing, process exit — not
//! an internal function's.
//!
//! Every test here is `#[ignore]`d: it loads `client/public/card-data.json`
//! (requires `cargo run --bin card-data-export` or the setup.sh script), which
//! is not available in unit-test CI — the same convention as
//! `greasefang_bounded.rs`/`whitemane_lion_bounded.rs`. Opt in via
//! `cargo test -p phase-ai --test ai_commander_batch_equivalence -- --ignored`.

use std::path::PathBuf;
use std::process::Command;

/// Resolves `client/public` the same way `greasefang_bounded.rs` et al. do: a
/// `PHASE_CARDS_PATH` override, else relative to the crate's manifest dir.
fn cards_dir() -> PathBuf {
    std::env::var("PHASE_CARDS_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("..")
                .join("client")
                .join("public")
        })
}

/// Small action cap so these tests run quickly while still exercising
/// several turns. The exact outcome (COMPLETED/ABORT/STALL) doesn't matter
/// for equivalence — only that a given seed+feed+cap combination reaches the
/// SAME outcome deterministically, single-game or batched.
const TEST_ACTION_CAP: &str = "300";

/// Runs the real `ai-commander` binary with `cards_dir()` as its positional
/// arg and `args` appended, and returns captured stdout as a `String`. Exit
/// code isn't asserted here: COMPLETED (0)/ABORT (2)/STALL (3) are all
/// legitimate outcomes for a bounded-action-cap test game, and every one of
/// them still prints a full `=== RESULT ===` block — `normalized_result_block`
/// panics if that block is missing, which already catches an actual crash.
///
/// `measurement` controls whether `PHASE_AI_MEASUREMENT=1` is set on the
/// child process (D1's harness escape hatch, matching the `PHASE_DUMP_*` env
/// convention). This is a process-level knob that `parse_cli` consults via
/// the `measurement_env` parameter, forcing a solo route to
/// `RunContext::Measurement`.
fn run_ai_commander_with_context(args: &[&str], measurement: bool) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ai-commander"));
    command.arg(cards_dir()).args(args);
    if measurement {
        command.env("PHASE_AI_MEASUREMENT", "1");
    }
    let output = command.output().expect("spawn ai-commander");
    String::from_utf8(output.stdout).expect("stdout is valid UTF-8")
}

/// Thin wrapper over [`run_ai_commander_with_context`] for call sites that
/// don't care about run context -- preserves the pre-D1 behavior (always
/// measurement) for them.
fn run_ai_commander(args: &[&str]) -> String {
    run_ai_commander_with_context(args, true)
}

/// Splits `stdout` into one chunk per game. Batch mode anchors on the
/// `--- GAME ` marker: each chunk then runs from one game's own marker up to
/// (not including) the next game's marker or EOF, so it's fully
/// self-contained — critically, a game's marker + per-seat tier echo (all
/// printed BEFORE that game's own "Game started." line) stay attributed to
/// THAT game, not leaked onto the end of the previous one. Single-game mode
/// never prints a marker, so the whole output is one chunk.
fn game_blocks(stdout: &str) -> Vec<&str> {
    const MARKER: &str = "--- GAME ";
    if !stdout.contains(MARKER) {
        return vec![stdout];
    }
    let mut starts: Vec<usize> = stdout.match_indices(MARKER).map(|(i, _)| i).collect();
    starts.push(stdout.len());
    starts.windows(2).map(|w| &stdout[w[0]..w[1]]).collect()
}

/// The `=== RESULT ===` epilogue through the end of one game's block (as
/// isolated by `game_blocks`), with the single inherently nondeterministic
/// line (`Elapsed: {:.1}s`, wall-clock) stripped so two separately-timed runs
/// can be compared for equality.
fn normalized_result_block(game_block: &str) -> String {
    let start = game_block
        .find("=== RESULT ===")
        .expect("game block contains a RESULT epilogue");
    game_block[start..]
        .lines()
        .filter(|line| !line.starts_with("Elapsed:"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Higher action cap for the cross-game-leakage regression below: the
/// wall-clock-deadline divergence this guards only surfaces deep into a game
/// (the original repro diverged around turn ~50 / action ~1700), so the tiny
/// `TEST_ACTION_CAP` used by the plumbing tests above would never reach the
/// decisions where an interactive search's budget could straddle. Still bounded
/// so an `#[ignore]` opt-in run stays minutes, not tens of minutes.
const LEAK_REGRESSION_ACTION_CAP: &str = "2000";

/// Regression for the cross-game state-leakage defect (pod-lab equivalence
/// gate; PR phase-rs/phase#6252): seed 95000004 won cleanly for P1 run solo but
/// STALLED at turn 60 when played as the 3rd game in a `--games-file` batch
/// (after 2 unrelated games in the same process). Root cause: the AI's
/// interactive search was bounded by a wall-clock deadline
/// (`AI_SEARCH_TIME_BUDGET_MS`), and a warmer/slower Nth-in-batch process
/// expired it on a mid-game decision the fresh solo process completed in full —
/// diverging the game. The fix runs every seat in measurement mode
/// (`build_seat_config`), disabling the wall-clock deadline so search is a pure
/// function of `(seed, difficulty, feed)`.
///
/// This encodes the repro shape end-to-end: the SAME game, run alone vs. run
/// 3rd after 2 unrelated games, must produce a byte-identical RESULT block.
/// (The deterministic, box-speed-independent catcher for this bug is
/// `ai_commander.rs`'s `seat_config_runs_in_measurement_mode_for_batch_reproducibility`
/// unit test; this is the integration-level forward guard for the whole path.)
#[test]
#[ignore = "loads card-data.json + runs real games; opt in via --ignored"]
fn batched_third_game_matches_same_game_run_alone() {
    let target_seed = "95000004";
    let prefix_a = "95000000";
    let prefix_b = "95000001";

    // The target game run entirely alone (single-game mode), explicitly
    // under measurement: this is the re-derived invariant post-D1 -- a
    // batch game (always Measurement) must match a solo game run under the
    // same PHASE_AI_MEASUREMENT harness escape hatch, not a bare solo run
    // (which routes to Interactive by default).
    let solo = run_ai_commander_with_context(
        &[
            "--seed",
            target_seed,
            "--difficulty",
            "Hard",
            "--action-cap",
            LEAK_REGRESSION_ACTION_CAP,
        ],
        true,
    );

    // The SAME target game, but 3rd in a batch process that first played two
    // unrelated games — the exact configuration that leaked before the fix.
    let pid = std::process::id();
    let games_file_path = std::env::temp_dir().join(format!("ai_commander_leak_repro_{pid}.txt"));
    std::fs::write(
        &games_file_path,
        format!("{prefix_a},Hard\n{prefix_b},Hard\n{target_seed},Hard\n"),
    )
    .expect("write games-file");
    let batch = run_ai_commander(&[
        "--games-file",
        games_file_path.to_str().unwrap(),
        "--action-cap",
        LEAK_REGRESSION_ACTION_CAP,
    ]);
    let _ = std::fs::remove_file(&games_file_path);

    let solo_blocks = game_blocks(&solo);
    let batch_blocks = game_blocks(&batch);
    assert_eq!(
        solo_blocks.len(),
        1,
        "single-game must print exactly one game"
    );
    assert_eq!(
        batch_blocks.len(),
        3,
        "batch must print exactly one block per games-file line"
    );

    // Block index 2 is the 3rd (target) game in the batch.
    assert_eq!(
        normalized_result_block(solo_blocks[0]),
        normalized_result_block(batch_blocks[2]),
        "the target game played 3rd in a batch must be bit-identical to the \
         same game run alone; a divergence here is the cross-game leak \
         (wall-clock-deadline non-determinism) regressing"
    );
}

#[test]
#[ignore = "loads card-data.json + runs real games; opt in via --ignored"]
fn batch_output_echoes_seed_and_parsed_difficulty_per_game() {
    let pid = std::process::id();
    let games_file_path = std::env::temp_dir().join(format!("ai_commander_echo_games_{pid}.txt"));
    std::fs::write(&games_file_path, "9101,Easy\n9102,VeryHard\n").expect("write games-file");

    let batch = run_ai_commander(&[
        "--games-file",
        games_file_path.to_str().unwrap(),
        "--action-cap",
        TEST_ACTION_CAP,
    ]);
    let _ = std::fs::remove_file(&games_file_path);

    // Each game's marker must carry BOTH the seed and the difficulty as
    // actually parsed from that games-file line — not the process-level
    // `--difficulty` (which batch mode ignores). A tier that silently fell
    // back to the default would show up here as a wrong `difficulty=` label.
    assert!(
        batch.contains("--- GAME seed=9101 difficulty=Easy ---"),
        "game 1 must echo its parsed seed+difficulty in its marker:\n{batch}"
    );
    assert!(
        batch.contains("--- GAME seed=9102 difficulty=VeryHard ---"),
        "game 2 must echo its parsed seed+difficulty in its marker:\n{batch}"
    );
}

#[test]
#[ignore = "loads card-data.json + runs real games; opt in via --ignored"]
fn single_game_stdout_is_deterministic_and_preamble_is_pinned() {
    let seed = "9201";
    let out1 = run_ai_commander(&[
        "--seed",
        seed,
        "--difficulty",
        "Easy",
        "--action-cap",
        TEST_ACTION_CAP,
    ]);
    let out2 = run_ai_commander(&[
        "--seed",
        seed,
        "--difficulty",
        "Easy",
        "--action-cap",
        TEST_ACTION_CAP,
    ]);

    // Two runs of the same seed/feed/difficulty must produce identical
    // output modulo wall-clock timing — the process-level half of the same
    // property `batched_third_game_matches_same_game_run_alone` asserts
    // across the batch boundary. If this one fails too, the divergence is in
    // the game itself, not in batch sequencing.
    let normalize = |s: &str| {
        s.lines()
            .filter(|l| !l.contains("elapsed=") && !l.starts_with("Elapsed:"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(normalize(&out1), normalize(&out2));

    // Pins the exact preamble single-game mode has always printed, in order.
    // A reordering (e.g. "Feed:" moving relative to "Seed:.../Difficulty:...")
    // would silently break the pod-lab harness's stdout parsing; this fails
    // loudly instead.
    let expected_preamble = format!(
        "=== 4-player Commander AI test ===\n\
         Feed: feeds/mtggoldfish-commander.json\n\
         Seed: {seed}   Difficulty: Easy\n\n"
    );
    assert!(
        out1.starts_with(&expected_preamble),
        "single-game preamble format changed:\n{out1}"
    );
}

/// D1 must-pass gate for B3: a plain single-game invocation (no
/// `--games-file`, no `PHASE_AI_MEASUREMENT` override) must route to
/// `RunContext::Interactive` and print the exact preamble marker
/// `ExecutionMode: interactive` (lowercase literal, not `{:?}`), placed after
/// the blank line that closes the existing pinned preamble (see
/// `single_game_stdout_is_deterministic_and_preamble_is_pinned`'s
/// `expected_preamble` -- that assertion is `starts_with`, so an appended
/// marker line does not conflict with it; that constant must not be edited).
#[test]
#[ignore = "loads card-data.json + runs real games; opt in via --ignored"]
fn single_game_default_route_prints_interactive_marker() {
    let out = run_ai_commander_with_context(
        &[
            "--seed",
            "9301",
            "--difficulty",
            "Easy",
            "--action-cap",
            TEST_ACTION_CAP,
        ],
        false,
    );
    assert!(
        out.contains("ExecutionMode: interactive"),
        "a plain single-game invocation must print the ExecutionMode: interactive marker line:
{out}"
    );
}

/// D1 must-pass gate for B3: a single-game invocation run under the
/// `PHASE_AI_MEASUREMENT` harness escape hatch must route to
/// `RunContext::Measurement` and print the exact preamble marker
/// `ExecutionMode: measurement` (lowercase literal, not `{:?}`). Sibling of
/// `single_game_default_route_prints_interactive_marker` -- together they
/// pin both `RunContext` variants at the same call site so neither can
/// silently regress. This is pod-lab's exact invocation shape (`runner.py`
/// sets `PHASE_AI_MEASUREMENT=1` on every single-game call per D7), so a
/// missing marker here is a live pod-lab tripwire failure, not a hypothetical.
#[test]
#[ignore = "loads card-data.json + runs real games; opt in via --ignored"]
fn single_game_measurement_env_prints_measurement_marker() {
    let out = run_ai_commander_with_context(
        &[
            "--seed",
            "9301",
            "--difficulty",
            "Easy",
            "--action-cap",
            TEST_ACTION_CAP,
        ],
        true,
    );
    assert!(
        out.contains("ExecutionMode: measurement"),
        "a single-game invocation under PHASE_AI_MEASUREMENT=1 must print the ExecutionMode: measurement marker line:
{out}"
    );
}
