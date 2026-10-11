//! CR 732.2a: every committed dump, restored through the production decoder and driven to its
//! first loop-shortcut offer, game over, or the beat budget, with what the classification
//! accessors answer there.
//!
//! The population is every `*.json.gz` under `crates/`, and the test asserts it equals what git
//! tracks there. The outcome strings are pinned in [`EXPECTED`].

use std::path::{Path, PathBuf};
use std::time::Instant;

use engine::analysis::loop_check::OfferRoad;
use engine::types::game_state::{GameState, LoopDetectionMode, PersistedGameState, WaitingFor};

use crate::loop_shortcut_drain_boards::{collect_gz, committed_document, drive_one_beat};

/// Beats a dump that restores short of an offer is driven, one `apply()` each.
const OFFER_BEAT_BUDGET: u32 = 40;

/// Restore a committed dump in `Interactive` mode through `PersistedGameState`, trying the bare
/// document, then its `gameState`, then that debug snapshot projected onto the versioned wire.
pub(crate) fn restore_committed(path: &Path) -> Result<GameState, String> {
    let document = committed_document(path);
    let decode = |board: serde_json::Value| {
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

/// The offer's published shape, with its road asserted to be the one that shape proves: a winner
/// or a per-cycle signature is the ring road's, and neither is the recorded period's.
fn offer_line(state: &GameState) -> Option<String> {
    let WaitingFor::LoopShortcut {
        predicted_winner,
        certificate,
        schema,
        road,
        ..
    } = &state.waiting_for
    else {
        return None;
    };
    let shaped = if predicted_winner.is_some() || certificate.per_cycle.is_some() {
        OfferRoad::Ring
    } else {
        OfferRoad::RecordedPeriod
    };
    assert_eq!(
        *road, shaped,
        "the offer's road is the one its shape proves"
    );
    Some(format!(
        "bounded={} threshold={:?} unbounded={:?} road={road:?}",
        certificate.per_cycle.is_some(),
        schema.measured_repetition_bound,
        certificate.unbounded
    ))
}

fn walk_outcome(path: &Path) -> String {
    let mut state = match restore_committed(path) {
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
    ("engine/tests/fixtures/codie_turn14.json.gz", "GAMEOVER@5"),
    ("engine/tests/fixtures/combo_infinite_pile_4p_offer.json.gz", "OFFER@restore bounded=false threshold=None unbounded=[TokensCreated] road=RecordedPeriod"),
    ("engine/tests/fixtures/combo_infinite_pile_4p_untapped_precast.json.gz", "BUDGET Priority"),
    ("engine/tests/fixtures/combo_infinite_pile_decklist_4p.json.gz", "RESTOREERR persisted game state has an invalid turn_number"),
    ("engine/tests/fixtures/cr733/authority_matrix.json.gz", "RESTOREERR persisted game state has an invalid turn_number"),
    ("engine/tests/fixtures/cr733/blocked_write_sites.json.gz", "RESTOREERR persisted game state has an invalid turn_number"),
    ("engine/tests/fixtures/cr733/rng_allocator_map.json.gz", "RESTOREERR persisted game state has an invalid turn_number"),
    ("engine/tests/fixtures/cr733/side_effect_map.json.gz", "RESTOREERR persisted game state has an invalid turn_number"),
    ("engine/tests/fixtures/dellian_emblem_conqueror_4p.json.gz", "BUDGET Priority"),
    ("engine/tests/fixtures/dina_conqueror_4p.json.gz", "OFFER@19 bounded=true threshold=Some(36) unbounded=[Life(PlayerId(0)), Life(PlayerId(1)), Life(PlayerId(2)), Life(PlayerId(3))] road=Ring"),
    ("engine/tests/fixtures/dina_conqueror_phase5_no_offer_4p.json.gz", "OFFER@21 bounded=true threshold=Some(34) unbounded=[Life(PlayerId(0)), Life(PlayerId(1)), Life(PlayerId(2)), Life(PlayerId(3))] road=Ring"),
    ("engine/tests/fixtures/dina_noff_turn5_4p.json.gz", "OFFER@21 bounded=true threshold=Some(31) unbounded=[Life(PlayerId(0)), Life(PlayerId(1)), Life(PlayerId(2)), Life(PlayerId(3))] road=Ring"),
    ("engine/tests/fixtures/f4_user_mode1_no_offer_4p.json.gz", "OFFER@37 bounded=true threshold=Some(87) unbounded=[LibraryDelta(PlayerId(0)), Counter(Plus1Plus1, Creature), TokensCreated, Life(PlayerId(1))] road=Ring"),
    ("engine/tests/fixtures/f4_user_mode2_accept_commits_nothing_4p.json.gz", "OFFER@39 bounded=true threshold=Some(84) unbounded=[LibraryDelta(PlayerId(0)), Counter(Plus1Plus1, Creature), TokensCreated, Life(PlayerId(1))] road=Ring"),
    ("engine/tests/fixtures/fantastic_four_bounded_loop_4p.json.gz", "OFFER@39 bounded=true threshold=Some(88) unbounded=[LibraryDelta(PlayerId(0)), Counter(Plus1Plus1, Creature), TokensCreated, Life(PlayerId(1))] road=Ring"),
    ("engine/tests/fixtures/integration_cards.json.gz", "RESTOREERR unversioned raw resolution state contains an Exploited trigg"),
    ("engine/tests/fixtures/kilo_freed_relic_pentad_4p.json.gz", "BUDGET Priority"),
    ("engine/tests/fixtures/kilo_freed_relic_pentad_max_of_one_4p.json.gz", "BUDGET Priority"),
    ("engine/tests/fixtures/lethal_lifegain_loss_4p.json.gz", "OFFER@restore bounded=true threshold=Some(2) unbounded=[Life(PlayerId(0)), Life(PlayerId(1))] road=Ring"),
    ("engine/tests/fixtures/mass_library_order_turn15.json.gz", "BUDGET Priority"),
    ("engine/tests/fixtures/sprout_witherbloom_realistic_lands_4p.json.gz", "BUDGET Priority"),
    ("engine/tests/fixtures/tenacity_exquisite_blood_4p.json.gz", "OFFER@restore bounded=false threshold=None unbounded=[Life(PlayerId(0)), Life(PlayerId(3))] road=Ring"),
    ("engine/tests/fixtures/vanquish_the_horde_manapayment_4p.json.gz", "BUDGET Priority"),
    ("engine/tests/fixtures/weird_drain_4p.json.gz", "OFFER@restore bounded=true threshold=Some(10) unbounded=[Life(PlayerId(0)), Life(PlayerId(2))] road=Ring"),
    ("engine/tests/fixtures/witherbloom_altar_sprout_swarm_4p.json.gz", "BUDGET Priority"),
    ("engine/tests/fixtures/witherbloom_sprout_lumaret_4p.json.gz", "BUDGET Priority"),
    ("engine/tests/fixtures/witherbloom_sprout_lumaret_simple_4p.json.gz", "BUDGET Priority"),
    ("engine/tests/integration/fixtures/issue_7087_recruit_discard_provenance.json.gz", "BUDGET Priority"),
    ("engine/tests/integration/fixtures/issue_7212_recruit_with_sibling_trigger.json.gz", "BUDGET Priority"),
    ("engine/tests/integration/fixtures/issue_8024_devour_rest_turn10.json.gz", "BUDGET Priority"),
    ("engine/tests/integration/fixtures/issue_8024_spell_rest_turn26.json.gz", "BUDGET Priority"),
    ("engine/tests/integration/fixtures/mycoloth_devour_wedge_turn15.json.gz", "BUDGET Priority"),
    ("engine/tests/integration/fixtures/mycoloth_devour_wedge_turn20.json.gz", "BUDGET Priority"),
    ("engine/tests/integration/fixtures/ureni_turn10_raw_resolution_stack.json.gz", "RESTOREERR This saved game is missing the private rules record for the "),
    ("engine/tests/integration/fixtures/zurs_weirding_nested_dispatching_pre_7485.json.gz", "RESTOREERR OwnerlessPostReplacementDispatch"),
    ("phase-ai/fixtures/scenarios/galvanic-blast-manapayment-turn4.json.gz", "BUDGET Priority"),
    ("phase-ai/fixtures/scenarios/invisible-woman-cosmic-crucible-mana.json.gz", "BUDGET Priority"),
];
