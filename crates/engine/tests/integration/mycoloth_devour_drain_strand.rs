//! CR 603.3 + CR 603.3b + CR 608.2c: two wedged Mycoloth boards recover at the
//! first action boundary — turn-15 at the priority boundary that action lands
//! on, turn-20 at the entry of the boundary itself.
//!
//! Both captures show the same permanent wedge: a `PostReplacement` resolution
//! frame whose resident drain is stuck in `DrainStatus::Dispatching`. Nothing
//! can retire that entry — `begin_dispatch` refuses a `Dispatching` resident,
//! `finish_paused_dispatch` pops only a `Paused` one, and `finish_dispatch`
//! needs a transient handle that died with its dispatcher's call frame. The
//! frame therefore outlives every removal path, `resolution_stack` stays
//! non-empty forever, and `triggers::resolution_completion_can_settle` is false
//! forever. Two rules-level consequences follow, and both are visible in the
//! captures: parked triggered abilities can never be put on the stack even
//! though CR 603.3b requires them there before any player receives priority,
//! and the stale resolving carrier can never settle (CR 608.2c).
//!
//! # Fixture provenance
//!
//! Derived from the reporter's raw client dumps (Discord thread
//! 1537641754298290226). The 12 MB raw dumps are deliberately NOT tracked.
//!
//! | artifact | bytes | sha256 |
//! |---|---|---|
//! | `game-state-turn-15-2026-08-15T14-02-22-524Z.json` (raw capture) | 11 944 525 | `ec8c609c1f2ccb92d76afc536ddd10aab6e9b9d62d15f408e2e40cdb81de0107` |
//! | derived `mycoloth_devour_wedge_turn15.json.gz` | 393 761 | `b7e83521f548fe0776a47d5954d887653e5ea29d7050421bfbae5bbcae138f25` |
//! | `game-state-turn-20-2026-08-15T01-13-36-601Z.json` (raw capture) | 13 351 646 | `1788737cf6d499f8878c9869546967c0aad768d8187ae2959d5cc0bc54dd6353` |
//! | derived `mycoloth_devour_wedge_turn20.json.gz` | 314 873 | `45f1422dfc84eb51e1a60e9dbfdf30c055c0de937cb13b96972951de5f32d7ec` |
//!
//! Byte-reproducible regeneration is the recipe below **plus the U5 `deck_size`
//! migration** — `-n` is load-bearing, since without it gzip stamps an mtime and
//! the digest never lands:
//!
//! ```text
//! jq -c '{gameState}' <dump>.json | gzip -9 -n \
//!   > crates/engine/tests/integration/fixtures/mycoloth_devour_wedge_turn15.json.gz
//! ```
//!
//! Both captures predate U5, so the recipe alone is NOT sufficient: each bare
//! `"deck_size": 100` must first become the adjacently-tagged `DeckSizeRule`
//! form `{"type":"Exactly","data":100}` — the variant taken from the sibling
//! `format` field, `Commander` for both captures. Piping a raw dump straight
//! through does not merely miss the digest; it yields a fixture that
//! `PersistedGameState` cannot deserialize, and a red test on a green engine.
//! Then the retired `combat_phases_started_this_turn` /
//! `end_steps_started_this_turn` keys must be rewritten to
//! `steps_started_this_turn` (`{"BeginCombat": n, "End": m}`, zeros dropped,
//! placed at the first old key).
//!
//! # What these fixtures do and do not prove
//!
//! They are post-wedge snapshots, so they prove **recovery**, not the instant of
//! stranding. `applied: []` on both captured drains is NOT provenance about how
//! the drain was installed: `apply_pending_post_replacement_effect`
//! unconditionally `std::mem::take`s the resident's `applied` set before
//! `begin_dispatch` can decline, so under the wedge that set is emptied at every
//! priority boundary regardless of its installed contents.
//!
//! They are also *legacy* payloads — no per-frame post-replacement id, no outer
//! allocator — so they exercise none of the identity-addressed dispatch wire
//! path. That is covered by the `types/resolution.rs` round-trip rows.
//!
//! This module must NOT be read as claiming the captured strand came from the
//! Devour delivery tail. What is true is narrower: the drain *shape*
//! (`source: null`, `event_source: null`) is consistent with a
//! `clear_post_replacement_source` caller, and these captures are post-wedge
//! snapshots that prove recovery rather than the instant of stranding. The one
//! producer attribution that IS measured is the **Zur's Weirding
//! draw-replacement path**, which was observed at `BASE_SHA` to leave a
//! `Dispatching` resident both mid-scenario (`[PostReplacement, MultiDraw,
//! OptionalEffect]` under `OpponentMayChoice`) and at rest (a single-frame
//! `PostReplacement` at `Priority` — the reporter's exact wedge shape). That is
//! why this file also carries `b2_zurs_weirding_replacement_leaves_no_dispatching_drain`,
//! which is NOT a Devour scenario and says so in its own doc comment.
//!
//! # What one action does — and where
//!
//! Rows **A4** and **A4b** together discriminate the *entry* evaluation point;
//! A4's red was observed at `BASE_SHA`, before the entry call site existed.
//!
//! * **turn-15**: the pass lands on `Priority` again (`priority_passes: []`), so
//!   `resume_pending_continuation_if_priority`'s gate is true. The
//!   priority-boundary sweeper retires the frame,
//!   `settle_resolving_stack_entry_after_continuation_resume` settles the
//!   carrier, and `run_post_action_pipeline`'s deferred-trigger drain runs — all
//!   three within one `PassPriority`. Rows A1–A3.
//! * **turn-20**: the pass advances the phase, so that gate is **false** for the
//!   resulting state and the post-action sweeper is never entered. The frame is
//!   retired by the **entry** sweep in `engine::apply_action_boundary_core`,
//!   which runs on the state *as found*, before `boundary_snapshot`. The parked
//!   abilities then reach the stack in the same action — but by a seam this
//!   change does not own (`turns::process_phase_triggers`), which is gated behind
//!   `resolution_completion_can_settle` and therefore could not run at all while
//!   the strand was present. A4 records the measured terminal shape and asserts
//!   strand removal plus that drain; it pins nothing about the terminal
//!   `waiting_for`. Row A4b proves the entry siting by repairing the state on an
//!   action the engine rejects.

use engine::game::engine::apply;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, PostReplacementContinuation, QuantityExpr, TargetFilter,
};
use engine::types::actions::{DebugAction, GameAction};
use engine::types::counter::CounterType;
use engine::types::game_state::{
    GameState, PendingMultiDraw, PersistedGameState, PersistedRestoreError,
    PersistedRestoreFinalization, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::resolution::{ResolutionStateWire, RESOLUTION_STATE_WIRE_VERSION};
use engine::types::zones::Zone;

fn gunzip(gz: &[u8]) -> String {
    use std::io::Read;
    let mut json = String::new();
    flate2::read::GzDecoder::new(gz)
        .read_to_string(&mut json)
        .expect("fixture .json.gz must inflate to UTF-8 JSON");
    json
}

/// Decode a capture's `["gameState"]` through the versioned resolution wire.
///
/// These rows exercise the live action-boundary repair, so they deliberately
/// stop before the production restore finalizer, which now repairs the terminal
/// strand at load time. The dedicated persisted-restore row below covers that
/// external boundary.
///
/// One projection is required first, and it is worth stating exactly why rather
/// than hiding it in a helper. `client/src/services/gameStateExport.ts` writes a
/// **debug snapshot of the runtime `GameState`**, not a persistence-wire save:
/// it carries the raw `resolution_stack` field and no `resolution_state_version`.
/// `PersistedGameState`'s decoder stamps an absent version as v1, and the v1
/// reader rejects any payload carrying `resolution_stack` outright — that
/// rejection is correct, because v1 predates typed frames entirely.
///
/// So the snapshot is first projected onto the historical v2 typed-frame wire
/// shape: move `resolution_stack` to `resolution_frames` and stamp version 2.
/// The current writer emits v4; this projection deliberately selects the
/// supported legacy reader. Nothing in the frame stack is touched — in
/// particular the wedged frame and its `Dispatching` drain cross verbatim.
fn projected_capture_snapshot(gz: &[u8]) -> serde_json::Value {
    let json = gunzip(gz);
    let envelope: serde_json::Value =
        serde_json::from_str(&json).expect("dump envelope parses as JSON");
    let mut snapshot = envelope["gameState"].clone();
    {
        let object = snapshot
            .as_object_mut()
            .expect("a captured gameState is a JSON object");
        assert!(
            !object.contains_key("resolution_state_version"),
            "the reporter's capture is an unversioned runtime debug snapshot"
        );
        let stack = object
            .remove("resolution_stack")
            .expect("the wedged capture carries a runtime resolution_stack");
        object.insert("resolution_frames".to_string(), stack);
        object.insert(
            "resolution_state_version".to_string(),
            serde_json::Value::from(2),
        );
    }
    snapshot
}

fn load_capture(gz: &[u8]) -> GameState {
    let snapshot = projected_capture_snapshot(gz);
    serde_json::from_value::<ResolutionStateWire>(snapshot)
        .expect("the projected snapshot deserializes through the versioned resolution decoder")
        .into_game_state()
}

fn restore_capture(gz: &[u8]) -> GameState {
    let snapshot = projected_capture_snapshot(gz);
    serde_json::from_value::<PersistedGameState>(snapshot)
        .expect("the projected snapshot deserializes through the production decoder")
        .into_game_state()
        .expect("the persisted capture satisfies the checked restore contract")
}

fn load_turn15() -> GameState {
    load_capture(include_bytes!(
        "fixtures/mycoloth_devour_wedge_turn15.json.gz"
    ))
}

fn load_turn20() -> GameState {
    load_capture(include_bytes!(
        "fixtures/mycoloth_devour_wedge_turn20.json.gz"
    ))
}

fn zurs_weirding_prompt_runner() -> engine::game::scenario::GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    {
        let mut zurs_weirding =
            scenario.add_creature_from_oracle(P0, "Zur's Weirding", 0, 1, ZURS_WEIRDING_ORACLE);
        zurs_weirding.as_enchantment();
    }
    scenario.with_library_top(P1, &["Grizzly Bears", "Forest", "Plains"]);
    scenario.with_library_top(P0, &["P0 Library 1", "P0 Library 2", "P0 Library 3"]);
    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    runner
}

#[test]
fn persisted_wedged_captures_settle_before_publication() {
    for capture in [
        include_bytes!("fixtures/mycoloth_devour_wedge_turn15.json.gz").as_slice(),
        include_bytes!("fixtures/mycoloth_devour_wedge_turn20.json.gz").as_slice(),
    ] {
        let state = restore_capture(capture);
        assert!(
            state.resolution_stack.is_empty(),
            "the production restore must not publish the ownerless frame"
        );
        assert!(
            state.resolving_stack_entry.is_none(),
            "the production restore must settle the terminal stack carrier"
        );
    }
}

/// A supported current-format Ready continuation remains admissible through the
/// production restore preparation boundary. The state is made with the public
/// runtime installer and an executable draw continuation, then serialized via
/// the current v4 persistence writer; the assertion protects live parked work
/// from being mistaken for an ownerless Dispatching drain.
#[test]
fn current_ready_postreplacement_continuation_remains_restorable() {
    let mut state = GameState::new_two_player(42);
    state.install_ready_continuation(PostReplacementContinuation::Template(Box::new(
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        ),
    )));

    let wire = serde_json::to_value(PersistedGameState::Raw(Box::new(state)))
        .expect("the current Ready state serializes through the persistence writer");
    assert_eq!(
        wire["resolution_state_version"],
        RESOLUTION_STATE_WIRE_VERSION
    );
    let restored = serde_json::from_value::<PersistedGameState>(wire)
        .expect("the current Ready persistence payload decodes")
        .prepare_for_restore(PersistedRestoreFinalization::Immediate)
        .expect("a valid Ready continuation remains admissible")
        .finalize_immediately()
        .expect("the Ready state finalizes without manufacturing an action");

    assert!(matches!(
        restored.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(0)
        }
    ));
    assert_eq!(
        post_replacement_drain_statuses(&restored),
        vec![vec!["Ready".to_string()]],
        "restore must preserve a live Ready continuation"
    );
}

/// The supported v1 raw persistence shape also crosses the actual restore
/// preparation boundary. Its legacy single-slot draw tail is migrated into a
/// typed draw frame, after which the checked restore remains playable.
#[test]
fn supported_v1_draw_state_restores_through_persisted_game_state() {
    let mut v1 =
        serde_json::to_value(GameState::new_two_player(43)).expect("serialize v1 baseline");
    v1["pending_multi_draw"] = serde_json::to_value(PendingMultiDraw {
        player: PlayerId(0),
        remaining: 2,
        accumulated: 1,
    })
    .expect("serialize legacy v1 draw tail");
    assert!(v1.get("resolution_state_version").is_none());
    assert!(v1.get("pending_multi_draw").is_some());

    let prepared = serde_json::from_value::<PersistedGameState>(v1)
        .expect("the persistence boundary supplies the v1 discriminator")
        .prepare_for_restore(PersistedRestoreFinalization::Immediate)
        .expect("the supported v1 draw state remains admissible");
    let restored = prepared
        .finalize_immediately()
        .expect("the v1 state finalizes without manufacturing an action");
    assert_eq!(
        restored.active_draw_sequence().map(|frame| frame.remaining),
        Some(2),
        "the legacy draw tail must be migrated to the active typed sequence"
    );

    let current = serde_json::to_value(PersistedGameState::Raw(Box::new(restored)))
        .expect("restored v1 state rewrites through the current persistence writer");
    assert_eq!(
        current["resolution_state_version"],
        RESOLUTION_STATE_WIRE_VERSION
    );
    assert!(current["resolution_frames"].is_object());
    assert!(current.get("pending_multi_draw").is_none());
}

/// Restore admission regression for the historically measured Zur's Weirding
/// prompt. The compressed source is a raw runtime `GameState` dump captured at
/// the pre-#7485 revision `b2071a7f41a422d8d43b66189e4ae976ce451e07`, after
/// the real debug draw reached `OpponentMayChoice`. At that revision the frame
/// stack was `[PostReplacement(Dispatching), MultiDraw, OptionalEffect]`.
/// Current healthy play does not produce this ownerless persisted state; the
/// fixture records the older runtime state and its lost synchronous owner.
/// No measured shallow `[PostReplacement(Dispatching), OptionalEffect]` producer
/// state was found: the current producer pauses its drain before publishing a
/// choice, while the pre-#7485 capture is the measured nested
/// `[PostReplacement(Dispatching), MultiDraw, OptionalEffect]` witness. The
/// stack's generic gate rules alone would admit a matching direct-choice child,
/// but that structural possibility is not runtime provenance and is not turned
/// into another synthetic defect case here.
///
/// `projected_capture_snapshot` applies the same persistence-wire projection
/// used for the reporter captures above: `resolution_stack` becomes
/// `resolution_frames`, version 2 is stamped, and all frame payloads cross
/// unchanged. The test then uses `PersistedGameState`'s production decoder and
/// `prepare_for_restore`, so it exercises admission rather than only frame
/// decoding.
///
/// Provenance/normalization: the pre-migration raw runtime JSON had SHA-256
/// `4cc99fda95ca784974a644f47b7c0b500e187e7896e508da604d6184fba51b11`. Its
/// Standard `format_config.deck_size: 60` is represented in the fixture as the
/// current `DeckSizeRule::Minimum(60)` tagged shape; the snapshot has none of
/// the retired phase-counter keys. The deterministic gzip fixture has SHA-256
/// `f1dcde618c4941adb1a5dab501b7f7f7e5cda0dd8e050e6afcc85b6b0207a0af`.
#[test]
fn persisted_historical_nested_dispatching_fails_closed_before_publication() {
    let snapshot = projected_capture_snapshot(include_bytes!(
        "fixtures/zurs_weirding_nested_dispatching_pre_7485.json.gz"
    ));
    assert_eq!(snapshot["resolution_state_version"], 2);
    assert_eq!(snapshot["waiting_for"]["type"], "OpponentMayChoice");
    assert_eq!(
        snapshot["resolution_frames"]["frames"][0]["type"],
        "PostReplacement"
    );
    assert_eq!(
        snapshot["resolution_frames"]["frames"][1]["type"],
        "MultiDraw"
    );
    assert_eq!(
        snapshot["resolution_frames"]["frames"][2]["type"],
        "OptionalEffect"
    );
    assert_eq!(
        snapshot["resolution_frames"]["frames"][0]["data"]["drains"][0]["status"],
        "Dispatching"
    );

    let persisted = serde_json::from_value::<PersistedGameState>(snapshot)
        .expect("the historical v2 payload decodes through PersistedGameState");
    let prepared = persisted.prepare_for_restore(PersistedRestoreFinalization::Immediate);
    assert!(matches!(
        prepared,
        Err(PersistedRestoreError::OwnerlessPostReplacementDispatch)
    ));
}

/// A nonresident drain entry is still unsupported when another drain in the
/// same nested frame is legitimately paused for the live choice.
#[test]
fn persisted_nonresident_dispatching_drain_is_rejected() {
    let snapshot = projected_capture_snapshot(include_bytes!(
        "fixtures/zurs_weirding_nested_dispatching_pre_7485.json.gz"
    ));
    let template = snapshot["resolution_frames"]["frames"][0]["data"]["drains"][0].clone();
    let with_statuses = |first_status: &str, second_status: &str| {
        let mut variant = snapshot.clone();
        let drains = variant["resolution_frames"]["frames"][0]["data"]["drains"]
            .as_array_mut()
            .expect("the historical PostReplacement frame has a drain stack");
        let mut first = template.clone();
        let mut second = template.clone();
        first["status"] = serde_json::Value::String(first_status.to_string());
        second["status"] = serde_json::Value::String(second_status.to_string());
        *drains = vec![first, second];
        serde_json::from_value::<PersistedGameState>(variant)
            .expect("the nested two-drain topology decodes")
            .prepare_for_restore(PersistedRestoreFinalization::Immediate)
    };

    assert!(
        with_statuses("Paused", "Paused").is_ok(),
        "the same two-drain topology is admissible without an ownerless dispatch"
    );
    for (first, second) in [("Dispatching", "Paused"), ("Paused", "Dispatching")] {
        assert!(matches!(
            with_statuses(first, second),
            Err(PersistedRestoreError::OwnerlessPostReplacementDispatch)
        ));
    }
}

/// The historical v2 fixture differs from a supported paused direct-choice
/// save in one lifecycle tag. This control proves the same topology and payload
/// remain admissible when the dispatch has actually paused.
#[test]
fn historical_v2_paused_direct_choice_remains_restorable() {
    let mut snapshot = projected_capture_snapshot(include_bytes!(
        "fixtures/zurs_weirding_nested_dispatching_pre_7485.json.gz"
    ));
    snapshot["resolution_frames"]["frames"][0]["data"]["drains"][0]["status"] =
        serde_json::Value::String("Paused".to_string());

    let state = serde_json::from_value::<PersistedGameState>(snapshot)
        .expect("the paused v2 direct-choice control decodes")
        .prepare_for_restore(PersistedRestoreFinalization::Immediate)
        .expect("a paused v2 direct-choice state remains admissible")
        .finalize_immediately()
        .expect("the paused v2 state finalizes without manufacturing an action");
    assert!(matches!(
        state.waiting_for,
        WaitingFor::OpponentMayChoice { .. }
    ));
    assert_eq!(
        post_replacement_drain_statuses(&state),
        vec![vec!["Paused".to_string()]],
        "the direct-choice save must retain its paused replacement continuation"
    );
}

/// The same genuinely paused direct-choice state remains readable through the
/// v3 wire. The v4 writer's only later draw-frame requirement, `delivery_owner`,
/// is removed before selecting the supported v3 reader.
#[test]
fn v3_paused_postreplacement_direct_choice_remains_restorable() {
    let mut runner = zurs_weirding_prompt_runner();
    runner
        .act(GameAction::Debug(DebugAction::DrawCards {
            player_id: P1,
            count: 1,
        }))
        .expect("debug draw reaches the direct-choice prompt");
    assert_eq!(
        post_replacement_drain_statuses(runner.state()),
        vec![vec!["Paused".to_string()]],
        "the current producer must leave a valid paused replacement continuation"
    );

    let mut wire = serde_json::to_value(PersistedGameState::Raw(Box::new(runner.state().clone())))
        .expect("current persisted wire serializes");
    assert_eq!(
        wire["resolution_state_version"],
        RESOLUTION_STATE_WIRE_VERSION
    );
    let delivery_owner = wire["resolution_frames"]["frames"][1]["data"]["draw_sequences"]["frames"]
        [0]
    .as_object_mut()
    .expect("the MultiDraw frame contains a draw-sequence frame")
    .remove("delivery_owner");
    assert!(
        delivery_owner.is_some(),
        "the current draw frame must carry the v4-only delivery owner"
    );
    wire["resolution_state_version"] = serde_json::Value::from(3);

    let state = serde_json::from_value::<PersistedGameState>(wire)
        .expect("the valid v3 paused direct-choice state decodes")
        .prepare_for_restore(PersistedRestoreFinalization::Immediate)
        .expect("the valid v3 paused direct-choice state remains admissible")
        .finalize_immediately()
        .expect("the valid v3 state finalizes");
    assert!(matches!(
        state.waiting_for,
        WaitingFor::OpponentMayChoice { .. }
    ));
    assert_eq!(
        post_replacement_drain_statuses(&state),
        vec![vec!["Paused".to_string()]]
    );
}

/// A current, genuinely paused direct-choice save must still resume through a
/// normal action with the same persisted semantics as uninterrupted play.
#[test]
fn current_paused_postreplacement_round_trip_resumes_like_uninterrupted() {
    let mut uninterrupted = zurs_weirding_prompt_runner();
    let mut restored = zurs_weirding_prompt_runner();
    for runner in [&mut uninterrupted, &mut restored] {
        runner
            .act(GameAction::Debug(DebugAction::DrawCards {
                player_id: P1,
                count: 1,
            }))
            .expect("debug draw reaches the direct-choice prompt");
    }

    assert!(matches!(
        restored.state().waiting_for,
        WaitingFor::OpponentMayChoice { .. }
    ));
    assert_eq!(
        post_replacement_drain_statuses(restored.state()),
        vec![vec!["Paused".to_string()]],
        "current healthy execution must pause the replacement before saving"
    );

    let wire = serde_json::to_value(PersistedGameState::Raw(Box::new(restored.state().clone())))
        .expect("current persisted wire serializes");
    assert_eq!(
        wire["resolution_state_version"],
        RESOLUTION_STATE_WIRE_VERSION
    );
    let state = serde_json::from_value::<PersistedGameState>(wire)
        .expect("current persisted wire decodes")
        .prepare_for_restore(PersistedRestoreFinalization::Immediate)
        .expect("current paused state remains admissible")
        .finalize_immediately()
        .expect("current paused state finalizes");
    *restored.state_mut() = state;

    let answer = GameAction::DecideOptionalEffect { accept: true };
    uninterrupted
        .act(answer.clone())
        .expect("uninterrupted direct-choice answer succeeds");
    restored
        .act(answer)
        .expect("restored direct-choice answer succeeds");

    let uninterrupted_wire = serde_json::to_value(PersistedGameState::Raw(Box::new(
        uninterrupted.state().clone(),
    )))
    .expect("uninterrupted result serializes");
    let restored_wire =
        serde_json::to_value(PersistedGameState::Raw(Box::new(restored.state().clone())))
            .expect("restored result serializes");
    assert_eq!(
        restored_wire, uninterrupted_wire,
        "normal action after restore must match uninterrupted resolution semantics"
    );
}

/// The per-`PostReplacement`-frame drain statuses of the LOADED runtime state,
/// read back through the stack's own `Serialize` impl. `PostReplacementDrainStack`
/// exposes only its resident, so this is how a test observes a multi-entry
/// strand without widening production API for a test's convenience.
fn post_replacement_drain_statuses(state: &GameState) -> Vec<Vec<String>> {
    let value =
        serde_json::to_value(&state.resolution_stack).expect("the resolution stack serializes");
    value["frames"]
        .as_array()
        .expect("frames is an array")
        .iter()
        .filter(|frame| frame["type"] == "PostReplacement")
        .map(|frame| {
            frame["data"]["drains"]
                .as_array()
                .expect("a post-replacement frame carries a drains array")
                .iter()
                .map(|drain| match &drain["status"] {
                    serde_json::Value::String(status) => status.clone(),
                    // `DrainStatus::Ready(_)` is externally tagged.
                    serde_json::Value::Object(map) => {
                        map.keys().next().cloned().unwrap_or_default()
                    }
                    other => other.to_string(),
                })
                .collect()
        })
        .collect()
}

fn deferred_sources(state: &GameState) -> Vec<u64> {
    state
        .deferred_triggers
        .iter()
        .map(|deferred| deferred.pending.source_id.0)
        .collect()
}

fn deferred_descriptions(state: &GameState) -> Vec<String> {
    state
        .deferred_triggers
        .iter()
        .map(|deferred| deferred.pending.description.clone().unwrap_or_default())
        .collect()
}

/// Every trigger source id that reached a CR 603.3b destination: on the stack,
/// or inside an in-flight APNAP ordering pass (three same-controller triggers
/// legitimately raise an ordering prompt before they are put on the stack).
fn triggers_reaching_the_stack(state: &GameState) -> Vec<u64> {
    let mut sources: Vec<u64> = state.stack.iter().map(|entry| entry.source_id.0).collect();
    if let Some(order) = &state.pending_trigger_order {
        for group in &order.groups {
            sources.extend(group.triggers.iter().map(|t| t.pending.source_id.0));
        }
    }
    sources.sort_unstable();
    sources
}

fn p1p1(state: &GameState, id: ObjectId) -> u32 {
    state
        .objects
        .get(&id)
        .expect("object present")
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// Reach-guard for the turn-15 board: the wedge must be genuinely present in the
/// loaded state, or every row below is vacuous. If `into_game_state()` ever
/// normalises the wedged `resolution_stack` away, this reds immediately rather
/// than letting the recovery rows pass for the wrong reason.
fn assert_turn15_wedge_present(state: &GameState) {
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player } if player == PlayerId(1)),
        "the capture is parked on P1 priority, got {:?}",
        state.waiting_for
    );
    assert_eq!(
        post_replacement_drain_statuses(state),
        vec![vec!["Dispatching".to_string()]],
        "exactly one PostReplacement frame carrying exactly one Dispatching drain"
    );
    assert_eq!(
        state.resolution_stack.len(),
        1,
        "the wedge is a single-frame resolution stack"
    );
    assert_eq!(
        deferred_sources(state),
        vec![52, 52, 199],
        "the three parked triggers are two source-52 dies drains and one source-199 sacrifice draw"
    );
    let descriptions = deferred_descriptions(state);
    assert_eq!(
        descriptions[0], descriptions[1],
        "the two source-52 firings share one ability"
    );
    assert!(
        descriptions[0]
            .contains("Whenever a creature you control dies, each opponent loses 1 life"),
        "source 52 is the Bastion-of-Remembrance dies drain, got {:?}",
        descriptions[0]
    );
    assert!(
        descriptions[2].contains("Whenever you sacrifice a creature, draw a card"),
        "source 199 is the sacrifice-draw ability, got {:?}",
        descriptions[2]
    );
    let carrier = state
        .resolving_stack_entry
        .as_ref()
        .expect("the capture carries a stale resolving stack entry");
    assert_eq!(carrier.id, ObjectId(97), "the stale carrier is object 97");
    assert_eq!(
        state.objects[&ObjectId(97)].zone,
        Zone::Graveyard,
        "object 97 (Witherbloom Command) has already finished resolving"
    );
    assert_eq!(
        state.objects[&ObjectId(84)].zone,
        Zone::Battlefield,
        "Mycoloth is on the battlefield"
    );
    assert_eq!(
        p1p1(state, ObjectId(84)),
        4,
        "Devour 2 x 2 creatures landed 4 +1/+1 counters — this is not a counter defect"
    );
}

/// Reach-guard for the turn-20 board: the multi-entry strand shape.
fn assert_turn20_wedge_present(state: &GameState) {
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player } if player == PlayerId(1)),
        "the capture is parked on P1 priority, got {:?}",
        state.waiting_for
    );
    assert_eq!(
        post_replacement_drain_statuses(state),
        vec![vec!["Dispatching".to_string(), "Dispatching".to_string()]],
        "one PostReplacement frame carrying TWO Dispatching drains — the multi-entry strand"
    );
    assert_eq!(
        deferred_sources(state),
        vec![157, 199],
        "two parked triggers: source 157 and the source-199 sacrifice draw"
    );
    assert!(
        state.resolving_stack_entry.is_some(),
        "the capture carries a stale resolving stack entry"
    );
}

/// **A1 — discriminating(U1).** CR 603.3b + CR 608.2c: the orphaned
/// `PostReplacement` frame is retired at the first priority boundary, so the
/// resolution stack empties.
///
/// Revert-failing assertion: `state.resolution_stack.is_empty()`. Without the
/// ownerless-strand sweep the dispatcher declines the `Dispatching` resident,
/// the `is_empty` frame-removal block is gated false because the strand is still
/// resident, and the frame survives — the assertion reads 1.
#[test]
fn a1_wedged_turn15_capture_retires_its_ownerless_dispatching_frame() {
    let mut state = load_turn15();
    assert_turn15_wedge_present(&state);

    apply(&mut state, PlayerId(1), GameAction::PassPriority).expect("PassPriority is legal");

    assert!(
        state.resolution_stack.is_empty(),
        "the ownerless Dispatching drain and its now-empty frame must both be gone, got {:?}",
        post_replacement_drain_statuses(&state)
    );
}

/// **A2 — discriminating(U1).** CR 603.3b: with the wedge cleared, the three
/// parked triggered abilities are put on the stack before any player receives
/// priority.
///
/// Revert-failing assertion: `deferred_triggers.is_empty()` plus the three
/// source ids reaching a CR 603.3b destination. Under the wedge
/// `resolution_completion_can_settle` is false forever, so
/// `can_drain_deferred_triggers` is false forever and all three stay parked.
#[test]
fn a2_wedged_turn15_capture_drains_its_parked_triggers() {
    let mut state = load_turn15();
    assert_turn15_wedge_present(&state);
    // Non-vacuity: the capture freezes priority OFF the active seat, so the
    // CR 117.3b row below is a real question rather than a restatement.
    assert_ne!(
        state.priority_player, state.active_player,
        "the turn-15 capture must freeze priority off the active seat"
    );

    apply(&mut state, PlayerId(1), GameAction::PassPriority).expect("PassPriority is legal");
    let priority_player = state.priority_player;
    // CR 117.3b: the active player receives priority after a spell or ability
    // resolves. Recovery must hand the refreshed window to the ACTIVE player,
    // not back to the holder the capture froze. Asserting the rule rather than
    // a literal seat: the correct seat differs between this row (PlayerId(0))
    // and A4 (PlayerId(1)), so a hardcoded id would re-encode a fixture
    // constant instead of the rule.
    assert_eq!(
        priority_player, state.active_player,
        "CR 117.3b: the recovered window returns priority to the active player"
    );
    apply(&mut state, priority_player, GameAction::PassPriority)
        .expect("the recovered priority holder accepts the next pass");

    assert!(
        state.deferred_triggers.is_empty(),
        "all three parked abilities must leave the deferred queue, still parked: {:?}",
        deferred_sources(&state)
    );
    assert_eq!(
        triggers_reaching_the_stack(&state),
        vec![52, 52, 199],
        "both source-52 dies-drain firings and the source-199 sacrifice draw reach the stack"
    );
}

/// **A3 — discriminating(U1).** CR 608.2c: the stale resolving carrier settles
/// once the resolution stack can complete.
///
/// Revert-failing assertion: `resolving_stack_entry.is_none()`. Under the wedge
/// `resolving_stack_entry_can_settle` is false forever and the carrier stays
/// `Some(97)` — a Witherbloom Command already in the graveyard, read as live by
/// `trigger_matchers`, `zones` and `bounce` on every later resolution.
#[test]
fn a3_wedged_turn15_capture_settles_its_stale_resolving_carrier() {
    let mut state = load_turn15();
    assert_turn15_wedge_present(&state);

    apply(&mut state, PlayerId(1), GameAction::PassPriority).expect("PassPriority is legal");

    assert!(
        state.resolving_stack_entry.is_none(),
        "the stale carrier must settle, got {:?}",
        state.resolving_stack_entry.as_ref().map(|entry| entry.id)
    );
}

/// **A4 — discriminating(U1), of the ENTRY evaluation point.** A wedge that is
/// *sitting at* a rest boundary when the state is loaded recovers even though
/// this action does not land on `Priority`; and the multi-entry strand recovers
/// in one boundary, because the sweep loops until it meets a `Ready` or `Paused`
/// resident, so BOTH stranded drains retire and the emptied frame is removed.
///
/// Revert-failing assertion: `resolution_stack.is_empty()` on a board whose one
/// frame carried two `Dispatching` entries. The discriminating patch is **(a2)**
/// — reverting only the two call lines at `engine::apply_action_boundary_core`'s
/// entry, leaving the priority-boundary sweep intact. **A4b** is the row that
/// isolates that entry point with no unmeasured premise, by repairing a state on
/// an action the engine REJECTS.
///
/// CORRECTION, recorded because an earlier round pinned assertions on it: the
/// `"Priority -> DeclareAttackers"` framing was a **MID-ACTION** reading taken at
/// the sweep hook inside `resume_pending_continuation_if_priority`, not the state
/// `apply` returns. `run_auto_pass_loop`'s `DeclareAttackers` arm auto-submits an
/// empty attack set on exactly this fixture's shape (`valid_attacker_ids` empty,
/// `phase_stops` absent by serde default), so the action does not end there
/// either. The settled post-action state is MEASURED rather than read:
///
/// ```text
/// waiting_for = Priority { player: PlayerId(1) }   phase = EndCombat
/// drains = []   deferred = []   stack.len() = 2
/// resolving_stack_entry = Some(ObjectId(430))   pending_completion = false
/// ```
///
/// **CR 603.3 + CR 603.3b:** both parked abilities reach the stack — the queue
/// empties and `stack.len()` goes from 0 to 2 — and the board rests at `Priority`
/// with no resolution frame. That is a TWO-DEFECT STACK resolving, and the second
/// half is not this change's work: `turns::process_phase_triggers` drains the
/// parked queue at a phase boundary. That drain is gated behind
/// `triggers::can_drain_deferred_triggers`, whose first condition is
/// `!resolution_completion_can_settle(state)`, and an ownerless `Dispatching`
/// strand pins that predicate false forever. So this fix is the PRECONDITION for
/// that one: without the strand removal the queue could not drain no matter how
/// many boundaries offered it the chance.
///
/// The first pass repairs the stale boundary without consuming that pass. The
/// next ordinary pass then drains the parked triggers, proving both the repair
/// and the resumed action path.
#[test]
fn a4_wedged_turn20_capture_retires_both_stranded_drains() {
    let mut state = load_turn20();
    assert_turn20_wedge_present(&state);

    apply(&mut state, PlayerId(1), GameAction::PassPriority).expect("PassPriority is legal");

    assert!(
        state.resolution_stack.is_empty(),
        "both stranded drains and their frame must be gone, got {:?}",
        post_replacement_drain_statuses(&state)
    );

    let priority_player = state.priority_player;
    // CR 117.3b, as in A2. This row's capture holds priority on the active seat
    // already, so on its own it does NOT discriminate; A2 carries that weight.
    // It is asserted here so the two recovery rows state one shared invariant.
    assert_eq!(
        priority_player, state.active_player,
        "CR 117.3b: the recovered window returns priority to the active player"
    );
    apply(&mut state, priority_player, GameAction::PassPriority)
        .expect("the recovered priority holder accepts the next pass");

    assert!(
        state.deferred_triggers.is_empty(),
        "CR 603.3 + CR 603.3b: with the strand gone, `resolution_completion_can_settle` is true \
         again and the parked abilities must LEAVE the deferred queue, still parked: {:?}",
        deferred_sources(&state)
    );
    // The paired positive, which A2 already carries for turn-15. Emptiness alone
    // is satisfied by a queue that was DISCARDED as well as by one that drained,
    // and discarding is a CR 603.3b violation that this row would otherwise pass.
    // Non-vacuous by construction: `assert_turn20_wedge_present` pins the input
    // queue to exactly these two sources before the action runs.
    assert_eq!(
        triggers_reaching_the_stack(&state),
        vec![157, 199],
        "CR 603.3b: both parked abilities must ARRIVE on the stack, not merely leave the queue"
    );
}

/// **A4b — discriminating(U1), the ENTRY evaluation point's isolator.**
///
/// The wedge is repaired even on an action the engine REJECTS. This is the only
/// row in the file whose green cannot be produced by the post-action
/// priority-boundary sweeper: an action that fails `check_actor_authorization`
/// never reaches `apply_action`, so `pass_priority_once_with_pipeline`,
/// `resume_pending_continuation_if_priority` and `run_post_action_pipeline` are
/// never called at all. What is left is the entry sweep in
/// `apply_action_boundary_core`, and the fact that it runs BEFORE
/// `let boundary_snapshot = state.clone();` — the snapshot every failure path
/// restores. A sweep sited after the snapshot would be rolled back with the
/// rejected action and this row would red.
///
/// CR 603.3 + CR 603.3b + CR 608.2c: an ownerless `Dispatching` resident is a
/// corrupt state, not a rules state, so removing it is not part of any action
/// and must survive an action's rollback.
#[test]
fn a4b_entry_sweep_repairs_the_wedge_even_on_a_rejected_action() {
    let mut state = load_turn20();
    assert_turn20_wedge_present(&state);

    // Player 0 does not hold priority (the capture rests at `Priority { player: 1 }`),
    // so `check_actor_authorization` rejects this before any reducer arm runs.
    // The `is_err` assertion is this row's reach-guard: if the action were ever
    // accepted, the row would fail here rather than silently measure the
    // accepted path.
    let rejected = apply(&mut state, PlayerId(0), GameAction::PassPriority);
    assert!(
        rejected.is_err(),
        "reach-guard: this action must be REJECTED, or the row measures the accepted path instead"
    );

    assert!(
        state.resolution_stack.is_empty(),
        "the entry sweep must repair the state even though the action was rejected, got {:?}",
        post_replacement_drain_statuses(&state)
    );
}

/// Verbatim from the shipped constant of the same name in
/// `crates/engine/tests/integration/issue_5657_zurs_weirding.rs` — inherited, not
/// paraphrased, so a later `/card-test` audit reads it as the co-witness rows'
/// own Oracle text.
const ZURS_WEIRDING_ORACLE: &str = "If a player would draw a card, they reveal it instead. Then any other player may pay 2 life. If a player does, put that card into its owner's graveyard. Otherwise, that player draws a card.";

/// **B2' — discriminating(U2 at the mid-scenario sample, U1 at the terminal
/// sample).** A real production replacement-continuation producer leaves no
/// `Dispatching` drain anywhere in the resolution stack at ANY point in its
/// scenario.
///
/// This is NOT a Devour scenario. It drives the Zur's Weirding draw-replacement
/// path — `replacement.rs`'s draw replacement → `OpponentMayChoice` fan-out →
/// the same single dispatcher this change fixes — because that is the producer
/// whose strand was MEASURED, rather than hypothesised, at `BASE_SHA`.
///
/// Its red was established by that measurement, not by running this row at
/// `BASE_SHA`: the recorded capture shows `resident=Dispatching` at TWO sample
/// points in all three shipped `issue_5657_zurs_weirding` rows — mid-scenario
/// (`len=3`, `waiting_for="OpponentMayChoice"`, frames
/// `[PostReplacement, MultiDraw, OptionalEffect]`, which is U2's witness because
/// the frame is buried where the two-deep positional accessor cannot see it) and
/// at rest (`len=1`, `waiting_for="Priority"`, the reporter's exact wedge shape,
/// which is U1's). Those three shipped rows are this row's co-witnesses, and they
/// carried this exact strand invisibly for their whole history, because the
/// invariant that would have caught it keys on `Paused`.
#[test]
fn b2_zurs_weirding_replacement_leaves_no_dispatching_drain() {
    // CR 603.3 + CR 603.3b + CR 608.2c: at NO point in this scenario may any
    // post-replacement frame anywhere in the resolution stack hold a
    // `Dispatching` entry. Sampling only at the end would let a strand that is
    // created and cleaned up mid-scenario pass unseen.
    //
    // A whole-stack walk is required rather than `active_post_replacement_drains()`,
    // because the strand this row targets was MEASURED buried at BASE_SHA
    // (frame index 0 of 3, `[PostReplacement, MultiDraw, OptionalEffect]`,
    // `waiting_for = OpponentMayChoice`) — precisely the shape the two-deep
    // positional accessor cannot see and U1's sweep therefore cannot reach.
    let assert_no_dispatching = |state: &GameState, at: &str| {
        let statuses = post_replacement_drain_statuses(state);
        assert!(
            !statuses
                .iter()
                .any(|frame| frame.iter().any(|s| s == "Dispatching")),
            "a Dispatching post-replacement drain survives at {at}: {statuses:?}"
        );
    };

    // The shipped block-scoped builder shape: the handle borrows `scenario`
    // mutably, so it is bound inside a block rather than chained.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    {
        let mut zurs_weirding =
            scenario.add_creature_from_oracle(P0, "Zur's Weirding", 0, 1, ZURS_WEIRDING_ORACLE);
        zurs_weirding.as_enchantment();
    }
    scenario.with_library_top(P1, &["Grizzly Bears", "Forest", "Plains"]);
    scenario.with_library_top(P0, &["P0 Library 1", "P0 Library 2", "P0 Library 3"]);
    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;

    let p0_life_before = runner.state().players[P0.0 as usize].life;
    assert_no_dispatching(runner.state(), "before the draw");

    runner
        .act(GameAction::Debug(DebugAction::DrawCards {
            player_id: P1,
            count: 1,
        }))
        .expect("debug draw must succeed");
    assert_no_dispatching(runner.state(), "after the debug draw");

    // Take the ACCEPT path: it runs the full replacement tail and puts the card
    // into its owner's graveyard, so the positive reach-guards below are real.
    let mut answered = 0;
    for _ in 0..120 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OpponentMayChoice { player, .. } => {
                assert_ne!(
                    player, P1,
                    "the drawing player must never be offered the opponent-may choice"
                );
                answered += 1;
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("opponent-may decision must succeed");
                assert_no_dispatching(runner.state(), "after the OpponentMayChoice answer");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() && answered > 0 => break,
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    runner.advance_until_stack_empty();
                    assert_no_dispatching(runner.state(), "after advance_until_stack_empty");
                    break;
                }
                assert_no_dispatching(runner.state(), "after a priority pass");
            }
        }
    }
    runner.advance_until_stack_empty();
    assert_no_dispatching(runner.state(), "after the scenario settles");

    // Positive reach-guards, asserted before the negative is trusted: the
    // replacement demonstrably RAN. Without these the whole-stack walk could be
    // green simply because the replacement path was never entered.
    assert_eq!(
        answered, 1,
        "reach-guard: the OpponentMayChoice fan-out must have been offered exactly once"
    );
    let p1_graveyard: Vec<String> = runner.state().players[P1.0 as usize]
        .graveyard
        .iter()
        .filter_map(|id| runner.state().objects.get(id).map(|o| o.name.clone()))
        .collect();
    let p1_hand: Vec<String> = runner.state().players[P1.0 as usize]
        .hand
        .iter()
        .filter_map(|id| runner.state().objects.get(id).map(|o| o.name.clone()))
        .collect();
    assert!(
        p1_graveyard.contains(&"Grizzly Bears".to_string()),
        "reach-guard: accepting must bin the revealed card in its owner's graveyard, got {p1_graveyard:?}"
    );
    assert!(
        !p1_hand.contains(&"Grizzly Bears".to_string()),
        "reach-guard: the binned card must not reach the drawing player's hand, got {p1_hand:?}"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        p0_life_before - 2,
        "reach-guard: the accepting player must have paid exactly 2 life"
    );
}
