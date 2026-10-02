use engine::game::effects::draw::{preview_draw_delivery, DrawDeliveryPreview};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::{DebugAction, GameAction};
use engine::types::card_type::{CoreType, Supertype};
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::{
    DrawSequenceFrameId, DrawSequenceStack, GameState, PersistedGameState, ReplacementChoiceKind,
    WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use serde_json::Value;

const BLOOD_SCRIVENER: &str = "If you would draw a card while you have no cards in hand, instead you draw two cards and you lose 1 life.";
const TEFERI: &str = "If you would draw a card except the first one you draw in each of your draw steps, draw two cards instead.";

fn f4_runner() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for index in 0..8 {
        scenario.add_card_to_library_top(P0, &format!("P0 library card {index}"));
        scenario.add_card_to_library_top(P1, &format!("P1 library card {index}"));
    }
    let scrivener = scenario
        .add_creature_from_oracle(P0, "Blood Scrivener", 2, 1, BLOOD_SCRIVENER)
        .id();
    let teferi = scenario
        .add_enchantment_from_oracle(P0, "Teferi's Ageless Insight", TEFERI)
        .as_legendary()
        .id();
    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    assert!(runner.state().objects[&teferi]
        .card_types
        .core_types
        .contains(&CoreType::Enchantment));
    assert!(runner.state().objects[&teferi]
        .card_types
        .supertypes
        .contains(&Supertype::Legendary));
    (runner, teferi, scrivener)
}

fn draw_one(runner: &mut GameRunner, player: PlayerId) -> Vec<GameEvent> {
    runner
        .act(GameAction::Debug(DebugAction::DrawCards {
            player_id: player,
            count: 1,
        }))
        .expect("the draw instruction must start")
        .events
}

fn ordering_prompt(runner: &GameRunner, first: ObjectId, second: ObjectId) -> usize {
    let (chooser, kind, candidates) = match runner.state().waiting_for.clone() {
        WaitingFor::ReplacementChoice {
            player,
            kind,
            candidates,
            ..
        } => (player, kind, candidates),
        other => panic!("expected replacement ordering prompt, got {other:?}"),
    };
    assert_eq!(chooser, P0, "the drawing player chooses the order");
    assert_eq!(kind, ReplacementChoiceKind::Order);
    assert!(candidates
        .iter()
        .any(|candidate| candidate.source_id == first));
    assert!(candidates
        .iter()
        .any(|candidate| candidate.source_id == second));
    candidates
        .iter()
        .position(|candidate| candidate.source_id == first)
        .expect("the selected replacement must be offered")
}

fn f4_result(teferi_first: bool, expected: (usize, usize, i32, usize, Option<i32>)) {
    let (mut runner, teferi, scrivener) = f4_runner();
    let (first, second) = if teferi_first {
        (teferi, scrivener)
    } else {
        (scrivener, teferi)
    };
    assert_eq!(runner.state().players[P0.0 as usize].hand.len(), 0);
    assert_eq!(runner.state().players[P0.0 as usize].life, 20);
    let initial_events = draw_one(&mut runner, P0);
    let before = runner
        .state()
        .active_draw_sequence()
        .map(|frame| (frame.remaining, frame.accumulated));
    let index = ordering_prompt(&runner, first, second);
    let (chooser, _, _) = match runner.state().waiting_for.clone() {
        WaitingFor::ReplacementChoice {
            player,
            kind,
            candidates,
            ..
        } => (player, kind, candidates),
        _ => unreachable!("ordering prompt was checked above"),
    };
    let selected = runner
        .act(GameAction::ChooseReplacement { index })
        .expect("the ordering choice must be accepted");
    let mut events = initial_events;
    events.extend(selected.events.clone());
    let applied_sources = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::ReplacementApplied { source_id, .. } => Some(*source_id),
            _ => None,
        })
        .collect::<Vec<_>>();
    runner.advance_until_stack_empty();

    let card_drawn = events
        .iter()
        .filter(|event| matches!(event, GameEvent::CardDrawn { player_id: P0, .. }))
        .count();
    let life_loss = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::LifeChanged {
                    player_id: P0,
                    amount: -1,
                    ..
                }
            )
        })
        .count();
    let delivered = (
        card_drawn,
        runner.state().players[P0.0 as usize].hand.len(),
        runner.state().players[P0.0 as usize].life,
        runner.state().players[P0.0 as usize].library.len(),
        runner.state().last_effect_count,
    );
    let actions = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::PlayerPerformedAction {
                    player_id: P0,
                    action: PlayerActionKind::Draw,
                    ..
                }
            )
        })
        .count();
    let ledger = runner
        .state()
        .player_actions_this_turn
        .iter()
        .filter(|entry| **entry == (P0, PlayerActionKind::Draw))
        .count();
    let remaining_after = runner
        .state()
        .active_draw_sequence()
        .map(|frame| (frame.remaining, frame.accumulated));
    println!(
        "task30 F4 chooser={chooser:?} order_first={first:?} remaining_before={before:?} remaining_after={remaining_after:?} applied_sources={applied_sources:?} CardDrawn={card_drawn} life_loss_events={life_loss} PlayerPerformedAction::Draw={actions} ledger={ledger} result={delivered:?}"
    );
    assert_eq!(delivered, expected);
    assert_eq!(life_loss, 1, "Blood Scrivener's rider resolves once");
    assert!(!matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    assert!(runner.state().resolving_stack_entry.is_none());
}

#[test]
fn f4_teferi_then_scrivener_delivers_three_and_loses_one_life() {
    f4_result(true, (3, 3, 19, 5, Some(3)));
}

#[test]
fn f4_scrivener_then_teferi_delivers_four_and_loses_one_life() {
    f4_result(false, (4, 4, 19, 4, Some(4)));
}

fn validate_owner(
    parent_player: PlayerId,
    child_player: PlayerId,
    owner: DrawSequenceFrameId,
    depth: usize,
) -> bool {
    let mut stack = DrawSequenceStack::default();
    stack.push(parent_player, 1);
    for _ in 1..depth {
        stack.push(parent_player, 1);
    }
    stack.push(child_player, 1);
    stack
        .active_mut()
        .expect("child frame was pushed")
        .delivery_owner = Some(owner);
    stack.validate().is_err()
}

#[test]
fn delivery_owner_validator_rejects_malformed_links() {
    let mut valid = DrawSequenceStack::default();
    let parent = valid.push(P0, 1);
    valid.push(P0, 1);
    valid.active_mut().unwrap().delivery_owner = Some(parent);
    assert!(
        valid.validate().is_ok(),
        "an immediate same-player owner is valid"
    );
    let cross_player = {
        let mut stack = DrawSequenceStack::default();
        let parent = stack.push(P0, 1);
        stack.push(P1, 1);
        stack.active_mut().unwrap().delivery_owner = Some(parent);
        stack.validate().is_err()
    };
    let missing_owner = validate_owner(P0, P0, DrawSequenceFrameId(99), 1);
    let self_owner = {
        let mut stack = DrawSequenceStack::default();
        stack.push(P0, 1);
        let child = stack.push(P0, 1);
        stack.active_mut().unwrap().delivery_owner = Some(child);
        stack.validate().is_err()
    };
    let non_immediate_parent = validate_owner(P0, P0, DrawSequenceFrameId(0), 2);
    let rejected = [
        cross_player,
        missing_owner,
        self_owner,
        non_immediate_parent,
    ];
    println!(
        "task30 owner validator rejects [cross-player, missing, self, non-immediate]={rejected:?}"
    );
    assert_eq!(rejected, [true; 4]);
}

fn start_dredge_wait() -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for index in 0..4 {
        scenario.add_card_to_library_top(P0, &format!("P0 library card {index}"));
    }
    scenario.add_creature_from_oracle(P0, "Blood Scrivener", 2, 1, BLOOD_SCRIVENER);
    let dredger = scenario
        .add_creature_to_graveyard(P0, "Test Dredger", 1, 1)
        .from_oracle_text_with_keywords(&["Dredge"], "Dredge 2")
        .id();
    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    draw_one(&mut runner, P0);
    let scrivener = runner
        .state()
        .objects
        .values()
        .find(|object| object.name == "Blood Scrivener")
        .expect("Scrivener permanent exists")
        .id;
    let index = ordering_prompt(&runner, scrivener, dredger);
    runner
        .act(GameAction::ChooseReplacement { index })
        .expect("choose Scrivener before Dredge");
    match runner.state().waiting_for {
        WaitingFor::ReplacementChoice {
            player: P0,
            kind: ReplacementChoiceKind::OptionalBranch,
            ..
        } => {}
        ref other => panic!("expected real Dredge accept/decline wait, got {other:?}"),
    }
    (runner, dredger)
}

fn contains_non_null(value: &Value, key: &str) -> bool {
    match value {
        Value::Object(map) => {
            map.get(key).is_some_and(|value| !value.is_null())
                || map.values().any(|child| contains_non_null(child, key))
        }
        Value::Array(items) => items.iter().any(|child| contains_non_null(child, key)),
        _ => false,
    }
}

fn remove_delivery_owner_fields(value: &mut Value) -> usize {
    match value {
        Value::Object(map) => {
            let removed = usize::from(map.remove("delivery_owner").is_some());
            removed
                + map
                    .values_mut()
                    .map(remove_delivery_owner_fields)
                    .sum::<usize>()
        }
        Value::Array(items) => items.iter_mut().map(remove_delivery_owner_fields).sum(),
        _ => 0,
    }
}

fn remove_non_null_delivery_owner_fields(value: &mut Value) -> usize {
    match value {
        Value::Object(map) => {
            let removed = usize::from(
                map.get("delivery_owner")
                    .is_some_and(|owner| !owner.is_null()),
            );
            if removed > 0 {
                map.remove("delivery_owner");
            }
            removed
                + map
                    .values_mut()
                    .map(remove_non_null_delivery_owner_fields)
                    .sum::<usize>()
        }
        Value::Array(items) => items
            .iter_mut()
            .map(remove_non_null_delivery_owner_fields)
            .sum(),
        _ => 0,
    }
}

fn contains_null(value: &Value, key: &str) -> bool {
    match value {
        Value::Object(map) => {
            map.get(key).is_some_and(Value::is_null)
                || map.values().any(|child| contains_null(child, key))
        }
        Value::Array(items) => items.iter().any(|child| contains_null(child, key)),
        _ => false,
    }
}

fn has_key(value: &Value, key: &str) -> bool {
    match value {
        Value::Object(map) => {
            map.contains_key(key) || map.values().any(|child| has_key(child, key))
        }
        Value::Array(items) => items.iter().any(|child| has_key(child, key)),
        _ => false,
    }
}

fn assert_baseline_a_v3_without_owner(value: &Value, trusted: bool) {
    let version_path = if trusted {
        "/state/resolution_state_version"
    } else {
        "/resolution_state_version"
    };
    assert_eq!(value.pointer(version_path).and_then(Value::as_u64), Some(3));
    assert!(!has_key(value, "delivery_owner"));
}

fn change_owned_child_to_other_player(value: &mut Value) -> bool {
    match value {
        Value::Object(map) => {
            if map
                .get("delivery_owner")
                .is_some_and(|owner| !owner.is_null())
            {
                map.insert(
                    "player".to_string(),
                    serde_json::to_value(P1).expect("serialize the other player ID"),
                );
                return true;
            }
            map.values_mut().any(change_owned_child_to_other_player)
        }
        Value::Array(items) => items.iter_mut().any(change_owned_child_to_other_player),
        _ => false,
    }
}

#[derive(Debug, PartialEq)]
struct DredgeOutcome {
    card_drawn: usize,
    draw_actions: usize,
    life_losses: usize,
    ledger: usize,
    hand: usize,
    life: i32,
    library: usize,
    last_effect_count: Option<i32>,
    stacks_settled: bool,
    no_replacement_prompt: bool,
    dredger_in_hand: bool,
    waiting_for: String,
}

fn complete_dredge_wait(mut runner: GameRunner, dredger: ObjectId) -> DredgeOutcome {
    let (chooser, kind, candidates) = match runner.state().waiting_for.clone() {
        WaitingFor::ReplacementChoice {
            player,
            kind,
            candidates,
            ..
        } => (player, kind, candidates),
        other => panic!("expected Dredge wait, got {other:?}"),
    };
    assert_eq!(chooser, P0);
    assert_eq!(kind, ReplacementChoiceKind::OptionalBranch);
    let index = candidates
        .iter()
        .position(|candidate| candidate.source_id == dredger)
        .unwrap();
    assert_eq!(
        candidates
            .get(1)
            .map(|candidate| candidate.description.as_str()),
        Some("Decline")
    );
    let result = runner
        .act(GameAction::ChooseReplacement { index })
        .expect("accept Dredge");
    runner.advance_until_stack_empty();
    let card_drawn = result
        .events
        .iter()
        .filter(|event| matches!(event, GameEvent::CardDrawn { player_id: P0, .. }))
        .count();
    let draw_actions = result
        .events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::PlayerPerformedAction {
                    player_id: P0,
                    action: PlayerActionKind::Draw,
                    ..
                }
            )
        })
        .count();
    let life_losses = result
        .events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::LifeChanged {
                    player_id: P0,
                    amount: -1,
                    ..
                }
            )
        })
        .count();
    let ledger = runner
        .state()
        .player_actions_this_turn
        .iter()
        .filter(|entry| **entry == (P0, PlayerActionKind::Draw))
        .count();
    let state = runner.state();
    let no_replacement_prompt = !matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. });
    DredgeOutcome {
        card_drawn,
        draw_actions,
        life_losses,
        ledger,
        hand: state.players[P0.0 as usize].hand.len(),
        life: state.players[P0.0 as usize].life,
        library: state.players[P0.0 as usize].library.len(),
        last_effect_count: state.last_effect_count,
        stacks_settled: state.stack.is_empty()
            && state.resolution_stack.is_empty()
            && state.resolving_stack_entry.is_none(),
        no_replacement_prompt,
        dredger_in_hand: state
            .objects
            .get(&dredger)
            .is_some_and(|object| object.zone == Zone::Hand),
        waiting_for: format!("{:?}", state.waiting_for),
    }
}

#[test]
fn canonical_raw_and_trusted_saves_require_draw_result_owners() {
    let (uninterrupted, dredger) = start_dredge_wait();
    let state = uninterrupted.state().clone();
    let wire = serde_json::to_value(PersistedGameState::capture(state.clone()))
        .expect("capture the real Dredge choice through the canonical save entry");
    let raw_wire = serde_json::to_value(PersistedGameState::Raw(Box::new(state)))
        .expect("write the same wait through the canonical raw save entry");
    let unversioned_raw_wire = serde_json::to_value(uninterrupted.state())
        .expect("write the same wait through the bare GameState serializer");
    assert!(unversioned_raw_wire.get("resolution_stack").is_some());
    assert!(unversioned_raw_wire
        .get("resolution_state_version")
        .is_none());
    assert_eq!(
        wire.pointer("/state/resolution_state_version")
            .and_then(Value::as_u64),
        Some(4)
    );
    assert_eq!(raw_wire["resolution_state_version"], Value::from(4_u64));
    assert!(
        contains_non_null(&wire, "delivery_owner"),
        "the paused save must carry the child owner"
    );
    assert!(
        contains_null(&wire, "delivery_owner"),
        "the paused save must distinguish a valid ownerless frame from an omitted field"
    );
    assert!(contains_non_null(&raw_wire, "delivery_owner"));
    assert!(contains_null(&raw_wire, "delivery_owner"));
    if let Some(path) = std::env::var_os("P1_TASK33_V4_SNAPSHOT_OUTPUT") {
        std::fs::write(path, serde_json::to_vec_pretty(&wire).unwrap())
            .expect("write the actual new-writer snapshot for the baseline-A reader test");
    }

    let restored = serde_json::from_value::<PersistedGameState>(wire.clone())
        .expect("current reader accepts its canonical save")
        .into_game_state()
        .expect("the canonical restore path accepts the real choice wait");
    let raw_restored = serde_json::from_value::<PersistedGameState>(raw_wire.clone())
        .expect("raw persistence accepts its canonical save")
        .into_game_state()
        .expect("the common raw restore path accepts the real choice wait");
    let unversioned_raw_restored =
        serde_json::from_value::<PersistedGameState>(unversioned_raw_wire)
            .expect("unversioned raw persistence infers the current owner-aware wire")
            .into_game_state()
            .expect("the common raw restore path accepts the bare GameState shape");
    let ordinary = complete_dredge_wait(uninterrupted, dredger);
    let resumed = complete_dredge_wait(GameRunner::from_state(restored), dredger);
    let raw_resumed = complete_dredge_wait(GameRunner::from_state(raw_restored), dredger);
    let unversioned_raw_resumed =
        complete_dredge_wait(GameRunner::from_state(unversioned_raw_restored), dredger);
    assert_eq!(
        resumed, ordinary,
        "trusted restored continuation matches uninterrupted resolution"
    );
    assert_eq!(
        raw_resumed, ordinary,
        "raw restored continuation matches uninterrupted resolution"
    );
    assert_eq!(
        unversioned_raw_resumed, ordinary,
        "unversioned raw restored continuation matches uninterrupted resolution"
    );

    let mut cross_player = wire.clone();
    assert!(change_owned_child_to_other_player(&mut cross_player));
    let rejected = match serde_json::from_value::<PersistedGameState>(cross_player) {
        Err(_) => true,
        Ok(persisted) => persisted.into_game_state().is_err(),
    };
    assert!(
        rejected,
        "canonical save/load rejects a child whose result owner is another player"
    );
    assert_eq!(ordinary.life_losses, 1, "Scrivener rider resolves once");
    assert_eq!(
        (
            ordinary.hand,
            ordinary.life,
            ordinary.library,
            ordinary.last_effect_count,
            ordinary.stacks_settled,
            ordinary.no_replacement_prompt,
            ordinary.dredger_in_hand,
        ),
        (2, 19, 1, Some(1), true, true, true)
    );
    println!("task30 save/restore outcome={ordinary:?}");

    let mut omitted_trusted = wire.clone();
    assert!(remove_delivery_owner_fields(&mut omitted_trusted) > 0);
    let trusted_error = serde_json::from_value::<PersistedGameState>(omitted_trusted)
        .expect_err("trusted v4 decode rejects a missing owner before restore")
        .to_string();
    assert!(trusted_error.contains("missing required delivery_owner"));

    let mut omitted_raw = raw_wire.clone();
    assert!(remove_delivery_owner_fields(&mut omitted_raw) > 0);
    let raw_error = serde_json::from_value::<PersistedGameState>(omitted_raw)
        .expect_err("raw v4 decode rejects a missing owner before restore")
        .to_string();
    assert!(raw_error.contains("missing required delivery_owner"));

    for (name, value, version_path) in [
        ("trusted", wire.clone(), "/state/resolution_state_version"),
        ("raw", raw_wire.clone(), "/resolution_state_version"),
    ] {
        let mut legacy = value;
        assert!(remove_non_null_delivery_owner_fields(&mut legacy) > 0);
        assert!(
            contains_null(&legacy, "delivery_owner"),
            "{name} retains an explicit null owner in the nested typed draw frame"
        );
        for version in [2_u64, 3_u64] {
            let mut versioned = legacy.clone();
            *versioned
                .pointer_mut(version_path)
                .expect("the canonical version field is present") = Value::from(version);
            let error = serde_json::from_value::<PersistedGameState>(versioned)
                .expect_err("a present null owner cannot be relabeled as a legacy version")
                .to_string();
            assert!(
                error.contains("requires resolution_state_version 4"),
                "{name} v{version} must reject the explicit-null owner: {error}"
            );
        }
    }

    let mut downgraded = raw_wire.clone();
    downgraded["resolution_state_version"] = Value::from(3_u64);
    let downgrade_error = serde_json::from_value::<PersistedGameState>(downgraded)
        .expect_err("a v4 owner field cannot be relabeled as v3")
        .to_string();
    assert!(downgrade_error.contains("requires resolution_state_version 4"));

    let mut unmarked = raw_wire;
    unmarked
        .as_object_mut()
        .expect("canonical raw save is an object")
        .remove("resolution_state_version");
    let unmarked_error = serde_json::from_value::<PersistedGameState>(unmarked)
        .expect_err("an unmarked typed-frame save cannot fall through to a legacy reader")
        .to_string();
    assert!(
        unmarked_error.contains("v1 resolution state must not contain resolution_frames"),
        "an unmarked canonical typed-frame save must fail at the legacy carrier boundary: {unmarked_error}"
    );
    println!("task33 save/restore outcome={ordinary:?}");
}

#[test]
fn unversioned_raw_game_state_with_owner_metadata_is_classified_as_v4() {
    let (runner, _) = start_dredge_wait();
    let raw = serde_json::to_value(runner.state()).expect("serialize a bare raw GameState");
    assert!(raw.get("resolution_stack").is_some());
    assert!(has_key(&raw, "delivery_owner"));
    let restored = serde_json::from_value::<GameState>(raw)
        .expect("the existing unversioned raw entry recognizes current owner metadata");
    assert!(restored
        .active_draw_sequence()
        .and_then(|frame| frame.delivery_owner)
        .is_some());
    println!("task33 unversioned raw GameState with explicit owner fields classified as v4");
}

#[test]
fn genuine_baseline_a_stable_raw_and_trusted_saves_restore() {
    for (name, fixture, trusted) in [
        (
            "stable raw",
            include_str!("fixtures/p1_9354_task33_a_stable_raw_v3.json"),
            false,
        ),
        (
            "stable trusted",
            include_str!("fixtures/p1_9354_task33_a_stable_trusted_v3.json"),
            true,
        ),
    ] {
        let wire: Value = serde_json::from_str(fixture).expect("baseline-A fixture is JSON");
        assert_baseline_a_v3_without_owner(&wire, trusted);
        let state = serde_json::from_value::<PersistedGameState>(wire)
            .expect("new reader accepts a genuine baseline-A stable save")
            .into_game_state()
            .expect("new reader restores a genuine baseline-A stable save");
        assert_eq!(state.players.len(), 2, "{name} restores both players");
        assert!(
            state.resolution_stack.is_empty(),
            "{name} has no pending frames"
        );
        assert!(
            state.stack.is_empty(),
            "{name} has no pending stack objects"
        );
        assert_eq!(state.players[P0.0 as usize].life, 20, "{name} keeps life");
    }
    println!("task33 genuine baseline-A stable raw/trusted v3 saves restored");
}

#[derive(Debug, PartialEq)]
struct IndependentDredgeRestoreOutcome {
    actual_draws: usize,
    hand: usize,
    last_effect_count: Option<i32>,
    library: usize,
    dredger_in_hand: bool,
    frames_settled: bool,
    no_replacement_prompt: bool,
}

fn resume_task34_legacy_independent_dredge_wait(
    wire: Value,
    decline: bool,
) -> Result<IndependentDredgeRestoreOutcome, String> {
    let persisted =
        serde_json::from_value::<PersistedGameState>(wire).map_err(|error| error.to_string())?;
    let state = persisted
        .into_game_state()
        .map_err(|error| error.to_string())?;
    let mut runner = GameRunner::from_state(state);
    let candidates = match runner.state().waiting_for.clone() {
        WaitingFor::ReplacementChoice {
            player: P0,
            kind: ReplacementChoiceKind::OptionalBranch,
            candidates,
            ..
        } => candidates,
        other => return Err(format!("restored state lost the Dredge choice: {other:?}")),
    };
    let accept = candidates
        .iter()
        .position(|candidate| candidate.description != "Decline")
        .ok_or_else(|| "restored Dredge choice has no accept branch".to_string())?;
    let decline_index = candidates
        .iter()
        .position(|candidate| candidate.description == "Decline")
        .ok_or_else(|| "restored Dredge choice has no decline branch".to_string())?;
    let dredger = candidates[accept].source_id;
    let index = if decline { decline_index } else { accept };
    let result = runner
        .act(GameAction::ChooseReplacement { index })
        .map_err(|error| format!("could not resume selected Dredge branch: {error}"))?;
    runner.advance_until_stack_empty();

    let actual_draws = result
        .events
        .iter()
        .filter(|event| matches!(event, GameEvent::CardDrawn { player_id: P0, .. }))
        .count();
    let state = runner.state();
    Ok(IndependentDredgeRestoreOutcome {
        actual_draws,
        hand: state.players[P0.0 as usize].hand.len(),
        last_effect_count: state.last_effect_count,
        library: state.players[P0.0 as usize].library.len(),
        dredger_in_hand: state
            .objects
            .get(&dredger)
            .is_some_and(|object| object.zone == Zone::Hand),
        frames_settled: state.stack.is_empty()
            && state.resolution_stack.is_empty()
            && state.resolving_stack_entry.is_none(),
        no_replacement_prompt: !matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
    })
}

#[test]
fn genuine_baseline_a_independent_dredge_wait_raw_and_trusted_resume_both_branches() {
    let mut restored_branches = 0;
    let mut refusals = Vec::new();
    for (name, fixture, trusted) in [
        (
            "independent Dredge raw",
            include_str!("fixtures/p1_9354_task34_a_independent_dredge_wait_raw_v3.json"),
            false,
        ),
        (
            "independent Dredge trusted",
            include_str!("fixtures/p1_9354_task34_a_independent_dredge_wait_trusted_v3.json"),
            true,
        ),
    ] {
        let wire: Value = serde_json::from_str(fixture).expect("baseline-A fixture is JSON");
        assert_baseline_a_v3_without_owner(&wire, trusted);
        for decline in [false, true] {
            match resume_task34_legacy_independent_dredge_wait(wire.clone(), decline) {
                Ok(outcome) => {
                    let expected = IndependentDredgeRestoreOutcome {
                        actual_draws: usize::from(decline),
                        hand: 1,
                        last_effect_count: Some(i32::from(decline)),
                        library: if decline { 3 } else { 2 },
                        dredger_in_hand: !decline,
                        frames_settled: true,
                        no_replacement_prompt: true,
                    };
                    assert_eq!(
                        outcome, expected,
                        "{name}, decline={decline}: Dredge resumes without a parent result owner"
                    );
                    restored_branches += 1;
                }
                Err(error) => {
                    println!("{name}, decline={decline}: {error}");
                    refusals.push(format!("{name}, decline={decline}: {error}"));
                }
            }
        }
    }
    assert!(
        refusals.is_empty(),
        "safe baseline-A independent waits must restore: {refusals:?}"
    );
    assert_eq!(
        restored_branches, 4,
        "raw/trusted each resume accept and decline"
    );
    println!("Task34 genuine A independent Dredge waits: raw/trusted accept=0 draws, decline=1 draw; all frames settled");
}

#[test]
fn genuine_baseline_a_ambiguous_dredge_wait_raw_and_trusted_are_rejected() {
    for (name, fixture, trusted) in [
        (
            "Dredge raw",
            include_str!("fixtures/p1_9354_task33_a_dredge_wait_raw_v3.json"),
            false,
        ),
        (
            "Dredge trusted",
            include_str!("fixtures/p1_9354_task33_a_dredge_wait_trusted_v3.json"),
            true,
        ),
    ] {
        let wire: Value = serde_json::from_str(fixture).expect("baseline-A fixture is JSON");
        assert_baseline_a_v3_without_owner(&wire, trusted);
        let error = serde_json::from_value::<PersistedGameState>(wire)
            .expect_err("new reader refuses an ambiguous baseline-A Dredge save before restore")
            .to_string();
        assert!(
            error.contains(
                "cannot safely restore a paused multi-draw replacement with a pending post-replacement continuation and no delivery_owner"
            ),
            "{name} reports the precise compatibility limit: {error}"
        );
    }
    println!(
        "task33 genuine baseline-A ambiguous Dredge raw/trusted v3 saves rejected before restore"
    );
}

#[test]
fn malformed_cross_player_delivery_owner_fails_closed_on_completion() {
    let (mut runner, dredger) = start_dredge_wait();
    let owner = runner
        .state()
        .active_draw_sequence()
        .and_then(|frame| frame.delivery_owner)
        .expect("the paused child draw has a delivery owner");
    runner
        .state_mut()
        .draw_sequence_frame_mut(owner)
        .expect("the owner frame is live")
        .player = P1;

    let candidates = match runner.state().waiting_for.clone() {
        WaitingFor::ReplacementChoice {
            player: P0,
            kind: ReplacementChoiceKind::OptionalBranch,
            candidates,
            ..
        } => candidates,
        other => panic!("expected the Dredge choice, got {other:?}"),
    };
    let index = candidates
        .iter()
        .position(|candidate| candidate.source_id == dredger)
        .expect("the Dredge replacement is offered");
    runner
        .act(GameAction::ChooseReplacement { index })
        .expect("accept the Dredge branch");
    runner.advance_until_stack_empty();

    assert!(runner.state_mut().draw_sequence_frame_mut(owner).is_none());
    let foreign_draw_actions = runner
        .state()
        .player_actions_this_turn
        .iter()
        .filter(|entry| **entry == (P1, PlayerActionKind::Draw))
        .count();
    assert_eq!(
        foreign_draw_actions, 0,
        "a cross-player child result must not publish a draw for its retired owner"
    );
}

#[test]
fn f3_observe_replacement_draw_event_and_turn_ledger_contract() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for index in 0..5 {
        scenario.add_card_to_library_top(P0, &format!("P0 library card {index}"));
    }
    scenario.add_creature_from_oracle(P0, "Blood Scrivener", 2, 1, BLOOD_SCRIVENER);
    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    let preview = preview_draw_delivery(runner.state(), P0, 1);
    let action = draw_one(&mut runner, P0);
    runner.advance_until_stack_empty();
    let observed = (
        preview,
        action
            .iter()
            .filter(|event| matches!(event, GameEvent::CardDrawn { player_id: P0, .. }))
            .count(),
        action
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    GameEvent::PlayerPerformedAction {
                        player_id: P0,
                        action: PlayerActionKind::Draw,
                        ..
                    }
                )
            })
            .count(),
        runner
            .state()
            .player_actions_this_turn
            .iter()
            .filter(|entry| **entry == (P0, PlayerActionKind::Draw))
            .count(),
        runner.state().players[P0.0 as usize].hand.len(),
        runner.state().players[P0.0 as usize].life,
        runner.state().last_effect_count,
    );
    println!("task30 F3 preview/events/action-ledger/board={observed:?}");
    assert_eq!(observed.0, DrawDeliveryPreview::Exact { delivered: 2 });
    assert_eq!(
        (observed.2, observed.3),
        (2, 2),
        "child and parent draw instructions complete once each"
    );
    assert_eq!(
        (observed.1, observed.4, observed.5, observed.6),
        (2, 2, 19, Some(2))
    );
}
