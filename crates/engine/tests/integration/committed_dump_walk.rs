//! CR 732.2a: every committed dump, restored through the production decoder and driven to its
//! first loop-shortcut offer, game over, or the beat budget, with what the classification
//! accessors answer there.
//!
//! The population is every `*.json.gz` under `crates/`, and the test asserts it equals what git
//! tracks there. The outcome strings are pinned in [`EXPECTED`].

use std::path::{Path, PathBuf};
use std::time::Instant;

use engine::types::game_state::{GameState, LoopDetectionMode, PersistedGameState, WaitingFor};

use crate::loop_shortcut_drain_boards::{
    collect_gz, committed_document, committed_document_as_recorded, drive_one_beat,
};

/// Beats a dump that restores short of an offer is driven, one `apply()` each.
const OFFER_BEAT_BUDGET: u32 = 40;

/// Restore a committed dump in `Interactive` mode through `PersistedGameState`, trying the bare
/// document, then its `gameState`, then that debug snapshot projected onto the versioned wire.
pub(crate) fn restore_committed(path: &Path) -> Result<GameState, String> {
    restore_document(path, committed_document(path), false)
}

/// [`restore_committed`] on the dump exactly as recorded, legacy table-wide
/// `spells_cast_last_turn` total included: the restore the walk pins (CR 117.1 + CR 603.4: a
/// nonzero total cannot say which player cast the spells of the previous turn, so production
/// refuses it).
fn restore_committed_as_recorded(path: &Path) -> Result<GameState, String> {
    restore_document(path, committed_document_as_recorded(path), true)
}

fn restore_document(
    path: &Path,
    document: serde_json::Value,
    keep_legacy_snapshot: bool,
) -> Result<GameState, String> {
    let decode = |mut board: serde_json::Value| {
        // The projected capture is built from the raw bytes, so the legacy total must be dropped
        // there too unless the restore under test is the as-recorded one.
        if !keep_legacy_snapshot {
            if let Some(board) = board.as_object_mut() {
                board.remove("spells_cast_last_turn");
            }
        }
        serde_json::from_value::<PersistedGameState>(board)
            .map_err(|error| error.to_string())
            .and_then(|persisted| {
                persisted
                    .into_game_state()
                    .map_err(|error| format!("{error:?}"))
            })
    };
    let mut restored = decode(document.clone());
    if restored.is_err() {
        if let Some(board) = document.get("gameState") {
            restored = decode(board.clone());
        }
    }
    // The projection panics on any other input, so it is tried only on its own precondition.
    let projectable = document
        .get("gameState")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|board| {
            board.contains_key("resolution_stack")
                && !board.contains_key("resolution_state_version")
        });
    if restored.is_err() && projectable {
        let bytes = std::fs::read(path).expect("the dump was read once already");
        restored =
            decode(crate::issue_8024_terminal_rest_captures::projected_capture_snapshot(&bytes));
    }
    let mut state = restored?;
    state.loop_detection = LoopDetectionMode::Interactive;
    Ok(state)
}

/// The offer's published shape and its predicted winner.
fn offer_line(state: &GameState) -> Option<String> {
    let WaitingFor::LoopShortcut {
        predicted_winner,
        certificate,
        schema,
        ..
    } = &state.waiting_for
    else {
        return None;
    };
    Some(format!(
        "bounded={} bound={:?} unbounded={:?} winner={predicted_winner:?}",
        certificate.per_cycle.is_some(),
        schema.repetition_bound,
        certificate.unbounded
    ))
}

fn walk_outcome(path: &Path) -> String {
    let mut state = match restore_committed_as_recorded(path) {
        Ok(state) => state,
        Err(error) => return format!("RESTOREERR {}", error.chars().take(60).collect::<String>()),
    };
    match offer_line(&state) {
        Some(offer) => format!("OFFER@restore {offer}"),
        None => {
            let mut aimed_at = None;
            let mut beat = 0;
            loop {
                if let Some(offer) = offer_line(&state) {
                    break format!("OFFER@{beat} {offer}");
                }
                if matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
                    break format!("GAMEOVER@{beat}");
                }
                if beat == OFFER_BEAT_BUDGET {
                    break format!("BUDGET {}", state.waiting_for.variant_name());
                }
                drive_one_beat(&mut state, &mut aimed_at);
                beat += 1;
            }
        }
    }
}

/// The legacy table-wide total a committed dump recorded, read from whichever root holds the game
/// state (debug export, trusted envelope, or bare state).
fn recorded_legacy_spells_cast_total(document: &serde_json::Value) -> Option<u64> {
    let root = document
        .get("gameState")
        .or_else(|| document.get("state"))
        .unwrap_or(document);
    root.get("spells_cast_last_turn")
        .and_then(serde_json::Value::as_u64)
}

/// CR 603.4 + CR 601.2i: a committed dump written before the per-player `spells_cast_last_turn`
/// snapshot carries one table-wide total of the previous turn; for a nonzero total the split between
/// players is unrecoverable, so production restore refuses it. The dumps stay byte-for-byte as
/// recorded (evidence). Unlike the ignored whole-population walk below, this runs in every build and
/// pins each nonzero dump: refused with the legacy message, and restorable once the total is
/// declared absent (paired, so the refusal is the total's and nothing else's).
#[test]
fn every_committed_dump_with_a_nonzero_legacy_spells_cast_total_is_refused() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files: Vec<PathBuf> = Vec::new();
    collect_gz(&root, &mut files);
    files.sort();
    // Relative paths stay `PathBuf`s and are compared component-wise, so the assertions hold with
    // either native separator.
    let mut refused: Vec<PathBuf> = Vec::new();
    let mut restorable_despite_total: Vec<PathBuf> = Vec::new();
    let mut refused_for_another_reason: Vec<PathBuf> = Vec::new();
    for path in &files {
        let recorded = committed_document_as_recorded(path);
        let Some(total) = recorded_legacy_spells_cast_total(&recorded) else {
            continue;
        };
        if total == 0 {
            continue;
        }
        let relative = path
            .strip_prefix(&root)
            .expect("collected under the root")
            .to_path_buf();
        match restore_committed_as_recorded(path) {
            Err(error) if error.contains("legacy spells_cast_last_turn snapshot") => {
                refused.push(relative)
            }
            Err(_) => refused_for_another_reason.push(relative),
            Ok(_) => restorable_despite_total.push(relative),
        }
    }
    // Reach: the refusal population is the one the ticket measured, not an empty walk.
    assert!(refused.len() >= 14, "refused dumps: {refused:#?}");
    for relative in &refused {
        // The positive control: with the total declared absent the dump restores. Any restore
        // error, the legacy one or an unrelated one, means the total alone is not the refusal.
        if let Err(error) = restore_committed(&root.join(relative)) {
            panic!(
                "{}: the paired restore without the total must succeed, but failed: {error}",
                relative.display()
            );
        }
    }
    // These carry a nonzero total yet restore as recorded: their restore goes through the
    // resolution-stack projection (`projected_capture_snapshot`), which rebuilds the state from the
    // capture's resolution record and never reads the legacy key.
    assert_eq!(
        restorable_despite_total,
        [
            Path::new("engine/tests/integration/fixtures/issue_8024_spell_rest_turn26.json.gz"),
            Path::new("engine/tests/integration/fixtures/mycoloth_devour_wedge_turn15.json.gz"),
            Path::new("engine/tests/integration/fixtures/mycoloth_devour_wedge_turn20.json.gz"),
        ],
        "{restorable_despite_total:#?}"
    );
    // A debug export that is refused for an older, unrelated reason before the total is read ("missing
    // the private rules record"); its total is pinned by `committed_dump_walk`'s EXPECTED row only.
    assert_eq!(
        refused_for_another_reason,
        [Path::new(
            "engine/tests/integration/fixtures/ureni_turn10_raw_resolution_stack.json.gz"
        )]
    );
}

/// Walks every committed dump, then asserts the outcome strings.
///
/// Measured at this row's first pin: about 8 minutes in the debug test profile, the projected
/// captures included, most of it spent driving dumps that never offer. The budget is the one
/// every offer the walk reaches needed. Run it with
/// `cargo test -p phase-engine --features test-support --test integration committed_dump_walk:: -- --ignored --nocapture`.
#[test]
#[ignore = "a walk of every committed dump, about 8 minutes; see the doc comment for the runner"]
fn every_committed_dump_reaches_its_pinned_outcome() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files: Vec<PathBuf> = Vec::new();
    collect_gz(&root, &mut files);
    let relative = |path: &Path| {
        path.strip_prefix(&root)
            .expect("collected under the root")
            .to_string_lossy()
            .into_owned()
    };
    let mut walked: Vec<String> = files.iter().map(|path| relative(path)).collect();
    walked.sort();

    let tracked = std::process::Command::new("git")
        .args(["ls-files", "-z", "--", "*.json.gz"])
        .current_dir(&root)
        .output()
        .expect("git runs");
    assert!(tracked.status.success(), "git ls-files failed");
    let mut tracked: Vec<String> = String::from_utf8(tracked.stdout)
        .expect("git prints UTF-8 paths")
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect();
    tracked.sort();
    assert_eq!(walked, tracked, "the walk covers exactly the tracked dumps");

    let started = Instant::now();
    let mut actual: Vec<(String, String)> = Vec::new();
    for path in &walked {
        let timer = Instant::now();
        let outcome = walk_outcome(&root.join(path));
        eprintln!("({path:?}, {outcome:?}), // {:?}", timer.elapsed());
        actual.push((path.clone(), outcome));
    }
    eprintln!("walked {} dumps in {:?}", actual.len(), started.elapsed());

    let expected: Vec<(String, String)> = EXPECTED
        .iter()
        .map(|(path, outcome)| (path.to_string(), outcome.to_string()))
        .collect();
    assert_eq!(actual, expected);
}

const EXPECTED: &[(&str, &str)] = &[
    ("engine/tests/fixtures/basalt_power_artifact_infinite_colorless.json.gz", "BUDGET Priority"),
    ("engine/tests/fixtures/codie_turn14.json.gz", "RESTOREERR legacy spells_cast_last_turn snapshot cannot say which playe"),
    ("engine/tests/fixtures/combo_infinite_pile_4p_offer.json.gz", "BUDGET Priority"),
    ("engine/tests/fixtures/combo_infinite_pile_4p_untapped_precast.json.gz", "BUDGET Priority"),
    ("engine/tests/fixtures/combo_infinite_pile_decklist_4p.json.gz", "RESTOREERR persisted game state has an invalid turn_number"),
    ("engine/tests/fixtures/cr733/authority_matrix.json.gz", "RESTOREERR persisted game state has an invalid turn_number"),
    ("engine/tests/fixtures/cr733/blocked_write_sites.json.gz", "RESTOREERR persisted game state has an invalid turn_number"),
    ("engine/tests/fixtures/cr733/rng_allocator_map.json.gz", "RESTOREERR persisted game state has an invalid turn_number"),
    ("engine/tests/fixtures/cr733/side_effect_map.json.gz", "RESTOREERR persisted game state has an invalid turn_number"),
    ("engine/tests/fixtures/dellian_emblem_conqueror_4p.json.gz", "RESTOREERR legacy spells_cast_last_turn snapshot cannot say which playe"),
    ("engine/tests/fixtures/dina_conqueror_4p.json.gz", "OFFER@6 bounded=true bound=Measured unbounded=[Life(PlayerId(0)), Life(PlayerId(1)), Life(PlayerId(2)), Life(PlayerId(3))] winner=Some(PlayerId(0))"),
    ("engine/tests/fixtures/dina_conqueror_phase5_no_offer_4p.json.gz", "OFFER@8 bounded=true bound=Measured unbounded=[Life(PlayerId(0)), Life(PlayerId(1)), Life(PlayerId(2)), Life(PlayerId(3))] winner=Some(PlayerId(0))"),
    ("engine/tests/fixtures/dina_noff_turn5_4p.json.gz", "OFFER@7 bounded=true bound=Measured unbounded=[Life(PlayerId(0)), Life(PlayerId(1)), Life(PlayerId(2)), Life(PlayerId(3))] winner=Some(PlayerId(0))"),
    ("engine/tests/fixtures/exquisite_blood_tenacity_alternating_offer_4p.json.gz", "RESTOREERR legacy spells_cast_last_turn snapshot cannot say which playe"),
    ("engine/tests/fixtures/exquisite_blood_tenacity_three_opponents_4p.json.gz", "RESTOREERR legacy spells_cast_last_turn snapshot cannot say which playe"),
    ("engine/tests/fixtures/f4_user_mode1_no_offer_4p.json.gz", "OFFER@19 bounded=true bound=Measured unbounded=[LibraryDelta(PlayerId(0)), Counter(Plus1Plus1, Creature), TokensCreated, Life(PlayerId(1))] winner=None"),
    ("engine/tests/fixtures/f4_user_mode2_accept_commits_nothing_4p.json.gz", "OFFER@24 bounded=true bound=Measured unbounded=[LibraryDelta(PlayerId(0)), Counter(Plus1Plus1, Creature), TokensCreated, Life(PlayerId(1))] winner=None"),
    ("engine/tests/fixtures/familiar_relic_turn5_2p.json.gz", "RESTOREERR legacy spells_cast_last_turn snapshot cannot say which playe"),
    ("engine/tests/fixtures/fantastic_four_bounded_loop_4p.json.gz", "OFFER@24 bounded=true bound=Measured unbounded=[LibraryDelta(PlayerId(0)), Counter(Plus1Plus1, Creature), TokensCreated, Life(PlayerId(1))] winner=None"),
    ("engine/tests/fixtures/kilo_freed_relic_pentad_4p.json.gz", "BUDGET Priority"),
    ("engine/tests/fixtures/kilo_freed_relic_pentad_max_of_one_4p.json.gz", "BUDGET Priority"),
    ("engine/tests/fixtures/legacy_deferred_spell_delivery/finale_of_promise_raw.json.gz", "RESTOREERR legacy deferred_spell_delivery snapshot cannot prove which o"),
    ("engine/tests/fixtures/legacy_deferred_spell_delivery/taigam_collected_conjuring_plain.json.gz", "RESTOREERR legacy deferred_spell_delivery snapshot cannot prove which o"),
    ("engine/tests/fixtures/legacy_deferred_spell_delivery/taigam_collected_conjuring_raw.json.gz", "RESTOREERR legacy deferred_spell_delivery snapshot cannot prove which o"),
    ("engine/tests/fixtures/lethal_lifegain_loss_4p.json.gz", "RESTOREERR legacy spells_cast_last_turn snapshot cannot say which playe"),
    ("engine/tests/fixtures/magda_treasure_turn21_4p.json.gz", "RESTOREERR legacy spells_cast_last_turn snapshot cannot say which playe"),
    ("engine/tests/fixtures/mass_library_order_turn15.json.gz", "RESTOREERR legacy spells_cast_last_turn snapshot cannot say which playe"),
    ("engine/tests/fixtures/niv_parun_curiosity_turn44_4p.json.gz", "BUDGET CommanderZoneChoice"),
    ("engine/tests/fixtures/smaug_devil_bats_4p.json.gz", "RESTOREERR legacy spells_cast_last_turn snapshot cannot say which playe"),
    ("engine/tests/fixtures/sprout_witherbloom_realistic_lands_4p.json.gz", "BUDGET Priority"),
    ("engine/tests/fixtures/tenacity_exquisite_blood_4p.json.gz", "OFFER@restore bounded=true bound=Measured unbounded=[Life(PlayerId(0)), Life(PlayerId(3))] winner=Some(PlayerId(0))"),
    ("engine/tests/fixtures/vanquish_the_horde_manapayment_4p.json.gz", "BUDGET Priority"),
    ("engine/tests/fixtures/weird_drain_4p.json.gz", "RESTOREERR legacy spells_cast_last_turn snapshot cannot say which playe"),
    ("engine/tests/fixtures/witherbloom_altar_sprout_swarm_4p.json.gz", "RESTOREERR legacy spells_cast_last_turn snapshot cannot say which playe"),
    ("engine/tests/fixtures/witherbloom_sprout_lumaret_4p.json.gz", "RESTOREERR legacy spells_cast_last_turn snapshot cannot say which playe"),
    ("engine/tests/fixtures/witherbloom_sprout_lumaret_simple_4p.json.gz", "BUDGET Priority"),
    ("engine/tests/integration/fixtures/issue_7087_recruit_discard_provenance.json.gz", "RESTOREERR legacy spells_cast_last_turn snapshot cannot say which playe"),
    ("engine/tests/integration/fixtures/issue_7212_recruit_with_sibling_trigger.json.gz", "RESTOREERR legacy spells_cast_last_turn snapshot cannot say which playe"),
    ("engine/tests/integration/fixtures/issue_8024_devour_rest_turn10.json.gz", "BUDGET Priority"),
    ("engine/tests/integration/fixtures/issue_8024_spell_rest_turn26.json.gz", "BUDGET Priority"),
    ("engine/tests/integration/fixtures/mycoloth_devour_wedge_turn15.json.gz", "BUDGET Priority"),
    ("engine/tests/integration/fixtures/mycoloth_devour_wedge_turn20.json.gz", "BUDGET Priority"),
    ("engine/tests/integration/fixtures/ureni_turn10_raw_resolution_stack.json.gz", "RESTOREERR This saved game is missing the private rules record for the "),
    ("engine/tests/integration/fixtures/zurs_weirding_nested_dispatching_pre_7485.json.gz", "RESTOREERR OwnerlessPostReplacementDispatch"),
    ("phase-ai/fixtures/scenarios/galvanic-blast-manapayment-turn4.json.gz", "BUDGET Priority"),
    ("phase-ai/fixtures/scenarios/invisible-woman-cosmic-crucible-mana.json.gz", "RESTOREERR legacy spells_cast_last_turn snapshot cannot say which playe"),
    ("phase-ai/fixtures/scenarios/judged-cryptbreaker-draw-step.json.gz", "RESTOREERR legacy spells_cast_last_turn snapshot cannot say which playe"),
    ("phase-ai/fixtures/scenarios/judged-karns-bastion-upkeep.json.gz", "RESTOREERR legacy spells_cast_last_turn snapshot cannot say which playe"),
];
