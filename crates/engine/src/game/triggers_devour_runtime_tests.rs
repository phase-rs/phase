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
use crate::types::ability::{EffectKind, PtValue, TargetFilter};
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
        keywords: vec![Keyword::Devour(n)],
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
