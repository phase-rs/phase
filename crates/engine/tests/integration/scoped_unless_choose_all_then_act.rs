//! Runtime regression for GitHub issue #11076 — a player-scoped "[instruction]
//! unless [that player] sacrifices / discards …" polls every affected player
//! before anything happens.
//!
//! CR 118.12a reads "[do something] unless [a player does something else]" as
//! "[that player] may [do something else]. If they don't, [do something]".
//! CR 101.4: with several players choosing, the active player chooses, then each
//! other player in turn order, each knowing the earlier choices (CR 101.4b),
//! and then the actions happen simultaneously. So no chosen permanent is
//! sacrificed, no chosen card discarded and no life lost until the last affected
//! player has answered.
//!
//! Every test drives verbatim printed Oracle text through the live parser and
//! answers the production prompts with public `GameAction`s.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::visibility::filter_state_for_viewer;
use engine::types::ability::AbilityCost;
use engine::types::actions::{GameAction, UnlessCostBranch};
use engine::types::game_state::{PlayerScopeUnlessAction, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

const BELLOWING_MAULER: &str = "At the beginning of your end step, each player loses 4 life unless they sacrifice a nontoken creature of their choice.";

const HAG_OF_CEASELESS_TORMENT: &str = "At the beginning of your upkeep, each opponent loses 3 life unless that player sacrifices a nonland permanent or discards a card.";

const COURT_OF_AMBITION: &str = "When this enchantment enters, you become the monarch.\nAt the beginning of your upkeep, each opponent loses 3 life unless they discard a card. If you're the monarch, instead each opponent loses 6 life unless they discard two cards.";

const OB_NIXILIS_THE_ADVERSARY: &str = "Casualty X. The copy isn't legendary and has starting loyalty X. (As you cast this spell, you may sacrifice a creature with power X. When you do, copy this spell. The copy becomes a token.)\n[+1]: Each opponent loses 2 life unless they discard a card. If you control a Demon or Devil, you gain 2 life.\n[−2]: Create a 1/1 red Devil creature token with \"When this token dies, it deals 1 damage to any target.\"\n[−7]: Target player draws seven cards and loses 7 life.";

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

/// The poll and every per-seat prompt are gone, and the game is back at priority.
fn assert_settled(runner: &GameRunner) {
    assert!(
        runner
            .state()
            .pending_player_scope_unless_punishment
            .is_none(),
        "the poll must be retired once it has settled"
    );
    assert!(runner.state().stack.is_empty(), "the trigger has resolved");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "expected priority after settlement, got {:?}",
        runner.state().waiting_for
    );
}

fn expect_unless_prompt(runner: &GameRunner, player: PlayerId) {
    match &runner.state().waiting_for {
        WaitingFor::UnlessPayment { player: actual, .. }
        | WaitingFor::UnlessPaymentChooseCost { player: actual, .. } => assert_eq!(
            *actual, player,
            "CR 101.4: the next player in APNAP order is asked"
        ),
        other => panic!("expected {player:?}'s unless prompt, got {other:?}"),
    }
}

/// The index of the branch of `player`'s live disjunctive prompt that `pick` selects.
fn branch_index(runner: &GameRunner, player: PlayerId, pick: fn(&AbilityCost) -> bool) -> usize {
    match &runner.state().waiting_for {
        WaitingFor::UnlessPaymentChooseCost {
            player: actual,
            costs,
            ..
        } if *actual == player => costs
            .iter()
            .position(pick)
            .expect("the printed branch reaches the prompt"),
        other => panic!("expected {player:?}'s choice of costs, got {other:?}"),
    }
}

fn select(runner: &mut GameRunner, card: ObjectId) {
    runner
        .act(GameAction::SelectCards { cards: vec![card] })
        .expect("the chosen object is a legal payment");
}

fn mauler_end_step(players: u8) -> (GameRunner, Vec<ObjectId>) {
    let mut scenario = GameScenario::new_n_player(players, 42);
    scenario.at_phase(Phase::PostCombatMain);
    scenario.add_creature_from_oracle(P0, "Bellowing Mauler", 4, 6, BELLOWING_MAULER);
    let bears = (0..players)
        .map(|seat| {
            scenario
                .add_creature(PlayerId(seat), &format!("Bear {seat}"), 2, 2)
                .id()
        })
        .collect();
    let mut runner = scenario.build();
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();
    (runner, bears)
}

/// CR 101.4: P0 chooses to sacrifice their Bear, but the Bear stays on the
/// battlefield while P1 makes their own choice, and P1 can see that choice
/// (CR 101.4b). Only after P1's answer is the Bear sacrificed and P1's life lost.
#[test]
fn mauler_sacrifice_waits_until_every_player_has_chosen() {
    let (mut runner, bears) = mauler_end_step(2);

    expect_unless_prompt(&runner, P0);
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("P0 chooses to sacrifice");
    match &runner.state().waiting_for {
        WaitingFor::WardSacrificeChoice {
            player, permanents, ..
        } => {
            assert_eq!(*player, P0);
            assert!(permanents.contains(&bears[0]), "P0's Bear is payable");
        }
        other => panic!("expected P0's sacrifice choice, got {other:?}"),
    }
    select(&mut runner, bears[0]);

    expect_unless_prompt(&runner, P1);
    assert_eq!(
        zone(&runner, bears[0]),
        Zone::Battlefield,
        "CR 101.4: nothing is sacrificed before the last player has chosen"
    );
    // CR 101.4b: the later chooser knows the earlier, public choice.
    let p1_view = filter_state_for_viewer(runner.state(), P1);
    let poll = p1_view
        .pending_player_scope_unless_punishment
        .as_deref()
        .expect("the open poll is projected to the next chooser");
    assert_eq!(poll.payments.len(), 1);
    assert_eq!(poll.payments[0].player, P0);
    assert_eq!(poll.payments[0].action, PlayerScopeUnlessAction::Sacrifice);
    assert_eq!(
        poll.payments[0]
            .objects
            .iter()
            .map(|pin| pin.object_id)
            .collect::<Vec<_>>(),
        vec![bears[0]],
        "P0's chosen Bear is visible to P1"
    );

    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("P1 declines");

    assert_eq!(zone(&runner, bears[0]), Zone::Graveyard, "P0 paid");
    assert_eq!(
        zone(&runner, bears[1]),
        Zone::Battlefield,
        "P1 paid nothing"
    );
    assert_eq!(runner.life(P0), 20, "paying spares P0");
    assert_eq!(runner.life(P1), 16, "declining costs P1 4 life");
    assert_settled(&runner);
}

/// CR 101.4 + CR 118.12a: Hag's two branches. P1 discards and P2 sacrifices;
/// P1's card stays in hand while P2 chooses, and P2 never sees which card it
/// is (CR 101.4a). Both payments happen only after P2 has answered.
#[test]
fn hag_discard_and_sacrifice_branches_settle_after_the_last_opponent() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::Untap);
    scenario.add_creature_from_oracle(
        P0,
        "Hag of Ceaseless Torment",
        2,
        2,
        HAG_OF_CEASELESS_TORMENT,
    );
    let p1_card = scenario.add_card_to_hand(P1, "P1 Filler");
    let p1_bear = scenario.add_creature(P1, "P1 Bear", 2, 2).id();
    scenario.add_card_to_hand(P2, "P2 Filler");
    let p2_bear = scenario.add_creature(P2, "P2 Bear", 2, 2).id();
    let mut runner = scenario.build();
    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();

    let discard = branch_index(&runner, P1, |cost| {
        matches!(cost, AbilityCost::Discard { .. })
    });
    runner
        .act(GameAction::ChooseUnlessCostBranch {
            choice: UnlessCostBranch::Pay { index: discard },
        })
        .expect("P1 chooses the discard branch");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::WardDiscardChoice { player: P1, .. }
        ),
        "P1 chooses which card, got {:?}",
        runner.state().waiting_for
    );
    select(&mut runner, p1_card);

    expect_unless_prompt(&runner, P2);
    assert_eq!(
        zone(&runner, p1_card),
        Zone::Hand,
        "CR 101.4: the chosen card is not discarded before every choice is made"
    );
    // CR 101.4a: the card P1 chose stays hidden from the other players.
    let p2_view = filter_state_for_viewer(runner.state(), P2);
    let p2_poll = p2_view
        .pending_player_scope_unless_punishment
        .as_deref()
        .expect("the open poll is projected");
    assert_eq!(p2_poll.payments[0].action, PlayerScopeUnlessAction::Discard);
    assert!(
        p2_poll.payments[0].objects.is_empty(),
        "P2 must not learn which card P1 chose"
    );
    let p1_view = filter_state_for_viewer(runner.state(), P1);
    assert_eq!(
        p1_view
            .pending_player_scope_unless_punishment
            .as_deref()
            .expect("the open poll is projected")
            .payments[0]
            .objects
            .len(),
        1,
        "the payer still sees their own choice"
    );

    let sacrifice = branch_index(&runner, P2, |cost| {
        matches!(cost, AbilityCost::Sacrifice(_))
    });
    runner
        .act(GameAction::ChooseUnlessCostBranch {
            choice: UnlessCostBranch::Pay { index: sacrifice },
        })
        .expect("P2 chooses the sacrifice branch");
    select(&mut runner, p2_bear);

    assert_eq!(zone(&runner, p1_card), Zone::Graveyard, "P1 discarded");
    assert_eq!(
        zone(&runner, p1_bear),
        Zone::Battlefield,
        "P1 sacrificed nothing"
    );
    assert_eq!(zone(&runner, p2_bear), Zone::Graveyard, "P2 sacrificed");
    for player in [P0, P1, P2] {
        assert_eq!(runner.life(player), 20, "nobody declined");
    }
    assert_settled(&runner);
}

/// CR 118.3 + CR 118.12a: Court of Ambition's monarch branch asks for two
/// cards. Both picks are collected before anything is discarded, and the
/// second pick is offered from the hand less the first.
#[test]
fn court_of_ambition_monarch_branch_collects_both_discards() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    scenario
        .add_creature(P0, "Court of Ambition", 0, 0)
        .as_enchantment()
        .from_oracle_text(COURT_OF_AMBITION);
    let first = scenario.add_card_to_hand(P1, "First Card");
    let second = scenario.add_card_to_hand(P1, "Second Card");
    let kept = scenario.add_card_to_hand(P1, "Kept Card");
    let mut runner = scenario.build();
    runner.state_mut().monarch = Some(P0);
    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();

    expect_unless_prompt(&runner, P1);
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("P1 chooses to discard");
    match &runner.state().waiting_for {
        WaitingFor::WardDiscardChoice {
            player, remaining, ..
        } => {
            assert_eq!(*player, P1);
            assert_eq!(*remaining, 2, "the monarch branch asks for two cards");
        }
        other => panic!("expected P1's discard choice, got {other:?}"),
    }
    select(&mut runner, first);

    match &runner.state().waiting_for {
        WaitingFor::WardDiscardChoice {
            player,
            cards,
            remaining,
            ..
        } => {
            assert_eq!(*player, P1);
            assert_eq!(*remaining, 1, "one card is still owed");
            assert!(
                !cards.contains(&first),
                "a picked card is not offered again"
            );
            assert!(cards.contains(&second) && cards.contains(&kept));
        }
        other => panic!("expected P1's second discard choice, got {other:?}"),
    }
    assert_eq!(
        zone(&runner, first),
        Zone::Hand,
        "a partial payment moves nothing"
    );
    select(&mut runner, second);

    assert_eq!(zone(&runner, first), Zone::Graveyard);
    assert_eq!(zone(&runner, second), Zone::Graveyard);
    assert_eq!(
        zone(&runner, kept),
        Zone::Hand,
        "exactly two cards are discarded"
    );
    assert_eq!(runner.life(P1), 20, "the full payment spares P1");
    assert_settled(&runner);
}

/// CR 608.2c: Ob Nixilis's +1 ends with an instruction that does not depend on
/// any opponent's choice. It resolves exactly once, after the poll, even when
/// every opponent pays — and also when one declines.
#[test]
fn ob_nixilis_controller_gains_life_once_whoever_pays() {
    for p2_pays in [true, false] {
        let mut scenario = GameScenario::new_n_player(3, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let walker = scenario
            .add_creature(P0, "Ob Nixilis, the Adversary", 0, 0)
            .as_planeswalker_with_loyalty("Nixilis", 3)
            .from_oracle_text(OB_NIXILIS_THE_ADVERSARY)
            .id();
        scenario
            .add_creature(P0, "Resident Demon", 2, 2)
            .with_subtypes(vec!["Demon"]);
        let p1_card = scenario.add_card_to_hand(P1, "P1 Filler");
        let p2_card = scenario.add_card_to_hand(P2, "P2 Filler");
        let mut runner = scenario.build();

        runner.activate(walker, 0).resolve();
        expect_unless_prompt(&runner, P1);
        runner
            .act(GameAction::PayUnlessCost { pay: true })
            .expect("P1 chooses to discard");
        select(&mut runner, p1_card);
        expect_unless_prompt(&runner, P2);
        assert_eq!(runner.life(P0), 20, "the gain waits for the whole poll");
        if p2_pays {
            runner
                .act(GameAction::PayUnlessCost { pay: true })
                .expect("P2 chooses to discard");
            select(&mut runner, p2_card);
        } else {
            runner
                .act(GameAction::PayUnlessCost { pay: false })
                .expect("P2 declines");
        }

        assert_eq!(
            runner.life(P0),
            22,
            "the controller gains 2 life exactly once"
        );
        assert_eq!(runner.life(P1), 20, "P1 paid");
        assert_eq!(zone(&runner, p1_card), Zone::Graveyard);
        if p2_pays {
            assert_eq!(runner.life(P2), 20, "P2 paid");
            assert_eq!(zone(&runner, p2_card), Zone::Graveyard);
        } else {
            assert_eq!(runner.life(P2), 18, "P2 declined");
            assert_eq!(zone(&runner, p2_card), Zone::Hand);
        }
        assert_settled(&runner);
    }
}

/// CR 104.3a + CR 800.4a + CR 101.4: the player being asked concedes. They
/// make no choice; the next player still in the game is asked, and the earlier
/// answer still counts.
#[test]
fn mauler_poll_moves_past_a_prompted_player_who_concedes() {
    let (mut runner, _bears) = mauler_end_step(3);
    expect_unless_prompt(&runner, P0);
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("P0 declines");
    expect_unless_prompt(&runner, P1);

    runner
        .act(GameAction::Concede { player_id: P1 })
        .expect("a player may concede at any time");

    expect_unless_prompt(&runner, P2);
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("P2 declines");

    assert_eq!(runner.life(P0), 16, "P0's earlier decline still applies");
    assert_eq!(runner.life(P2), 16, "P2 declined");
    assert_settled(&runner);
}

/// CR 608.2m + CR 800.4a: the instruction's controller concedes while a later
/// player's prompt is open. The ability had already started to resolve, so it
/// continues to resolve fully: the surviving players still answer, the payer's
/// sacrifice happens, and the decliner loses 4 life. P0 makes no further
/// choice, and their earlier decline goes with them.
#[test]
fn mauler_poll_continues_after_its_controller_concedes() {
    let (mut runner, bears) = mauler_end_step(3);
    expect_unless_prompt(&runner, P0);
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("P0 declines");
    expect_unless_prompt(&runner, P1);

    runner
        .act(GameAction::Concede { player_id: P0 })
        .expect("a player may concede at any time");

    expect_unless_prompt(&runner, P1);
    assert!(
        runner
            .state()
            .pending_player_scope_unless_punishment
            .is_some(),
        "CR 608.2m: the poll outlives its controller"
    );
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("P1 chooses to sacrifice");
    select(&mut runner, bears[1]);
    expect_unless_prompt(&runner, P2);
    assert_eq!(
        zone(&runner, bears[1]),
        Zone::Battlefield,
        "CR 101.4: still nothing moves before the last surviving player has chosen"
    );
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("P2 declines");

    assert_eq!(zone(&runner, bears[1]), Zone::Graveyard, "P1 paid");
    assert_eq!(
        zone(&runner, bears[2]),
        Zone::Battlefield,
        "P2 paid nothing"
    );
    assert_eq!(runner.life(P1), 20, "paying spares P1");
    assert_eq!(runner.life(P2), 16, "declining costs P2 4 life");
    assert_settled(&runner);
}

const DAUTHI_VOIDWALKER: &str = "Shadow (This creature can block or be blocked by only creatures with shadow.)\nIf a card would be put into an opponent's graveyard from anywhere, instead exile it with a void counter on it.\n{T}, Sacrifice this creature: Choose an exiled card an opponent owns with a void counter on it. You may play it this turn without paying its mana cost.";
const REST_IN_PEACE: &str = "When this enchantment enters, exile all graveyards.\nIf a card or token would be put into a graveyard from anywhere, exile it instead.";
const FIERY_TEMPER: &str = "Fiery Temper deals 3 damage to any target.\nMadness {R} (If you discard this card, discard it into exile. When you do, cast it for its madness cost or put it into your graveyard.)";
const ASSAULT_SUIT: &str = "Equipped creature gets +2/+2, has haste, can't attack you or planeswalkers you control, and can't be sacrificed.\nAt the beginning of each opponent's upkeep, you may have that player gain control of equipped creature until end of turn. If you do, untap it.\nEquip {3}";
const RISHADAN_CUTPURSE: &str = "When this creature enters, each opponent sacrifices a permanent of their choice unless they pay {1}.";

/// CR 101.4 + CR 614.1: P0 chooses to sacrifice Dauthi Voidwalker and P1 chooses
/// P1's Mauler. Because all choices are collected before any permanent leaves
/// the battlefield and all sacrifices consult replacements simultaneously,
/// Voidwalker still replaces P1's Mauler's graveyard move with exile + void
/// counter.
#[test]
fn mauler_simultaneous_sacrifice_preserves_dauthi_voidwalker_replacement() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    scenario.add_creature_from_oracle(P0, "Bellowing Mauler", 4, 6, BELLOWING_MAULER);
    let walker = scenario
        .add_creature_from_oracle(P0, "Dauthi Voidwalker", 3, 2, DAUTHI_VOIDWALKER)
        .id();
    let p1_mauler = scenario
        .add_creature_from_oracle(P1, "Bellowing Mauler", 4, 6, BELLOWING_MAULER)
        .id();
    let mut runner = scenario.build();
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();

    expect_unless_prompt(&runner, P0);
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("P0 chooses to sacrifice");
    select(&mut runner, walker);

    expect_unless_prompt(&runner, P1);
    assert_eq!(zone(&runner, walker), Zone::Battlefield);
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("P1 chooses to sacrifice");
    select(&mut runner, p1_mauler);

    assert_eq!(zone(&runner, walker), Zone::Graveyard);
    assert_eq!(
        zone(&runner, p1_mauler),
        Zone::Exile,
        "Dauthi Voidwalker must replace the co-sacrificed Mauler's graveyard move"
    );
    assert_eq!(
        runner.state().objects[&p1_mauler]
            .counters
            .get(&engine::types::counter::CounterType::Generic(
                "void".to_string()
            ))
            .copied(),
        Some(1),
        "exiled Mauler must receive a void counter"
    );
    assert_settled(&runner);
}

/// CR 101.4 + CR 616.1: When both Rest in Peace and Dauthi Voidwalker apply to
/// P1's co-sacrificed Mauler, settlement suspends on P1's ReplacementChoice
/// and resumes cleanly once answered without repeating moves.
#[test]
fn mauler_simultaneous_sacrifice_with_rip_and_voidwalker_suspends_and_resumes_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    scenario.add_creature_from_oracle(P0, "Bellowing Mauler", 4, 6, BELLOWING_MAULER);
    let walker = scenario
        .add_creature_from_oracle(P0, "Dauthi Voidwalker", 3, 2, DAUTHI_VOIDWALKER)
        .id();
    let p1_mauler = scenario
        .add_creature_from_oracle(P1, "Bellowing Mauler", 4, 6, BELLOWING_MAULER)
        .id();
    scenario.add_enchantment_from_oracle(P0, "Rest in Peace", REST_IN_PEACE);
    let mut runner = scenario.build();
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();

    runner.act(GameAction::PayUnlessCost { pay: true }).unwrap();
    select(&mut runner, walker);
    runner.act(GameAction::PayUnlessCost { pay: true }).unwrap();
    select(&mut runner, p1_mauler);

    let voidwalker_index = match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice {
            player, candidates, ..
        } => {
            assert_eq!(*player, P1, "affected player P1 orders replacements");
            candidates
                .iter()
                .position(|c| {
                    c.source_name.contains("Voidwalker")
                        || c.description.to_lowercase().contains("void")
                })
                .expect("Dauthi Voidwalker must still be an available replacement")
        }
        other => panic!("expected ReplacementChoice for P1, got {other:?}"),
    };
    runner
        .act(GameAction::ChooseReplacement {
            index: voidwalker_index,
        })
        .expect("choosing Voidwalker resumes settlement");

    assert_eq!(zone(&runner, walker), Zone::Exile);
    assert_eq!(zone(&runner, p1_mauler), Zone::Exile);
    assert_eq!(
        runner.state().objects[&p1_mauler]
            .counters
            .get(&engine::types::counter::CounterType::Generic(
                "void".to_string()
            ))
            .copied(),
        Some(1),
        "choosing Dauthi Voidwalker's replacement puts a void counter on P1's Mauler"
    );
    assert_settled(&runner);
}

/// Answer the open CR 616.1 ordering prompt with the replacement `source`
/// generates, after checking that `chooser` is asked and that it applies.
fn choose_replacement_from(runner: &mut GameRunner, chooser: PlayerId, source: ObjectId) {
    let index = match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice {
            player, candidates, ..
        } => {
            assert_eq!(
                *player, chooser,
                "CR 616.1: the affected permanent's controller orders the replacements"
            );
            candidates
                .iter()
                .position(|candidate| candidate.source_id == source)
                .unwrap_or_else(|| {
                    panic!("{source:?}'s replacement must apply to {chooser:?}'s sacrifice")
                })
        }
        other => panic!("expected {chooser:?}'s replacement choice, got {other:?}"),
    };
    runner
        .act(GameAction::ChooseReplacement { index })
        .expect("the offered replacement is a legal answer");
}

fn void_counters(runner: &GameRunner, id: ObjectId) -> Option<u32> {
    runner.state().objects[&id]
        .counters
        .get(&engine::types::counter::CounterType::Generic(
            "void".to_string(),
        ))
        .copied()
}

/// Both players sacrifice their Dauthi Voidwalker, and P1 controls Rest in
/// Peace. CR 616.1: P0 orders the two replacements for P0's Voidwalker (Rest in
/// Peace, P1's Voidwalker) and picks Rest in Peace. CR 101.4: that answer moves
/// nothing yet, so P0's Voidwalker is still on the battlefield when P1's
/// sacrifice is consulted, and P1 can apply it: P1's Voidwalker is exiled with a
/// void counter. Then both move at once.
fn assert_answer_waits_for_the_whole_batch(
    runner: &mut GameRunner,
    p0_walker: ObjectId,
    p1_walker: ObjectId,
    rest_in_peace: ObjectId,
) {
    choose_replacement_from(runner, P0, rest_in_peace);
    assert_eq!(
        zone(runner, p0_walker),
        Zone::Battlefield,
        "CR 101.4: an answered replacement does not move its permanent ahead of the batch"
    );
    assert_eq!(zone(runner, p1_walker), Zone::Battlefield);

    choose_replacement_from(runner, P1, p0_walker);

    assert_eq!(zone(runner, p0_walker), Zone::Exile, "Rest in Peace");
    assert_eq!(void_counters(runner, p0_walker), None);
    assert_eq!(zone(runner, p1_walker), Zone::Exile);
    assert_eq!(
        void_counters(runner, p1_walker),
        Some(1),
        "P0's co-sacrificed Voidwalker replaced P1's Voidwalker's move"
    );
}

/// CR 101.4 + CR 616.1: the answer to one sacrifice's replacement ordering
/// waits for the rest of Bellowing Mauler's batch to be consulted.
#[test]
fn mauler_replacement_answer_waits_until_every_sacrifice_is_consulted() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    scenario.add_creature_from_oracle(P0, "Bellowing Mauler", 4, 6, BELLOWING_MAULER);
    let p0_walker = scenario
        .add_creature_from_oracle(P0, "Dauthi Voidwalker", 3, 2, DAUTHI_VOIDWALKER)
        .id();
    let p1_walker = scenario
        .add_creature_from_oracle(P1, "Dauthi Voidwalker", 3, 2, DAUTHI_VOIDWALKER)
        .id();
    let rest_in_peace = scenario
        .add_enchantment_from_oracle(P1, "Rest in Peace", REST_IN_PEACE)
        .id();
    let mut runner = scenario.build();
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();

    expect_unless_prompt(&runner, P0);
    runner.act(GameAction::PayUnlessCost { pay: true }).unwrap();
    select(&mut runner, p0_walker);
    expect_unless_prompt(&runner, P1);
    runner.act(GameAction::PayUnlessCost { pay: true }).unwrap();
    select(&mut runner, p1_walker);

    assert_answer_waits_for_the_whole_batch(&mut runner, p0_walker, p1_walker, rest_in_peace);
    assert_eq!(runner.life(P0), 20, "P0 paid");
    assert_eq!(runner.life(P1), 20, "P1 paid");
    assert_settled(&runner);
}

const INNOCENT_BLOOD: &str = "Each player sacrifices a creature of their choice.";

/// CR 101.4 + CR 616.1: the same holds for a plain "each player sacrifices"
/// batch, which shares the simultaneous sacrifice authority.
#[test]
fn innocent_blood_replacement_answer_waits_until_every_sacrifice_is_consulted() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let blood = scenario
        .add_spell_to_hand_from_oracle(P0, "Innocent Blood", false, INNOCENT_BLOOD)
        .with_mana_cost(engine::types::mana::ManaCost::Cost {
            shards: vec![engine::types::mana::ManaCostShard::Black],
            generic: 0,
        })
        .id();
    let swamp = scenario.add_basic_land(P0, engine::types::mana::ManaColor::Black);
    let p0_walker = scenario
        .add_creature_from_oracle(P0, "Dauthi Voidwalker", 3, 2, DAUTHI_VOIDWALKER)
        .id();
    let p0_bear = scenario.add_creature(P0, "P0 Bear", 2, 2).id();
    let p1_walker = scenario
        .add_creature_from_oracle(P1, "Dauthi Voidwalker", 3, 2, DAUTHI_VOIDWALKER)
        .id();
    let p1_bear = scenario.add_creature(P1, "P1 Bear", 2, 2).id();
    let rest_in_peace = scenario
        .add_enchantment_from_oracle(P1, "Rest in Peace", REST_IN_PEACE)
        .id();
    let mut runner = scenario.build();

    runner.cast(blood).commit();
    assert!(
        runner.state().objects[&swamp].tapped,
        "Innocent Blood's {{B}} was paid"
    );
    for _ in 0..2 {
        runner.act(GameAction::PassPriority).unwrap();
    }
    for (player, walker) in [(P0, p0_walker), (P1, p1_walker)] {
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::EffectZoneChoice { player: chooser, .. } if chooser == player
            ),
            "CR 101.4: {player:?} chooses in APNAP order, got {:?}",
            runner.state().waiting_for
        );
        select(&mut runner, walker);
    }

    assert_answer_waits_for_the_whole_batch(&mut runner, p0_walker, p1_walker, rest_in_peace);
    assert_eq!(zone(&runner, p0_bear), Zone::Battlefield);
    assert_eq!(zone(&runner, p1_bear), Zone::Battlefield);
    assert_settled(&runner);
}

/// CR 702.35a + CR 118.12: A deferred discard chosen during Hag's poll routes
/// through the outer Discard replacement pass at settlement (Madness exiles
/// Fiery Temper instead of putting it into the graveyard), then finishes the
/// remaining settlement steps.
#[test]
fn hag_madness_discard_routes_through_discard_replacement() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::Untap);
    scenario.add_creature_from_oracle(
        P0,
        "Hag of Ceaseless Torment",
        2,
        2,
        HAG_OF_CEASELESS_TORMENT,
    );
    let temper = scenario
        .add_spell_to_hand(P1, "Fiery Temper", false)
        .with_mana_cost(engine::types::mana::ManaCost::Cost {
            shards: vec![engine::types::mana::ManaCostShard::Red],
            generic: 1,
        })
        .from_oracle_text_with_keywords(&["Madness"], FIERY_TEMPER)
        .id();
    scenario.add_card_to_hand(P1, "P1 Other Card");
    let p2_bear = scenario.add_creature(P2, "P2 Bear", 2, 2).id();
    let mut runner = scenario.build();
    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();

    let discard = branch_index(&runner, P1, |cost| {
        matches!(cost, AbilityCost::Discard { .. })
    });
    runner
        .act(GameAction::ChooseUnlessCostBranch {
            choice: UnlessCostBranch::Pay { index: discard },
        })
        .unwrap();
    select(&mut runner, temper);

    let sacrifice = branch_index(&runner, P2, |cost| {
        matches!(cost, AbilityCost::Sacrifice(_))
    });
    runner
        .act(GameAction::ChooseUnlessCostBranch {
            choice: UnlessCostBranch::Pay { index: sacrifice },
        })
        .unwrap();
    select(&mut runner, p2_bear);

    assert_eq!(
        zone(&runner, temper),
        Zone::Exile,
        "Madness must redirect the deferred discard of Fiery Temper into exile"
    );
    assert_eq!(zone(&runner, p2_bear), Zone::Graveyard);
    assert_eq!(runner.life(P1), 20);
    assert_eq!(runner.life(P2), 20);
}

/// CR 701.21 + CR 118.12a: A permanent with `CantBeSacrificed` (Assault Suit)
/// is excluded from the unless-sacrifice pool, so a player whose only creature
/// cannot be sacrificed declines and loses 4 life.
#[test]
fn mauler_excludes_cant_be_sacrificed_permanent_from_pool() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    scenario.add_creature_from_oracle(P0, "Bellowing Mauler", 4, 6, BELLOWING_MAULER);
    let p0_bear = scenario.add_creature(P0, "P0 Bear", 2, 2).id();
    let p1_suited = scenario.add_creature(P1, "Suited Creature", 2, 2).id();
    let suit = scenario
        .add_artifact_from_oracle(P1, "Assault Suit", ASSAULT_SUIT)
        .with_subtypes(vec!["Equipment"])
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.objects.get_mut(&suit).unwrap().attached_to =
            Some(engine::game::game_object::AttachTarget::Object(p1_suited));
        state
            .objects
            .get_mut(&p1_suited)
            .unwrap()
            .attachments
            .push(suit);
        state.layers_dirty.mark_full();
    }
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();

    runner.act(GameAction::PayUnlessCost { pay: true }).unwrap();
    select(&mut runner, p0_bear);

    expect_unless_prompt(&runner, P1);
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("attempting to pay with only a CantBeSacrificed creature falls through to decline");

    assert_eq!(zone(&runner, p0_bear), Zone::Graveyard);
    assert_eq!(
        zone(&runner, p1_suited),
        Zone::Battlefield,
        "CantBeSacrificed creature must remain on the battlefield"
    );
    assert_eq!(
        runner.life(P1),
        16,
        "P1 could not sacrifice so P1 loses 4 life"
    );
    assert_settled(&runner);
}

/// CR 104.3a + CR 800.4a + CR 101.4: when the final prompted payer concedes,
/// earlier surviving payers' choices settle immediately.
#[test]
fn mauler_poll_settles_earlier_answers_when_final_payer_concedes() {
    let (mut runner, bears) = mauler_end_step(3);
    expect_unless_prompt(&runner, P0);
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("P0 chooses to sacrifice");
    select(&mut runner, bears[0]);

    expect_unless_prompt(&runner, P1);
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("P1 declines");

    expect_unless_prompt(&runner, P2);
    assert_eq!(zone(&runner, bears[0]), Zone::Battlefield);
    assert_eq!(runner.life(P1), 20);

    runner
        .act(GameAction::Concede { player_id: P2 })
        .expect("P2 concedes while prompted");

    assert_eq!(
        zone(&runner, bears[0]),
        Zone::Graveyard,
        "P0's sacrifice settles"
    );
    assert_eq!(runner.life(P0), 20);
    assert_eq!(runner.life(P1), 16, "P1's decline settles");
    assert_settled(&runner);
}

/// CR 608.2c: Mana-gated scoped unless (Rishadan Cutpurse) stays outside the
/// selection poll and routes every decliner through their own sacrifice choice.
#[test]
fn rishadan_cutpurse_decliners_each_complete_sacrifice_choice() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let cutpurse = scenario
        .add_creature_to_hand_from_oracle(P0, "Rishadan Cutpurse", 1, 1, RISHADAN_CUTPURSE)
        .with_mana_cost(engine::types::mana::ManaCost::Cost {
            shards: vec![engine::types::mana::ManaCostShard::Blue],
            generic: 2,
        })
        .id();
    for _ in 0..3 {
        scenario.add_basic_land(P0, engine::types::mana::ManaColor::Blue);
    }
    let p1_a = scenario.add_basic_land(P1, engine::types::mana::ManaColor::Blue);
    let _p1_b = scenario.add_basic_land(P1, engine::types::mana::ManaColor::Blue);
    let p2_a = scenario.add_basic_land(P2, engine::types::mana::ManaColor::Blue);
    let _p2_b = scenario.add_basic_land(P2, engine::types::mana::ManaColor::Blue);
    let mut runner = scenario.build();

    runner.cast(cutpurse).commit();
    // Pass priority around the 3-player table to resolve Rishadan Cutpurse,
    // then again to resolve its ETB trigger.
    for _ in 0..6 {
        runner.act(GameAction::PassPriority).unwrap();
    }

    expect_unless_prompt(&runner, P1);
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .unwrap();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::EffectZoneChoice { player: P1, .. }
        ),
        "P1 must be prompted to sacrifice a permanent, got {:?}",
        runner.state().waiting_for
    );
    select(&mut runner, p1_a);

    expect_unless_prompt(&runner, P2);
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .unwrap();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::EffectZoneChoice { player: P2, .. }
        ),
        "P2 must also be prompted to sacrifice a permanent, got {:?}",
        runner.state().waiting_for
    );
    select(&mut runner, p2_a);

    assert_eq!(zone(&runner, p1_a), Zone::Graveyard);
    assert_eq!(zone(&runner, p2_a), Zone::Graveyard);
    assert_settled(&runner);
}
