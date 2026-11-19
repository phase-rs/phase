//! Issue #9760: CR 733.1 — cancelling a cast reverses the whole action and
//! cancels any payments already made. Toxic Deluge's X life paid as an
//! additional cost must be refunded when the cast is cancelled at mana payment.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const TOXIC_DELUGE: &str =
    "As an additional cost to cast this spell, pay X life.\nAll creatures get -X/-X until end of turn.";

#[test]
fn cancel_at_mana_payment_refunds_x_life_paid_as_additional_cost() {
    cancel_toxic_deluge_at_mana_payment(false);
}

/// CR 733.1: "No abilities trigger ... as a result of an undone action" — a
/// "whenever you lose life" trigger from the undone payment must not survive.
#[test]
fn cancel_at_mana_payment_drops_life_loss_triggers_from_undone_payment() {
    cancel_toxic_deluge_at_mana_payment(true);
}

fn cancel_toxic_deluge_at_mana_payment(with_life_loss_trigger: bool) {
    let mut s = GameScenario::new();
    if with_life_loss_trigger {
        s.add_creature_from_oracle(
            P0,
            "Life Watcher",
            1,
            1,
            "Whenever you lose life, you gain 1 life.",
        );
    }
    s.at_phase(Phase::PreCombatMain);
    let deluge = s
        .add_spell_to_hand_from_oracle(P0, "Toxic Deluge", false, TOXIC_DELUGE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 2,
        })
        .id();
    s.add_vanilla(P1, 3, 3);
    for _ in 0..3 {
        s.add_basic_land(P0, ManaColor::Black);
    }
    s.with_life(P0, 10);
    let mut r = s.build();
    let card_id = r.state().objects[&deluge].card_id;
    r.act(GameAction::CastSpell {
        object_id: deluge,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Manual,
    })
    .expect("cast begins");
    for _ in 0..6 {
        match r.state().waiting_for.clone() {
            WaitingFor::ChooseXValue { .. } => {
                r.act(GameAction::ChooseX { value: 3 }).expect("announce X");
            }
            WaitingFor::ManaPayment { .. } => break,
            other => panic!("unexpected prompt before ManaPayment: {other:?}"),
        }
    }
    assert!(matches!(
        r.state().waiting_for,
        WaitingFor::ManaPayment { .. }
    ));
    assert_eq!(r.life(P0), 7, "X life is paid on reaching mana payment");
    r.act(GameAction::CancelCast)
        .expect("cancel at ManaPayment");
    assert_eq!(r.state().objects[&deluge].zone, Zone::Hand);
    // CR 733.1: "any payments already made are canceled".
    assert_eq!(r.life(P0), 10);
    assert!(r.state().stack.is_empty(), "no trigger survives the cancel");
    assert!(r.state().pending_trigger.is_none());
}
