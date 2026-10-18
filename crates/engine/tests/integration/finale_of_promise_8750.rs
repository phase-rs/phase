//! Issue #8750 — Finale of Promise.
//!
//! Oracle:
//!   You may cast up to one target instant card and/or up to one target sorcery
//!   card from your graveyard each with mana value X or less without paying
//!   their mana costs. If a spell cast this way would be put into your
//!   graveyard, exile it instead. If X is 10 or more, copy each of those spells
//!   twice. You may choose new targets for the copies.
//!
//! Rulings this file pins: the chosen cards are cast as Finale resolves, in
//! either order; with X 10 or more each of them is then copied twice, and the
//! copies go on the stack in any order; the copies are not cast.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastOfferKind, CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const FINALE: &str = "You may cast up to one target instant card and/or up to one target \
sorcery card from your graveyard each with mana value X or less without paying their mana \
costs. If a spell cast this way would be put into your graveyard, exile it instead. If X is \
10 or more, copy each of those spells twice. You may choose new targets for the copies.";

struct Board {
    runner: GameRunner,
    finale: ObjectId,
    /// Instant, mana value 2: "You gain 2 life."
    instant: ObjectId,
    /// Sorcery, mana value 3: "You gain 3 life."
    sorcery: ObjectId,
}

/// Finale of Promise in hand with exactly `{X}{R}{R}` for the given X in the
/// pool, and an instant (mana value 2) and a sorcery (mana value 3) in the
/// graveyard. `instant_text` replaces the instant's rules text.
fn board(x: u32, instant_text: &str) -> Board {
    board_with(x, instant_text, |_| {})
}

/// `board`, with `extra` adding to the scenario before it is built.
fn board_with(x: u32, instant_text: &str, extra: impl FnOnce(&mut GameScenario)) -> Board {
    board_with_sorcery(x, instant_text, "You gain 3 life.", extra)
}

/// `board_with`, with `sorcery_text` as the sorcery's rules text.
fn board_with_sorcery(
    x: u32,
    instant_text: &str,
    sorcery_text: &str,
    extra: impl FnOnce(&mut GameScenario),
) -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let finale = scenario
        .add_spell_to_hand_from_oracle(P0, "Finale of Promise", false, FINALE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Red, ManaCostShard::Red],
            generic: 0,
        })
        .id();
    let instant = scenario
        .add_spell_to_graveyard(P0, "Graveyard Instant", true)
        .with_mana_cost(ManaCost::generic(2))
        .from_oracle_text(instant_text)
        .id();
    let sorcery = scenario
        .add_spell_to_graveyard(P0, "Graveyard Sorcery", false)
        .with_mana_cost(ManaCost::generic(3))
        .from_oracle_text(sorcery_text)
        .id();
    let source = ObjectId(0);
    let mut pool = vec![
        ManaUnit::new(ManaType::Red, source, false, vec![]),
        ManaUnit::new(ManaType::Red, source, false, vec![]),
    ];
    pool.extend((0..x).map(|_| ManaUnit::new(ManaType::Colorless, source, false, vec![])));
    scenario.with_mana_pool(P0, pool);
    extra(&mut scenario);
    Board {
        runner: scenario.build(),
        finale,
        instant,
        sorcery,
    }
}

/// Cast Finale with X, choosing `picks[i]` for target slot `i` (instant slot,
/// then sorcery slot; `None` chooses no target), and pass priority until it
/// resolves to its cast window. Returns each slot's legal targets.
fn cast_finale(b: &mut Board, x: u32, picks: [Option<ObjectId>; 2]) -> Vec<Vec<TargetRef>> {
    let card_id = b.runner.state().objects[&b.finale].card_id;
    b.runner
        .act(GameAction::CastSpell {
            object_id: b.finale,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Finale of Promise must begin");
    let mut legal = Vec::new();
    for _ in 0..16 {
        let action = match &b.runner.state().waiting_for {
            WaitingFor::ChooseXValue { .. } => GameAction::ChooseX { value: x },
            WaitingFor::TargetSelection { target_slots, .. } => {
                let slot = legal.len();
                legal.push(target_slots[slot].legal_targets.clone());
                GameAction::ChooseTarget {
                    target: picks[slot].map(TargetRef::Object),
                }
            }
            WaitingFor::Priority { .. } if !b.runner.state().stack.is_empty() => {
                GameAction::PassPriority
            }
            // The window's decline is the "you may"; Finale asks nothing first.
            WaitingFor::OptionalEffectChoice { .. } => {
                panic!("Finale must not ask \"you may\" before its cast window")
            }
            _ => {
                // CR 608.2g: Finale is still resolving while its window is open.
                assert_eq!(zone(&b.runner, b.finale), Zone::Stack);
                return legal;
            }
        };
        b.runner
            .act(action)
            .expect("casting and resolving Finale must succeed");
    }
    panic!("Finale of Promise never reached its cast window");
}

fn window_candidates(runner: &GameRunner) -> Vec<ObjectId> {
    match &runner.state().waiting_for {
        WaitingFor::CastOffer {
            player: P0,
            kind: CastOfferKind::FreeCastWindow { candidates, .. },
        } => candidates.clone(),
        other => panic!("expected Finale's cast window, got {other:?}"),
    }
}

fn cast_from_window(runner: &mut GameRunner, selection: Option<ObjectId>) {
    runner
        .act(GameAction::FreeCastWindowChoice { selection })
        .expect("the window choice must succeed");
}

/// CR 405.3: answer the copy-order prompts with `picks`, one per prompt, and
/// return how many prompts there were. Each prompt must offer its pick.
fn order_copies(runner: &mut GameRunner, picks: &[ObjectId]) -> usize {
    let mut asked = 0;
    while let WaitingFor::SpellCopyOrderChoice {
        player, choices, ..
    } = &runner.state().waiting_for
    {
        assert_eq!(*player, P0);
        let pick = picks[asked];
        assert!(choices.contains(&pick), "{pick:?} not offered: {choices:?}");
        runner
            .act(GameAction::SelectCards { cards: vec![pick] })
            .expect("a copy-order pick must succeed");
        asked += 1;
    }
    asked
}

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

/// CR 601.2c: each slot offers only its own card type, and only a card with
/// mana value X or less — X is announced before targets (CR 601.2b).
#[test]
fn each_slot_offers_only_its_own_type() {
    let mut b = board(3, "You gain 2 life.");
    let picks = [Some(b.instant), Some(b.sorcery)];
    let legal = cast_finale(&mut b, 3, picks);
    assert_eq!(
        legal,
        vec![
            vec![TargetRef::Object(b.instant)],
            vec![TargetRef::Object(b.sorcery)],
        ]
    );
}

/// A card above X is not offered for its slot.
#[test]
fn a_card_above_x_is_not_offered() {
    let mut b = board(2, "You gain 2 life.");
    let picks = [Some(b.instant), None];
    let legal = cast_finale(&mut b, 2, picks);
    assert_eq!(legal[0], vec![TargetRef::Object(b.instant)]);
    assert!(
        legal[1..].iter().all(Vec::is_empty),
        "the sorcery's mana value 3 is above X = 2, so its slot offers nothing: {legal:?}"
    );
    assert_eq!(window_candidates(&b.runner), vec![b.instant]);
}

/// CR 608.2g + ruling: both chosen cards are cast while Finale resolves, in the
/// order the player picks, and each is exiled instead of going to the graveyard.
#[test]
fn both_targets_are_cast_in_either_order_and_exiled() {
    let mut b = board(3, "You gain 2 life.");
    let picks = [Some(b.instant), Some(b.sorcery)];
    cast_finale(&mut b, 3, picks);
    assert_eq!(window_candidates(&b.runner), vec![b.instant, b.sorcery]);
    cast_from_window(&mut b.runner, Some(b.sorcery));
    assert_eq!(zone(&b.runner, b.sorcery), Zone::Stack);
    assert_eq!(window_candidates(&b.runner), vec![b.instant]);
    cast_from_window(&mut b.runner, Some(b.instant));
    assert_eq!(zone(&b.runner, b.instant), Zone::Stack);
    b.runner.advance_until_stack_empty();

    assert_eq!(b.runner.state().players[0].life, 25);
    assert_eq!(zone(&b.runner, b.instant), Zone::Exile);
    assert_eq!(zone(&b.runner, b.sorcery), Zone::Exile);
    assert_eq!(
        zone(&b.runner, b.finale),
        Zone::Graveyard,
        "the exile rider names the spells cast this way, not Finale"
    );
}

/// "And/or": the player may cast one of the chosen cards and leave the other.
#[test]
fn a_chosen_card_can_be_left_in_the_graveyard() {
    let mut b = board(3, "You gain 2 life.");
    let picks = [Some(b.instant), Some(b.sorcery)];
    cast_finale(&mut b, 3, picks);
    cast_from_window(&mut b.runner, Some(b.instant));
    cast_from_window(&mut b.runner, None);
    b.runner.advance_until_stack_empty();

    assert_eq!(b.runner.state().players[0].life, 22);
    assert_eq!(zone(&b.runner, b.instant), Zone::Exile);
    assert_eq!(zone(&b.runner, b.sorcery), Zone::Graveyard);
}

/// CR 707.10: with X = 10 each spell cast this way is copied twice; the copies
/// resolve and cease to exist, the originals are exiled.
#[test]
fn x_ten_copies_each_spell_cast_this_way_twice() {
    let mut b = board(10, "You gain 2 life.");
    let picks = [Some(b.instant), Some(b.sorcery)];
    cast_finale(&mut b, 10, picks);
    cast_from_window(&mut b.runner, Some(b.instant));
    cast_from_window(&mut b.runner, Some(b.sorcery));
    assert_eq!(
        order_copies(&mut b.runner, &[b.instant, b.instant]),
        2,
        "no prompt once only the sorcery has copies left"
    );
    assert_eq!(
        b.runner.state().stack.len(),
        6,
        "two originals and two copies of each: {:?}",
        b.runner.state().stack
    );
    // CR 608.2n: the copies were the last of Finale's instructions, so it is
    // already in its graveyard while the spells it cast wait on the stack.
    assert_eq!(zone(&b.runner, b.finale), Zone::Graveyard);
    b.runner.advance_until_stack_empty();

    assert_eq!(b.runner.state().players[0].life, 20 + 3 * (2 + 3));
    assert_eq!(zone(&b.runner, b.instant), Zone::Exile);
    assert_eq!(zone(&b.runner, b.sorcery), Zone::Exile);
    assert_eq!(zone(&b.runner, b.finale), Zone::Graveyard);
}

/// "Those spells" are the spells cast this way: a chosen card the player did
/// not cast is not copied.
#[test]
fn x_ten_copies_only_the_card_that_was_cast() {
    let mut b = board(10, "You gain 2 life.");
    let picks = [Some(b.instant), Some(b.sorcery)];
    cast_finale(&mut b, 10, picks);
    cast_from_window(&mut b.runner, Some(b.instant));
    cast_from_window(&mut b.runner, None);
    b.runner.advance_until_stack_empty();

    assert_eq!(b.runner.state().players[0].life, 20 + 3 * 2);
    assert_eq!(zone(&b.runner, b.sorcery), Zone::Graveyard);
}

/// Below 10 nothing is copied.
#[test]
fn x_nine_copies_nothing() {
    let mut b = board(9, "You gain 2 life.");
    let picks = [Some(b.instant), Some(b.sorcery)];
    cast_finale(&mut b, 9, picks);
    cast_from_window(&mut b.runner, Some(b.instant));
    cast_from_window(&mut b.runner, Some(b.sorcery));
    assert_eq!(b.runner.state().stack.len(), 2);
    b.runner.advance_until_stack_empty();

    assert_eq!(b.runner.state().players[0].life, 25);
}

/// CR 707.10c: each copy of a targeted spell offers its controller new targets.
/// Keeping the original target sends all three instances at the opponent.
#[test]
fn each_copy_of_a_targeted_spell_may_choose_new_targets() {
    let mut b = board(10, "This spell deals 2 damage to target player.");
    let picks = [Some(b.instant), None];
    cast_finale(&mut b, 10, picks);
    cast_from_window(&mut b.runner, Some(b.instant));
    let mut retargets = 0;
    for _ in 0..16 {
        let action = match &b.runner.state().waiting_for {
            WaitingFor::TargetSelection { .. } => GameAction::ChooseTarget {
                target: Some(TargetRef::Player(P1)),
            },
            WaitingFor::CopyRetarget { .. } => {
                retargets += 1;
                GameAction::KeepAllCopyTargets
            }
            _ => break,
        };
        b.runner
            .act(action)
            .expect("targeting the instant and its copies must succeed");
    }
    assert_eq!(retargets, 2, "one retarget choice per copy");
    b.runner.advance_until_stack_empty();

    assert_eq!(b.runner.state().players[1].life, 20 - 3 * 2);
    assert_eq!(zone(&b.runner, b.instant), Zone::Exile);
}

/// With no instant chosen, the sorcery alone is still cast through the window
/// and, at X = 10, copied twice.
#[test]
fn only_the_sorcery_slot_chosen_is_cast_and_copied() {
    let mut b = board(10, "You gain 2 life.");
    let picks = [None, Some(b.sorcery)];
    cast_finale(&mut b, 10, picks);
    assert_eq!(window_candidates(&b.runner), vec![b.sorcery]);
    cast_from_window(&mut b.runner, Some(b.sorcery));
    b.runner.advance_until_stack_empty();

    assert_eq!(b.runner.state().players[0].life, 20 + 3 * 3);
    assert_eq!(zone(&b.runner, b.sorcery), Zone::Exile);
    assert_eq!(zone(&b.runner, b.instant), Zone::Graveyard);
}

/// Ruling: a card that is both an instant and a sorcery (a split card off the
/// stack, CR 709.4c) may be chosen for both slots, but once cast it can't be
/// cast a second time — and it is one of "those spells" once, copied twice.
#[test]
fn a_card_chosen_for_both_slots_is_cast_and_copied_once() {
    let mut split = ObjectId(0);
    let mut b = board_with(10, "You gain 2 life.", |scenario| {
        split = scenario
            .add_spell_to_graveyard(P0, "Instant and Sorcery", true)
            .as_sorcery()
            .with_mana_cost(ManaCost::generic(1))
            .from_oracle_text("You gain 1 life.")
            .id();
    });
    let picks = [Some(split), Some(split)];
    cast_finale(&mut b, 10, picks);
    match &b.runner.state().waiting_for {
        WaitingFor::CastOffer {
            kind:
                CastOfferKind::FreeCastWindow {
                    candidates,
                    remaining_casts,
                    ..
                },
            ..
        } => {
            assert_eq!(*candidates, vec![split]);
            assert_eq!(*remaining_casts, Some(1));
        }
        other => panic!("expected Finale's cast window, got {other:?}"),
    }
    cast_from_window(&mut b.runner, Some(split));
    b.runner.advance_until_stack_empty();

    assert_eq!(b.runner.state().players[0].life, 20 + 3);
    assert_eq!(zone(&b.runner, split), Zone::Exile);
}

/// CR 614.1a + CR 707.10: Twinning Staff — "If you would copy a spell one or
/// more times, instead copy it that many times plus an additional time." Each
/// spell's copying is its own event, so each spell gets three copies.
#[test]
fn twinning_staff_adds_a_copy_to_each_spell() {
    let mut b = board_with(10, "You gain 2 life.", |scenario| {
        scenario.add_artifact_from_oracle(
            P0,
            "Twinning Staff",
            "If you would copy a spell one or more times, instead copy it that many times plus an additional time. You may choose new targets for the additional copy.\n{7}, {T}: Copy target instant or sorcery spell you control. You may choose new targets for the copy.",
        );
    });
    let picks = [Some(b.instant), Some(b.sorcery)];
    cast_finale(&mut b, 10, picks);
    cast_from_window(&mut b.runner, Some(b.instant));
    cast_from_window(&mut b.runner, Some(b.sorcery));
    assert_eq!(
        order_copies(&mut b.runner, &[b.instant, b.instant, b.instant]),
        3
    );
    assert_eq!(
        b.runner.state().stack.len(),
        2 + 2 * 3,
        "two originals and three copies of each: {:?}",
        b.runner.state().stack
    );
    b.runner.advance_until_stack_empty();

    assert_eq!(b.runner.state().players[0].life, 20 + 4 * (2 + 3));
}

/// CR 405.3 + ruling: the copies of both spells go on the stack in any order,
/// interleaved too, and the chosen order holds across each copy's retarget
/// choice (CR 707.10c). "Double your life total" and "gains 2 life" don't
/// commute, so the final life total shows the order.
#[test]
fn the_controller_interleaves_the_copies_across_retarget_choices() {
    let mut b = board_with_sorcery(
        10,
        "Target player gains 2 life.",
        "Double your life total.",
        |_| {},
    );
    let picks = [Some(b.instant), Some(b.sorcery)];
    cast_finale(&mut b, 10, picks);
    cast_from_window(&mut b.runner, Some(b.instant));
    let mut order_picks = vec![b.sorcery, b.instant, b.sorcery].into_iter();
    let mut prompts = Vec::new();
    for _ in 0..32 {
        let action = match &b.runner.state().waiting_for {
            WaitingFor::TargetSelection { .. } => GameAction::ChooseTarget {
                target: Some(TargetRef::Player(P0)),
            },
            WaitingFor::CastOffer { .. } => {
                cast_from_window(&mut b.runner, Some(b.sorcery));
                continue;
            }
            WaitingFor::SpellCopyOrderChoice { choices, .. } => {
                let pick = order_picks.next().expect("one pick per prompt");
                assert!(choices.contains(&pick));
                prompts.push("order");
                GameAction::SelectCards { cards: vec![pick] }
            }
            WaitingFor::CopyRetarget { .. } => {
                prompts.push("retarget");
                GameAction::KeepAllCopyTargets
            }
            _ => break,
        };
        b.runner.act(action).expect("each choice must succeed");
    }
    assert_eq!(
        prompts,
        ["order", "order", "retarget", "order", "retarget"],
        "the sorcery's copies have no targets; the last instant copy needs no order prompt"
    );
    assert_eq!(b.runner.state().stack.len(), 6);
    b.runner.advance_until_stack_empty();

    // Bottom to top: instant, sorcery, then the copies sorcery, instant,
    // sorcery, instant. Top first: 20 +2 x2 +2 x2 x2 +2 = 186. The fixed
    // grouping (instant, instant, sorcery, sorcery) would give 170.
    assert_eq!(b.runner.state().players[0].life, 186);
}
