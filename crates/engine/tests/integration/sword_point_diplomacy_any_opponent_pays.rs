//! Regression for issue #10949: Sword-Point Diplomacy never offered an
//! opponent the choice to pay life.
//!
//! CR 118.12a: "[effect] unless [player] pays [cost]" — the named player may pay,
//! and if they do the effect doesn't happen. CR 608.2c: "for each of those cards"
//! runs the instruction once per revealed card. CR 101.4: with several opponents,
//! each chooses in turn order and the first to pay prevents the effect.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

/// Verbatim printed Oracle text.
const SWORD_POINT_DIPLOMACY: &str = "Reveal the top three cards of your library. For each of those cards, put that card into your hand unless any opponent pays 3 life. Then exile the rest.";

struct Setup {
    runner: GameRunner,
    revealed: Vec<ObjectId>,
}

fn setup(player_count: u8) -> Setup {
    let mut scenario = GameScenario::new_n_player(player_count, 42);
    scenario.at_phase(Phase::PreCombatMain);
    // Library is built bottom-first: each add lands on top, so `revealed` is the
    // top three cards and "Deep Card" sits below them.
    scenario.add_card_to_library_top(P0, "Deep Card");
    let revealed: Vec<ObjectId> = (1..=3)
        .map(|i| scenario.add_card_to_library_top(P0, &format!("Revealed {i}")))
        .collect();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Sword-Point Diplomacy", false, SWORD_POINT_DIPLOMACY)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 2,
        })
        .id();
    let mut runner = scenario.build();
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool;
    for mana in [ManaType::Black, ManaType::Black, ManaType::Black] {
        pool.add(ManaUnit::new(mana, ObjectId(0), false, vec![]));
    }
    runner.cast(spell).resolve();
    Setup { runner, revealed }
}

fn prompted_player(runner: &GameRunner) -> PlayerId {
    match &runner.state().waiting_for {
        WaitingFor::UnlessPayment { player, .. } => *player,
        other => panic!("expected WaitingFor::UnlessPayment, got {other:?}"),
    }
}

fn answer(runner: &mut GameRunner, pay: bool) {
    runner
        .act(GameAction::PayUnlessCost { pay })
        .expect("answer unless payment");
}

fn zones(runner: &GameRunner, ids: &[ObjectId]) -> Vec<Zone> {
    ids.iter()
        .map(|&id| runner.state().objects[&id].zone)
        .collect()
}

/// The opponent is prompted once per revealed card (the reveal happens first).
/// Declining every time puts all three cards into the controller's hand and
/// loses no life; paying every time costs 3 life each and exiles them all.
#[test]
fn opponent_declines_every_card_goes_to_hand() {
    let Setup {
        mut runner,
        revealed,
    } = setup(2);
    for _ in 0..3 {
        assert_eq!(prompted_player(&runner), P1);
        answer(&mut runner, false);
    }
    runner.advance_until_stack_empty();
    assert!(zones(&runner, &revealed).iter().all(|z| *z == Zone::Hand));
    assert_eq!(runner.life(P1), 20);
}

#[test]
fn opponent_pays_every_card_is_exiled() {
    let Setup {
        mut runner,
        revealed,
    } = setup(2);
    for _ in 0..3 {
        assert_eq!(prompted_player(&runner), P1);
        answer(&mut runner, true);
    }
    runner.advance_until_stack_empty();
    assert!(zones(&runner, &revealed).iter().all(|z| *z == Zone::Exile));
    assert_eq!(runner.life(P1), 20 - 9);
}

/// Mixed answers: only the paid-for card is exiled; the rest are in hand, and the
/// card below the reveal is untouched.
#[test]
fn mixed_answers_split_cards_between_hand_and_exile() {
    let Setup {
        mut runner,
        revealed,
    } = setup(2);
    answer(&mut runner, true);
    answer(&mut runner, false);
    answer(&mut runner, false);
    runner.advance_until_stack_empty();
    let in_hand = zones(&runner, &revealed)
        .iter()
        .filter(|z| **z == Zone::Hand)
        .count();
    let in_exile = zones(&runner, &revealed)
        .iter()
        .filter(|z| **z == Zone::Exile)
        .count();
    assert_eq!((in_hand, in_exile), (2, 1));
    assert_eq!(runner.life(P1), 17);
}

/// CR 118.12a + CR 101.4: any opponent may pay — if the first declines, the next
/// is asked, and the first to pay stops that card from reaching the hand.
#[test]
fn second_opponent_may_pay_after_first_declines() {
    let Setup {
        mut runner,
        revealed,
    } = setup(3);
    assert_eq!(prompted_player(&runner), P1);
    answer(&mut runner, false);
    assert_eq!(prompted_player(&runner), P2);
    answer(&mut runner, true);
    assert_eq!(runner.life(P2), 17);
    assert_eq!(runner.life(P1), 20);
    // The next card starts a fresh poll with the first opponent again.
    assert_eq!(prompted_player(&runner), P1);
    answer(&mut runner, false);
    answer(&mut runner, false);
    answer(&mut runner, false);
    answer(&mut runner, false);
    runner.advance_until_stack_empty();
    let in_exile = zones(&runner, &revealed)
        .iter()
        .filter(|z| **z == Zone::Exile)
        .count();
    assert_eq!(in_exile, 1);
}
