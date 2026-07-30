//! Tests for Blitz (CR 702.152). Declared from `game/mod.rs` so `blitz.rs`
//! stays implementation-only.

use super::blitz::install_blitz_riders;
use crate::game::keywords::has_haste;
use crate::game::layers::evaluate_layers;
use crate::game::stack::resolve_top;
use crate::game::triggers::{check_delayed_triggers, process_triggers};
use crate::game::zones::{create_object, move_to_zone};
use crate::types::ability::{DelayedTriggerCondition, Effect, TargetFilter};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;

/// Put a creature on the battlefield under player 0 and install the Blitz riders
/// on it (as the stack resolution path does for a blitz-cast spell).
fn blitz_creature_on_battlefield(state: &mut GameState) -> ObjectId {
    let id = create_object(
        state,
        CardId(1),
        PlayerId(0),
        "Blitz Beater".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&id).unwrap();
        obj.power = Some(2);
        obj.base_power = Some(2);
        obj.toughness = Some(2);
        obj.base_toughness = Some(2);
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types.core_types.push(CoreType::Creature);
    }
    install_blitz_riders(state, id, PlayerId(0));
    id
}

/// Stock the player's library so a draw resolves rather than triggering a
/// draw-from-empty loss.
fn stock_library(state: &mut GameState, owner: PlayerId, n: usize) {
    for i in 0..n {
        let card = create_object(
            state,
            CardId(100 + i as u64),
            owner,
            format!("Library Card {i}"),
            Zone::Library,
        );
        let _ = card;
    }
}

/// CR 702.152a: the blitz permanent gains haste.
#[test]
fn install_grants_haste() {
    let mut state = GameState::new_two_player(42);
    let id = blitz_creature_on_battlefield(&mut state);
    evaluate_layers(&mut state);
    assert!(
        has_haste(&state.objects[&id]),
        "a blitz-cast creature must have haste"
    );
}

/// CR 702.152a: the blitz permanent has "When this is put into a graveyard from
/// the battlefield, draw a card" — granted into its trigger set.
#[test]
fn install_grants_dies_draw_trigger() {
    let mut state = GameState::new_two_player(42);
    let id = blitz_creature_on_battlefield(&mut state);
    let obj = &state.objects[&id];
    let has_dies_draw = obj.trigger_definitions.iter_all().any(|t| {
        matches!(t.definition.mode, TriggerMode::ChangesZone)
            && t.definition.origin == Some(Zone::Battlefield)
            && t.definition.destination == Some(Zone::Graveyard)
            && matches!(t.definition.valid_card, Some(TargetFilter::SelfRef))
            && matches!(
                t.definition.execute.as_deref().map(|a| &*a.effect),
                Some(Effect::Draw { .. })
            )
    });
    assert!(has_dies_draw, "dies-draw trigger must be granted");
}

/// CR 702.152a: the blitz permanent is scheduled to be sacrificed at the
/// beginning of the next end step (one-shot delayed trigger).
#[test]
fn install_schedules_next_end_step_sacrifice() {
    let mut state = GameState::new_two_player(42);
    let id = blitz_creature_on_battlefield(&mut state);
    let dt = state
        .delayed_triggers
        .iter()
        .find(|d| d.source_id == id)
        .expect("a delayed sacrifice trigger must be scheduled");
    assert_eq!(
        dt.condition,
        DelayedTriggerCondition::AtNextPhase { phase: Phase::End }
    );
    assert!(dt.one_shot, "the sacrifice fires once");
    assert!(
        matches!(
            &dt.ability.effect,
            Effect::Sacrifice {
                target: TargetFilter::SelfRef,
                ..
            }
        ),
        "the delayed effect sacrifices this permanent"
    );
}

/// CR 702.152a: at the next end step the delayed trigger sacrifices the
/// permanent (it ends up in its owner's graveyard).
#[test]
fn end_step_sacrifice_resolves() {
    let mut state = GameState::new_two_player(42);
    state.active_player = PlayerId(0);
    let id = blitz_creature_on_battlefield(&mut state);

    state.phase = Phase::End;
    let stacked =
        check_delayed_triggers(&mut state, &[GameEvent::PhaseChanged { phase: Phase::End }]);
    assert!(!stacked.is_empty(), "the end-step sacrifice must fire");
    resolve_top(&mut state, &mut Vec::new());

    assert_eq!(state.objects[&id].zone, Zone::Graveyard);
    assert!(!state.battlefield.contains(&id));
}

/// CR 702.152a + CR 514.2: a creature blitzed DURING the end step is sacrificed
/// at the beginning of the NEXT end step, not the current one. In real play the
/// current end step's `PhaseChanged{End}` is dispatched before the blitz spell
/// resolves, so by the time the delayed trigger exists that event has already
/// passed; it then fires only on the next fresh `End` phase. This models that
/// timeline: the trigger ignores intervening (non-End) phase changes and fires
/// on the next `End` event. (Contrast a naive test that re-injects `End`
/// immediately after install — `AtNextPhase{End}` fires on *any* `End` event, so
/// that would assert the wrong thing.)
#[test]
fn blitz_during_end_step_sacrifices_at_the_next_end_step() {
    let mut state = GameState::new_two_player(42);
    state.active_player = PlayerId(0);
    // The end step is already underway (its `PhaseChanged{End}` has fired) when
    // the blitz creature resolves and installs its riders.
    state.phase = Phase::End;
    let id = blitz_creature_on_battlefield(&mut state);

    // The rest of the turn passes (e.g. an intervening upkeep). A non-End phase
    // change must NOT fire the end-step sacrifice; it stays pending.
    let stacked = check_delayed_triggers(
        &mut state,
        &[GameEvent::PhaseChanged {
            phase: Phase::Upkeep,
        }],
    );
    assert!(
        stacked.is_empty(),
        "the sacrifice must not fire during the end step it was created in / on a non-End phase"
    );
    assert!(
        state.delayed_triggers.iter().any(|dt| dt.source_id == id),
        "the delayed sacrifice stays pending until the next end step"
    );

    // The next end step begins (a fresh `End` event): now the sacrifice fires.
    let stacked_next =
        check_delayed_triggers(&mut state, &[GameEvent::PhaseChanged { phase: Phase::End }]);
    assert!(
        !stacked_next.is_empty(),
        "the sacrifice fires at the beginning of the next end step (CR 514.2)"
    );
}

/// CR 702.152a: when the blitz permanent dies, its controller draws a card.
#[test]
fn dies_draw_trigger_draws_on_death() {
    let mut state = GameState::new_two_player(42);
    state.active_player = PlayerId(0);
    stock_library(&mut state, PlayerId(0), 3);
    let id = blitz_creature_on_battlefield(&mut state);

    let hand_before = state.players[0].hand.len();

    let mut events = Vec::new();
    move_to_zone(&mut state, id, Zone::Graveyard, &mut events);
    process_triggers(&mut state, &events);
    assert!(!state.stack.is_empty(), "dies-draw must reach the stack");
    resolve_top(&mut state, &mut Vec::new());

    assert_eq!(
        state.players[0].hand.len(),
        hand_before + 1,
        "controller draws a card when the blitz creature dies"
    );
}
