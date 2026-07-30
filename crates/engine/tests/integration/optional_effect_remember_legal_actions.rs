//! Regression: a keyed optional trigger's “don't ask again” choices must be
//! surfaced in the engine legal-action snapshot so the client can safely queue
//! the selected answer while animations settle.

use engine::ai_support::legal_actions;
use engine::game::engine::apply_as_current;
use engine::game::zones::create_object;
use engine::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::{
    AutoMayChoice, GameState, MayTriggerAutoChoiceKey, MayTriggerOrigin, WaitingFor,
};
use engine::types::identifiers::CardId;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

#[test]
fn keyed_optional_effect_exposes_and_resolves_remember_choices() {
    let mut state = GameState::new_two_player(42);
    let source_id = create_object(
        &mut state,
        CardId(903),
        PlayerId(0),
        "Optional source".to_string(),
        Zone::Battlefield,
    );
    let key = MayTriggerAutoChoiceKey {
        player: PlayerId(0),
        source_id,
        origin: MayTriggerOrigin::Printed { trigger_index: 0 },
    };
    state.waiting_for = WaitingFor::OptionalEffectChoice {
        player: PlayerId(0),
        source_id,
        description: None,
        may_trigger_key: Some(key.clone()),
    };
    let mut ability = ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
        Vec::new(),
        source_id,
        PlayerId(0),
    );
    ability.optional = true;
    state.push_optional_effect_frame(engine::types::OptionalEffectFrame {
        ability: Box::new(ability),
        trigger_event: None,
        trigger_match_count: None,
    });

    let accept = GameAction::DecideOptionalEffectAndRemember {
        choice: AutoMayChoice::Accept,
    };
    let decline = GameAction::DecideOptionalEffectAndRemember {
        choice: AutoMayChoice::Decline,
    };
    let actions = legal_actions(&state);
    assert!(
        actions.contains(&accept) && actions.contains(&decline),
        "a keyed optional prompt must expose both remember actions: {actions:?}"
    );

    let mut unkeyed = state.clone();
    let WaitingFor::OptionalEffectChoice {
        may_trigger_key, ..
    } = &mut unkeyed.waiting_for
    else {
        unreachable!("fixture must remain an optional-effect choice");
    };
    *may_trigger_key = None;
    assert!(
        !legal_actions(&unkeyed)
            .iter()
            .any(|action| matches!(action, GameAction::DecideOptionalEffectAndRemember { .. })),
        "unkeyed optional prompts must not offer the remember action"
    );

    apply_as_current(&mut state, accept).expect("remembered accept must be legal");
    assert_eq!(state.players[0].life, 21);
    assert!(state
        .may_trigger_auto_choices
        .iter()
        .any(|record| record.key == key && record.choice == AutoMayChoice::Accept));
}
