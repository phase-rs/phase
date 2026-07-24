//! Issue #5981: Gift recipient selection + Peerless Recycling gift-gated targets.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{AdditionalCostOrigin, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use engine::types::ObjectId;

const PEERLESS_RECYCLING: &str =
    "Gift a card (You may promise an opponent a gift as you cast this spell. \
If you do, they draw a card before its other effects.)\n\
Return target permanent card from your graveyard to your hand. If the gift was promised, \
instead return two target permanent cards from your graveyard to your hand.";

fn with_green_mana(scenario: &mut GameScenario, player: PlayerId, n: usize) {
    scenario.with_mana_pool(
        player,
        (0..n)
            .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
            .collect(),
    );
}

fn add_gift_spell_from_oracle(
    scenario: &mut GameScenario,
    player: PlayerId,
    name: &str,
    oracle_text: &str,
) -> engine::types::ObjectId {
    // MTGJSON supplies the bare "Gift" keyword hint; scenario inference cannot
    // FromStr a "Gift a card (reminder…)" line, so pass the production hint.
    scenario
        .add_spell_to_hand(player, name, true)
        .from_oracle_text_with_keywords(&["Gift"], oracle_text)
        .id()
}

#[test]
fn peerless_recycling_decline_returns_one_and_no_gift_draw() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    with_green_mana(&mut scenario, P0, 2);
    scenario.add_card_to_library_top(P1, "Gift Card");
    let gy_a = scenario
        .add_creature_to_graveyard(P0, "Bear Cub", 2, 2)
        .id();
    let gy_b = scenario
        .add_creature_to_graveyard(P0, "Cougar Cub", 2, 2)
        .id();
    let spell =
        add_gift_spell_from_oracle(&mut scenario, P0, "Peerless Recycling", PEERLESS_RECYCLING);

    let mut runner = scenario.build();
    let p1_hand_before = runner.state().players[1].hand.len();
    runner
        .cast(spell)
        .decline_optional()
        .target_objects(&[gy_a])
        .resolve();

    assert_eq!(runner.state().objects[&gy_a].zone, Zone::Hand);
    assert_eq!(
        runner.state().objects[&gy_b].zone,
        Zone::Graveyard,
        "unpromised Peerless returns exactly one permanent card"
    );
    assert_eq!(
        runner.state().players[1].hand.len(),
        p1_hand_before,
        "declining Gift must not draw for the opponent"
    );
}

#[test]
fn peerless_recycling_promise_returns_two_and_opponent_draws() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    with_green_mana(&mut scenario, P0, 2);
    let draw_card = scenario.add_card_to_library_top(P1, "Gift Card");
    let gy_a = scenario
        .add_creature_to_graveyard(P0, "Bear Cub", 2, 2)
        .id();
    let gy_b = scenario
        .add_creature_to_graveyard(P0, "Cougar Cub", 2, 2)
        .id();
    let spell =
        add_gift_spell_from_oracle(&mut scenario, P0, "Peerless Recycling", PEERLESS_RECYCLING);

    let mut runner = scenario.build();
    runner
        .cast(spell)
        .accept_optional()
        .target_objects(&[gy_a, gy_b])
        .resolve();

    assert_eq!(runner.state().objects[&gy_a].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&gy_b].zone, Zone::Hand);
    assert!(
        runner.state().players[1].hand.contains(&draw_card),
        "promised Gift a card must draw for the sole opponent"
    );
}

#[test]
fn peerless_recycling_three_player_chooses_gift_recipient_not_next_player() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    with_green_mana(&mut scenario, P0, 2);
    let p1_draw = scenario.add_card_to_library_top(P1, "P1 Draw");
    let p2_draw = scenario.add_card_to_library_top(PlayerId(2), "P2 Draw");
    let gy_a = scenario
        .add_creature_to_graveyard(P0, "Bear Cub", 2, 2)
        .id();
    let gy_b = scenario
        .add_creature_to_graveyard(P0, "Cougar Cub", 2, 2)
        .id();
    let spell =
        add_gift_spell_from_oracle(&mut scenario, P0, "Peerless Recycling", PEERLESS_RECYCLING);

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Peerless Recycling");

    let mut chose_recipient = false;
    let mut pending_targets = vec![gy_a, gy_b];
    for _ in 0..200 {
        match &runner.state().waiting_for {
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("complete mana payment");
            }
            WaitingFor::OptionalCostChoice { origin, .. } => {
                assert_eq!(
                    *origin,
                    AdditionalCostOrigin::Gift,
                    "Gift optional cost must carry Gift origin"
                );
                runner
                    .act(GameAction::DecideOptionalCost { pay: true })
                    .expect("promise Gift");
            }
            WaitingFor::ChooseGiftRecipient { candidates, .. } => {
                assert!(
                    candidates.contains(&PlayerId(2)),
                    "P2 must be a legal Gift recipient"
                );
                // Seat-order next player after P0 is P1; deliberately choose P2.
                runner
                    .act(GameAction::ChooseGiftRecipient {
                        opponent: PlayerId(2),
                    })
                    .expect("choose Gift recipient P2");
                chose_recipient = true;
            }
            WaitingFor::TargetSelection { .. } => {
                let target = pending_targets.remove(0);
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("choose GY target");
            }
            WaitingFor::OrderTriggers { .. } | WaitingFor::Priority { .. }
                if !runner.state().stack.is_empty() =>
            {
                runner
                    .act(GameAction::PassPriority)
                    .expect("advance Peerless");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            other => panic!("unexpected prompt while casting Peerless: {other:?}"),
        }
    }

    assert!(
        chose_recipient,
        "3p Gift promise must raise ChooseGiftRecipient"
    );
    assert_eq!(runner.state().objects[&gy_a].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&gy_b].zone, Zone::Hand);
    assert!(
        runner.state().players[2].hand.contains(&p2_draw),
        "chosen Gift recipient P2 must draw"
    );
    assert!(
        runner.state().players[1].library.contains(&p1_draw),
        "seat-next P1 must NOT receive the gift when P2 was chosen"
    );
    let _ = runner;
}

const GIFT_DRAW_SPELL: &str =
    "Gift a card (You may promise an opponent a gift as you cast this spell. \
If you do, they draw a card before its other effects.)\n\
Draw a card.";

/// Non-Peerless Gift delivery: 3p cast that chooses the non–seat-next opponent.
/// Reverting `gift_delivery` to `next_player` fails this even if Peerless
/// targeting still works.
#[test]
fn gift_card_three_player_delivery_uses_chosen_recipient_not_next_player() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    with_green_mana(&mut scenario, P0, 2);
    let p1_draw = scenario.add_card_to_library_top(P1, "P1 Draw");
    let p2_draw = scenario.add_card_to_library_top(PlayerId(2), "P2 Draw");
    let spell = add_gift_spell_from_oracle(&mut scenario, P0, "Gift Draw Test", GIFT_DRAW_SPELL);

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Gift Draw Test");

    let mut chose_non_first = false;
    for _ in 0..200 {
        match &runner.state().waiting_for {
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("complete mana payment");
            }
            WaitingFor::OptionalCostChoice { origin, .. } => {
                assert_eq!(*origin, AdditionalCostOrigin::Gift);
                runner
                    .act(GameAction::DecideOptionalCost { pay: true })
                    .expect("promise Gift");
            }
            WaitingFor::ChooseGiftRecipient { candidates, .. } => {
                assert!(
                    candidates.contains(&PlayerId(2)),
                    "P2 must be a legal Gift recipient"
                );
                // Deliberately not seat-order next (P1) — revert of next_player fails here.
                runner
                    .act(GameAction::ChooseGiftRecipient {
                        opponent: PlayerId(2),
                    })
                    .expect("choose Gift recipient P2");
                chose_non_first = true;
            }
            WaitingFor::OrderTriggers { .. } | WaitingFor::Priority { .. }
                if !runner.state().stack.is_empty() =>
            {
                runner.act(GameAction::PassPriority).expect("advance spell");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            other => panic!("unexpected prompt while casting Gift Draw Test: {other:?}"),
        }
    }

    assert!(
        chose_non_first,
        "3p Gift promise must raise ChooseGiftRecipient"
    );
    assert!(
        runner.state().players[2].hand.contains(&p2_draw),
        "chosen Gift recipient must draw"
    );
    assert!(
        runner.state().players[1].library.contains(&p1_draw),
        "seat-next opponent must not receive the gift"
    );
}
