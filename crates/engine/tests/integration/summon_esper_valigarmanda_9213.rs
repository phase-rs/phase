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
use engine::types::ability::{AdditionalCost, AdditionalCostOrigin, EffectKind, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
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

/// Warped Space, the unlocked door's text (`client/public/card-data.json`).
const WARPED_SPACE: &str = "Once each turn, you may pay {0} rather than pay the mana cost for a \
spell you cast from exile.";

/// Anhelo, the Painter, verbatim.
const ANHELO: &str = "Deathtouch\nThe first instant or sorcery spell you cast each turn has \
casualty 2. (As you cast that spell, you may sacrifice a creature with power 2 or greater. When \
you do, copy the spell and you may choose new targets for the copy.)";

/// Djinn Illuminatus's replicate line, verbatim.
const DJINN_ILLUMINATUS: &str = "Flying\nEach instant and sorcery spell you cast has replicate. \
The replicate cost is equal to its mana cost. (When you cast it, copy it for each time you paid \
its replicate cost. You may choose new targets for the copies.)";

/// Evermind, verbatim: an instant with no mana cost.
const EVERMIND: &str = "(Nonexistent mana costs can't be paid.)\n\
Draw a card.\n\
Splice onto Arcane {1}{U} (As you cast an Arcane spell, you may reveal this card from your hand \
and pay its splice cost. If you do, add this card's effects to that spell.)";

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
/// by `choose_chapter_one`. Returns the events of the passes it made.
fn settle(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..64 {
        match &runner.state().waiting_for {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                let result = runner.act(GameAction::PassPriority).expect("pass priority");
                events.extend(result.events);
            }
            _ => return events,
        }
    }
    panic!("stack did not settle: {:?}", runner.state().waiting_for);
}

/// Castable: "You gain 3 life." Uncastable while no spell is on the stack
/// (CR 601.2c): "Counter target spell."
const GAIN_THREE: &str = "You gain 3 life.";
const GAIN_ONE: &str = "You gain 1 life.";
const COUNTERSPELL: &str = "Counter target spell.";

/// `{1}{W}`: red mana pays it only through the any-type rider.
fn white_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::White],
        generic: 1,
    }
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
    /// Events from the passes that resolved chapter II up to its pick.
    chapter_two_events: Vec<GameEvent>,
}

/// The Saga with no lore counter, an instant (`instant_text`) in P0's
/// graveyard and a sorcery (`sorcery_text`) in P1's. Chapter I runs on P0's
/// next precombat main, chapter II on the one after; returns with chapter II
/// resolving.
fn reach_chapter_two(instant_text: &str, sorcery_text: &str) -> Board {
    reach_chapter_two_with(VALIGARMANDA, instant_text, white_cost(), sorcery_text)
}

/// [`reach_chapter_two`] with a given Saga text and sorcery cost.
fn reach_chapter_two_with(
    saga_text: &str,
    instant_text: &str,
    sorcery_cost: ManaCost,
    sorcery_text: &str,
) -> Board {
    let sorcery = OpponentCard {
        name: "Linked Sorcery",
        instant: false,
        cost: sorcery_cost,
        text: sorcery_text,
    };
    reach_chapter_two_on(saga_text, instant_text, sorcery, WarpedSpace::Absent)
}

/// The card chapter I exiles from P1's graveyard.
struct OpponentCard<'a> {
    name: &'a str,
    instant: bool,
    cost: ManaCost,
    text: &'a str,
}

/// Whether P0 controls Warped Space at chapter II, and whether its "Once each
/// turn" is already spent that turn.
#[derive(Clone, Copy, PartialEq)]
enum WarpedSpace {
    Absent,
    Unspent,
    Spent,
}

/// [`reach_chapter_two_with`] with a given P1 card and Warped Space.
fn reach_chapter_two_on(
    saga_text: &str,
    instant_text: &str,
    opponent_card: OpponentCard,
    warped_space: WarpedSpace,
) -> Board {
    reach_chapter_two_on_with(saga_text, instant_text, opponent_card, warped_space, |_| {})
}

fn reach_chapter_two_on_with(
    saga_text: &str,
    instant_text: &str,
    opponent_card: OpponentCard,
    warped_space: WarpedSpace,
    extra: impl FnOnce(&mut GameScenario),
) -> Board {
    let mut scenario = GameScenario::new();
    extra(&mut scenario);
    let host = (warped_space != WarpedSpace::Absent).then(|| {
        scenario
            .add_enchantment_from_oracle(P0, "Warped Space", WARPED_SPACE)
            .id()
    });

    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest", "Forest"]);
    scenario.with_library_top(P1, &["Forest", "Forest", "Forest", "Forest"]);
    let saga = scenario
        .add_creature(P0, "Summon: Esper Valigarmanda", 6, 6)
        .as_enchantment()
        .with_subtypes(vec!["Saga"])
        .from_oracle_text(saga_text)
        .id();
    let instant = blue_instant(&mut scenario, P0, instant_text);
    let sorcery = scenario
        .add_spell_to_graveyard(P1, opponent_card.name, opponent_card.instant)
        .with_mana_cost(opponent_card.cost)
        .from_oracle_text(opponent_card.text)
        .id();
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
    if warped_space == WarpedSpace::Spent {
        let host = host.expect("Warped Space is on the battlefield");
        runner
            .state_mut()
            .alt_cost_grant_permissions_used
            .insert(host);
    }
    let chapter_two_events = settle(&mut runner);
    Board {
        runner,
        sorcery,
        instant,
        unlinked,
        saga,
        chapter_two_events,
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

/// How many times the Saga's cast instruction reported completion.
fn cast_completions(events: &[GameEvent], saga: ObjectId) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::CastFromZone,
                    source_id,
                    ..
                } if *source_id == saga
            )
        })
        .count()
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
/// III then offers only the linked card still in exile. The instruction
/// reports completion once, when the cast is made, not when the pick opens.
#[test]
fn chapter_two_casts_a_linked_card_paid_with_its_red_mana() {
    let Board {
        mut runner,
        sorcery,
        instant,
        unlinked,
        saga,
        chapter_two_events: mut events,
    } = reach_chapter_two(GAIN_ONE, GAIN_THREE);
    assert_eq!(
        cast_completions(&events, saga),
        0,
        "no completion while the pick is open"
    );
    let mut pool = pool_of(&runner);
    pool.sort();
    let mut expected = vec![sorcery, instant];
    expected.sort();
    assert_eq!(pool, expected, "the linked pool, without {unlinked:?}");

    let life_before = runner.life(P0);
    let picked = runner
        .act(GameAction::SelectCards {
            cards: vec![sorcery],
        })
        .expect("pick the linked sorcery");
    events.extend(picked.events);
    for _ in 0..8 {
        if !matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
            break;
        }
        let paid = runner
            .act(GameAction::PassPriority)
            .expect("pay {1}{W} from the pool");
        events.extend(paid.events);
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
    assert_eq!(cast_completions(&events, saga), 1, "one completion");
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
/// leaves no permission behind; the instruction still completes once.
#[test]
fn no_castable_linked_card_opens_no_pick() {
    let Board {
        runner,
        sorcery,
        instant,
        saga,
        chapter_two_events,
        ..
    } = reach_chapter_two(COUNTERSPELL, COUNTERSPELL);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty(),
        "chapter II resolved without a pick: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        cast_completions(&chapter_two_events, saga),
        1,
        "one completion"
    );
    let castable = spell_objects_available_to_cast(runner.state(), P0);
    for card in [sorcery, instant] {
        assert_eq!(runner.state().objects[&card].zone, Zone::Exile);
        assert!(!castable.contains(&card), "no permission over {card:?}");
    }
}

/// CR 608.2g: picking nothing casts nothing, and no permission lingers — the
/// linked cards cannot be cast later in the turn. The decline completes the
/// instruction once.
#[test]
fn declining_the_pick_leaves_no_later_permission() {
    let Board {
        mut runner,
        sorcery,
        instant,
        saga,
        chapter_two_events: mut events,
        ..
    } = reach_chapter_two(GAIN_ONE, GAIN_THREE);
    assert_eq!(pool_of(&runner).len(), 2, "reach guard: the pick opened");
    let declined = runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("pick nothing");
    events.extend(declined.events);
    assert_eq!(cast_completions(&events, saga), 1, "one completion");
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

/// CR 118.6 + CR 601.2h: a linked sorcery with no mana cost (Living End,
/// Crashing Footfalls) has an unpayable cost. Chapter II casts at the printed
/// cost, so that card is not offered and cannot be cast for free; the payable
/// linked instant still is.
#[test]
fn a_linked_card_with_no_mana_cost_is_not_offered() {
    let Board {
        mut runner,
        sorcery,
        instant,
        ..
    } = reach_chapter_two_with(VALIGARMANDA, GAIN_ONE, ManaCost::NoCost, GAIN_THREE);
    assert_eq!(
        runner.state().objects[&sorcery].mana_cost,
        ManaCost::NoCost,
        "reach guard: the linked sorcery has no mana cost"
    );
    assert_eq!(pool_of(&runner), vec![instant]);
    assert!(
        runner
            .act(GameAction::SelectCards {
                cards: vec![sorcery],
            })
            .is_err(),
        "picking it directly is refused"
    );
    assert_eq!(runner.state().objects[&sorcery].zone, Zone::Exile);
}

/// Control for the test above: a linked sorcery costing {0} has a payable
/// cost, so the same chapter offers it and casts it, paying nothing.
#[test]
fn a_linked_card_costing_zero_is_offered_and_cast() {
    let Board {
        mut runner,
        sorcery,
        instant,
        ..
    } = reach_chapter_two_with(VALIGARMANDA, GAIN_ONE, ManaCost::zero(), GAIN_THREE);
    let mut pool = pool_of(&runner);
    pool.sort();
    let mut expected = vec![sorcery, instant];
    expected.sort();
    assert_eq!(pool, expected, "both linked cards are offered");
    let pool_before = runner.state().players[0].mana_pool.total();
    assert_eq!(
        pool_before, 2,
        "reach guard: the chapter's {{R}}{{R}} floats"
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![sorcery],
        })
        .expect("pick the linked sorcery");
    for _ in 0..8 {
        if !matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pay {0}");
    }
    assert_eq!(
        runner.state().objects[&sorcery].zone,
        Zone::Stack,
        "the {{0}} sorcery is cast, found {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        pool_before,
        "{{0}} spends no mana"
    );
}

/// Chapter II with Evermind (no mana cost) exiled from P1's graveyard and
/// Warped Space as given.
fn reach_chapter_two_evermind(warped_space: WarpedSpace) -> Board {
    let evermind = OpponentCard {
        name: "Evermind",
        instant: true,
        cost: ManaCost::NoCost,
        text: EVERMIND,
    };
    let board = reach_chapter_two_on(VALIGARMANDA, GAIN_ONE, evermind, warped_space);
    assert_eq!(
        board.runner.state().objects[&board.sorcery].mana_cost,
        ManaCost::NoCost,
        "reach guard: Evermind has no mana cost"
    );
    board
}

fn sorted(mut cards: Vec<ObjectId>) -> Vec<ObjectId> {
    cards.sort();
    cards
}

/// CR 118.6a: "If an alternative cost is applied to an unpayable cost,
/// including an effect that allows a player to cast a spell without paying
/// its mana cost, the alternative cost may be paid." Warped Space offers {0}
/// rather than the mana cost of a spell cast from exile, so chapter II offers
/// P1's Evermind. CR 601.2h: "Unpayable costs can't be paid." The {0} is the
/// only way to cast it, so it is applied without a choice and its
/// once-each-turn use is spent.
#[test]
fn warped_space_casts_a_linked_card_with_no_mana_cost_for_zero() {
    let Board {
        mut runner,
        sorcery: evermind,
        instant,
        ..
    } = reach_chapter_two_evermind(WarpedSpace::Unspent);
    assert_eq!(pool_of(&runner), sorted(vec![evermind, instant]));
    let pool_before = runner.state().players[0].mana_pool.total();
    runner
        .act(GameAction::SelectCards {
            cards: vec![evermind],
        })
        .expect("pick Evermind");
    assert_eq!(
        runner.state().objects[&evermind].zone,
        Zone::Stack,
        "cast with no cost choice, found {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        pool_before,
        "{{0}} spends no mana"
    );
    assert_eq!(
        runner.state().alt_cost_grant_permissions_used.len(),
        1,
        "Warped Space is used for this turn"
    );
}

/// Warped Space's once-each-turn use already spent this turn leaves no
/// alternative cost, so Evermind is not offered and a direct pick is refused,
/// as without Warped Space.
#[test]
fn a_spent_warped_space_does_not_offer_a_card_with_no_mana_cost() {
    for warped_space in [WarpedSpace::Spent, WarpedSpace::Absent] {
        let Board {
            mut runner,
            sorcery: evermind,
            instant,
            ..
        } = reach_chapter_two_evermind(warped_space);
        assert_eq!(pool_of(&runner), vec![instant]);
        assert!(
            runner
                .act(GameAction::SelectCards {
                    cards: vec![evermind],
                })
                .is_err(),
            "picking Evermind directly is refused"
        );
        assert_eq!(runner.state().objects[&evermind].zone, Zone::Exile);
    }
}

/// CR 601.2a: the caster becomes the spell's controller, so P1's card cast
/// by P0 from exile gets P0's Warped Space. With a payable printed cost
/// ({1}{W}) it is a choice: accepting pays {0} and spends Warped Space;
/// declining pays {1}{W} with the chapter's {R}{R} and leaves it unused.
#[test]
fn warped_space_reaches_the_opponents_linked_card() {
    for pay_zero in [true, false] {
        let sorcery = OpponentCard {
            name: "Linked Sorcery",
            instant: false,
            cost: white_cost(),
            text: GAIN_THREE,
        };
        let Board {
            mut runner,
            sorcery,
            ..
        } = reach_chapter_two_on(VALIGARMANDA, GAIN_ONE, sorcery, WarpedSpace::Unspent);
        assert_eq!(runner.state().objects[&sorcery].owner, P1);
        let pool_before = runner.state().players[0].mana_pool.total();
        runner
            .act(GameAction::SelectCards {
                cards: vec![sorcery],
            })
            .expect("pick the linked sorcery");
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::OptionalCostChoice { .. }
            ),
            "Warped Space's {{0}} is offered, found {:?}",
            runner.state().waiting_for
        );
        runner
            .act(GameAction::DecideOptionalCost { pay: pay_zero })
            .expect("decide the alternative");
        for _ in 0..8 {
            if !matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
                break;
            }
            runner.act(GameAction::PassPriority).expect("pay {1}{W}");
        }
        assert_eq!(runner.state().objects[&sorcery].zone, Zone::Stack);
        let (mana_left, slots_used) = if pay_zero { (pool_before, 1) } else { (0, 0) };
        assert_eq!(runner.state().players[0].mana_pool.total(), mana_left);
        assert_eq!(
            runner.state().alt_cost_grant_permissions_used.len(),
            slots_used
        );
    }
}

/// CR 118.9d: "If an alternative cost is being paid to cast a spell, any
/// additional costs, cost increases, and cost reductions that affect that
/// spell are applied to that alternative cost." Untargeted Evermind with
/// Anhelo, the Painter's casualty 2 and Warped Space: casualty is offered
/// after the {0} is applied. Accepting sacrifices the 3/3 and copies the
/// spell (CR 702.153a); declining keeps it. Both cast for {0} and spend
/// Warped Space once.
#[test]
fn warped_space_keeps_casualty_for_a_card_with_no_mana_cost() {
    for sacrifice in [true, false] {
        let evermind = OpponentCard {
            name: "Evermind",
            instant: true,
            cost: ManaCost::NoCost,
            text: EVERMIND,
        };
        let mut fodder = None;
        let Board {
            mut runner,
            sorcery: evermind,
            ..
        } = reach_chapter_two_on_with(
            VALIGARMANDA,
            GAIN_ONE,
            evermind,
            WarpedSpace::Unspent,
            |scenario| {
                scenario.add_creature_from_oracle(P0, "Anhelo, the Painter", 1, 1, ANHELO);
                fodder = Some(scenario.add_creature(P0, "Fodder", 3, 3).id());
            },
        );
        let fodder = fodder.expect("Fodder is on the battlefield");
        let pool_before = runner.state().players[0].mana_pool.total();
        runner
            .act(GameAction::SelectCards {
                cards: vec![evermind],
            })
            .expect("pick Evermind");
        assert!(
            matches!(
                &runner.state().waiting_for,
                WaitingFor::OptionalCostChoice {
                    cost: AdditionalCost::Optional { .. },
                    ..
                }
            ),
            "casualty is offered, found {:?}",
            runner.state().waiting_for
        );
        runner
            .act(GameAction::DecideOptionalCost { pay: sacrifice })
            .expect("decide casualty");
        if sacrifice && runner.state().objects[&fodder].zone == Zone::Battlefield {
            runner
                .act(GameAction::SelectCards {
                    cards: vec![fodder],
                })
                .expect("sacrifice Fodder");
        }
        assert_eq!(
            runner.state().objects[&evermind].zone,
            Zone::Stack,
            "cast for {{0}}, found {:?}",
            runner.state().waiting_for
        );
        assert_eq!(runner.state().players[0].mana_pool.total(), pool_before);
        assert_eq!(runner.state().alt_cost_grant_permissions_used.len(), 1);
        let fodder_zone = if sacrifice {
            Zone::Graveyard
        } else {
            Zone::Battlefield
        };
        assert_eq!(runner.state().objects[&fodder].zone, fodder_zone);
        let hand_before = runner.state().players[0].hand.len();
        for _ in 0..20 {
            if runner.state().stack.is_empty() {
                break;
            }
            runner.act(GameAction::PassPriority).expect("resolve");
        }
        assert!(runner.state().stack.is_empty());
        let draws = if sacrifice { 2 } else { 1 };
        assert_eq!(
            runner.state().players[0].hand.len(),
            hand_before + draws,
            "Evermind and casualty's copy each draw a card"
        );
    }
}

/// CR 118.6a + CR 601.2b: an additional cost decided before targets (Anhelo,
/// the Painter's granted casualty) does not skip Warped Space's {0} for a
/// targeted linked card with no mana cost: after casualty and targets it is
/// cast for {0}, spending Warped Space.
#[test]
fn warped_space_follows_a_casualty_decision_for_a_card_with_no_mana_cost() {
    let draw_three = OpponentCard {
        name: "Linked Draw",
        instant: false,
        cost: ManaCost::NoCost,
        text: "Target player draws three cards.",
    };
    let Board {
        mut runner,
        sorcery,
        ..
    } = reach_chapter_two_on_with(
        VALIGARMANDA,
        GAIN_ONE,
        draw_three,
        WarpedSpace::Unspent,
        |scenario| {
            scenario.add_creature_from_oracle(P0, "Anhelo, the Painter", 1, 1, ANHELO);
            scenario.add_creature(P0, "Fodder", 3, 3);
        },
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![sorcery],
        })
        .expect("pick the linked card");
    assert!(
        matches!(
            &runner.state().waiting_for,
            WaitingFor::OptionalCostChoice {
                cost: AdditionalCost::Optional { .. },
                ..
            }
        ),
        "reach guard: casualty is offered first, found {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalCost { pay: false })
        .expect("decline casualty");
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Player(P0)],
        })
        .expect("target P0");
    assert_eq!(
        runner.state().objects[&sorcery].zone,
        Zone::Stack,
        "cast for {{0}}, found {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().alt_cost_grant_permissions_used.len(), 1);
}

/// CR 118.9d with a printed cost: P1's `{1}{W}` "Draw a card." instant gets
/// Warped Space's {0} choice and Anhelo, the Painter's casualty 2. Whichever
/// cost P0 picks, casualty follows: the 3/3 is sacrificed and the copy draws
/// a second card. Accepting pays {0} and spends Warped Space; declining pays
/// `{1}{W}` with the chapter's mana and leaves it unused.
#[test]
fn warped_space_keeps_casualty_for_a_card_with_a_printed_cost() {
    for pay_zero in [true, false] {
        let card = OpponentCard {
            name: "Linked Draw",
            instant: true,
            cost: white_cost(),
            text: "Draw a card.",
        };
        let mut fodder = None;
        let Board {
            mut runner,
            sorcery: linked,
            ..
        } = reach_chapter_two_on_with(
            VALIGARMANDA,
            GAIN_ONE,
            card,
            WarpedSpace::Unspent,
            |scenario| {
                scenario.add_creature_from_oracle(P0, "Anhelo, the Painter", 1, 1, ANHELO);
                fodder = Some(scenario.add_creature(P0, "Fodder", 3, 3).id());
            },
        );
        let fodder = fodder.expect("Fodder is on the battlefield");
        let pool_before = runner.state().players[0].mana_pool.total();
        runner
            .act(GameAction::SelectCards {
                cards: vec![linked],
            })
            .expect("pick the linked card");
        let mut prompts = Vec::new();
        for _ in 0..12 {
            match runner.state().waiting_for.clone() {
                WaitingFor::OptionalCostChoice { cost, origin, .. } => {
                    let alternative = matches!(cost, AdditionalCost::Choice(..));
                    let expected_origin = if alternative {
                        AdditionalCostOrigin::Other
                    } else {
                        AdditionalCostOrigin::Casualty
                    };
                    assert_eq!(origin, expected_origin, "the prompt names its cost");
                    prompts.push(if alternative {
                        "alternative"
                    } else {
                        "casualty"
                    });
                    runner
                        .act(GameAction::DecideOptionalCost {
                            pay: !alternative || pay_zero,
                        })
                        .expect("decide a cost");
                }
                WaitingFor::PayCost { .. } => {
                    runner
                        .act(GameAction::SelectCards {
                            cards: vec![fodder],
                        })
                        .expect("sacrifice Fodder");
                }
                WaitingFor::ManaPayment { .. } => {
                    runner.act(GameAction::PassPriority).expect("pay {1}{W}");
                }
                _ => break,
            }
        }
        assert_eq!(prompts, vec!["alternative", "casualty"]);
        assert_eq!(runner.state().objects[&linked].zone, Zone::Stack);
        assert_eq!(runner.state().objects[&fodder].zone, Zone::Graveyard);
        let (mana_left, uses) = if pay_zero { (pool_before, 1) } else { (0, 0) };
        assert_eq!(runner.state().players[0].mana_pool.total(), mana_left);
        assert_eq!(runner.state().alt_cost_grant_permissions_used.len(), uses);
        let hand_before = runner.state().players[0].hand.len();
        for _ in 0..20 {
            if runner.state().stack.is_empty() {
                break;
            }
            runner.act(GameAction::PassPriority).expect("resolve");
        }
        assert_eq!(
            runner.state().players[0].hand.len(),
            hand_before + 2,
            "the card and casualty's copy each draw a card"
        );
    }
}

/// CR 118.9d with a repeatable additional cost: P1's `{R}` instant gets
/// Warped Space's {0} choice and Djinn Illuminatus's replicate. The {0}
/// answer stays an alternative-cost answer: accepting pays {0} and spends
/// Warped Space, declining pays `{R}` and leaves it unused. Replicate is
/// offered after either answer.
#[test]
fn warped_space_keeps_replicate_for_a_card_with_a_printed_cost() {
    for pay_zero in [true, false] {
        let card = OpponentCard {
            name: "Linked Draw",
            instant: true,
            cost: ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            },
            text: "Draw a card.",
        };
        let Board {
            mut runner,
            sorcery: linked,
            ..
        } = reach_chapter_two_on_with(
            VALIGARMANDA,
            GAIN_ONE,
            card,
            WarpedSpace::Unspent,
            |scenario| {
                scenario.add_creature_from_oracle(P0, "Djinn Illuminatus", 3, 5, DJINN_ILLUMINATUS);
            },
        );
        let pool_before = runner.state().players[0].mana_pool.total();
        runner
            .act(GameAction::SelectCards {
                cards: vec![linked],
            })
            .expect("pick the linked card");
        let mut prompts = Vec::new();
        for _ in 0..12 {
            match runner.state().waiting_for.clone() {
                WaitingFor::OptionalCostChoice { cost, origin, .. } => {
                    let alternative = matches!(cost, AdditionalCost::Choice(..));
                    prompts.push((alternative, origin));
                    runner
                        .act(GameAction::DecideOptionalCost {
                            pay: alternative && pay_zero,
                        })
                        .expect("decide a cost");
                }
                WaitingFor::ManaPayment { .. } => {
                    runner.act(GameAction::PassPriority).expect("pay {R}");
                }
                _ => break,
            }
        }
        assert_eq!(
            prompts,
            vec![
                (true, AdditionalCostOrigin::Other),
                (false, AdditionalCostOrigin::Replicate)
            ]
        );
        assert_eq!(runner.state().objects[&linked].zone, Zone::Stack);
        let (mana_left, uses) = if pay_zero {
            (pool_before, 1)
        } else {
            (pool_before - 1, 0)
        };
        assert_eq!(runner.state().players[0].mana_pool.total(), mana_left);
        assert_eq!(runner.state().alt_cost_grant_permissions_used.len(), uses);
    }
}
