//! Force of Rage — delayed "Sacrifice those tokens" must take every token.
//!
//! Oracle: "Create two 3/1 red Elemental creature tokens with trample and
//! haste. Sacrifice those tokens at the beginning of your next upkeep."
//!
//! CR 701.21a + CR 608.2c + CR 603.7: the delayed trigger's referent is the
//! whole set the spell created. A field report (2026-09-15) on the shipped
//! build showed the upkeep trigger asking the controller to choose ONE of the
//! two tokens and leaving the other on the battlefield; the parser/unit tests
//! passed, so this drives the full cast → delayed trigger → upkeep pipeline.

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::ObjectId;

const ORACLE: &str = "Create two 3/1 red Elemental creature tokens with trample and haste. Sacrifice those tokens at the beginning of your next upkeep.";

fn elementals(state: &engine::types::game_state::GameState) -> Vec<ObjectId> {
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| state.objects[id].is_token && state.objects[id].name.contains("Elemental"))
        .collect()
}

#[test]
fn force_of_rage_upkeep_sacrifices_both_tokens() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..6)
            .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
            .collect(),
    );
    // Both players need cards to draw across the two turns the test plays.
    for player in [P0, engine::types::player::PlayerId(1)] {
        for _ in 0..4 {
            scenario.add_card_to_library_top(player, "Wastes");
        }
    }
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Force of Rage", true, ORACLE)
        .id();
    let mut runner = scenario.build();
    let cast_turn = runner.state().turn_number;

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Force of Rage");
    runner.advance_until_stack_empty();
    assert_eq!(
        elementals(runner.state()).len(),
        2,
        "Force of Rage must create two Elemental tokens"
    );

    // CR 603.7: "your next upkeep" is P0's upkeep on the turn after the
    // opponent's. Play both turns (no attacks, no blocks) until P0 reaches its
    // draw step, so the delayed trigger has fired and resolved.
    for _ in 0..400 {
        let st = runner.state();
        if st.turn_number > cast_turn && st.active_player == P0 && st.phase == Phase::Draw {
            break;
        }
        let action = match &st.waiting_for {
            WaitingFor::EffectZoneChoice { count, cards, .. } => panic!(
                "upkeep sacrifice must not ask to choose {count} of {} tokens",
                cards.len()
            ),
            WaitingFor::DeclareAttackers { .. } => None,
            WaitingFor::DeclareBlockers { .. } => Some(GameAction::DeclareBlockers {
                assignments: vec![],
            }),
            WaitingFor::Priority { .. } => Some(GameAction::PassPriority),
            other => panic!("unexpected wait while advancing to the next upkeep: {other:?}"),
        };
        match action {
            None => {
                runner.declare_attackers(&[]).expect("declare no attackers");
            }
            Some(action) => {
                runner.act(action).expect("advance toward the next upkeep");
            }
        }
    }

    assert!(
        elementals(runner.state()).is_empty(),
        "both Elemental tokens must be sacrificed at the next upkeep, left: {:?}",
        elementals(runner.state())
    );
}
