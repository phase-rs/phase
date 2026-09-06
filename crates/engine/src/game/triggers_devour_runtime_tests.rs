//! CR 702.82a + CR 614.1c + CR 614.12a runtime integration: a
//! Devour-bearing creature's Hand→Battlefield ZoneChange routes through
//! the synthesized `Moved` replacement, whose `Effect::Sacrifice` execute
//! is non-modifier work — the pipeline stashes it as a
//! `PostReplacementContinuation` and drains it after the move completes,
//! raising a ranged sacrifice `EffectZoneChoice`. The Sacrifice
//! completion stamps `state.last_effect_count`, which the chained
//! `PutCounter` sub-ability reads directly through
//! `QuantityRef::PreviousEffectCount`.
//!
//! Lives in `game/triggers.rs` rather than `database/synthesis.rs::tests`
//! so it can reach the `pub(super)` post-replacement-continuation drain
//! API (`apply_pending_post_replacement_effect`) — the same call
//! `stack.rs:575` makes during normal spell resolution.

use crate::database::synthesis::synthesize_all;
use crate::game::printed_cards::apply_card_face_to_object;
use crate::game::zones::{create_object, move_to_zone};
use crate::types::ability::{
    EffectKind, PtValue, QuantityExpr, QuantityModification, QuantityRef, ReplacementDefinition,
    TargetFilter, TypeFilter,
};
use crate::types::actions::GameAction;
use crate::types::card::CardFace;
use crate::types::card_type::CoreType;
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::keywords::Keyword;
use crate::types::player::PlayerId;
use crate::types::replacements::ReplacementEvent;
use crate::types::zones::Zone;

/// Build a creature face carrying `Keyword::Devour(n)` and run the full
/// synthesis pipeline. `CardFace::default()` leaves the mana cost zero
/// and no other abilities so the runtime test exercises only Devour.
fn devour_face(name: &str, n: u32) -> CardFace {
    let mut face = CardFace {
        name: name.to_string(),
        power: Some(PtValue::Fixed(3)),
        toughness: Some(PtValue::Fixed(3)),
        keywords: vec![Keyword::Devour {
            n,
            quality: TypeFilter::Creature,
        }],
        ..CardFace::default()
    };
    face.card_type.core_types.push(CoreType::Creature);
    synthesize_all(&mut face);
    face
}

/// Build a creature face carrying `Keyword::Devour { n, quality }` (CR 702.82c)
/// and run the full synthesis pipeline.
fn devour_face_q(name: &str, n: u32, quality: TypeFilter) -> CardFace {
    let mut face = CardFace {
        name: name.to_string(),
        power: Some(PtValue::Fixed(3)),
        toughness: Some(PtValue::Fixed(3)),
        keywords: vec![Keyword::Devour { n, quality }],
        ..CardFace::default()
    };
    face.card_type.core_types.push(CoreType::Creature);
    synthesize_all(&mut face);
    face
}

fn setup_state_with_priority(controller: PlayerId) -> GameState {
    let mut state = GameState::new_two_player(42);
    state.turn_number = 2;
    state.phase = crate::types::phase::Phase::PreCombatMain;
    state.active_player = controller;
    state.priority_player = controller;
    state.waiting_for = WaitingFor::Priority { player: controller };
    state
}

/// Place a plain vanilla 2/2 creature on the battlefield under `controller`.
fn battlefield_creature(state: &mut GameState, controller: PlayerId, name: &str) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let id = create_object(
        state,
        card_id,
        controller,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.base_card_types = obj.card_types.clone();
    obj.power = Some(2);
    obj.toughness = Some(2);
    obj.base_power = Some(2);
    obj.base_toughness = Some(2);
    id
}

/// Place a basic Forest (a Land) on the battlefield under `controller`.
fn battlefield_land(state: &mut GameState, controller: PlayerId, name: &str) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let id = create_object(
        state,
        card_id,
        controller,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Land);
    obj.card_types.subtypes.push("Forest".to_string());
    obj.base_card_types = obj.card_types.clone();
    id
}

/// Place an artifact (optionally carrying `subtypes`, e.g. "Food") on the
/// battlefield under `controller`.
fn battlefield_artifact(
    state: &mut GameState,
    controller: PlayerId,
    name: &str,
    subtypes: &[&str],
) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let id = create_object(
        state,
        card_id,
        controller,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Artifact);
    for s in subtypes {
        obj.card_types.subtypes.push((*s).to_string());
    }
    obj.base_card_types = obj.card_types.clone();
    id
}

/// Drive a Devour creature's Hand→Battlefield ZoneChange through the replacement
/// pipeline after `setup` populates the battlefield, then drain the
/// post-replacement continuation. Mirrors `drive_devour_etb_to_sacrifice_choice`
/// but hands the caller full control of the battlefield (mixed land/creature
/// pools) so the quality axis (CR 702.82c) is observable.
fn drive_devour_etb_with_battlefield(
    face: &CardFace,
    controller: PlayerId,
    setup: impl FnOnce(&mut GameState),
) -> (GameState, ObjectId) {
    assert!(
        face.replacements
            .iter()
            .any(|r| matches!(r.event, ReplacementEvent::Moved)
                && matches!(r.valid_card, Some(TargetFilter::SelfRef))),
        "test fixture must carry a synthesized Devour ETB replacement; got {:?}",
        face.replacements
    );

    let mut state = setup_state_with_priority(controller);
    setup(&mut state);

    let next_card = CardId(state.next_object_id);
    let obj_id = create_object(
        &mut state,
        next_card,
        controller,
        face.name.clone(),
        Zone::Hand,
    );
    {
        let obj = state.objects.get_mut(&obj_id).unwrap();
        apply_card_face_to_object(obj, face);
    }

    let proposed = crate::types::proposed_event::ProposedEvent::zone_change(
        obj_id,
        Zone::Hand,
        Zone::Battlefield,
        None,
    );
    let mut events = Vec::new();
    let result = crate::game::replacement::replace_event(&mut state, proposed, &mut events);
    let crate::game::replacement::ReplacementResult::Execute(event) = result else {
        panic!("Devour ETB pipeline must return Execute, got {result:?}");
    };
    let crate::types::proposed_event::ProposedEvent::ZoneChange { object_id, to, .. } = event
    else {
        panic!("pipeline must yield a ZoneChange execute event");
    };
    move_to_zone(&mut state, object_id, to, &mut events);

    assert!(
        state.has_post_replacement_drain(),
        "Devour's non-modifier execute (Effect::Sacrifice) must be stashed as a \
         post-replacement continuation"
    );
    state.clear_post_replacement_source();
    let _ = crate::game::engine_replacement::apply_pending_post_replacement_effect(
        &mut state,
        Some(obj_id),
        None,
        Some(ReplacementEvent::Moved),
        &mut events,
    );

    (state, obj_id)
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

/// Install the AddCounter quantity replacement used by Doubling Season-class
/// effects without depending on a particular card's parser output.
fn install_counter_doubler(state: &mut GameState, controller: PlayerId) {
    let card_id = CardId(state.next_object_id);
    let id = create_object(
        state,
        card_id,
        controller,
        "Counter Doubler".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&id)
        .expect("counter doubler exists")
        .replacement_definitions
        .push(
            ReplacementDefinition::new(ReplacementEvent::AddCounter)
                .quantity_modification(QuantityModification::DOUBLE),
        );
}

/// Drive a Devour creature's Hand→Battlefield ZoneChange through the
/// replacement pipeline, then drain the post-replacement continuation —
/// the same call `stack.rs:575` makes during real spell resolution.
/// Returns the parked state on the Sacrifice `EffectZoneChoice`.
///
/// `fodder` plain vanilla creatures are pre-placed under `controller` so
/// they form the eligible sacrifice pool.
fn drive_devour_etb_to_sacrifice_choice(
    face: &CardFace,
    controller: PlayerId,
    fodder: usize,
) -> (GameState, ObjectId) {
    // Sanity-check the synthesizer wired a Devour replacement onto the
    // face — a misfire would otherwise surface as a generic "prompt
    // never fired" downstream.
    assert!(
        face.replacements
            .iter()
            .any(|r| matches!(r.event, ReplacementEvent::Moved)
                && matches!(r.valid_card, Some(TargetFilter::SelfRef))),
        "test fixture must carry a synthesized Devour ETB replacement; \
             got replacements={:?}",
        face.replacements
    );

    let mut state = setup_state_with_priority(controller);
    for i in 0..fodder {
        battlefield_creature(&mut state, controller, &format!("Sac Fodder {i}"));
    }
    let next_card = CardId(state.next_object_id);
    let obj_id = create_object(
        &mut state,
        next_card,
        controller,
        face.name.clone(),
        Zone::Hand,
    );
    {
        let obj = state.objects.get_mut(&obj_id).unwrap();
        apply_card_face_to_object(obj, face);
    }

    let proposed = crate::types::proposed_event::ProposedEvent::zone_change(
        obj_id,
        Zone::Hand,
        Zone::Battlefield,
        None,
    );
    let mut events = Vec::new();
    let result = crate::game::replacement::replace_event(&mut state, proposed, &mut events);
    let crate::game::replacement::ReplacementResult::Execute(event) = result else {
        panic!("Devour ETB pipeline must return Execute, got {result:?}");
    };
    let crate::types::proposed_event::ProposedEvent::ZoneChange { object_id, to, .. } = event
    else {
        panic!("pipeline must yield a ZoneChange execute event");
    };
    move_to_zone(&mut state, object_id, to, &mut events);

    assert!(
        state.has_post_replacement_drain(),
        "Devour's non-modifier execute (Effect::Sacrifice) must be \
             stashed as a post-replacement continuation by the pipeline"
    );
    state.clear_post_replacement_source();
    let _ = crate::game::engine_replacement::apply_pending_post_replacement_effect(
        &mut state,
        Some(obj_id),
        None,
        Some(ReplacementEvent::Moved),
        &mut events,
    );

    (state, obj_id)
}

/// CR 702.82a + CR 614.12a: a Devour creature's ETB raises a ranged
/// sacrifice prompt over the controller's creatures. With Devour
/// unwired (before this fix) NO prompt fires — this assertion is the
/// observable "as-enters sacrifice prompt never fires" bug from #532.
#[test]
fn devour_etb_raises_ranged_sacrifice_prompt() {
    let face = devour_face("Gorger Wurm", 1);
    let (state, _devour) = drive_devour_etb_to_sacrifice_choice(&face, PlayerId(0), 2);

    match &state.waiting_for {
        WaitingFor::EffectZoneChoice {
            player,
            min_count,
            up_to,
            effect_kind,
            ..
        } => {
            assert_eq!(
                *player,
                PlayerId(0),
                "the sacrifice choice is the controller's"
            );
            assert_eq!(*min_count, 0, "CR 702.82a: an empty sacrifice is legal");
            assert!(
                *up_to,
                "Devour offers a ranged 'sacrifice any number' choice"
            );
            assert_eq!(
                *effect_kind,
                EffectKind::Sacrifice,
                "the Devour prompt is a Sacrifice choice"
            );
        }
        other => panic!("expected an EffectZoneChoice, got {other:?}"),
    }
}

/// PRIMARY DISCRIMINATOR for the counter-count linkage bug. Sacrificing
/// two creatures to Devour 1 places exactly two +1/+1 counters on the
/// entering permanent. Under v1's `PreviousEffectAmount` route this would
/// resolve to 0 (the ranged Sacrifice never stamps `last_effect_amount`);
/// under the direct `PreviousEffectCount` route it reads `last_effect_count = 2`.
#[test]
fn devour_1_full_sacrifice_places_one_counter_per_creature() {
    let face = devour_face("Gorger Wurm", 1);
    let (mut state, devour) = drive_devour_etb_to_sacrifice_choice(&face, PlayerId(0), 2);

    let WaitingFor::EffectZoneChoice { cards, .. } = &state.waiting_for else {
        panic!("expected the Devour sacrifice choice");
    };
    assert!(
        cards.len() >= 2,
        "two pre-placed creatures must be eligible Devour sacrifices, got {cards:?}"
    );
    let to_sacrifice: Vec<ObjectId> = cards.iter().copied().take(2).collect();

    crate::game::engine::apply_as_current(
        &mut state,
        GameAction::SelectCards {
            cards: to_sacrifice.clone(),
        },
    )
    .unwrap();

    assert_eq!(
        state.objects.get(&devour).unwrap().zone,
        Zone::Battlefield,
        "the Devour creature must end up on the battlefield"
    );
    assert_eq!(
        p1p1(&state, devour),
        2,
        "Devour 1 + two creatures sacrificed → 2 +1/+1 counters (CR 702.82a)"
    );
    for sac in &to_sacrifice {
        assert_eq!(
            state.objects.get(sac).unwrap().zone,
            Zone::Graveyard,
            "each sacrificed creature must be in the graveyard"
        );
    }
}

/// CR 702.82a: an empty sacrifice is legal — the Devour creature enters
/// with 0 counters. NOTE: this case alone does NOT discriminate the v1
/// linkage bug (both `PreviousEffectAmount` and `PreviousEffectCount`
/// resolve to 0 here). It is paired with the full-sacrifice test above —
/// that test is the true linkage-bug discriminator.
#[test]
fn devour_1_empty_sacrifice_enters_with_zero_counters() {
    let face = devour_face("Gorger Wurm", 1);
    let (mut state, devour) = drive_devour_etb_to_sacrifice_choice(&face, PlayerId(0), 2);

    crate::game::engine::apply_as_current(&mut state, GameAction::SelectCards { cards: vec![] })
        .unwrap();

    assert_eq!(
        state.objects.get(&devour).unwrap().zone,
        Zone::Battlefield,
        "the Devour creature still enters when nothing is sacrificed"
    );
    assert_eq!(
        p1p1(&state, devour),
        0,
        "an empty Devour sacrifice places 0 counters (CR 702.82a)"
    );
    assert!(
        !matches!(state.waiting_for, WaitingFor::EffectZoneChoice { .. }),
        "no further sacrifice prompt should remain after the empty choice"
    );
}

/// CR 702.82a: Devour 2 places N=2 counters per creature sacrificed.
/// One sacrifice → 2 counters, via the synthesizer's
/// `QuantityExpr::Multiply { factor: 2, .. }` wrapping
/// `PreviousEffectCount`.
#[test]
fn devour_2_one_sacrifice_places_two_counters() {
    let face = devour_face("Mycoloth", 2);
    let (mut state, devour) = drive_devour_etb_to_sacrifice_choice(&face, PlayerId(0), 2);

    let WaitingFor::EffectZoneChoice { cards, .. } = &state.waiting_for else {
        panic!("expected the Devour sacrifice choice");
    };
    let one = vec![*cards.first().expect("at least one eligible creature")];

    crate::game::engine::apply_as_current(&mut state, GameAction::SelectCards { cards: one })
        .unwrap();

    assert_eq!(
        p1p1(&state, devour),
        2,
        "Devour 2 + one creature sacrificed → 2 +1/+1 counters (N per sacrifice)"
    );
}

/// CR 702.82a + CR 614.16: Devour's continuation count is distinct
/// from a concurrently live enclosing event amount. Two sacrifices for Mycoloth
/// (Devour 2) make four counters, then the AddCounter doubler makes eight.
#[test]
fn devour_uses_previous_effect_count_not_outer_event_amount() {
    let face = devour_face("Mycoloth", 2);
    let (mut state, devour) = drive_devour_etb_with_battlefield(&face, PlayerId(0), |state| {
        battlefield_creature(state, PlayerId(0), "Sac Fodder 0");
        battlefield_creature(state, PlayerId(0), "Sac Fodder 1");
        install_counter_doubler(state, PlayerId(0));
    });
    state.current_trigger_event = Some(GameEvent::DamageDealt {
        source_id: ObjectId(999),
        target: crate::types::ability::TargetRef::Player(PlayerId(1)),
        amount: 1,
        is_combat: false,
        excess: 0,
    });
    let event_amount = QuantityExpr::Ref {
        qty: QuantityRef::EventContextAmount,
    };
    assert_eq!(
        crate::game::quantity::resolve_quantity(&state, &event_amount, PlayerId(0), devour),
        1,
        "reach-guard: the generic event-context ref still sees the outer scalar event"
    );

    let WaitingFor::EffectZoneChoice { cards, .. } = &state.waiting_for else {
        panic!("expected the Devour sacrifice choice");
    };
    let chosen: Vec<ObjectId> = cards.iter().copied().take(2).collect();
    assert_eq!(chosen.len(), 2, "two creatures must be eligible for Devour");
    crate::game::engine::apply_as_current(&mut state, GameAction::SelectCards { cards: chosen })
        .unwrap();

    assert_eq!(
        p1p1(&state, devour),
        8,
        "2 sacrifices × Devour 2 × doubler = 8"
    );
    let previous_count = QuantityExpr::Ref {
        qty: QuantityRef::PreviousEffectCount,
    };
    assert_eq!(
        crate::game::quantity::resolve_quantity(&state, &previous_count, PlayerId(0), devour),
        2,
        "the direct continuation-local count remains the two selected creatures"
    );
    assert_eq!(
        crate::game::quantity::resolve_quantity(&state, &event_amount, PlayerId(0), devour),
        1,
        "reach-guard: EventContextAmount must retain its outer-event precedence"
    );

    let (mut empty, empty_devour) =
        drive_devour_etb_with_battlefield(&face, PlayerId(0), |state| {
            battlefield_creature(state, PlayerId(0), "Declined Fodder 0");
            battlefield_creature(state, PlayerId(0), "Declined Fodder 1");
            install_counter_doubler(state, PlayerId(0));
        });
    empty.current_trigger_event = Some(GameEvent::DamageDealt {
        source_id: ObjectId(999),
        target: crate::types::ability::TargetRef::Player(PlayerId(1)),
        amount: 1,
        is_combat: false,
        excess: 0,
    });
    crate::game::engine::apply_as_current(&mut empty, GameAction::SelectCards { cards: vec![] })
        .unwrap();

    assert_eq!(
        p1p1(&empty, empty_devour),
        0,
        "an empty Devour choice stays zero despite the outer event"
    );
    assert_eq!(
        crate::game::quantity::resolve_quantity(&empty, &previous_count, PlayerId(0), empty_devour),
        0,
        "the direct continuation-local count records the empty selection"
    );
    assert_eq!(
        crate::game::quantity::resolve_quantity(&empty, &event_amount, PlayerId(0), empty_devour),
        1,
        "reach-guard: the generic event-context ref still sees the outer scalar event"
    );
}

/// P (PRIMARY, the reported bug — Famished Worldsire "Devour land 3", CR 702.82c):
/// the ETB sacrifice pool is the controller's LANDS; a co-present creature is
/// EXCLUDED. Sacrificing 2 lands to Devour 3 places 3×2 = 6 +1/+1 counters.
///
/// Revert-sensitive: if the quality drops to the CR 702.82a creature default, the
/// pool would offer the creature (not the lands) and this test fails on the pool
/// membership assertions.
#[test]
fn devour_land_3_sacrifices_lands_not_creatures() {
    let face = devour_face_q("Famished Worldsire", 3, TypeFilter::Land);

    let mut land_ids = Vec::new();
    let mut creature_id = ObjectId(0);
    let (mut state, devour) = drive_devour_etb_with_battlefield(&face, PlayerId(0), |state| {
        land_ids.push(battlefield_land(state, PlayerId(0), "Forest 1"));
        land_ids.push(battlefield_land(state, PlayerId(0), "Forest 2"));
        creature_id = battlefield_creature(state, PlayerId(0), "Bystander Bear");
    });

    let WaitingFor::EffectZoneChoice {
        cards, effect_kind, ..
    } = &state.waiting_for
    else {
        panic!(
            "expected the Devour land sacrifice choice, got {:?}",
            state.waiting_for
        );
    };
    assert_eq!(*effect_kind, EffectKind::Sacrifice);
    for land in &land_ids {
        assert!(
            cards.contains(land),
            "CR 702.82c: each controlled land must be an eligible Devour-land sacrifice; pool={cards:?}"
        );
    }
    assert!(
        !cards.contains(&creature_id),
        "CR 702.82c: a creature must NOT be offered to a Devour-land sacrifice; pool={cards:?}"
    );

    crate::game::engine::apply_as_current(
        &mut state,
        GameAction::SelectCards {
            cards: land_ids.clone(),
        },
    )
    .unwrap();

    assert_eq!(
        state.objects.get(&devour).unwrap().zone,
        Zone::Battlefield,
        "the Devour-land creature enters the battlefield"
    );
    assert_eq!(
        p1p1(&state, devour),
        6,
        "Devour 3 + two lands sacrificed → 3×2 = 6 +1/+1 counters (CR 702.82c counter math)"
    );
    assert_eq!(
        state.objects.get(&creature_id).unwrap().zone,
        Zone::Battlefield,
        "the bystander creature was never eligible and survives"
    );
}

/// C (CONTROL, CR 702.82a default preserved): a plain "Devour 2" creature offers
/// its CREATURES and excludes lands. One sacrifice → 2 counters. Proves the
/// creature default survives the parameterization.
#[test]
fn devour_creature_default_excludes_lands() {
    let face = devour_face_q("Mycoloth", 2, TypeFilter::Creature);

    let mut creature_ids = Vec::new();
    let mut land_id = ObjectId(0);
    let (mut state, devour) = drive_devour_etb_with_battlefield(&face, PlayerId(0), |state| {
        creature_ids.push(battlefield_creature(state, PlayerId(0), "Fodder A"));
        creature_ids.push(battlefield_creature(state, PlayerId(0), "Fodder B"));
        land_id = battlefield_land(state, PlayerId(0), "Idle Forest");
    });

    let WaitingFor::EffectZoneChoice { cards, .. } = &state.waiting_for else {
        panic!(
            "expected the Devour creature sacrifice choice, got {:?}",
            state.waiting_for
        );
    };
    for creature in &creature_ids {
        assert!(
            cards.contains(creature),
            "creatures are eligible; pool={cards:?}"
        );
    }
    assert!(
        !cards.contains(&land_id),
        "CR 702.82a: a land must NOT be offered to a plain Devour sacrifice; pool={cards:?}"
    );

    let one = vec![creature_ids[0]];
    crate::game::engine::apply_as_current(&mut state, GameAction::SelectCards { cards: one })
        .unwrap();
    assert_eq!(
        p1p1(&state, devour),
        2,
        "Devour 2 + one creature → 2 counters (creature default intact)"
    );
}

/// B (BOUNDARY, CR 702.82a "may sacrifice"): Devour land 3 with ZERO controlled
/// lands (only creatures present) → no eligible land, so the creature still
/// enters with 0 counters and no land is consumed.
#[test]
fn devour_land_3_with_no_lands_enters_with_zero_counters() {
    let face = devour_face_q("Famished Worldsire", 3, TypeFilter::Land);

    let mut creature_id = ObjectId(0);
    let (mut state, devour) = drive_devour_etb_with_battlefield(&face, PlayerId(0), |state| {
        creature_id = battlefield_creature(state, PlayerId(0), "Non-Land Bear");
    });

    // With an empty eligible land pool and min_count 0, a ranged sacrifice may
    // either auto-resolve or surface an empty prompt; either way no creature is
    // offered and the empty choice is declined.
    if let WaitingFor::EffectZoneChoice { cards, .. } = &state.waiting_for {
        assert!(
            !cards.contains(&creature_id),
            "CR 702.82c: a creature is never a legal Devour-land sacrifice; pool={cards:?}"
        );
        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::SelectCards { cards: vec![] },
        )
        .unwrap();
    }

    assert_eq!(
        state.objects.get(&devour).unwrap().zone,
        Zone::Battlefield,
        "CR 702.82a: the creature still enters when no land can be sacrificed"
    );
    assert_eq!(
        p1p1(&state, devour),
        0,
        "no land sacrificed → 0 +1/+1 counters"
    );
    assert_eq!(
        state.objects.get(&creature_id).unwrap().zone,
        Zone::Battlefield,
        "the bystander creature is untouched"
    );
}

/// A (subtype class — Caprichrome "Devour artifact 1", CR 702.82c): the pool is
/// the controller's ARTIFACTS; a creature is excluded. One artifact sacrificed →
/// 1 counter.
#[test]
fn devour_artifact_1_sacrifices_artifacts_not_creatures() {
    let face = devour_face_q("Caprichrome", 1, TypeFilter::Artifact);

    let mut artifact_id = ObjectId(0);
    let mut creature_id = ObjectId(0);
    let (mut state, devour) = drive_devour_etb_with_battlefield(&face, PlayerId(0), |state| {
        artifact_id = battlefield_artifact(state, PlayerId(0), "Trinket", &[]);
        creature_id = battlefield_creature(state, PlayerId(0), "Bystander Bear");
    });

    let WaitingFor::EffectZoneChoice { cards, .. } = &state.waiting_for else {
        panic!(
            "expected the Devour artifact sacrifice choice, got {:?}",
            state.waiting_for
        );
    };
    assert!(
        cards.contains(&artifact_id),
        "artifacts are eligible; pool={cards:?}"
    );
    assert!(
        !cards.contains(&creature_id),
        "CR 702.82c: a creature must NOT be offered to a Devour-artifact sacrifice; pool={cards:?}"
    );

    crate::game::engine::apply_as_current(
        &mut state,
        GameAction::SelectCards {
            cards: vec![artifact_id],
        },
    )
    .unwrap();
    assert_eq!(
        p1p1(&state, devour),
        1,
        "Devour artifact 1 + one artifact → 1 +1/+1 counter"
    );
}

/// A (subtype class — Feasting Hobbit "Devour Food 3", CR 702.82c + CR 205.3g):
/// the `Subtype("Food")` quality narrows the pool to FOOD artifacts only — a
/// plain (non-Food) artifact AND a creature are both excluded. Proves the
/// runtime `subtypes.contains("Food")` path (filter.rs) matches the canonical
/// subtype the parser emits. One Food sacrificed → 3 counters.
#[test]
fn devour_food_3_sacrifices_only_food_subtype() {
    let face = devour_face_q(
        "Feasting Hobbit",
        3,
        TypeFilter::Subtype("Food".to_string()),
    );

    let mut food_id = ObjectId(0);
    let mut plain_artifact_id = ObjectId(0);
    let mut creature_id = ObjectId(0);
    let (mut state, devour) = drive_devour_etb_with_battlefield(&face, PlayerId(0), |state| {
        food_id = battlefield_artifact(state, PlayerId(0), "Food Token", &["Food"]);
        plain_artifact_id = battlefield_artifact(state, PlayerId(0), "Trinket", &[]);
        creature_id = battlefield_creature(state, PlayerId(0), "Bystander Bear");
    });

    let WaitingFor::EffectZoneChoice { cards, .. } = &state.waiting_for else {
        panic!(
            "expected the Devour Food sacrifice choice, got {:?}",
            state.waiting_for
        );
    };
    assert!(
        cards.contains(&food_id),
        "the Food token is eligible; pool={cards:?}"
    );
    assert!(
        !cards.contains(&plain_artifact_id),
        "CR 205.3g: a non-Food artifact is NOT a Food; pool={cards:?}"
    );
    assert!(
        !cards.contains(&creature_id),
        "a creature is NOT a Food; pool={cards:?}"
    );

    crate::game::engine::apply_as_current(
        &mut state,
        GameAction::SelectCards {
            cards: vec![food_id],
        },
    )
    .unwrap();
    assert_eq!(
        p1p1(&state, devour),
        3,
        "Devour Food 3 + one Food → 3 +1/+1 counters"
    );
}

// ---------------------------------------------------------------------------
// Post-replacement drain-strand rows. The dispatcher these exercise is shared by
// every as-enters replacement continuation, not by Devour alone; Devour is the
// cheapest production driver for it that needs no card data.
// ---------------------------------------------------------------------------

/// Place a permanent under `controller` carrying the triggered abilities parsed
/// from `oracle_text`, and index them so they can fire. Verbatim Oracle text is
/// used rather than a hand-built `TriggerDefinition` so the fixture cannot take a
/// different parser branch than the real card.
fn battlefield_trigger_observer(
    state: &mut GameState,
    controller: PlayerId,
    name: &str,
    oracle_text: &str,
) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let id = create_object(
        state,
        card_id,
        controller,
        name.to_string(),
        Zone::Battlefield,
    );
    let parsed = crate::parser::oracle::parse_oracle_text(
        oracle_text,
        name,
        &[],
        &["Enchantment".to_string()],
        &[],
    );
    assert!(
        !parsed.triggers.is_empty(),
        "the observer fixture must parse at least one trigger from {oracle_text:?}"
    );
    let mut face = CardFace {
        name: name.to_string(),
        triggers: parsed.triggers,
        ..CardFace::default()
    };
    face.card_type.core_types.push(CoreType::Enchantment);
    {
        let obj = state.objects.get_mut(&id).unwrap();
        apply_card_face_to_object(obj, &face);
    }
    crate::game::trigger_index::reindex_object_triggers(state, id);
    id
}

/// True when any `PostReplacement` frame in the stack still holds a
/// `Dispatching` drain. Read back through the stack's own `Serialize` impl
/// because `PostReplacementDrainStack` exposes only its resident, and a strand
/// can sit below a nested entry.
fn has_dispatching_drain(state: &GameState) -> bool {
    let value =
        serde_json::to_value(&state.resolution_stack).expect("the resolution stack serializes");
    value["frames"].as_array().is_some_and(|frames| {
        frames.iter().any(|frame| {
            frame["type"] == "PostReplacement"
                && frame["data"]["drains"].as_array().is_some_and(|drains| {
                    drains.iter().any(|drain| drain["status"] == "Dispatching")
                })
        })
    })
}

/// The per-`PostReplacement`-frame drain statuses of the runtime state, read
/// back through the stack's own `Serialize` impl. `PostReplacementDrainStack`
/// exposes only its resident, so this is how a test observes WHICH status a
/// parked entry carries — and a multi-entry strand — without widening
/// production API for a test's convenience. A verbatim port of the shipped
/// helper of the same name in
/// `crates/engine/tests/integration/mycoloth_devour_drain_strand.rs`.
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

/// **B2-devour — guard (relabelled).** The Devour producer path leaves no
/// `Dispatching` drain behind.
///
/// This row DISCRIMINATES NOTHING and must not be read as evidence for the
/// producer analysis. It was measured on a tree with U1 and U2 both absent and
/// reported no strand at all, so it is green on `main` and must stay green under
/// every discrimination patch in the plan's §5.8. It is kept as an honest
/// regression witness for the reporter's own card class — Devour is what the
/// reporter played — not as a red-capable row.
///
/// The discriminating producer row is
/// `b2_zurs_weirding_replacement_leaves_no_dispatching_drain` in
/// `tests/integration/mycoloth_devour_drain_strand.rs`, built on the Zur's
/// Weirding draw-replacement path, which was MEASURED to strand at BASE_SHA.
///
/// CR 702.82a + CR 614.12a + CR 603.3b. Positive reach-guards, so no negative
/// here can be satisfied vacuously: the prompt was actually raised, the counters
/// landed (`p1p1 == 2 * n_sacrificed`), and both sacrificed creatures reached the
/// graveyard so their observers really did fire.
#[test]
fn b2_devour_producer_path_leaves_no_dispatching_drain() {
    let face = devour_face("Mycoloth", 2);
    let mut fodder = Vec::new();
    let (mut state, devour) = drive_devour_etb_with_battlefield(&face, PlayerId(0), |state| {
        fodder.push(battlefield_creature(state, PlayerId(0), "Sac Fodder 0"));
        fodder.push(battlefield_creature(state, PlayerId(0), "Sac Fodder 1"));
        battlefield_trigger_observer(
            state,
            PlayerId(0),
            "Bastion of Remembrance",
            "Whenever a creature you control dies, each opponent loses 1 life and you gain 1 life.",
        );
        battlefield_trigger_observer(
            state,
            PlayerId(0),
            "Sacrifice Ledger",
            "Whenever you sacrifice a creature, draw a card.",
        );
    });

    let WaitingFor::EffectZoneChoice { cards, .. } = &state.waiting_for else {
        panic!(
            "reach-guard: the Devour sacrifice prompt must be raised, got {:?}",
            state.waiting_for
        );
    };
    let chosen: Vec<ObjectId> = fodder
        .iter()
        .copied()
        .filter(|id| cards.contains(id))
        .collect();
    assert_eq!(chosen.len(), 2, "both fodder creatures are eligible");

    crate::game::engine::apply_as_current(
        &mut state,
        GameAction::SelectCards {
            cards: chosen.clone(),
        },
    )
    .unwrap();

    // Positive reach-guards: the path really resolved.
    assert_eq!(
        p1p1(&state, devour),
        4,
        "Devour 2 x 2 sacrifices → 4 +1/+1 counters (CR 702.82a)"
    );
    for id in &chosen {
        assert_eq!(
            state.objects.get(id).unwrap().zone,
            Zone::Graveyard,
            "each sacrificed creature reached the graveyard, so its observers fired"
        );
    }

    assert!(
        !has_dispatching_drain(&state),
        "no post-replacement drain may survive its own dispatch as Dispatching"
    );
}

/// **H1 — guard.** *Hostile: the empty/decline path.* CR 702.82a: a Devour
/// entry with ZERO creatures sacrificed still installs a continuation and still
/// retires it, leaving no resolution frame behind.
///
/// Positive reach-guard: the prompt was genuinely raised before the empty
/// submission, so `resolution_stack.is_empty()` cannot pass because nothing ever
/// installed a drain.
///
/// **H7 — guard, the non-interference witness for the ENTRY call site.** The
/// submission below routes `apply_as_current -> apply_as_current_with_mode
/// -> apply_action_boundary_for_semantic_owner
/// -> apply_action_boundary_with_stack_limit -> apply_action_boundary_core`,
/// so the entry-side sweep added at that function runs on THIS state — a
/// healthy one, parked mid-prompt with a live owner.
/// This row is therefore also the row that ESTABLISHES the premise §5.4's
/// safety rests on: parked-awaiting-input work carries `Paused`, never
/// `Dispatching` (CR 614.12a — the as-enters choice is still being made, so the
/// drain is live rules work). The pre-submit `post_replacement_drain_statuses`
/// assertion is the witness; the post-submit assertions are the survival proof.
#[test]
fn h1_devour_empty_sacrifice_retires_its_drain_and_frame() {
    let face = devour_face("Gorger Wurm", 1);
    let (mut state, devour) = drive_devour_etb_to_sacrifice_choice(&face, PlayerId(0), 2);

    assert!(
        matches!(state.waiting_for, WaitingFor::EffectZoneChoice { .. }),
        "reach-guard: a drain was installed and its prompt raised, got {:?}",
        state.waiting_for
    );

    // NON-INTERFERENCE WITNESS for the entry-side sweep added at
    // `engine::apply_action_boundary_core`. The submission below routes
    // `apply_as_current -> apply_as_current_with_mode
    //  -> apply_action_boundary_for_semantic_owner
    //  -> apply_action_boundary_with_stack_limit -> apply_action_boundary_core`,
    // so the entry sweep runs on THIS state — a healthy one, parked mid-prompt
    // with a live owner. CR 614.12a: the as-enters choice is still being made,
    // so the drain is live parked work and MUST survive the boundary. Its
    // resident is `Paused`, never `Dispatching`, which is exactly the status the
    // sweep's exhaustive match refuses to pop — and it is what makes the
    // "Dispatching + no live dispatch => ownerless" predicate sound. A sweep
    // written with a wildcard arm, or scoped to `!Paused`, destroys it here and
    // reds the completion assertions below.
    assert_eq!(
        post_replacement_drain_statuses(&state),
        vec![vec!["Paused".to_string()]],
        "reach-guard: exactly one parked post-replacement drain, and it must be live parked \
         work (`Paused`) so the entry sweep provably runs over a healthy resident"
    );

    crate::game::engine::apply_as_current(&mut state, GameAction::SelectCards { cards: vec![] })
        .unwrap();

    assert_eq!(
        p1p1(&state, devour),
        0,
        "an empty Devour sacrifice places 0 counters (CR 702.82a)"
    );
    assert!(
        !has_dispatching_drain(&state),
        "the declined continuation must not leave a Dispatching drain"
    );
    assert!(
        state.resolution_stack.is_empty(),
        "the decline branch leaves no resolution frame behind, got {:?}",
        state.resolution_stack.len()
    );
}

/// **H4 — guard.** *Hostile: source/controller change.* CR 603.3b: a dies
/// observer controlled by a DIFFERENT player than the devourer's controller
/// still reaches the stack when the devoured creatures die.
///
/// Positive reach-guard: the observer's controller is asserted to differ from
/// the devourer's controller, and the sacrificed creatures are asserted to have
/// reached the graveyard — so "the trigger fired" is not satisfiable by an
/// observer that never saw a death.
#[test]
fn h4_foreign_controller_dies_observer_still_reaches_the_stack() {
    let face = devour_face("Mycoloth", 2);
    let mut fodder = Vec::new();
    let mut observer = ObjectId(0);
    let (mut state, devour) = drive_devour_etb_with_battlefield(&face, PlayerId(0), |state| {
        fodder.push(battlefield_creature(state, PlayerId(0), "Sac Fodder 0"));
        fodder.push(battlefield_creature(state, PlayerId(0), "Sac Fodder 1"));
        observer = battlefield_trigger_observer(
            state,
            PlayerId(1),
            "Foreign Mourner",
            "Whenever a creature dies, you gain 1 life.",
        );
    });

    assert_ne!(
        state.objects[&observer].controller, state.objects[&devour].controller,
        "reach-guard: the observer must be controlled by the OTHER player"
    );

    let WaitingFor::EffectZoneChoice { cards, .. } = &state.waiting_for else {
        panic!(
            "reach-guard: the Devour sacrifice prompt must be raised, got {:?}",
            state.waiting_for
        );
    };
    let chosen: Vec<ObjectId> = fodder
        .iter()
        .copied()
        .filter(|id| cards.contains(id))
        .collect();
    assert_eq!(chosen.len(), 2, "both fodder creatures are eligible");

    crate::game::engine::apply_as_current(
        &mut state,
        GameAction::SelectCards {
            cards: chosen.clone(),
        },
    )
    .unwrap();

    for id in &chosen {
        assert_eq!(
            state.objects.get(id).unwrap().zone,
            Zone::Graveyard,
            "reach-guard: the observer really did see a creature die"
        );
    }

    // Pin WHICH destination, not merely that one of them holds. A disjunction over
    // {stack, deferred, pending order} is satisfied by a trigger that fired and then
    // parked forever — the exact failure this change repairs — so it cannot witness
    // CR 603.3b for a guard row whose claim is arrival. `assert_eq!` on the observed
    // destination reports the actual one when it moves.
    let destination = if state.stack.iter().any(|entry| entry.source_id == observer) {
        "stack"
    } else if state
        .deferred_triggers
        .iter()
        .any(|deferred| deferred.pending.source_id == observer)
    {
        "deferred"
    } else if state.pending_trigger_order.as_ref().is_some_and(|order| {
        order.groups.iter().any(|group| {
            group
                .triggers
                .iter()
                .any(|pending| pending.pending.source_id == observer)
        })
    }) {
        "pending_order"
    } else {
        "nowhere"
    };
    assert_eq!(
        destination, "stack",
        "CR 603.3b: the foreign-controller dies trigger must reach the stack"
    );
}
