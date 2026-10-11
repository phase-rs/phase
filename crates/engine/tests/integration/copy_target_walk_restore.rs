//! CR 707.10c + CR 601.2c + CR 115.7d: restoring a parked copy target walk
//! (`WaitingFor::CopyRetarget`). A save from before the walk recorded its mode
//! and picks is rebuilt (mode inferred, decided prefix replayed through the
//! reducer's own gate); the walk is truncated at the first pick the restored
//! board refuses and re-asks from there. A fresh-copy announcement with no
//! legal announcement fails closed (it is not resumable, and its recorded cast
//! is not silently undone, CR 733.1).

use std::sync::Arc;

use engine::game::effects::paradigm::{arm_paradigm, enqueue_offer_if_any};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::{create_object, move_to_zone};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, EffectKind, TargetFilter, TargetRef, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CopyChoiceMode, GameState, PersistedGameState, PersistedRestoreError,
    PersistedRestoreFinalization, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const HEX: &str = "Destroy six target creatures.";
const TWINCAST: &str =
    "Copy target instant or sorcery spell. You may choose new targets for the copy.";

fn free_spell(s: &mut GameScenario, name: &str, text: &str, instant: bool) -> ObjectId {
    s.add_spell_to_hand_from_oracle(P0, name, instant, text)
        .with_mana_cost(ManaCost::zero())
        .id()
}

fn try_restore(value: serde_json::Value) -> Result<GameState, PersistedRestoreError> {
    serde_json::from_value::<PersistedGameState>(value)
        .expect("persisted state decodes")
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)?
        .finalize_after_rehydration(|_| Ok(()))
}

fn save(state: &GameState) -> serde_json::Value {
    serde_json::to_value(PersistedGameState::capture(state.clone())).unwrap()
}

/// The persisted `CopyRetarget` payload (the object holding `target_slots`).
fn walk_wire(value: &mut serde_json::Value) -> &mut serde_json::Map<String, serde_json::Value> {
    fn find(
        value: &mut serde_json::Value,
    ) -> Option<&mut serde_json::Map<String, serde_json::Value>> {
        match value {
            serde_json::Value::Object(map) => {
                if map.contains_key("target_slots") && map.contains_key("copy_id") {
                    return Some(map);
                }
                map.values_mut().find_map(find)
            }
            serde_json::Value::Array(values) => values.iter_mut().find_map(find),
            _ => None,
        }
    }
    find(value).expect("a parked CopyRetarget payload")
}

/// Rewrite a parked walk into the pre-mode wire: no `mode`, `picks` or
/// permissions; each slot's `current` and the cursor as the old handler wrote
/// them.
fn to_legacy_walk(
    value: &mut serde_json::Value,
    currents: &[Option<TargetRef>],
    current_slot: usize,
) {
    let walk = walk_wire(value);
    for key in ["mode", "picks", "can_keep_rest"] {
        walk.remove(key);
    }
    let slots = walk
        .get_mut("target_slots")
        .and_then(serde_json::Value::as_array_mut)
        .expect("slot array");
    assert_eq!(slots.len(), currents.len(), "reach: one current per slot");
    for (slot, current) in slots.iter_mut().zip(currents) {
        let slot = slot.as_object_mut().expect("slot object");
        slot.remove("address");
        slot.remove("can_keep");
        slot.insert("current".into(), serde_json::to_value(current).unwrap());
    }
    walk.insert("current_slot".into(), serde_json::json!(current_slot));
}

struct HexBoard {
    runner: GameRunner,
    copy_id: ObjectId,
    announced: Vec<TargetRef>,
    g: ObjectId,
    h: ObjectId,
}

/// Hex at X1..X6, copied by Twincast; G and H are also on the battlefield.
fn hex_board() -> HexBoard {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let xs: Vec<ObjectId> = (1..=6)
        .map(|i| s.add_creature(P1, &format!("X{i}"), 2, 2).id())
        .collect();
    let g = s.add_creature(P1, "G", 2, 2).id();
    let h = s.add_creature(P1, "H", 2, 2).id();
    let hex = free_spell(&mut s, "Hex", HEX, false);
    let twincast = free_spell(&mut s, "Twincast", TWINCAST, true);
    let mut runner = s.build();
    runner.cast(hex).target_objects(&xs).commit();
    runner.cast(twincast).target_object(hex).commit();
    for _ in 0..16 {
        match runner.state().waiting_for {
            WaitingFor::CopyRetarget { .. } => break,
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept");
            }
            _ => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
        }
    }
    let WaitingFor::CopyRetarget {
        copy_id,
        target_slots,
        mode,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("expected the copy walk");
    };
    assert_eq!(
        *mode,
        Some(CopyChoiceMode::Retarget),
        "reach: a retarget walk"
    );
    assert_eq!(target_slots.len(), 6, "reach: six addressed positions");
    let copy_id = *copy_id;
    HexBoard {
        runner,
        copy_id,
        announced: xs.iter().copied().map(TargetRef::Object).collect(),
        g,
        h,
    }
}

fn copy_targets(state: &GameState, copy_id: ObjectId) -> Vec<TargetRef> {
    state
        .stack
        .iter()
        .find(|entry| entry.id == copy_id)
        .and_then(|entry| entry.ability())
        .expect("the copy on the stack")
        .targets
        .clone()
}

/// The legacy Hex + Twincast save the old handler admitted: G chosen at
/// positions 0 and 1 (one instance of "target" naming G twice, CR 115.3), the
/// cursor at position 2.
fn legacy_hex_gg(board: &HexBoard) -> serde_json::Value {
    let g = TargetRef::Object(board.g);
    let mut currents: Vec<Option<TargetRef>> = board.announced.iter().cloned().map(Some).collect();
    currents[0] = Some(g.clone());
    currents[1] = Some(g);
    let mut wire = save(board.runner.state());
    to_legacy_walk(&mut wire, &currents, 2);
    wire
}

/// §3 recovery (CR 115.7d + CR 115.3 + CR 707.10c): the legacy `[G, G]` save
/// restores with the accepted first pick kept and the walk truncated at the
/// refused second pick, then completes normally.
#[test]
fn legacy_walk_restore_truncates_at_the_first_refused_pick_and_re_asks() {
    let board = hex_board();
    let (g, h) = (TargetRef::Object(board.g), TargetRef::Object(board.h));
    let restored = try_restore(legacy_hex_gg(&board)).expect("the legacy walk restores");
    let WaitingFor::CopyRetarget {
        target_slots,
        current_slot,
        mode,
        picks,
        can_keep_rest,
        ..
    } = restored.waiting_for.clone()
    else {
        panic!("the walk is still parked, got {:?}", restored.waiting_for);
    };
    // (1) The accepted first pick survives, and with it a confirmed witness:
    //     position 1 still has an answer.
    assert_eq!(mode, Some(CopyChoiceMode::Retarget), "mode inferred");
    // (2) The cursor and the picks stop at the refused position.
    assert_eq!(picks, Some(vec![Some(g.clone())]), "picks truncated to [G]");
    assert_eq!(current_slot, 1, "cursor at the refused position");
    // (3) The discarded suffix is reset to the original occurrences.
    let currents: Vec<Option<TargetRef>> = target_slots
        .iter()
        .map(|slot| slot.current.clone())
        .collect();
    assert_eq!(
        currents,
        board
            .announced
            .iter()
            .cloned()
            .map(Some)
            .collect::<Vec<_>>(),
        "every slot shows its announced target; the stale G at position 1 is gone"
    );
    // (4) Permissions are recomputed on the restored board.
    assert!(target_slots[1].can_keep, "keeping X2 completes");
    assert!(
        !target_slots[1].legal_alternatives.contains(&g),
        "G is not offered again at position 1 (CR 115.3)"
    );
    assert!(target_slots[1].legal_alternatives.contains(&h));
    assert!(can_keep_rest, "keeping the rest after [G] completes");
    assert!(
        target_slots.iter().all(|slot| slot.address.is_some()),
        "every slot is addressed"
    );
    assert_eq!(
        copy_targets(&restored, board.copy_id),
        board.announced,
        "nothing is written to the copy before finalization"
    );
    // (7) An immediate re-save and re-restore reproduces the same prompt.
    let again = try_restore(save(&restored)).expect("the restored walk re-restores");
    assert_eq!(
        serde_json::to_value(&again.waiting_for).unwrap(),
        serde_json::to_value(&restored.waiting_for).unwrap(),
        "re-save/re-restore is a fixed point"
    );
    // (5) A replacement at the refused position succeeds through the action
    //     path; (6) finalization happens exactly once.
    let mut runner = GameRunner::from_state(restored);
    assert!(
        GameRunner::from_state(runner.state().clone())
            .act(GameAction::ChooseTarget {
                target: Some(g.clone()),
            })
            .is_err(),
        "G at position 1 is still refused"
    );
    let mut events: Vec<GameEvent> = Vec::new();
    events.extend(
        runner
            .act(GameAction::ChooseTarget {
                target: Some(h.clone()),
            })
            .expect("H replaces the refused pick")
            .events,
    );
    events.extend(
        runner
            .act(GameAction::KeepAllCopyTargets)
            .expect("keeping the rest finalizes")
            .events,
    );
    let resolved = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::CopySpell,
                    ..
                }
            )
        })
        .count();
    assert_eq!(resolved, 1, "the walk finalizes exactly once");
    let mut expected = board.announced.clone();
    expected[0] = g;
    expected[1] = h;
    assert_eq!(copy_targets(runner.state(), board.copy_id), expected);
}

/// Malformed controls stay rejected: a cursor past the slots, and a decided
/// prefix with an unchosen slot inside it.
#[test]
fn malformed_legacy_walks_are_rejected() {
    let board = hex_board();
    let announced: Vec<Option<TargetRef>> = board.announced.iter().cloned().map(Some).collect();
    let mut past_end = save(board.runner.state());
    to_legacy_walk(&mut past_end, &announced, 9);
    assert!(matches!(
        try_restore(past_end),
        Err(PersistedRestoreError::InvalidCopyTargetWalk(_))
    ));
    let mut hole = announced.clone();
    hole[0] = None;
    let mut inconsistent = save(board.runner.state());
    to_legacy_walk(&mut inconsistent, &hole, 2);
    assert!(matches!(
        try_restore(inconsistent),
        Err(PersistedRestoreError::InvalidCopyTargetWalk(_))
    ));
    // Control: the untouched legacy shape restores.
    let mut control = save(board.runner.state());
    to_legacy_walk(&mut control, &announced, 0);
    assert!(
        try_restore(control).is_ok(),
        "control: a well-formed legacy walk"
    );
}

fn seed_destroy_paradigm_source(state: &mut GameState) -> ObjectId {
    let id = create_object(
        state,
        CardId(120),
        P0,
        "Paradigm Destroy".to_string(),
        Zone::Exile,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Sorcery);
    obj.base_card_types = obj.card_types.clone();
    obj.mana_cost = ManaCost::generic(2);
    Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Destroy {
            target: TargetFilter::Typed(TypedFilter::creature()),
            cant_regenerate: false,
        },
    ));
    id
}

/// A parked Paradigm copy announcing "destroy target creature" against V.
fn paradigm_announce_board() -> (GameRunner, ObjectId, ObjectId) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let v = s.add_creature(P1, "V", 2, 2).id();
    let mut runner = s.build();
    let source = {
        let state = runner.state_mut();
        let source = seed_destroy_paradigm_source(state);
        arm_paradigm(state, source, P0, "Paradigm Destroy");
        assert!(enqueue_offer_if_any(state, P0));
        source
    };
    runner
        .act(GameAction::CastParadigmCopy { source })
        .expect("the paradigm copy is cast");
    let WaitingFor::CopyRetarget {
        copy_id,
        mode,
        target_slots,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("expected the announcement walk");
    };
    assert_eq!(
        *mode,
        Some(CopyChoiceMode::Announce),
        "reach: an announcement"
    );
    assert_eq!(
        target_slots[0].legal_alternatives,
        vec![TargetRef::Object(v)],
        "reach: V is the only legal announcement"
    );
    let copy_id = *copy_id;
    (runner, copy_id, v)
}

/// §3, Main's ruling (a): an old-format Paradigm announcement whose restored
/// board has no legal announcement fails closed before the state is
/// published, with a non-resumable error. The copy's cast is already recorded
/// (CR 733.1), so it is neither resumed nor silently undone.
#[test]
fn legacy_announcement_with_no_feasible_target_fails_closed() {
    let (mut runner, copy_id, v) = paradigm_announce_board();
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), v, Zone::Graveyard, &mut events);
    let mut wire = save(runner.state());
    to_legacy_walk(&mut wire, &[None], 0);
    assert_eq!(
        try_restore(wire).map(|_| ()),
        Err(PersistedRestoreError::NonResumableCopyAnnouncement { copy_id }),
        "restore refuses the state"
    );
}

/// Control and mode-specific recovery assertions for an announcement: the
/// feasible legacy announcement restores as an Announce walk with nothing to
/// keep (CR 601.2c), keeping is refused, and the replacement is a chosen
/// target; finalization hands off once.
#[test]
fn legacy_announcement_restores_as_an_announce_walk() {
    let (runner, copy_id, v) = paradigm_announce_board();
    let mut wire = save(runner.state());
    to_legacy_walk(&mut wire, &[None], 0);
    let restored = try_restore(wire).expect("the feasible announcement restores");
    let WaitingFor::CopyRetarget {
        mode,
        picks,
        current_slot,
        target_slots,
        can_keep_rest,
        ..
    } = restored.waiting_for.clone()
    else {
        panic!("the announcement is parked");
    };
    assert_eq!(mode, Some(CopyChoiceMode::Announce), "mode inferred");
    assert_eq!(picks, Some(Vec::new()));
    assert_eq!(current_slot, 0);
    assert!(!target_slots[0].can_keep, "nothing to keep");
    assert!(!can_keep_rest);
    assert_eq!(
        target_slots[0].legal_alternatives,
        vec![TargetRef::Object(v)]
    );
    let mut runner = GameRunner::from_state(restored);
    for refused in [
        GameAction::ChooseTarget { target: None },
        GameAction::KeepAllCopyTargets,
    ] {
        assert!(
            GameRunner::from_state(runner.state().clone())
                .act(refused.clone())
                .is_err(),
            "{refused:?} is refused for an announcement"
        );
    }
    let events = runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(v)),
        })
        .expect("V is announced")
        .events;
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, GameEvent::EffectResolved { .. }))
            .count(),
        1,
        "the walk finalizes and hands off once"
    );
    assert_eq!(
        copy_targets(runner.state(), copy_id),
        vec![TargetRef::Object(v)]
    );
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::CopyRetarget { .. }),
        "no dangling copy walk"
    );
}

/// The positional retarget wire (protocol 108): `null` keeps a position, a
/// bare target chooses it, so a pre-positional bare vector still decodes (as
/// every position chosen) and no tagged pick type is on the wire.
#[test]
fn retarget_spell_wire_is_null_for_keep_and_bare_for_choose() {
    let chosen = TargetRef::Object(ObjectId(7));
    let action = GameAction::RetargetSpell {
        new_targets: vec![Some(chosen.clone()), None],
    };
    let wire = serde_json::to_value(&action).unwrap();
    let picks = wire
        .pointer("/data/new_targets")
        .expect("new_targets on the wire");
    assert_eq!(
        *picks,
        serde_json::json!([serde_json::to_value(&chosen).unwrap(), null])
    );
    assert_eq!(serde_json::from_value::<GameAction>(wire).unwrap(), action);
    let legacy = serde_json::json!({
        "type": "RetargetSpell",
        "data": { "new_targets": [serde_json::to_value(&chosen).unwrap()] },
    });
    assert_eq!(
        serde_json::from_value::<GameAction>(legacy).unwrap(),
        GameAction::RetargetSpell {
            new_targets: vec![Some(chosen)],
        }
    );
}
