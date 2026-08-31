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
    ActionResult, CastOfferKind, ExileLinkKind, GameState, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::{EtbTapState, Zone};

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
