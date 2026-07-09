//! Issue #4361 — Heartwood Storyteller: "Whenever a player casts a noncreature
//! spell, each of that player's opponents may draw a card."
//!
//! Two defects validated here at runtime:
//!   1. The draw recipient is EACH OPPONENT OF THE CASTER (the triggering
//!      player), not the Storyteller's controller. When opponent B casts a
//!      noncreature spell, A draws; when A casts, B draws.
//!   2. The "may" is a per-recipient optional — each opponent is independently
//!      prompted (`WaitingFor::OptionalEffectChoice`) and may decline.
//!
//! Resolves the parsed trigger execute body directly (the tempt_with_discovery
//! pattern) with a live SpellCast `current_trigger_event` so the
//! `OpponentOfTriggeringPlayer` player_scope fans out against the caster.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::engine::apply;
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const ORACLE: &str =
    "Whenever a player casts a noncreature spell, each of that player's opponents may draw a card.";

fn hand_len(state: &GameState, player: PlayerId) -> usize {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .hand
        .len()
}

/// Stock each player's library so a Draw has a card available.
fn fill_libraries(state: &mut GameState, n: usize) {
    for (pi, player) in [PlayerId(0), PlayerId(1)].into_iter().enumerate() {
        for i in 0..n {
            create_object(
                state,
                CardId(1000 + (pi as u64) * 100 + i as u64),
                player,
                "Forest".to_string(),
                Zone::Library,
            );
        }
    }
}

/// Build a 2-player state with Heartwood Storyteller on P0's battlefield and
/// stocked libraries.
fn setup() -> (GameState, ObjectId) {
    let mut state = GameState::new_two_player(42);
    let storyteller = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Heartwood Storyteller".to_string(),
        Zone::Battlefield,
    );
    fill_libraries(&mut state, 4);
    (state, storyteller)
}

/// Fire the cast trigger for `caster`. Sets the live SpellCast event so
/// `OpponentOfTriggeringPlayer` resolves the caster, then resolves the parsed
/// execute body.
fn fire_cast_trigger(
    state: &mut GameState,
    storyteller: ObjectId,
    controller: PlayerId,
    caster: PlayerId,
) {
    state.current_trigger_event = Some(GameEvent::SpellCast {
        card_id: CardId(9000),
        controller: caster,
        object_id: ObjectId(9000),
    });
    let parsed = parse_oracle_text(
        ORACLE,
        "Heartwood Storyteller",
        &[],
        &["Creature".to_string()],
        &["Treefolk".to_string()],
    );
    let trigger = parsed
        .triggers
        .first()
        .expect("Heartwood cast trigger present");
    let exec = trigger.execute.as_ref().expect("trigger execute present");
    let ability = build_resolved_from_def(exec, storyteller, controller);
    let mut events = Vec::new();
    resolve_ability_chain(state, &ability, &mut events, 0).expect("trigger execute resolves");
}

/// When A (controller) casts a noncreature spell, A's opponent B is prompted
/// and — on accept — draws; A (the caster) is not a recipient.
#[test]
fn controller_casts_opponent_prompted_and_draws() {
    let (mut state, storyteller) = setup();
    let a_before = hand_len(&state, PlayerId(0));
    let b_before = hand_len(&state, PlayerId(1));

    fire_cast_trigger(&mut state, storyteller, PlayerId(0), PlayerId(0));

    // B (the caster's only opponent) is the prompted recipient.
    assert!(
        matches!(
            state.waiting_for,
            WaitingFor::OptionalEffectChoice {
                player: PlayerId(1),
                ..
            }
        ),
        "B must be prompted to draw, got {:?}",
        state.waiting_for
    );
    apply(
        &mut state,
        PlayerId(1),
        GameAction::DecideOptionalEffect { accept: true },
    )
    .expect("B accepts the optional draw");

    assert_eq!(
        hand_len(&state, PlayerId(1)),
        b_before + 1,
        "A casts → A's opponent B draws one"
    );
    assert_eq!(
        hand_len(&state, PlayerId(0)),
        a_before,
        "A casts → A (the caster) is not a recipient"
    );
}

/// Multi-authority proof: when B (the opponent) casts, A is the prompted
/// recipient and draws — confirming the base is the CASTER, not the constant
/// controller A.
#[test]
fn opponent_casts_controller_prompted_and_draws() {
    let (mut state, storyteller) = setup();
    let a_before = hand_len(&state, PlayerId(0));
    let b_before = hand_len(&state, PlayerId(1));

    fire_cast_trigger(&mut state, storyteller, PlayerId(0), PlayerId(1));

    assert!(
        matches!(
            state.waiting_for,
            WaitingFor::OptionalEffectChoice {
                player: PlayerId(0),
                ..
            }
        ),
        "A must be prompted to draw when B casts, got {:?}",
        state.waiting_for
    );
    apply(
        &mut state,
        PlayerId(0),
        GameAction::DecideOptionalEffect { accept: true },
    )
    .expect("A accepts the optional draw");

    assert_eq!(
        hand_len(&state, PlayerId(0)),
        a_before + 1,
        "B casts → B's opponent A draws one"
    );
    assert_eq!(
        hand_len(&state, PlayerId(1)),
        b_before,
        "B casts → B (the caster) is not a recipient"
    );
}

/// Decline path: a prompted opponent who declines draws nothing.
#[test]
fn opponent_may_decline_draws_nothing() {
    let (mut state, storyteller) = setup();
    let b_before = hand_len(&state, PlayerId(1));

    fire_cast_trigger(&mut state, storyteller, PlayerId(0), PlayerId(0));

    assert!(matches!(
        state.waiting_for,
        WaitingFor::OptionalEffectChoice {
            player: PlayerId(1),
            ..
        }
    ));
    apply(
        &mut state,
        PlayerId(1),
        GameAction::DecideOptionalEffect { accept: false },
    )
    .expect("B declines the optional draw");

    assert_eq!(
        hand_len(&state, PlayerId(1)),
        b_before,
        "declining the 'may' draws nothing"
    );
}
