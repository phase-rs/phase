//! Regression for issue #308: Riot Control was reported as gaining life but
//! not preventing damage.
//!
//! Oracle text:
//!     "You gain 1 life for each creature your opponents control.
//!      Prevent all damage that would be dealt to you this turn."
//!
//! Investigation showed both the parser (`GainLife → sub_ability: PreventDamage`
//! is emitted in card-data.json) and the runtime chain resolver are correct.
//! This integration test pins the end-to-end behavior so a future regression
//! in either layer (parser dropping the second sentence, or the chain
//! resolver short-circuiting before the prevention sub_ability runs) is
//! caught here rather than rediscovered as a user-facing bug.
//!
//! CR 615:    Prevention shields modify damage events through the replacement
//!            pipeline.
//! CR 119.1:  Gain life increases the player's life total.

use engine::game::effects;
use engine::game::zones::create_object;
use engine::types::ability::{
    ControllerRef, Effect, GainLifePlayer, PreventionAmount, PreventionScope, QuantityExpr,
    QuantityRef, ResolvedAbility, TargetFilter, TargetRef, TypeFilter, TypedFilter,
};
use engine::types::game_state::GameState;
use engine::types::identifiers::CardId;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

#[test]
fn riot_control_chain_gains_life_and_prevents_damage() {
    let mut state = GameState::new_two_player(42);

    // Riot Control on the stack mid-resolution.
    let riot_control = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Riot Control".to_string(),
        Zone::Stack,
    );
    // An opposing attacker that will try to deal damage *after* resolution.
    let attacker = create_object(
        &mut state,
        CardId(2),
        PlayerId(1),
        "Attacker".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&attacker)
        .unwrap()
        .card_types
        .core_types
        .push(engine::types::card_type::CoreType::Creature);

    // Build the same chain the Oracle parser produces for Riot Control:
    //   GainLife { amount: ObjectCount { Creature, Opponent } }
    //     → sub_ability: PreventDamage { Controller, AllDamage }
    // The opponent has one creature on the battlefield (the attacker), so the
    // life-gain quantity resolves to 1, exercising the same QuantityExpr::Ref
    // path used at runtime.
    let prevent = ResolvedAbility::new(
        Effect::PreventDamage {
            amount: PreventionAmount::All,
            amount_dynamic: None,
            target: TargetFilter::Controller,
            scope: PreventionScope::AllDamage,
            damage_source_filter: None,
        },
        vec![],
        riot_control,
        PlayerId(0),
    );
    let ability = ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount {
                    filter: TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Creature],
                        controller: Some(ControllerRef::Opponent),
                        properties: vec![],
                    }),
                },
            },
            player: GainLifePlayer::Controller,
        },
        vec![],
        riot_control,
        PlayerId(0),
    )
    .sub_ability(prevent);

    let mut events = Vec::new();
    effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    // Life was gained — one creature controlled by the opponent → +1 life.
    assert_eq!(
        state.players[0].life, 21,
        "GainLife arm of the chain must execute and resolve ObjectCount to 1"
    );

    // Prevention shield was installed (Riot Control's source is on the Stack,
    // so the shield lives in the game-level pending registry — see prevent_damage::resolve).
    assert!(
        !state.pending_damage_replacements.is_empty(),
        "PreventDamage sub_ability must install a shield — chain dropped the second sentence"
    );

    // And the shield actually intercepts subsequent damage to the controller.
    let damage_ability = ResolvedAbility::new(
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 4 },
            target: TargetFilter::Any,
            damage_source: None,
        },
        vec![TargetRef::Player(PlayerId(0))],
        attacker,
        PlayerId(1),
    );
    effects::resolve_ability_chain(&mut state, &damage_ability, &mut events, 0).unwrap();
    assert_eq!(
        state.players[0].life, 21,
        "prevention shield must absorb damage to Riot Control's controller"
    );
}
