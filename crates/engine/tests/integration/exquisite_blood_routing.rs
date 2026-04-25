//! Regression: Exquisite Blood must route "you gain that much life" to the
//! ability's controller — not to the triggering opponent.
//!
//! Oracle text: "Whenever an opponent loses life, you gain that much life."
//!
//! Historical bug: a post-hoc `rewire_player_scoped_execute_to_triggering_player`
//! pass in `oracle_trigger.rs` re-bound the execute ability's `player_scope` to
//! `TriggeringPlayer` whenever the trigger's subject was opponent-scoped. That
//! ignored the *effect*-level subject ("you"), causing the opponent to gain
//! life instead of the controller. The fix deletes the rewire and relies on
//! the effect's own subject (`GainLifePlayer::Controller`) to route correctly.
//!
//! CR 119.3: If an effect causes a player to gain life, that player's life
//!           total is adjusted accordingly — the recipient is determined by
//!           the effect, not by the triggering event.
//! CR 603.7c: `EventContextAmount` resolves to the triggering event's amount.
//! CR 608.2k: Pronoun resolution — "you" in trigger effect text resolves to
//! the ability controller, regardless of the trigger's subject.

use engine::game::effects;
use engine::types::ability::{Effect, GainLifePlayer, QuantityExpr, QuantityRef, ResolvedAbility};
use engine::types::events::GameEvent;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;

#[test]
fn exquisite_blood_heals_controller_not_triggering_opponent() {
    let mut state = GameState::new_two_player(42);
    let controller = PlayerId(0);
    let opponent = PlayerId(1);
    state.players[0].life = 20;
    state.players[1].life = 15;

    // Simulate the trigger fire: opponent just lost 5 life.
    state.current_trigger_event = Some(GameEvent::LifeChanged {
        player_id: opponent,
        amount: -5,
    });

    // Build the parsed shape Exquisite Blood lowers to:
    //   GainLife { amount: EventContextAmount, player: Controller }
    // No player_scope — the effect-level subject is authoritative.
    let ability = ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            },
            player: GainLifePlayer::Controller,
        },
        vec![],
        ObjectId(100), // Exquisite Blood
        controller,
    );

    let mut events = Vec::new();
    effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    assert_eq!(
        state.players[0].life, 25,
        "controller must gain 5 life (matching opponent's life-loss amount)"
    );
    assert_eq!(
        state.players[1].life, 15,
        "opponent's life must NOT change — the trigger fed information \
         (the amount) but not the recipient"
    );
}
