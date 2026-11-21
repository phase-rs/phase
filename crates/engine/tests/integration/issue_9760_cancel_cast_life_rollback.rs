//! Issue #9760: CR 733.1 — cancelling a cast reverses the whole action and
//! cancels any payments already made. Toxic Deluge's X life paid as an
//! additional cost must be refunded when the cast is cancelled at mana payment.
//! No abilities trigger from the undone action, and any announcement triggers
//! staged during cost payment must not survive the cancellation.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const TOXIC_DELUGE: &str =
    "As an additional cost to cast this spell, pay X life.\nAll creatures get -X/-X until end of turn.";
const VILIS_NAME: &str = "Vilis, Broker of Blood";
const VILIS: &str =
    "Flying\n{B}, Pay 2 life: Target creature gets -1/-1 until end of turn.\nWhenever you lose life, draw that many cards. (Damage causes loss of life.)";

#[test]
fn cancel_at_mana_payment_refunds_x_life_paid_as_additional_cost() {
    let mut s = GameScenario::new();
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
    assert_eq!(r.life(P0), 10, "life refunded back to 10");
    assert!(r.state().stack.is_empty(), "no trigger survives the cancel");
    assert!(r.state().pending_trigger.is_none());
    assert_eq!(
        r.state().waiting_for,
        WaitingFor::Priority { player: P0 },
        "settled return to priority"
    );
}

/// CR 733.1: "No abilities trigger and no effects apply as a result of an undone action."
/// Uses printed card witness Vilis, Broker of Blood ("Whenever you lose life, draw that many cards").
/// The test verifies trigger reach during payment in the announcement carrier, then proves
/// cancellation cleans up the payment, drops the carrier triggers, draws zero cards, and returns to priority.
#[test]
fn cancel_at_mana_payment_drops_life_loss_triggers_from_undone_payment() {
    let mut s = GameScenario::new();
    let vilis = s.add_creature_from_oracle(P0, VILIS_NAME, 8, 8, VILIS).id();
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
    for _ in 0..10 {
        s.add_card_to_library_top(P0, "Swamp");
    }
    s.with_life(P0, 10);
    let mut r = s.build();
    let initial_hand = r.state().players[0].hand.len();

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

    // Establish trigger reach: Vilis's "Whenever you lose life" trigger was observed
    // and collected into the announcement carrier during cost payment.
    let pending_val = serde_json::to_value(
        r.state()
            .pending_cast
            .as_ref()
            .expect("pending cast present at ManaPayment"),
    )
    .expect("serialize pending cast");
    let carrier = pending_val
        .get("activation_trigger_collection")
        .and_then(|v| v.as_object())
        .expect("carrier collected announcement triggers during cost payment");
    let contexts = carrier
        .get("pending_contexts")
        .and_then(|v| v.as_array())
        .expect("carrier has pending_contexts array");
    let vilis_json = serde_json::to_value(vilis).expect("serialize vilis id");
    assert!(
        contexts.iter().any(|ctx| {
            let source = ctx
                .get("pending")
                .and_then(|p| p.get("source_id"))
                .or_else(|| ctx.get("source_id"));
            source == Some(&vilis_json)
        }),
        "Vilis trigger must be collected in the announcement carrier"
    );

    // Cancel the cast at ManaPayment
    r.act(GameAction::CancelCast)
        .expect("cancel at ManaPayment");

    // CR 733.1: Action is reversed, payments are refunded, no abilities trigger.
    assert_eq!(r.state().objects[&deluge].zone, Zone::Hand);
    assert_eq!(r.life(P0), 10, "paid life restored after cancel");
    assert!(r.state().stack.is_empty(), "no trigger survives on stack");
    assert!(r.state().pending_trigger.is_none(), "no pending trigger");
    assert_eq!(
        r.state().players[0].hand.len(),
        initial_hand,
        "Vilis drew no cards from the cancelled payment"
    );
    assert_eq!(
        r.state().waiting_for,
        WaitingFor::Priority { player: P0 },
        "settled return to priority"
    );
}

/// Positive control: completing the cast commits the life payment and places
/// Vilis, Broker of Blood's trigger on the stack, drawing cards on resolution.
#[test]
fn completed_cast_with_life_payment_triggers_vilis() {
    let mut s = GameScenario::new();
    let vilis = s.add_creature_from_oracle(P0, VILIS_NAME, 8, 8, VILIS).id();
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
    for _ in 0..10 {
        s.add_card_to_library_top(P0, "Swamp");
    }
    s.with_life(P0, 10);
    let mut r = s.build();
    let initial_hand = r.state().players[0].hand.len();

    let card_id = r.state().objects[&deluge].card_id;
    r.act(GameAction::CastSpell {
        object_id: deluge,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("cast begins");

    for _ in 0..6 {
        match r.state().waiting_for.clone() {
            WaitingFor::ChooseXValue { .. } => {
                r.act(GameAction::ChooseX { value: 3 }).expect("announce X");
            }
            WaitingFor::Priority { player } if player == P0 => break,
            other => panic!("unexpected prompt during Auto cast: {other:?}"),
        }
    }

    assert_eq!(r.life(P0), 7, "3 life paid for X = 3");
    // Cast completed: Toxic Deluge is on the stack, and Vilis's trigger is on the stack
    assert!(
        r.state().stack.iter().any(|e| e.source_id == vilis),
        "Vilis trigger must be placed on the stack after cast completes"
    );

    // Resolve Vilis's draw trigger: both players pass priority
    r.act(GameAction::PassPriority).expect("P0 passes");
    r.act(GameAction::PassPriority).expect("P1 passes");

    // Vilis drew 3 cards for 3 life lost
    // initial_hand had Toxic Deluge which was cast (-1), plus 3 drawn = initial_hand + 2
    assert_eq!(
        r.state().players[0].hand.len(),
        initial_hand - 1 + 3,
        "Vilis draws 3 cards for the 3 life lost"
    );
}
