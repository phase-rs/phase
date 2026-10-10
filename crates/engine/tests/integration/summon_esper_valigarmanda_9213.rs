//! Issue #9213 — Summon: Esper Valigarmanda, driven through the real Saga
//! pipeline.
//!
//! Chapters II–IV print "Add {R} for each lore counter on this Saga. You may
//! cast an instant or sorcery card exiled with this Saga, and mana of any type
//! can be spent to cast that spell." The ruling: "You cast the spell while the
//! ability is resolving and still on the stack. You can't wait to cast it later
//! in the turn." (CR 608.2g). The cards are the ones chapter I exiled
//! (CR 607.2a: the two abilities are linked).
//!
//! Before this fix the cast clause was the gap `linked_exile_resolution_cast`
//! and nothing was offered. Now the chapter offers ONE linked card, chosen as
//! it resolves, at its printed cost, payable with the chapter's red mana.
//!
//! The Saga is built from its printed text (CI has no card database). Both lore
//! counters come from the CR 714.3c turn-based action, and chapter I exiles the
//! cards itself, so the exile link is the production one.

use engine::game::casting::spell_objects_available_to_cast;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Summon: Esper Valigarmanda, verbatim (`client/public/card-data.json`).
const VALIGARMANDA: &str = "(As this Saga enters and after your draw step, add a lore \
counter. Sacrifice after IV.)\n\
I — Exile an instant or sorcery card from each graveyard.\n\
II, III, IV — Add {R} for each lore counter on this Saga. You may cast an instant or sorcery \
card exiled with this Saga, and mana of any type can be spent to cast that spell.\n\
Flying, haste";

/// Park the game at the end of P0's turn, so the next two `advance_to_phase`
/// calls reach P1's, then P0's, precombat main (CR 714.3c).
fn park_for_next_p0_precombat_main(runner: &mut GameRunner) {
    let state = runner.state_mut();
    state.turn_number = 1;
    state.active_player = P0;
    state.phase = Phase::End;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };
}

fn to_next_p0_precombat_main(runner: &mut GameRunner) {
    park_for_next_p0_precombat_main(runner);
    runner.advance_to_phase(Phase::PreCombatMain);
    runner.pass_both_players();
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().active_player, P0);
}

/// Pass priority until the stack is empty or a non-priority prompt opens,
/// answering trigger-order prompts. Chapter I's per-card choices are answered
/// by `choose_chapter_one`.
fn settle(runner: &mut GameRunner) {
    for _ in 0..64 {
        match &runner.state().waiting_for {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            _ => return,
        }
    }
    panic!("stack did not settle: {:?}", runner.state().waiting_for);
}

/// Castable: "You gain 3 life." Uncastable while no spell is on the stack
/// (CR 601.2c): "Counter target spell."
const GAIN_THREE: &str = "You gain 3 life.";
const GAIN_ONE: &str = "You gain 1 life.";
const COUNTERSPELL: &str = "Counter target spell.";

/// A `{1}{W}` sorcery: red mana pays it only through the any-type rider.
fn white_sorcery(
    scenario: &mut GameScenario,
    owner: engine::types::player::PlayerId,
    text: &str,
) -> ObjectId {
    scenario
        .add_spell_to_graveyard(owner, "White Sorcery", false)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 1,
        })
        .from_oracle_text(text)
        .id()
}

/// A `{U}` instant.
fn blue_instant(
    scenario: &mut GameScenario,
    owner: engine::types::player::PlayerId,
    text: &str,
) -> ObjectId {
    scenario
        .add_spell_to_graveyard(owner, "Blue Instant", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        })
        .from_oracle_text(text)
        .id()
}

struct Board {
    runner: GameRunner,
    /// Exiled by chapter I from P1's graveyard.
    sorcery: ObjectId,
    /// Exiled by chapter I from P0's graveyard.
    instant: ObjectId,
    /// An instant already in exile, not linked to the Saga.
    unlinked: ObjectId,
    saga: ObjectId,
}

/// The Saga with no lore counter, an instant (`instant_text`) in P0's
/// graveyard and a sorcery (`sorcery_text`) in P1's. Chapter I runs on P0's
/// next precombat main, chapter II on the one after; returns with chapter II
/// resolving.
fn reach_chapter_two(instant_text: &str, sorcery_text: &str) -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest", "Forest"]);
    scenario.with_library_top(P1, &["Forest", "Forest", "Forest", "Forest"]);
    let saga = scenario
        .add_creature(P0, "Summon: Esper Valigarmanda", 6, 6)
        .as_enchantment()
        .with_subtypes(vec!["Saga"])
        .from_oracle_text(VALIGARMANDA)
        .id();
    let instant = blue_instant(&mut scenario, P0, instant_text);
    let sorcery = white_sorcery(&mut scenario, P1, sorcery_text);
    let unlinked = scenario
        .add_spell_to_exile(P0, "Unlinked Instant", true)
        .from_oracle_text("You gain 5 life.")
        .id();
    let mut runner = scenario.build();

    // Chapter I: the CR 714.3c lore counter triggers it; it exiles one
    // instant or sorcery card from each graveyard.
    to_next_p0_precombat_main(&mut runner);
    assert_eq!(lore(&runner, saga), 1);
    settle(&mut runner);
    choose_chapter_one(&mut runner, &[instant, sorcery]);
    settle(&mut runner);
    for card in [instant, sorcery] {
        assert_eq!(
            runner.state().objects[&card].zone,
            Zone::Exile,
            "reach guard: chapter I exiled {card:?}"
        );
    }

    // Chapter II.
    to_next_p0_precombat_main(&mut runner);
    assert_eq!(lore(&runner, saga), 2);
    settle(&mut runner);
    Board {
        runner,
        sorcery,
        instant,
        unlinked,
        saga,
    }
}

/// Answer chapter I's per-graveyard choices (CR 101.4c: the controller makes
/// every pick, so they order them), picking `cards`. Asserts P0 chooses from
/// each graveyard and that exactly one card is picked per graveyard.
fn choose_chapter_one(runner: &mut GameRunner, cards: &[ObjectId]) {
    let mut picks = 0;
    for _ in 0..8 {
        match &runner.state().waiting_for {
            WaitingFor::ChooseFromZoneOpponentChooser {
                player, candidates, ..
            } => {
                assert_eq!(*player, P0, "the controller orders the picks");
                assert_eq!(candidates, &vec![P0, P1], "both graveyards are offered");
                let first = candidates[0];
                runner
                    .act(GameAction::ChooseZoneOpponentChooser { opponent: first })
                    .expect("order the per-graveyard picks");
            }
            WaitingFor::ChooseFromZoneChoice {
                player,
                cards: options,
                count,
                ..
            } => {
                assert_eq!(*player, P0, "the controller picks from every graveyard");
                assert_eq!(*count, 1, "one card per graveyard");
                let pick: Vec<ObjectId> = options
                    .iter()
                    .copied()
                    .filter(|id| cards.contains(id))
                    .collect();
                assert_eq!(pick.len(), 1, "one candidate per graveyard: {options:?}");
                runner
                    .act(GameAction::SelectCards { cards: pick })
                    .expect("chapter I pick");
                picks += 1;
            }
            _ => break,
        }
        settle(runner);
    }
    assert_eq!(picks, cards.len(), "one pick per graveyard");
}

fn lore(runner: &GameRunner, saga: ObjectId) -> u32 {
    runner.state().objects[&saga]
        .counters
        .get(&CounterType::Lore)
        .copied()
        .unwrap_or(0)
}

fn pool_of(runner: &GameRunner) -> Vec<ObjectId> {
    match &runner.state().waiting_for {
        WaitingFor::EffectZoneChoice {
            cards,
            zone: Zone::Exile,
            count: 1,
            up_to: true,
            ..
        } => cards.clone(),
        other => panic!("chapter II offers a pick from the linked exile, found {other:?}"),
    }
}

/// CR 607.2a + CR 608.2g + CR 609.4b: chapter II offers exactly the two linked
/// cards, not the unlinked exiled instant; the chosen `{1}{W}` sorcery is cast
/// while the chapter resolves and is paid with the chapter's {R}{R}. Chapter
/// III then offers only the linked card still in exile.
#[test]
fn chapter_two_casts_a_linked_card_paid_with_its_red_mana() {
    let Board {
        mut runner,
        sorcery,
        instant,
        unlinked,
        saga,
    } = reach_chapter_two(GAIN_ONE, GAIN_THREE);
    let mut pool = pool_of(&runner);
    pool.sort();
    let mut expected = vec![sorcery, instant];
    expected.sort();
    assert_eq!(pool, expected, "the linked pool, without {unlinked:?}");

    let life_before = runner.life(P0);
    runner
        .act(GameAction::SelectCards {
            cards: vec![sorcery],
        })
        .expect("pick the linked sorcery");
    for _ in 0..8 {
        if !matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pay {1}{W} from the pool");
    }
    assert_eq!(
        runner.state().objects[&sorcery].zone,
        Zone::Stack,
        "the sorcery is cast during the chapter's resolution, found {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "{{R}}{{R}} paid {{1}}{{W}} through the any-type rider"
    );
    runner.advance_until_stack_empty();
    assert_eq!(runner.life(P0), life_before + 3, "the sorcery resolved");
    assert_eq!(runner.state().objects[&instant].zone, Zone::Exile);

    to_next_p0_precombat_main(&mut runner);
    assert_eq!(lore(&runner, saga), 3);
    settle(&mut runner);
    assert_eq!(
        pool_of(&runner),
        vec![instant],
        "chapter III: the cast sorcery left the linked pool"
    );
}

/// CR 608.2d: a linked card that could not be cast right now ("Counter target
/// spell." with no spell on the stack) is not offered, so picking it cannot
/// throw away the chapter's cast.
#[test]
fn an_uncastable_linked_card_is_not_offered() {
    let Board {
        runner, sorcery, ..
    } = reach_chapter_two(COUNTERSPELL, GAIN_THREE);
    assert_eq!(pool_of(&runner), vec![sorcery]);
}

/// CR 608.2d: with no castable linked card, the chapter offers nothing and
/// leaves no permission behind.
#[test]
fn no_castable_linked_card_opens_no_pick() {
    let Board {
        runner,
        sorcery,
        instant,
        ..
    } = reach_chapter_two(COUNTERSPELL, COUNTERSPELL);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty(),
        "chapter II resolved without a pick: {:?}",
        runner.state().waiting_for
    );
    let castable = spell_objects_available_to_cast(runner.state(), P0);
    for card in [sorcery, instant] {
        assert_eq!(runner.state().objects[&card].zone, Zone::Exile);
        assert!(!castable.contains(&card), "no permission over {card:?}");
    }
}

/// CR 608.2g: picking nothing casts nothing, and no permission lingers — the
/// linked cards cannot be cast later in the turn.
#[test]
fn declining_the_pick_leaves_no_later_permission() {
    let Board {
        mut runner,
        sorcery,
        instant,
        ..
    } = reach_chapter_two(GAIN_ONE, GAIN_THREE);
    assert_eq!(pool_of(&runner).len(), 2, "reach guard: the pick opened");
    runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("pick nothing");
    runner.advance_until_stack_empty();
    for card in [sorcery, instant] {
        assert_eq!(runner.state().objects[&card].zone, Zone::Exile);
    }
    let castable = spell_objects_available_to_cast(runner.state(), P0);
    assert!(
        !castable.contains(&sorcery) && !castable.contains(&instant),
        "no lingering permission over the linked exile: {castable:?}"
    );
}
