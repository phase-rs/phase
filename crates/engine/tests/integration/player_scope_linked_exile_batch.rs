//! Regression for exact linked-exile batches across interactive player-scope fan-out.

use engine::game::effects::resolve_ability_chain;
use engine::game::engine::apply;
use engine::game::zones::create_object;
use engine::types::ability::{
    CardPlayMode, CastFromZoneDriver, ControllerRef, Effect, EffectKind, FilterProp,
    LibraryPosition, PlayerFilter, QuantityExpr, ResolutionCastWindow, ResolvedAbility,
    SubAbilityLink, TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::{
    ActionResult, CastOfferKind, ExileLinkKind, GameState, PersistedGameState, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::resolution::FrameKind;
use engine::types::zones::{EtbTapState, Zone};
use serde_json::Value;

fn graveyard_creature(
    state: &mut GameState,
    card_id: u64,
    owner: PlayerId,
    name: &str,
) -> ObjectId {
    let id = create_object(
        state,
        CardId(card_id),
        owner,
        name.to_string(),
        Zone::Graveyard,
    );
    let object = state.objects.get_mut(&id).expect("created object exists");
    object.card_types.core_types.push(CoreType::Creature);
    object.base_card_types = object.card_types.clone();
    id
}

fn choose_exile(state: &mut GameState, player: PlayerId, card: ObjectId) -> ActionResult {
    match &state.waiting_for {
        WaitingFor::EffectZoneChoice {
            player: prompted,
            cards,
            destination: Some(Zone::Exile),
            track_exiled_by_source,
            ..
        } => {
            assert_eq!(*prompted, player);
            assert!(cards.contains(&card));
            assert!(*track_exiled_by_source);
        }
        other => panic!("expected exile choice for {player:?}, got {other:?}"),
    }
    apply(state, player, GameAction::SelectCards { cards: vec![card] })
        .expect("player-scope exile choice resolves")
}

fn cast_window(source: ObjectId) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::CastFromZone {
            target: TargetFilter::ExiledBySource,
            without_paying_mana_cost: true,
            mode: CardPlayMode::Cast,
            cast_transformed: false,
            alt_ability_cost: None,
            constraint: None,
            duration: None,
            driver: CastFromZoneDriver::ResolutionWindow {
                bounds: ResolutionCastWindow::UNBOUNDED,
            },
            mana_spend_permission: None,
            additional_cost: None,
            cast_cost_modifier: None,
        },
        vec![],
        source,
        PlayerId(0),
    )
}

fn scoped_graveyard_exile(source: ObjectId, tail: ResolvedAbility) -> ResolvedAbility {
    let mut exile = ResolvedAbility::new(
        Effect::ChangeZone {
            origin: Some(Zone::Graveyard),
            destination: Zone::Exile,
            target: TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Card],
                controller: Some(ControllerRef::You),
                properties: vec![FilterProp::InZone {
                    zone: Zone::Graveyard,
                }],
            }),
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: EtbTapState::Unspecified,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
            enters_modified_if: None,
        },
        vec![],
        source,
        PlayerId(0),
    );
    exile.player_scope = Some(PlayerFilter::All);
    exile.sub_ability = Some(Box::new(tail));
    exile
}

/// CR 608.2f: each paused player contributes an independent exact batch. The
/// final resolution window must see their union, including the intermediate
/// resumed seat whose producer is separated by another producer barrier.
#[test]
fn player_scope_pauses_accumulate_exact_linked_batch_for_final_cast_window() {
    let mut state = GameState::new(FormatConfig::standard(), 3, 42);
    let source = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Player-scope source".to_string(),
        Zone::Battlefield,
    );
    let p0_pick = graveyard_creature(&mut state, 10, PlayerId(0), "P0 pick");
    let p0_other = graveyard_creature(&mut state, 11, PlayerId(0), "P0 other");
    let p1_pick = graveyard_creature(&mut state, 20, PlayerId(1), "P1 pick");
    let p1_other = graveyard_creature(&mut state, 21, PlayerId(1), "P1 other");
    let p2_pick = graveyard_creature(&mut state, 30, PlayerId(2), "P2 pick");
    let p2_other = graveyard_creature(&mut state, 31, PlayerId(2), "P2 other");

    let exile = scoped_graveyard_exile(source, cast_window(source));

    resolve_ability_chain(&mut state, &exile, &mut Vec::new(), 0)
        .expect("player-scope chain starts");
    choose_exile(&mut state, PlayerId(0), p0_pick);
    state = serde_json::from_value(serde_json::to_value(&state).expect("pause serializes"))
        .expect("pause restores");
    choose_exile(&mut state, PlayerId(1), p1_pick);
    let prior_count_before_final_continuation = state.last_effect_count;
    let final_result = choose_exile(&mut state, PlayerId(2), p2_pick);

    assert_eq!(
        final_result
            .events
            .iter()
            .filter(|event| matches!(event, GameEvent::ZoneChanged { object_id, .. } if *object_id == p2_pick))
            .count(),
        1,
        "the final seat must publish its real exile exactly once"
    );
    assert!(
        !final_result.events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::NoOp,
                ..
            }
        )),
        "synthetic queue terminator must not emit an event: {:?}",
        final_result.events
    );
    assert_eq!(
        state.last_effect_count,
        prior_count_before_final_continuation
    );

    let WaitingFor::CastOffer {
        player: PlayerId(0),
        kind: CastOfferKind::FreeCastWindow { candidates, .. },
    } = &state.waiting_for
    else {
        panic!(
            "expected final free-cast window, got {:?}",
            state.waiting_for
        );
    };
    let mut actual = candidates.clone();
    actual.sort_by_key(|id| id.0);
    let mut expected = vec![p0_pick, p1_pick, p2_pick];
    expected.sort_by_key(|id| id.0);
    assert_eq!(
        actual, expected,
        "window must contain the exact three-seat union"
    );
    for picked in expected {
        assert_eq!(state.objects[&picked].zone, Zone::Exile);
        assert!(state.exile_links.iter().any(|link| {
            link.exiled_id == picked
                && link.source_id == source
                && link.kind == ExileLinkKind::TrackedBySource
        }));
    }
    for unpicked in [p0_other, p1_other, p2_other] {
        assert!(!candidates.contains(&unpicked));
        assert_eq!(state.objects[&unpicked].zone, Zone::Graveyard);
    }
}

/// An ordinary scoped `SequentialSibling` is not queue provenance. Even when
/// it is itself an exile producer, the prior player-scope batch must stop at
/// that producer barrier; only its own exact batch reaches its cast window.
#[test]
fn ordinary_scoped_sequential_exile_producer_remains_a_batch_barrier() {
    let mut state = GameState::new(FormatConfig::standard(), 3, 43);
    let source = create_object(
        &mut state,
        CardId(100),
        PlayerId(0),
        "Barrier source".to_string(),
        Zone::Battlefield,
    );
    let p0_pick = graveyard_creature(&mut state, 110, PlayerId(0), "P0 prior batch");
    let _p0_other = graveyard_creature(&mut state, 111, PlayerId(0), "P0 other");
    let p1_pick = graveyard_creature(&mut state, 120, PlayerId(1), "P1 prior batch");
    let _p1_other = graveyard_creature(&mut state, 121, PlayerId(1), "P1 other");
    let p2_pick = graveyard_creature(&mut state, 130, PlayerId(2), "P2 prior batch");
    let _p2_other = graveyard_creature(&mut state, 131, PlayerId(2), "P2 other");
    let barrier_hit = graveyard_creature(&mut state, 140, PlayerId(0), "Barrier hit");
    engine::game::zones::move_to_zone(&mut state, barrier_hit, Zone::Library, &mut vec![]);

    let mut barrier = ResolvedAbility::new(
        Effect::ExileTop {
            player: TargetFilter::Controller,
            count: QuantityExpr::Fixed { value: 1 },
            position: LibraryPosition::Top,
            face_down: false,
            actor: engine::types::ability::LibraryInstructionActor::Controller,
        },
        vec![],
        source,
        PlayerId(0),
    );
    barrier.scoped_player = Some(PlayerId(0));
    barrier.sub_link = SubAbilityLink::SequentialSibling;
    barrier.sub_ability = Some(Box::new(cast_window(source)));
    let exile = scoped_graveyard_exile(source, barrier);

    resolve_ability_chain(&mut state, &exile, &mut Vec::new(), 0).expect("fan-out starts");
    choose_exile(&mut state, PlayerId(0), p0_pick);
    choose_exile(&mut state, PlayerId(1), p1_pick);
    choose_exile(&mut state, PlayerId(2), p2_pick);

    let WaitingFor::CastOffer {
        kind: CastOfferKind::FreeCastWindow { candidates, .. },
        ..
    } = &state.waiting_for
    else {
        panic!("expected barrier cast window, got {:?}", state.waiting_for);
    };
    assert_eq!(candidates, &vec![barrier_hit]);
    assert!([p0_pick, p1_pick, p2_pick]
        .iter()
        .all(|prior| !candidates.contains(prior)));
}

fn final_window(state: &GameState) -> Option<Vec<ObjectId>> {
    match &state.waiting_for {
        WaitingFor::CastOffer {
            kind: CastOfferKind::FreeCastWindow { candidates, .. },
            ..
        } => {
            let mut window = candidates.clone();
            window.sort_by_key(|id| id.0);
            Some(window)
        }
        _ => None,
    }
}

/// CR 104.3a + CR 800.4a: a seat that concedes before it is prompted leaves the
/// game; the survivors' linked exiles still reach the window.
#[test]
fn player_scope_linked_exile_concession_keeps_survivors_window() {
    let mut state = GameState::new(FormatConfig::standard(), 3, 42);
    let source = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Player-scope source".to_string(),
        Zone::Battlefield,
    );
    let mut picks = Vec::new();
    for (seat, player) in [PlayerId(0), PlayerId(1), PlayerId(2)].iter().enumerate() {
        let base = 10 * (seat as u64 + 1);
        picks.push(graveyard_creature(&mut state, base, *player, "pick"));
        graveyard_creature(&mut state, base + 1, *player, "other");
    }
    let exile = scoped_graveyard_exile(source, cast_window(source));
    resolve_ability_chain(&mut state, &exile, &mut Vec::new(), 0).expect("the clause starts");
    choose_exile(&mut state, PlayerId(0), picks[0]);
    apply(
        &mut state,
        PlayerId(2),
        GameAction::Concede {
            player_id: PlayerId(2),
        },
    )
    .expect("P2 concedes while P1's prompt stands");
    choose_exile(&mut state, PlayerId(1), picks[1]);
    assert_eq!(
        final_window(&state),
        Some(vec![picks[0], picks[1]]),
        "the window holds exactly the survivors' exiles, never the leaver's card"
    );
}

// ── Legacy saves carrying the retired linked-exile sidecar (decision 7) ──────
//
// The fixtures were written at the phase base (resolution wire 5) by the
// retired sidecar's own build: a three-seat scoped graveyard exile whose tail is
// a linked free-cast window, paused at
// * `legs` — after P0's answer, P1 prompted (the legs carrier),
// * `queue_end` — P2 the only prompted seat (the queue-end carrier),
// * `auto_tail` — after P0's answer, P1 prompted, P2 automatic after restore,
// and Itazura, Lingering Wick paused on its number choice (a tail that reads the
// tracked set, which the retired form never recorded).

const LEGACY_LEGS_RAW: &str = include_str!("fixtures/p1_9565_linked_exile_legs_raw_v5.json");
const LEGACY_LEGS_TRUSTED: &str =
    include_str!("fixtures/p1_9565_linked_exile_legs_trusted_v5.json");
const LEGACY_QUEUE_END_RAW: &str =
    include_str!("fixtures/p1_9565_linked_exile_queue_end_raw_v5.json");
const LEGACY_QUEUE_END_TRUSTED: &str =
    include_str!("fixtures/p1_9565_linked_exile_queue_end_trusted_v5.json");
const LEGACY_AUTO_TAIL_RAW: &str =
    include_str!("fixtures/p1_9565_linked_exile_auto_tail_raw_v5.json");
const LEGACY_AUTO_TAIL_TRUSTED: &str =
    include_str!("fixtures/p1_9565_linked_exile_auto_tail_trusted_v5.json");
const LEGACY_ITAZURA_RAW: &str = include_str!("fixtures/p1_9565_itazura_raw_v5.json");
const LEGACY_ITAZURA_TRUSTED: &str = include_str!("fixtures/p1_9565_itazura_trusted_v5.json");

/// The resolution-wire versions that could carry the retired sidecar.
const LEGACY_VERSIONS: [u64; 4] = [5, 4, 3, 2];

#[derive(Clone, Copy, Debug)]
enum Form {
    Raw,
    Trusted,
}

impl Form {
    fn prefix(self) -> &'static str {
        match self {
            Form::Raw => "",
            Form::Trusted => "/state",
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Shape {
    Legs,
    QueueEnd,
    AutoTail,
}

fn legacy_fixture(shape: Shape, form: Form) -> &'static str {
    match (shape, form) {
        (Shape::Legs, Form::Raw) => LEGACY_LEGS_RAW,
        (Shape::Legs, Form::Trusted) => LEGACY_LEGS_TRUSTED,
        (Shape::QueueEnd, Form::Raw) => LEGACY_QUEUE_END_RAW,
        (Shape::QueueEnd, Form::Trusted) => LEGACY_QUEUE_END_TRUSTED,
        (Shape::AutoTail, Form::Raw) => LEGACY_AUTO_TAIL_RAW,
        (Shape::AutoTail, Form::Trusted) => LEGACY_AUTO_TAIL_TRUSTED,
    }
}

const SHAPES: [Shape; 3] = [Shape::Legs, Shape::QueueEnd, Shape::AutoTail];
const FORMS: [Form; 2] = [Form::Raw, Form::Trusted];

fn relabel(text: &str, form: Form, version: u64) -> Value {
    let mut value: Value = serde_json::from_str(text).expect("fixture parses");
    let pointer = format!("{}/resolution_state_version", form.prefix());
    *value
        .pointer_mut(&pointer)
        .expect("the fixture declares its version") = Value::from(version);
    value
}

fn restore(value: Value) -> Result<GameState, String> {
    serde_json::from_value::<PersistedGameState>(value)
        .map_err(|error| error.to_string())?
        .into_game_state()
        .map_err(|error| format!("{error:?}"))
}

fn save(state: &GameState, form: Form) -> Value {
    match form {
        Form::Raw => serde_json::to_value(PersistedGameState::Raw(Box::new(state.clone()))),
        Form::Trusted => serde_json::to_value(PersistedGameState::capture(state.clone())),
    }
    .expect("the current build saves")
}

fn count_key(value: &Value, key: &str) -> usize {
    match value {
        Value::Object(map) => map
            .iter()
            .map(|(name, child)| usize::from(name == key) + count_key(child, key))
            .sum(),
        Value::Array(items) => items.iter().map(|child| count_key(child, key)).sum(),
        _ => 0,
    }
}

/// The source and each seat's "pick" card, read from the restored objects.
fn legacy_ids(state: &GameState) -> (ObjectId, [ObjectId; 3]) {
    let mut source = None;
    let mut picks = [None; 3];
    for (id, object) in &state.objects {
        match object.name.as_str() {
            "Player-scope source" => source = Some(*id),
            "pick" => picks[object.owner.0 as usize] = Some(*id),
            _ => {}
        }
    }
    (
        source.expect("the fixture holds the source"),
        picks.map(|pick| pick.expect("every seat holds a pick")),
    )
}

fn owners(state: &GameState) -> usize {
    state
        .resolution_stack
        .iter()
        .filter(|frame| frame.kind() == FrameKind::PlayerScopeClause)
        .count()
}

/// Answers the prompts the shape still owes after a restore. `Legs` and
/// `AutoTail` owe P1 (and `Legs` P2); `QueueEnd` owes P2.
fn finish_legacy_clause(state: &mut GameState, shape: Shape, picks: [ObjectId; 3]) {
    let seats: &[usize] = match shape {
        Shape::Legs => &[1, 2],
        Shape::QueueEnd => &[2],
        Shape::AutoTail => &[1],
    };
    for &seat in seats {
        if matches!(state.waiting_for, WaitingFor::EffectZoneChoice { .. }) {
            choose_exile(state, PlayerId(seat as u8), picks[seat]);
        }
    }
}

/// CR 608.2f + CR 607.2a, decision 7: every legacy carrier at every version that
/// could hold it, in both persisted forms, converts to the `PlayerScopeClause`
/// owner and reaches the same window the unsaved run reaches: every seat's pick.
#[test]
fn legacy_player_scope_sidecar_save_migrates_to_owner() {
    for shape in SHAPES {
        for form in FORMS {
            let text = legacy_fixture(shape, form);
            assert_eq!(
                count_key(
                    &serde_json::from_str(text).expect("parses"),
                    "player_scope_linked_exile"
                ),
                1,
                "reach guard: the {shape:?} {form:?} fixture carries one sidecar"
            );
            for version in LEGACY_VERSIONS {
                let label = format!("{shape:?} {form:?} v{version}");
                let mut state = restore(relabel(text, form, version))
                    .unwrap_or_else(|error| panic!("{label} restores: {error}"));
                assert_eq!(owners(&state), 1, "{label}: the restore holds one owner");
                let resaved = serde_json::to_value(&state).expect("serializes");
                assert_eq!(count_key(&resaved, "MigratedLinkedExile"), 1, "{label}");
                assert_eq!(
                    count_key(&resaved, "player_scope_linked_exile"),
                    0,
                    "{label}"
                );
                let (source, picks) = legacy_ids(&state);
                finish_legacy_clause(&mut state, shape, picks);
                assert_eq!(
                    final_window(&state),
                    Some(picks.to_vec()),
                    "{label}: the window holds every seat's pick"
                );
                if matches!(shape, Shape::AutoTail) {
                    assert!(
                        state.exile_links.iter().any(|link| {
                            link.exiled_id == picks[2]
                                && link.source_id == source
                                && link.kind == ExileLinkKind::TrackedBySource
                        }),
                        "{label}: the post-restore automatic seat's exile is linked to the source"
                    );
                }
            }
            let refused = restore(relabel(text, form, 6))
                .expect_err("the current version never carries the sidecar");
            assert!(
                refused.contains("retired player-scope linked-exile sidecar"),
                "{shape:?} {form:?} v6: {refused}"
            );
        }
    }
}

/// Decision 7, sub-shape (i): Itazura's tail reads the tracked set the retired
/// form never recorded, so the save is refused with a diagnostic that names it.
#[test]
fn legacy_itazura_mid_clause_save_is_refused() {
    for (form, text) in [
        (Form::Raw, LEGACY_ITAZURA_RAW),
        (Form::Trusted, LEGACY_ITAZURA_TRUSTED),
    ] {
        assert_eq!(
            count_key(
                &serde_json::from_str(text).expect("parses"),
                "player_scope_linked_exile"
            ),
            1,
            "reach guard: the {form:?} Itazura save carries the sidecar"
        );
        for version in LEGACY_VERSIONS {
            let refused = restore(relabel(text, form, version))
                .expect_err("an unresumable legacy clause is refused");
            assert!(
                refused.contains("player-scope clause of Itazura, Lingering Wick")
                    && refused.contains("restart the game from a current save"),
                "{form:?} v{version}: {refused}"
            );
        }
    }
}

/// Decision 7 + E1.14(c): the converter admits a migrated clause only with
/// exile-destination producer evidence. The legs fixture with its leg head and
/// its standing prompt rewritten to the graveyard is refused; unmodified, it
/// migrates.
#[test]
fn legacy_non_exile_producer_save_is_refused() {
    for form in FORMS {
        let text = legacy_fixture(Shape::Legs, form);
        let prefix = form.prefix();
        let mut value: Value = serde_json::from_str(text).expect("parses");
        let leg_destination =
            format!("{prefix}/resolution_frames/frames/0/data/pending/chain/effect/destination");
        assert_eq!(
            value.pointer(&leg_destination),
            Some(&Value::from("Exile")),
            "reach guard: the leg head is an exile producer"
        );
        *value
            .pointer_mut(&leg_destination)
            .expect("leg destination") = Value::from("Graveyard");
        *value
            .pointer_mut(&format!("{prefix}/waiting_for/data/destination"))
            .expect("the standing prompt names a destination") = Value::from("Graveyard");
        let refused = restore(value).expect_err("non-exile producer evidence is refused");
        assert!(
            refused.contains("player-scope clause of Player-scope source"),
            "{form:?}: {refused}"
        );
        assert!(
            restore(relabel(text, form, 5)).is_ok(),
            "{form:?}: the unmodified fixture migrates"
        );
    }
}

/// Decision 7, sub-shape (ii): a save holding two carriers is refused.
#[test]
fn legacy_two_carrier_payload_is_refused() {
    for form in FORMS {
        let mut value: Value =
            serde_json::from_str(legacy_fixture(Shape::Legs, form)).expect("parses");
        let frames = value
            .pointer_mut(&format!("{}/resolution_frames/frames", form.prefix()))
            .and_then(Value::as_array_mut)
            .expect("the fixture holds typed frames");
        let carrier = frames[0].clone();
        frames.insert(0, carrier);
        assert_eq!(
            count_key(&value, "player_scope_linked_exile"),
            2,
            "reach guard: the payload holds two carriers"
        );
        let refused = restore(value).expect_err("two carriers are refused");
        assert!(
            refused.contains("paused inside a player-scope clause"),
            "{form:?}: {refused}"
        );
    }
}

/// Decision 3: a migrated owner re-saved at the current version keeps its seed
/// unchanged and carries no sidecar; restoring that save holds exactly one owner
/// and still reaches every seat's pick.
#[test]
fn legacy_restore_then_current_save_round_trips_the_seed() {
    for shape in SHAPES {
        for form in FORMS {
            let text = legacy_fixture(shape, form);
            let legacy_batch = {
                let value: Value = serde_json::from_str(text).expect("parses");
                let pointer = format!(
                    "{}/resolution_frames/frames/0/data/pending/player_scope_linked_exile/batch",
                    form.prefix()
                );
                value
                    .pointer(&pointer)
                    .cloned()
                    .unwrap_or(Value::Array(Vec::new()))
            };
            for version in LEGACY_VERSIONS {
                let label = format!("{shape:?} {form:?} v{version}");
                let mut state = restore(relabel(text, form, version))
                    .unwrap_or_else(|error| panic!("{label} restores: {error}"));
                let (_, picks) = legacy_ids(&state);
                // Advance to a later prompt where the shape still owes one.
                if matches!(shape, Shape::Legs) {
                    choose_exile(&mut state, PlayerId(1), picks[1]);
                }
                let saved = save(&state, form);
                assert_eq!(count_key(&saved, "player_scope_linked_exile"), 0, "{label}");
                assert_eq!(
                    saved
                        .pointer(&format!("{}/resolution_state_version", form.prefix()))
                        .and_then(Value::as_u64),
                    Some(6),
                    "{label}: the re-save is at the current version"
                );
                let seeds: Vec<&Value> = saved
                    .pointer(&format!("{}/resolution_frames/frames", form.prefix()))
                    .and_then(Value::as_array)
                    .expect("typed frames")
                    .iter()
                    .filter_map(|frame| {
                        frame.pointer("/data/publication/MigratedLinkedExile/batch")
                    })
                    .collect();
                assert_eq!(seeds, vec![&legacy_batch], "{label}: the seed is unchanged");
                let mut again = restore(saved)
                    .unwrap_or_else(|error| panic!("{label} re-save restores: {error}"));
                assert_eq!(owners(&again), 1, "{label}: exactly one owner");
                let remaining = match shape {
                    Shape::Legs => Shape::QueueEnd,
                    other => other,
                };
                finish_legacy_clause(&mut again, remaining, picks);
                assert_eq!(final_window(&again), Some(picks.to_vec()), "{label}");
            }
        }
    }
}
