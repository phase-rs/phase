//! Regression for issue #10949: Sword-Point Diplomacy never offered an
//! opponent the choice to pay life.
//!
//! CR 118.12a: "[effect] unless [player] pays [cost]" — the named player may pay,
//! and if they do the effect doesn't happen. CR 608.2c: "for each of those cards"
//! runs the instruction once per revealed card. CR 608.2f: the resolving
//! controller chooses the relative order of those instructions, so the cards
//! are ordered before any opponent is polled (the card's ruling: "You choose
//! the order to perform this process for the cards"). CR 101.4: with several
//! opponents, each chooses in turn order and the first to pay prevents the
//! effect.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::visibility::filter_state_for_viewer;
use engine::types::actions::GameAction;
use engine::types::game_state::{MemberOrderPurpose, WaitingFor};
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
    deep_card: ObjectId,
}

fn setup(player_count: u8) -> Setup {
    let mut scenario = GameScenario::new_n_player(player_count, 42);
    scenario.at_phase(Phase::PreCombatMain);
    // Library is built bottom-first: each add lands on top, so `revealed` is the
    // top three cards and "Deep Card" sits below them.
    let deep_card = scenario.add_card_to_library_top(P0, "Deep Card");
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
    Setup {
        runner,
        revealed,
        deep_card,
    }
}

/// CR 608.2f: while two or more cards are unprocessed, the controlling player
/// picks the next one before its payment prompt. Take the first offered card
/// (these tests are about the payments, not the order).
fn choose_processing_order(runner: &mut GameRunner) {
    if matches!(
        runner.state().waiting_for,
        WaitingFor::MemberOrderChoice { .. }
    ) {
        let first = order_prompt_choices(runner)[0];
        pick(runner, first);
    }
}

fn prompted_card(runner: &GameRunner) -> (PlayerId, ObjectId) {
    match &runner.state().waiting_for {
        WaitingFor::UnlessPayment {
            player,
            affected_object,
            ..
        } => (
            *player,
            affected_object.expect("affected_object should be set for member-driven unless loop"),
        ),
        other => panic!("expected WaitingFor::UnlessPayment, got {other:?}"),
    }
}

/// A completed payment sequence must leave the stack empty and hand priority
/// back — `advance_until_stack_empty` also stops at an unanswered
/// prompt, so zone assertions alone would accept a stranded continuation.
fn assert_resolution_finished(runner: &GameRunner) {
    assert!(
        runner.state().stack.is_empty(),
        "resolution must finish with an empty stack"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must return priority, got {:?}",
        runner.state().waiting_for
    );
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
        deep_card,
    } = setup(2);
    for _ in 0..3 {
        choose_processing_order(&mut runner);
        let (player, card) = prompted_card(&runner);
        assert_eq!(player, P1);
        assert!(
            revealed.contains(&card),
            "prompted card must be one of the revealed cards"
        );
        answer(&mut runner, false);
    }
    runner.advance_until_stack_empty();
    assert_resolution_finished(&runner);
    assert!(zones(&runner, &revealed).iter().all(|z| *z == Zone::Hand));
    assert_eq!(runner.state().objects[&deep_card].zone, Zone::Library);
    assert_eq!(runner.life(P1), 20);
}

#[test]
fn opponent_pays_every_card_is_exiled() {
    let Setup {
        mut runner,
        revealed,
        deep_card,
    } = setup(2);
    for _ in 0..3 {
        choose_processing_order(&mut runner);
        let (player, card) = prompted_card(&runner);
        assert_eq!(player, P1);
        assert!(
            revealed.contains(&card),
            "prompted card must be one of the revealed cards"
        );
        answer(&mut runner, true);
    }
    runner.advance_until_stack_empty();
    assert_resolution_finished(&runner);
    assert!(zones(&runner, &revealed).iter().all(|z| *z == Zone::Exile));
    assert_eq!(runner.state().objects[&deep_card].zone, Zone::Library);
    assert_eq!(runner.life(P1), 20 - 9);
}

/// The payer's own projected state (what a client or reconnect receives) names
/// the card each prompt decides, and that revealed card is visible to them.
#[test]
fn payer_view_names_the_card_each_prompt_decides() {
    let Setup {
        mut runner,
        revealed,
        ..
    } = setup(2);
    let mut seen = Vec::new();
    for _ in 0..3 {
        choose_processing_order(&mut runner);
        let (_, card) = prompted_card(&runner);
        let view = filter_state_for_viewer(runner.state(), P1);
        let WaitingFor::UnlessPayment {
            affected_object, ..
        } = &view.waiting_for
        else {
            panic!("payer view must still be UnlessPayment");
        };
        assert_eq!(*affected_object, Some(card));
        assert_eq!(view.objects[&card].name, runner.state().objects[&card].name);
        assert!(view.objects[&card].name.starts_with("Revealed "));
        seen.push(card);
        answer(&mut runner, false);
    }
    runner.advance_until_stack_empty();
    assert_resolution_finished(&runner);
    seen.sort();
    let mut expected = revealed.clone();
    expected.sort();
    assert_eq!(
        seen, expected,
        "each revealed card is prompted exactly once"
    );
}

/// Mixed answers: only the paid-for card is exiled; the rest are in hand, and the
/// card below the reveal is untouched.
#[test]
fn mixed_answers_split_cards_between_hand_and_exile() {
    let Setup {
        mut runner,
        revealed,
        deep_card,
    } = setup(2);
    // First card: pay -> should be exiled
    choose_processing_order(&mut runner);
    let (_, paid_card) = prompted_card(&runner);
    assert!(revealed.contains(&paid_card));
    answer(&mut runner, true);
    // Second card: decline -> should go to hand
    choose_processing_order(&mut runner);
    let (_, declined_card1) = prompted_card(&runner);
    assert!(revealed.contains(&declined_card1));
    assert_ne!(declined_card1, paid_card);
    answer(&mut runner, false);
    // Third card: decline -> should go to hand
    choose_processing_order(&mut runner);
    let (_, declined_card2) = prompted_card(&runner);
    assert!(revealed.contains(&declined_card2));
    assert_ne!(declined_card2, paid_card);
    assert_ne!(declined_card2, declined_card1);
    answer(&mut runner, false);
    runner.advance_until_stack_empty();
    assert_resolution_finished(&runner);
    // Verify card identity: paid card exiled, declined cards in hand
    assert_eq!(runner.state().objects[&paid_card].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&declined_card1].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&declined_card2].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&deep_card].zone, Zone::Library);
    assert_eq!(runner.life(P1), 17);
}

/// CR 118.12a + CR 101.4: any opponent may pay — if the first declines, the next
/// is asked about the same card, and the first to pay stops that card from
/// reaching the hand. Each card starts a fresh poll with the first opponent.
#[test]
fn second_opponent_may_pay_after_first_declines() {
    let Setup {
        mut runner,
        revealed,
        deep_card,
    } = setup(3);
    // First card: P1 declines, then P2 is asked about the same card and pays.
    choose_processing_order(&mut runner);
    let (player, paid_card) = prompted_card(&runner);
    assert_eq!(player, P1);
    assert!(revealed.contains(&paid_card));
    answer(&mut runner, false);
    let (player, card) = prompted_card(&runner);
    assert_eq!(player, P2);
    assert_eq!(card, paid_card, "P2 is asked about the card P1 declined");
    answer(&mut runner, true);
    assert_eq!(runner.life(P2), 17);
    assert_eq!(runner.life(P1), 20);
    // Each remaining card: a fresh poll — P1 then P2, both decline.
    let mut kept = Vec::new();
    for _ in 0..2 {
        choose_processing_order(&mut runner);
        let (player, card) = prompted_card(&runner);
        assert_eq!(
            player, P1,
            "each card's poll starts with the first opponent"
        );
        assert!(revealed.contains(&card));
        assert_ne!(card, paid_card);
        assert!(!kept.contains(&card));
        answer(&mut runner, false);
        let (player, same) = prompted_card(&runner);
        assert_eq!(player, P2);
        assert_eq!(same, card, "P2 is asked about the card P1 declined");
        answer(&mut runner, false);
        kept.push(card);
    }
    runner.advance_until_stack_empty();
    assert_resolution_finished(&runner);
    assert_eq!(runner.state().objects[&paid_card].zone, Zone::Exile);
    for card in &kept {
        assert_eq!(runner.state().objects[card].zone, Zone::Hand);
    }
    assert_eq!(runner.state().objects[&deep_card].zone, Zone::Library);
    assert_eq!(runner.life(P1), 20);
    assert_eq!(runner.life(P2), 17);
}

/// The open card-order prompt: asserts the resolving controller is asked and
/// returns the offered cards, sorted.
fn order_prompt_choices(runner: &GameRunner) -> Vec<ObjectId> {
    match &runner.state().waiting_for {
        WaitingFor::MemberOrderChoice {
            player,
            choices,
            purpose,
            ..
        } => {
            assert_eq!(*player, P0, "the resolving controller orders the cards");
            assert_eq!(*purpose, MemberOrderPurpose::InstructionLoop);
            let mut choices = choices.clone();
            choices.sort();
            choices
        }
        other => panic!("expected the card-order prompt, got {other:?}"),
    }
}

fn sorted(ids: &[ObjectId]) -> Vec<ObjectId> {
    let mut ids = ids.to_vec();
    ids.sort();
    ids
}

fn pick(runner: &mut GameRunner, card: ObjectId) {
    runner
        .act(GameAction::SelectCards { cards: vec![card] })
        .expect("submit the next card to process");
}

/// CR 608.2f: the controller chooses the order the per-card process runs in, and
/// the payment prompts and movements follow that chosen order through every
/// pause. Processing the bottom-most revealed card first is the reverse of the
/// library order, so a library-order implementation cannot pass.
#[test]
fn controller_chooses_the_card_processing_order() {
    let Setup {
        mut runner,
        revealed,
        deep_card,
    } = setup(2);
    // `revealed` is deepest-first: revealed[0] is the last of the three in
    // library order. Before any opponent is polled, the controller is offered
    // every revealed card.
    assert_eq!(order_prompt_choices(&runner), sorted(&revealed));
    assert_eq!(runner.life(P1), 20, "no payment before the order choice");
    pick(&mut runner, revealed[0]);

    let (player, first) = prompted_card(&runner);
    assert_eq!(player, P1);
    assert_eq!(first, revealed[0], "the chosen card is processed first");
    answer(&mut runner, false);
    // The declined card moved at once; the unprocessed cards have not.
    assert_eq!(runner.state().objects[&revealed[0]].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&revealed[1]].zone, Zone::Library);
    assert_eq!(runner.state().objects[&revealed[2]].zone, Zone::Library);

    // After the pause, the order prompt offers only the unprocessed cards.
    assert_eq!(
        order_prompt_choices(&runner),
        sorted(&[revealed[1], revealed[2]])
    );
    pick(&mut runner, revealed[1]);
    let (player, second) = prompted_card(&runner);
    assert_eq!(player, P1);
    assert_eq!(second, revealed[1], "the next chosen card follows");
    answer(&mut runner, true);
    assert_eq!(runner.life(P1), 17);
    assert_eq!(
        runner.state().objects[&revealed[1]].zone,
        Zone::Library,
        "a paid-for card waits for \"Then exile the rest\""
    );

    // The last member has no ordering choice left.
    let (player, third) = prompted_card(&runner);
    assert_eq!(player, P1);
    assert_eq!(third, revealed[2], "the remaining card is processed last");
    answer(&mut runner, false);

    runner.advance_until_stack_empty();
    assert_resolution_finished(&runner);
    assert_eq!(runner.state().objects[&revealed[0]].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&revealed[1]].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&revealed[2]].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&deep_card].zone, Zone::Library);
    assert_eq!(runner.life(P1), 17);
}
