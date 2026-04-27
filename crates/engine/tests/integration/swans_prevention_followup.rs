//! Regression: Swans of Bryn Argoll's prevention follow-up draws cards for
//! the *damage source's* controller, not Swans's controller.
//!
//! Oracle text: "If a source would deal damage to ~, prevent that damage.
//! The source's controller draws cards equal to the damage prevented this way."
//!
//! Historical bug: `Effect::Draw` short-circuited on `target.is_context_ref()`
//! and returned `ability.controller` (Swans's controller — the prevented
//! player), ignoring the auto-resolved target slot. The fix routes context-ref
//! targets through `resolve_player_for_context_ref`, which reads
//! `state.post_replacement_event_source` and returns the damage source's
//! controller.
//!
//! CR 615.5: Some prevention effects also include an additional effect, which
//!           may refer to the amount of damage that was prevented.
//! CR 609.7: Some effects apply to damage from a source — "the source's
//!           controller draws cards equal to the damage prevented this way".
//! CR 121.1: A player draws a card by putting the top card of their library
//!           into their hand.

use engine::game::effects;
use engine::game::zones::create_object;
use engine::types::ability::{Effect, QuantityExpr, QuantityRef, ResolvedAbility, TargetFilter};
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

#[test]
fn swans_followup_draws_for_damage_sources_controller() {
    let mut state = GameState::new_two_player(42);
    let swans_controller = PlayerId(0);
    let damage_source_controller = PlayerId(1);

    // Stack the libraries so we can detect a draw on player 1.
    // Use plain Forest stand-ins — content doesn't matter.
    let p1_lib_card = create_object(
        &mut state,
        CardId(1),
        damage_source_controller,
        "Forest".to_string(),
        Zone::Library,
    );
    state.players[1].library.push_back(p1_lib_card);

    // The "damage source" object the prevention shield captured.
    let damage_source = create_object(
        &mut state,
        CardId(2),
        damage_source_controller,
        "Goblin".to_string(),
        Zone::Battlefield,
    );

    // Simulate the prevention applier's `Prevented` arm having stamped state:
    // - `last_effect_count` carries the prevented amount (1 damage)
    // - `post_replacement_event_source` carries the damage source's id
    state.last_effect_count = Some(1);
    state.post_replacement_event_source = Some(damage_source);

    let p1_hand_before = state.players[1].hand.len();
    let p0_hand_before = state.players[0].hand.len();

    // Build the parsed shape Swans's follow-up lowers to:
    //   Draw { count: EventContextAmount, target: PostReplacementSourceController }
    // ResolvedAbility's source/controller mirror what
    // `apply_post_replacement_effect` would supply: Swans (P0).
    let ability = ResolvedAbility::new(
        Effect::Draw {
            count: QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            },
            target: TargetFilter::PostReplacementSourceController,
        },
        vec![],
        ObjectId(999), // Swans
        swans_controller,
    );

    let mut events = Vec::new();
    effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    assert_eq!(
        state.players[1].hand.len(),
        p1_hand_before + 1,
        "damage source's controller (P1) must draw 1 card"
    );
    assert_eq!(
        state.players[0].hand.len(),
        p0_hand_before,
        "Swans's controller (P0) must NOT draw — the follow-up routes to the \
         prevented event's source controller, not the shield's controller"
    );
}
