//! CR 702.82a + CR 614.1c + CR 614.12a runtime integration: a
//! Devour-bearing creature's Hand→Battlefield ZoneChange routes through
//! the synthesized `Moved` replacement, whose `Effect::Sacrifice` execute
//! is non-modifier work — the pipeline stashes it as a
//! `PostReplacementContinuation` and drains it after the move completes,
//! raising a ranged sacrifice `EffectZoneChoice`. The Sacrifice
//! completion stamps `state.last_effect_count`, which the chained
//! `PutCounter` sub-ability's `QuantityRef::EventContextAmount` reads via
//! its `.or(last_effect_count)` fallback.
//!
//! Lives in `game/triggers.rs` rather than `database/synthesis.rs::tests`
//! so it can reach the `pub(super)` post-replacement-continuation drain
//! API (`apply_pending_post_replacement_effect`) — the same call
//! `stack.rs:575` makes during normal spell resolution.

use crate::database::synthesis::synthesize_all;
use crate::game::printed_cards::apply_card_face_to_object;
use crate::game::zones::{create_object, move_to_zone};
use crate::types::ability::{EffectKind, PtValue, TargetFilter, TypeFilter};
use crate::types::actions::GameAction;
use crate::types::card::CardFace;
use crate::types::card_type::CoreType;
use crate::types::counter::CounterType;
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
/// under v2's `EventContextAmount` it reads `last_effect_count = 2`.
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
/// linkage bug (both `PreviousEffectAmount` and `EventContextAmount`
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
/// `EventContextAmount`.
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
