//! Regression for GitHub issue #3486 — player-to-player life total exchange.
//!
//! Soul Conduit: "Two target players exchange life totals."
//! Mirror Universe / Magus of the Mirror: "Exchange life totals with target opponent."

use engine::game::effects::exchange_life::resolve;
use engine::types::ability::{
    ControllerRef, Effect, ResolvedAbility, TargetFilter, TargetRef, TypedFilter,
};
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;

#[test]
fn soul_conduit_phrase_parses_exchange_life_totals() {
    use engine::parser::oracle_effect::parse_effect_chain;
    use engine::types::ability::AbilityKind;

    let def = parse_effect_chain(
        "Two target players exchange life totals.",
        AbilityKind::Activated,
    );
    assert_eq!(
        *def.effect,
        Effect::ExchangeLifeTotals {
            player_a: TargetFilter::Player,
            player_b: TargetFilter::Player,
        }
    );
}

#[test]
fn mirror_universe_phrase_parses_exchange_with_opponent() {
    use engine::parser::oracle_effect::parse_effect_chain;
    use engine::types::ability::AbilityKind;

    let def = parse_effect_chain(
        "Exchange life totals with target opponent.",
        AbilityKind::Activated,
    );
    assert_eq!(
        *def.effect,
        Effect::ExchangeLifeTotals {
            player_a: TargetFilter::Controller,
            player_b: TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent),
            ),
        }
    );
}

#[test]
fn exchange_life_totals_resolves_for_two_targeted_players() {
    let mut state = GameState::new_two_player(3486);
    state.players[0].life = 18;
    state.players[1].life = 7;

    let ability = ResolvedAbility::new(
        Effect::ExchangeLifeTotals {
            player_a: TargetFilter::Player,
            player_b: TargetFilter::Player,
        },
        vec![
            TargetRef::Player(PlayerId(0)),
            TargetRef::Player(PlayerId(1)),
        ],
        ObjectId(1),
        PlayerId(0),
    );
    let mut events = Vec::new();
    resolve(&mut state, &ability, &mut events).unwrap();

    assert_eq!(state.players[0].life, 7);
    assert_eq!(state.players[1].life, 18);
}

#[test]
fn exchange_life_totals_with_opponent_swaps_controller_and_target() {
    let mut state = GameState::new_two_player(3487);
    state.players[0].life = 12;
    state.players[1].life = 30;

    let ability = ResolvedAbility::new(
        Effect::ExchangeLifeTotals {
            player_a: TargetFilter::Controller,
            player_b: TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent),
            ),
        },
        vec![TargetRef::Player(PlayerId(1))],
        ObjectId(1),
        PlayerId(0),
    );
    let mut events = Vec::new();
    resolve(&mut state, &ability, &mut events).unwrap();

    assert_eq!(state.players[0].life, 30);
    assert_eq!(state.players[1].life, 12);
}
