//! CR 118.12 + CR 118.3 + CR 608.2d: "[A player] may [discard N cards]. If [that
//! player] does, [effect]" makes the discard a cost paid on resolution, and "a
//! player can't pay a cost without having the necessary resources to pay it
//! fully". With fewer eligible cards than N the discard is an impossible option:
//! it is never offered, its "If you do" consequence does not happen, and an "If
//! you don't" consequence does.
//!
//! Every fixture drives the real pipeline (cast or upkeep trigger → `apply`) and
//! uses the printed Oracle text (verified against MTGJSON).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const THRILLING_DISCOVERY: &str =
    "You gain 2 life. Then you may discard two cards. If you do, draw three cards.";

const LIM_DULS_PALADIN: &str = "Trample\nAt the beginning of your upkeep, you may discard a card. If you don't, sacrifice this creature and draw a card.\nWhenever this creature becomes blocked, it gets +6/+3 until end of turn.\nWhenever this creature attacks and isn't blocked, it assigns no combat damage this turn and defending player loses 4 life.";

fn cast(runner: &mut GameRunner, object_id: ObjectId) {
    let card_id = runner.state().objects[&object_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast from hand");
}

fn hand_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

fn library_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].library.len()
}

fn assert_optional_prompt(runner: &GameRunner, player: PlayerId, why: &str) {
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player: p, .. } if p == player
        ),
        "{why}: expected an OptionalEffectChoice for {player:?}, got {:?}",
        runner.state().waiting_for
    );
}

fn assert_settled_priority(runner: &GameRunner, why: &str) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "{why}: expected Priority, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "{why}: the stack must be empty"
    );
}

/// Thrilling Discovery in P0's hand beside `other_cards` other cards, with three
/// cards to draw.
fn thrilling_discovery_board(other_cards: usize) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Thrilling Discovery", false, THRILLING_DISCOVERY)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let others = (0..other_cards)
        .map(|n| scenario.add_card_to_hand(P0, &format!("Other Card {n}")))
        .collect();
    scenario.with_library_top(P0, &["Card A", "Card B", "Card C"]);
    (scenario.build(), spell, others)
}

/// CR 118.3: one card can't pay "discard two cards", so Thrilling Discovery gains
/// 2 life and offers nothing: the card stays in hand and nothing is drawn. Two
/// cards make the discard offerable.
#[test]
fn thrilling_discovery_with_one_other_card_offers_no_discard() {
    let (mut runner, spell, others) = thrilling_discovery_board(1);
    let life_before = runner.life(P0);
    let library_before = library_len(&runner, P0);

    cast(&mut runner, spell);
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.life(P0),
        life_before + 2,
        "reach: the spell resolved"
    );
    assert_settled_priority(&runner, "a two-card discard from one card is never offered");
    assert_eq!(
        runner.state().objects[&others[0]].zone,
        Zone::Hand,
        "nothing discarded"
    );
    assert_eq!(
        library_len(&runner, P0),
        library_before,
        "the rider drew nothing"
    );
    assert_eq!(hand_len(&runner, P0), 1);

    // Positive control: two other cards pay the discard, so it is offered.
    let (mut runner, spell, _) = thrilling_discovery_board(2);
    let life_before = runner.life(P0);
    cast(&mut runner, spell);
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.life(P0),
        life_before + 2,
        "reach: the life gain resolved first"
    );
    assert_optional_prompt(&runner, P0, "two cards pay a two-card discard");
}

/// Lim-Dûl's Paladin under P1's control at P1's upkeep, with `hand_cards` cards
/// in P1's hand and a card to draw.
fn paladin_upkeep(hand_cards: usize) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let paladin = scenario
        .add_creature_from_oracle(P1, "Lim-Dûl's Paladin", 0, 3, LIM_DULS_PALADIN)
        .id();
    for n in 0..hand_cards {
        scenario.add_card_to_hand(P1, &format!("Fodder {n}"));
    }
    scenario.with_library_top(P1, &["Card A", "Card B"]);
    let mut runner = scenario.build();
    runner.advance_to_phase(Phase::Upkeep);
    assert_eq!(runner.state().active_player, P1, "reach: P1's upkeep");
    assert!(
        runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == paladin),
        "reach: the Paladin's upkeep trigger is on the stack"
    );
    (runner, paladin)
}

/// CR 118.12 + CR 608.2d: with an empty hand the Paladin's "you may discard a
/// card" can't be chosen, so its "If you don't" branch runs: the Paladin is
/// sacrificed and P1 draws a card. With a card in hand the discard is offered.
#[test]
fn lim_duls_paladin_empty_hand_runs_its_if_you_dont_branch() {
    let (mut runner, paladin) = paladin_upkeep(0);
    let library_before = library_len(&runner, P1);
    runner.advance_until_stack_empty();
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "an empty-hand discard is never offered, got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty(), "the trigger resolved");
    assert_eq!(
        runner.state().objects[&paladin].zone,
        Zone::Graveyard,
        "the If you don't branch sacrificed the Paladin"
    );
    assert_eq!(hand_len(&runner, P1), 1, "and drew a card");
    assert_eq!(library_len(&runner, P1), library_before - 1);

    // Positive control: a card in hand makes the discard offerable.
    let (mut runner, _) = paladin_upkeep(1);
    runner.advance_until_stack_empty();
    assert_optional_prompt(&runner, P1, "a card in hand pays the discard");
}

const CHECKS_AND_BALANCES: &str = "Cast this spell only if there are three or more players in the game.\nWhenever a player casts a spell, each of that player's opponents may discard a card. If they do, counter that spell.";

/// CR 118.12 + CR 118.3 + CR 608.2d, per seat: Checks and Balances fans "may discard
/// a card" out to each opponent of the caster (CR 608.2c). The empty-handed opponent
/// can't pay, so only the opponent holding a card is asked; when that opponent
/// declines, nobody discarded and the spell is not countered.
#[test]
fn checks_and_balances_asks_only_the_opponent_who_can_discard() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let p2 = PlayerId(2);
    scenario.add_enchantment_from_oracle(P0, "Checks and Balances", CHECKS_AND_BALANCES);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Draw", false, "Draw a card.")
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let fodder = scenario.add_card_to_hand(p2, "Fodder");
    scenario.with_library_top(P0, &["Card A", "Card B"]);
    let mut runner = scenario.build();
    assert_eq!(
        hand_len(&runner, P1),
        0,
        "test precondition: P1 holds no card"
    );
    let library_before = library_len(&runner, P0);

    cast(&mut runner, spell);
    runner.advance_until_stack_empty();
    assert_optional_prompt(
        &runner,
        p2,
        "only the opponent who can pay the discard is asked",
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("P2 declines the discard");
    runner.advance_until_stack_empty();
    assert_settled_priority(&runner, "no other opponent is asked");
    assert_eq!(
        runner.state().objects[&fodder].zone,
        Zone::Hand,
        "P2 kept its card"
    );
    assert_eq!(
        library_len(&runner, P0),
        library_before - 1,
        "the spell resolved and drew its card instead of being countered"
    );
}
