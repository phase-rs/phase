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
//! The Saga uses its printed text; the Room uses the small raw-MTGJSON fixture,
//! so no installed card database is required. Both lore counters come from the
//! CR 714.3c turn-based action, and chapter I exiles the cards itself, so the
//! exile link is the production one.

use engine::game::casting::spell_objects_available_to_cast;
use engine::game::planeswalker::can_activate_loyalty_ability;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::ability::{AdditionalCost, AdditionalCostOrigin, EffectKind, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameRng, RandomDraw, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use rand::Rng;

/// Summon: Esper Valigarmanda, verbatim (`client/public/card-data.json`).
const VALIGARMANDA: &str = "(As this Saga enters and after your draw step, add a lore \
counter. Sacrifice after IV.)\n\
I — Exile an instant or sorcery card from each graveyard.\n\
II, III, IV — Add {R} for each lore counter on this Saga. You may cast an instant or sorcery \
card exiled with this Saga, and mana of any type can be spent to cast that spell.\n\
Flying, haste";

/// Both printed Room faces, reparsed from a small MTGJSON fixture in CI too.
fn warped_space_db() -> &'static engine::database::card_db::CardDatabase {
    static DB: std::sync::OnceLock<engine::database::card_db::CardDatabase> =
        std::sync::OnceLock::new();
    DB.get_or_init(|| {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/charred_foyer_warped_space_9751_atomic.json");
        engine::database::card_db::CardDatabase::from_mtgjson(&path)
            .expect("parse both printed Room doors")
    })
}

fn add_warped_space(scenario: &mut GameScenario) -> ObjectId {
    scenario.add_real_card(P0, "Charred Foyer", Zone::Hand, warped_space_db())
}

// Build the pre-existing host by actually casting its right door. This keeps
// its live face, mana cost and unlocked designation on the production path.
fn seed_unlocked_warped_space(runner: &mut GameRunner) {
    let hosts: Vec<_> = runner
        .state()
        .objects
        .values()
        .filter(|object| object.base_name == "Charred Foyer" && object.zone == Zone::Hand)
        .map(|object| object.id)
        .collect();
    for host in hosts {
        let before = runner.state().players[0].mana_pool.clone();
        for color in [
            engine::types::mana::ManaType::Red,
            engine::types::mana::ManaType::Red,
            engine::types::mana::ManaType::Colorless,
            engine::types::mana::ManaType::Colorless,
            engine::types::mana::ManaType::Colorless,
            engine::types::mana::ManaType::Colorless,
        ] {
            runner.state_mut().players[0]
                .mana_pool
                .add(engine::types::mana::ManaUnit::new(
                    color,
                    ObjectId(0),
                    false,
                    vec![],
                ));
        }
        runner.cast(host).modal_back_face(true).resolve();
        assert_eq!(runner.state().objects[&host].zone, Zone::Battlefield);
        assert_eq!(runner.state().objects[&host].name, "Warped Space");
        assert_eq!(
            runner.state().objects[&host].room_unlocks,
            Some(engine::game::game_object::RoomUnlockState {
                left_unlocked: false,
                right_unlocked: true
            })
        );
        assert_eq!(runner.state().players[0].mana_pool, before);
    }
}

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
    for _ in 0..4 {
        runner.advance_to_phase(Phase::PreCombatMain);
        if runner.state().active_player == P0 && runner.state().phase == Phase::PreCombatMain {
            return;
        }
        match runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => {
                runner.declare_attackers(&[]).expect("P1 does not attack");
            }
            _ => panic!("next P0 main stopped at {:?}", runner.state().waiting_for),
        }
    }
    panic!("next P0 main was not reached");
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
    let host = (warped_space != WarpedSpace::Absent).then(|| add_warped_space(&mut scenario));

    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest", "Forest"]);
    scenario.with_library_top(P1, &["Forest", "Forest", "Forest", "Forest"]);
    let saga = scenario
        .add_creature(P0, "Summon: Esper Valigarmanda", 6, 6)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 3,
        })
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
    seed_unlocked_warped_space(&mut runner);

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
        if matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
            runner
                .act(GameAction::PassPriority)
                .expect("commit zero mana and casualty together");
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

/// Fling, verbatim.
const FLING: &str = "As an additional cost to cast this spell, sacrifice a creature.\n\
Fling deals damage equal to the sacrificed creature's power to any target.";

/// What a linked Fling cast at chapter II paid and did.
struct FlingCast {
    /// The cost prompts in order: "casualty", "fling".
    prompts: Vec<&'static str>,
    /// The creatures each Fling sacrifice prompt offered.
    fling_choices: Vec<Vec<ObjectId>>,
    /// The 3/3 casualty is paid with and the 2/2 Fling's cost is paid with.
    big: ObjectId,
    small: ObjectId,
    runner: GameRunner,
    fling: ObjectId,
}

/// Chapter II casts P1's linked Fling (`{1}{R}`) at P1, with Anhelo, the
/// Painter's casualty 2 when `anhelo`, answering casualty with `casualty` and
/// Warped Space's {0} choice with `pay_zero`. A sacrifice prompt that offers
/// Anhelo (power 1) is Fling's "sacrifice a creature" and gets the 2/2; one
/// that does not is casualty's and gets the 3/3.
fn cast_linked_fling(
    anhelo: bool,
    casualty: bool,
    warped_space: WarpedSpace,
    pay_zero: bool,
) -> FlingCast {
    let card = OpponentCard {
        name: "Fling",
        instant: true,
        cost: ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 1,
        },
        text: FLING,
    };
    let mut ids = None;
    let Board {
        mut runner,
        sorcery: fling,
        ..
    } = reach_chapter_two_on_with(VALIGARMANDA, GAIN_ONE, card, warped_space, |scenario| {
        let anhelo = anhelo.then(|| {
            scenario
                .add_creature_from_oracle(P0, "Anhelo, the Painter", 1, 1, ANHELO)
                .id()
        });
        let big = scenario.add_creature(P0, "Big Fodder", 3, 3).id();
        let small = scenario.add_creature(P0, "Small Fodder", 2, 2).id();
        ids = Some((anhelo, big, small));
    });
    let (anhelo, big, small) = ids.expect("the creatures are on the battlefield");
    runner
        .act(GameAction::SelectCards { cards: vec![fling] })
        .expect("pick Fling");
    let mut prompts = Vec::new();
    let mut fling_choices = Vec::new();
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalCostChoice { cost, .. } => {
                let alternative = matches!(cost, AdditionalCost::Choice(..));
                prompts.push(if alternative {
                    "alternative"
                } else {
                    "casualty"
                });
                let pay = if alternative { pay_zero } else { casualty };
                runner
                    .act(GameAction::DecideOptionalCost { pay })
                    .expect("decide a cost");
            }
            WaitingFor::PayCost { choices, .. } => {
                let fling_cost = anhelo.is_none_or(|anhelo| choices.contains(&anhelo));
                if fling_cost {
                    prompts.push("fling");
                    fling_choices.push(choices.clone());
                }
                let pick = if fling_cost { small } else { big };
                runner
                    .act(GameAction::SelectCards { cards: vec![pick] })
                    .expect("sacrifice a creature");
            }
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Player(P1)],
                    })
                    .expect("target P1");
            }
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pay {1}{R}");
            }
            _ => break,
        }
    }
    FlingCast {
        prompts,
        fling_choices,
        big,
        small,
        runner,
        fling,
    }
}

/// Resolve the stack, keeping each copy's targets; returns P1's life lost.
fn resolve_fling(runner: &mut GameRunner) -> i32 {
    let life_before = runner.state().players[1].life;
    for _ in 0..20 {
        drain_order_triggers_with_identity(runner.state_mut());
        if matches!(runner.state().waiting_for, WaitingFor::CopyRetarget { .. }) {
            runner
                .act(GameAction::KeepAllCopyTargets)
                .expect("keep the copy's target");
            continue;
        }
        if runner.state().stack.is_empty() {
            break;
        }
        runner.act(GameAction::PassPriority).expect("resolve");
    }
    assert!(runner.state().stack.is_empty());
    life_before - runner.state().players[1].life
}

/// CR 601.2f + CR 601.2h + CR 118.8: Anhelo, the Painter's casualty is
/// decided before targets, and Fling's "sacrifice a creature" is still part
/// of the total cost. Declining casualty, the 2/2 is sacrificed for Fling,
/// which deals 2. Accepting, the 3/3 pays casualty and the 2/2 pays Fling, so
/// Fling and its copy (CR 707.10) each deal the 2/2's power.
#[test]
fn a_linked_fling_pays_its_sacrifice_beside_granted_casualty() {
    for casualty in [false, true] {
        let FlingCast {
            prompts,
            fling_choices,
            big,
            small,
            mut runner,
            fling,
        } = cast_linked_fling(true, casualty, WarpedSpace::Absent, false);
        assert_eq!(prompts, vec!["casualty", "fling"]);
        // The 3/3 chosen for casualty is not offered again for Fling's
        // sacrifice, even while its sacrifice waits for mana payment.
        let offered = &fling_choices[0];
        assert!(offered.contains(&small), "{offered:?}");
        assert_eq!(offered.contains(&big), !casualty, "{offered:?}");
        assert_eq!(runner.state().objects[&fling].zone, Zone::Stack);
        // CR 702.153b: only the casualty answer is a casualty payment;
        // Fling's sacrifice is not one.
        let payments = &runner
            .state()
            .stack
            .iter()
            .find(|entry| entry.id == fling)
            .and_then(|entry| entry.ability())
            .expect("Fling is on the stack")
            .context
            .additional_cost_payments;
        let casualty_payments: Vec<_> = payments
            .iter()
            .filter(|payment| payment.origin == AdditionalCostOrigin::Casualty)
            .map(|payment| (payment.origin_ordinal, payment.count))
            .collect();
        let expected: Vec<(u32, u32)> = if casualty { vec![(0, 1)] } else { vec![] };
        assert_eq!(casualty_payments, expected);
        assert_eq!(runner.state().objects[&small].zone, Zone::Graveyard);
        let big_zone = if casualty {
            Zone::Graveyard
        } else {
            Zone::Battlefield
        };
        assert_eq!(runner.state().objects[&big].zone, big_zone);
        let damage = if casualty { 4 } else { 2 };
        assert_eq!(resolve_fling(&mut runner), damage);
    }
}

/// The control: without a casualty grant, the linked Fling asks for its
/// sacrifice once and deals the sacrificed 2/2's power.
#[test]
fn a_linked_fling_without_casualty_pays_its_sacrifice_once() {
    let FlingCast {
        prompts,
        fling_choices: _,
        big,
        small,
        mut runner,
        fling,
    } = cast_linked_fling(false, false, WarpedSpace::Absent, false);
    assert_eq!(prompts, vec!["fling"]);
    assert_eq!(runner.state().objects[&fling].zone, Zone::Stack);
    assert_eq!(runner.state().objects[&small].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&big].zone, Zone::Battlefield);
    assert_eq!(resolve_fling(&mut runner), 2);
}

/// CR 118.9d + CR 601.2f: the Room's alternative replaces only Fling's
/// mana cost. Its sacrifice and each elected casualty remain independent.
#[test]
fn a_linked_fling_composes_warped_space_with_its_sacrifice_and_casualty() {
    for casualty in [false, true] {
        for pay_zero in [false, true] {
            let FlingCast {
                prompts,
                fling_choices,
                big,
                small,
                mut runner,
                fling,
            } = cast_linked_fling(true, casualty, WarpedSpace::Unspent, pay_zero);
            assert_eq!(prompts, vec!["alternative", "casualty", "fling"]);
            assert_eq!(runner.state().objects[&fling].zone, Zone::Stack);
            assert_eq!(runner.state().objects[&small].zone, Zone::Graveyard);
            assert_eq!(
                runner.state().objects[&big].zone,
                if casualty {
                    Zone::Graveyard
                } else {
                    Zone::Battlefield
                }
            );
            assert!(fling_choices[0].contains(&small));
            assert_eq!(fling_choices[0].contains(&big), !casualty);
            assert_eq!(
                runner.state().players[0].mana_pool.total(),
                if pay_zero { 2 } else { 0 }
            );
            assert_eq!(
                runner.state().alt_cost_grant_permissions_used.len(),
                usize::from(pay_zero)
            );
            assert_eq!(resolve_fling(&mut runner), if casualty { 4 } else { 2 });
        }
    }
}

/// CR 118.9: Warped Space's {0} is offered to a targeted spell with Anhelo's
/// casualty. The alternative is elected before the casualty selection;
/// both declarations survive target selection.
/// P1's `{1}{W}` "Target player draws a card.": accepting pays {0} and spends
/// Warped Space; declining pays `{1}{W}` with the chapter's mana. Casualty
/// copies either way.
#[test]
fn warped_space_is_elected_before_casualty_and_targets_for_a_targeted_card() {
    for pay_zero in [true, false] {
        let card = OpponentCard {
            name: "Linked Draw",
            instant: true,
            cost: white_cost(),
            text: "Target player draws a card.",
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
                WaitingFor::OptionalCostChoice { cost, .. } => {
                    let alternative = matches!(cost, AdditionalCost::Choice(..));
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
                WaitingFor::TargetSelection { .. } => {
                    prompts.push("targets");
                    runner
                        .act(GameAction::SelectTargets {
                            targets: vec![TargetRef::Player(P0)],
                        })
                        .expect("target P0");
                }
                WaitingFor::ManaPayment { .. } => {
                    runner.act(GameAction::PassPriority).expect("pay {1}{W}");
                }
                _ => break,
            }
        }
        assert_eq!(prompts, vec!["alternative", "casualty", "targets"]);
        assert_eq!(runner.state().objects[&linked].zone, Zone::Stack);
        assert_eq!(runner.state().objects[&fodder].zone, Zone::Graveyard);
        let (mana_left, uses) = if pay_zero { (pool_before, 1) } else { (0, 0) };
        assert_eq!(runner.state().players[0].mana_pool.total(), mana_left);
        assert_eq!(runner.state().alt_cost_grant_permissions_used.len(), uses);
        let hand_before = runner.state().players[0].hand.len();
        for _ in 0..20 {
            drain_order_triggers_with_identity(runner.state_mut());
            if matches!(runner.state().waiting_for, WaitingFor::CopyRetarget { .. }) {
                runner
                    .act(GameAction::KeepAllCopyTargets)
                    .expect("keep the copy's target");
                continue;
            }
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

/// CR 601.2b/f/h: elect Warped Space's {0} before paying Anhelo's casualty.
/// Opalescence animates the Room; sacrificing that actual grantor does not
/// invalidate the elected alternative. Other fodder is the positive partner.
#[test]
fn a_linked_card_with_no_mana_cost_retains_the_alternative_when_its_source_is_sacrificed() {
    for sacrifice_grantor in [true, false] {
        let card = OpponentCard {
            name: "Linked Draw",
            instant: false,
            cost: ManaCost::NoCost,
            text: "Target player draws three cards.",
        };
        let mut ids = None;
        let Board {
            mut runner,
            sorcery: linked,
            ..
        } = reach_chapter_two_on_with(
            VALIGARMANDA,
            GAIN_ONE,
            card,
            WarpedSpace::Absent,
            |scenario| {
                scenario.add_creature_from_oracle(P0, "Anhelo, the Painter", 1, 1, ANHELO);
                scenario.add_enchantment_from_oracle(P0, "Opalescence",
                    "Each other non-Aura enchantment is a creature in addition to its other types and has base power and base toughness each equal to its mana value.");
                let grantor = add_warped_space(scenario);
                let fodder = scenario.add_creature(P0, "Fodder", 3, 3).id();
                ids = Some((grantor, fodder));
            },
        );
        let (grantor, fodder) = ids.expect("the creatures are on the battlefield");
        assert_eq!(runner.state().objects[&grantor].name, "Warped Space");
        assert_eq!(
            runner.state().objects[&grantor].power,
            Some(6),
            "Opalescence uses the unlocked right door's printed {{4}}{{R}}{{R}}"
        );
        assert_eq!(
            runner.state().objects[&grantor].room_unlocks,
            Some(engine::game::game_object::RoomUnlockState {
                left_unlocked: false,
                right_unlocked: true
            })
        );
        runner
            .act(GameAction::SelectCards {
                cards: vec![linked],
            })
            .expect("the {0} admits the linked card");
        let paid_with = if sacrifice_grantor { grantor } else { fodder };
        let mut refused = false;
        for _ in 0..12 {
            let result = match runner.state().waiting_for.clone() {
                WaitingFor::OptionalCostChoice { origin, .. } => {
                    assert_eq!(origin, AdditionalCostOrigin::Casualty);
                    runner.act(GameAction::DecideOptionalCost { pay: true })
                }
                WaitingFor::PayCost { .. } => {
                    let result = runner.act(GameAction::SelectCards {
                        cards: vec![paid_with],
                    });
                    assert_eq!(
                        runner.state().objects[&paid_with].zone,
                        Zone::Battlefield,
                        "zero-mana selection is still unpaid before targets and final payment"
                    );
                    result
                }
                WaitingFor::TargetSelection { .. } => runner.act(GameAction::SelectTargets {
                    targets: vec![TargetRef::Player(P0)],
                }),
                WaitingFor::ManaPayment { .. } => runner.act(GameAction::PassPriority),
                _ => break,
            };
            if result.is_err() {
                refused = true;
                break;
            }
        }
        assert!(
            !refused,
            "the elected alternative survives its source's departure"
        );
        assert_eq!(runner.state().objects[&paid_with].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&linked].zone, Zone::Stack);
        assert_eq!(runner.state().alt_cost_grant_permissions_used.len(), 1);
        assert!(runner.state().pending_cast.is_none());
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }
}

/// Synthetic grammar witness from review: the real parser and reducer must
/// consume a forwarded two-card pool as one resolution-time choice (CR 608.2g).
#[test]
fn a_forwarded_linked_exile_pool_casts_at_most_one_card_without_lingering_permissions() {
    for accept in [false, true] {
        let mut scenario = GameScenario::new();
        let first = scenario
            .add_spell_to_library_top(P0, "First", true)
            .with_mana_cost(ManaCost::zero())
            .from_oracle_text(GAIN_ONE)
            .id();
        let second = scenario
            .add_spell_to_library_top(P0, "Second", false)
            .with_mana_cost(ManaCost::zero())
            .from_oracle_text(GAIN_THREE)
            .id();
        let source = scenario.add_spell_to_hand(P0, "Forwarded pool witness", true)
            .with_mana_cost(ManaCost::zero())
            .from_oracle_text("Exile the top two cards of your library. You may cast an instant or sorcery card exiled with ~.").id();
        let mut runner = scenario.build();
        runner
            .act(GameAction::CastSpell {
                object_id: source,
                card_id: runner.state().objects[&source].card_id,
                targets: vec![],
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
            })
            .expect("cast producer");
        settle(&mut runner);
        let mut offered = pool_of(&runner);
        offered.sort();
        let mut expected = vec![first, second];
        expected.sort();
        assert_eq!(
            offered, expected,
            "reach guard: both linked members offered now"
        );
        assert!(runner
            .act(GameAction::SelectCards {
                cards: vec![first, second]
            })
            .is_err());
        assert_eq!(pool_of(&runner).len(), 2);
        runner
            .act(GameAction::SelectCards {
                cards: if accept { vec![first] } else { vec![] },
            })
            .expect("choose zero or one");
        for _ in 0..8 {
            if !matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
                break;
            }
            runner.act(GameAction::PassPriority).expect("pay zero");
        }
        assert_eq!(runner.state().objects[&second].zone, Zone::Exile);
        assert_eq!(
            runner.state().objects[&first].zone,
            if accept { Zone::Stack } else { Zone::Exile }
        );
        assert!(runner.state().objects[&second]
            .casting_permissions
            .is_empty());
        assert!(runner.state().objects[&first]
            .casting_permissions
            .is_empty());
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert!(runner.state().pending_cast.is_none());
    }
}

/// CR 601.2f/h + CR 733.1: targeting Terror adds an unpayable life cost
/// after casualty was declared. Reversal settles the entire cast and chapter;
/// with enough life, the same entry pays the imposed cost and both sacrifices.
#[test]
fn an_illegal_linked_cast_reverses_its_declaration_and_resumes_its_parent() {
    for enough_life in [false, true] {
        let card = OpponentCard {
            name: "Fling",
            instant: true,
            cost: ManaCost::zero(),
            text: FLING,
        };
        let mut ids = None;
        let Board {
            mut runner,
            sorcery: linked,
            saga,
            ..
        } = reach_chapter_two_on_with(
            VALIGARMANDA,
            GAIN_ONE,
            card,
            WarpedSpace::Absent,
            |scenario| {
                scenario.with_life(P0, if enough_life { 5 } else { 2 });
                scenario.add_creature_from_oracle(P0, "Anhelo, the Painter", 1, 1, ANHELO);
                let fodder = scenario.add_creature(P0, "Casualty Fodder", 3, 3).id();
                let small = scenario.add_creature(P0, "Fling Fodder", 1, 1).id();
                let terror = scenario.add_creature_from_oracle(P1, "Terror of the Peaks", 5, 4,
                    "Flying\nSpells your opponents cast that target this creature cost an additional 3 life to cast.\nWhenever another creature you control enters, this creature deals damage equal to that creature's power to any target.").id();
                ids = Some((fodder, small, terror));
            },
        );
        let (fodder, small, terror) = ids.unwrap();
        let mana_before = runner.state().players[0].mana_pool.clone();
        let mut events = Vec::new();
        runner
            .act(GameAction::SelectCards {
                cards: vec![linked],
            })
            .inspect(|result| {
                events.extend(result.events.clone());
            })
            .expect("pick Fling");
        runner
            .act(GameAction::DecideOptionalCost { pay: true })
            .inspect(|result| {
                events.extend(result.events.clone());
            })
            .expect("elect casualty");
        runner
            .act(GameAction::SelectCards {
                cards: vec![fodder],
            })
            .inspect(|result| {
                events.extend(result.events.clone());
            })
            .expect("reserve casualty fodder");
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ));
        let _result = runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(terror)],
            })
            .inspect(|result| {
                events.extend(result.events.clone());
            })
            .expect("settle legal declaration or reverse illegal cast");
        if enough_life {
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::PayCost { .. }
            ));
            runner
                .act(GameAction::SelectCards { cards: vec![small] })
                .inspect(|result| {
                    events.extend(result.events.clone());
                })
                .expect("Fling sacrifice");
            runner
                .act(GameAction::PassPriority)
                .inspect(|result| {
                    events.extend(result.events.clone());
                })
                .expect("commit total");
            assert_eq!(runner.life(P0), 2);
            assert_eq!(runner.state().objects[&linked].zone, Zone::Stack);
            assert_eq!(runner.state().objects[&fodder].zone, Zone::Graveyard);
            assert_eq!(runner.state().objects[&small].zone, Zone::Graveyard);
        } else {
            assert_eq!(runner.life(P0), 2);
            assert_eq!(runner.state().players[0].mana_pool, mana_before);
            assert_eq!(runner.state().objects[&fodder].zone, Zone::Battlefield);
            assert_eq!(runner.state().objects[&small].zone, Zone::Battlefield);
            assert_eq!(runner.state().objects[&linked].zone, Zone::Exile);
            assert!(runner.state().objects[&linked]
                .casting_permissions
                .is_empty());
            assert!(runner
                .state()
                .stack
                .iter()
                .all(|entry| entry.id != linked && entry.source_id != saga));
            assert!(runner.state().pending_cast.is_none());
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            assert_eq!(cast_completions(&events, saga), 1);
        }
    }
}

// Reproduce the pre-election save omissions through the normal decoder.
fn legacy_pending_save(state: &engine::types::game_state::GameState) -> serde_json::Value {
    fn omit_election(value: &mut serde_json::Value) -> usize {
        match value {
            serde_json::Value::Object(fields) => {
                let own = usize::from(fields.remove("alternative_cost_election").is_some());
                own + fields.values_mut().map(omit_election).sum::<usize>()
            }
            serde_json::Value::Array(values) => values.iter_mut().map(omit_election).sum(),
            _ => 0,
        }
    }
    let mut value = serde_json::to_value(state).expect("save current unfinished cast");
    assert!(value
        .as_object_mut()
        .unwrap()
        .remove("spell_cast_reversal")
        .is_some());
    assert!(
        omit_election(&mut value) > 0,
        "the fixture really lacks the new declaration field"
    );
    value
}

/// CR 733.1: life/discard paid in an earlier answer is restored when the cast
/// is canceled, including after save/restore. Deferred casualty stays unpaid.
#[test]
fn canceling_a_linked_cast_restores_prior_life_or_discard_and_settles_the_chapter() {
    for discard in [false, true] {
        for (reload, library_operation) in [(false, 0u8), (true, 0), (false, 2), (false, 1)] {
            let library_disclosure = library_operation != 0;
            let card = OpponentCard { name: "Bitter Triumph", instant: true,
            cost: ManaCost::generic(2),
            text: "As an additional cost to cast this spell, discard a card or pay 3 life.\nDestroy target creature or planeswalker." };
            let mut ids = None;
            let Board {
                mut runner,
                sorcery: linked,
                saga,
                ..
            } = reach_chapter_two_on_with(
                VALIGARMANDA,
                GAIN_ONE,
                card,
                WarpedSpace::Unspent,
                |scenario| {
                    scenario.add_creature_from_oracle(P0, "Anhelo, the Painter", 1, 1, ANHELO);
                    scenario.add_land_to_hand(P0, "Plains");
                    scenario.add_land_to_hand(P0, "Island");
                    scenario
                        .add_spell_to_hand(P0, "Unrelated private hand marker 9751", true)
                        .with_mana_cost(ManaCost::zero())
                        .from_oracle_text(GAIN_ONE);
                    let fodder = scenario.add_creature(P0, "Fodder", 3, 3).id();
                    let target = scenario.add_creature(P1, "Target", 3, 3).id();
                    // Synthetic accepted mana grammar, for CR 733.1's library exception.
                    let forest = if library_disclosure {
                        scenario
                            .add_land_from_oracle(
                                P0,
                                "Library exception witness",
                                if library_operation == 2 {
                                    "{T}: Add {G}. Shuffle your library."
                                } else {
                                    "{T}: Add {G}. Reveal the top card of your library."
                                },
                            )
                            .id()
                    } else {
                        scenario.add_basic_land(P0, engine::types::mana::ManaColor::Green)
                    };
                    ids = Some((fodder, target, forest));
                },
            );
            let (fodder, target, forest) = ids.unwrap();
            runner.state_mut().loop_detection =
                engine::types::game_state::LoopDetectionMode::Interactive;
            let life_before = runner.life(P0);
            let hand_before = runner.state().players[0].hand.clone();
            let card_to_discard = hand_before[0];
            let mana_before = runner.state().players[0].mana_pool.clone();
            let mut events = Vec::new();
            runner
                .act(GameAction::SelectCards {
                    cards: vec![linked],
                })
                .inspect(|result| {
                    events.extend(result.events.clone());
                })
                .expect("pick Bitter Triumph");
            if !discard && !reload && !library_disclosure {
                let error = serde_json::from_value::<engine::types::game_state::GameState>(
                    legacy_pending_save(runner.state()),
                )
                .expect_err("legacy alternative-vs-additional Choice has no provenance");
                assert!(error.to_string().contains(
                    "Legacy cost choice has no alternative-vs-additional declaration authority"
                ));
            }
            runner
                .act(GameAction::DecideOptionalCost { pay: true })
                .inspect(|result| {
                    events.extend(result.events.clone());
                })
                .expect("elect zero");
            runner
                .act(GameAction::DecideOptionalCost { pay: true })
                .inspect(|result| {
                    events.extend(result.events.clone());
                })
                .expect("elect casualty");
            runner
                .act(GameAction::SelectCards {
                    cards: vec![fodder],
                })
                .inspect(|result| {
                    events.extend(result.events.clone());
                })
                .expect("reserve fodder");
            runner
                .act(GameAction::SelectTargets {
                    targets: vec![TargetRef::Object(target)],
                })
                .inspect(|result| {
                    events.extend(result.events.clone());
                })
                .expect("target creature");
            runner
                .act(GameAction::DecideOptionalCost { pay: discard })
                .inspect(|result| {
                    events.extend(result.events.clone());
                })
                .expect("decide printed cost");
            if discard {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![card_to_discard],
                    })
                    .inspect(|result| {
                        events.extend(result.events.clone());
                    })
                    .expect("pay discard");
                assert_eq!(
                    runner.state().objects[&card_to_discard].zone,
                    Zone::Graveyard
                );
            } else {
                assert_eq!(
                    runner.life(P0),
                    life_before - 3,
                    "reach guard: prior life payment"
                );
            }
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::ManaPayment { .. }
            ));
            let saved = serde_json::to_value(runner.state()).unwrap();
            let reversal = saved
                .get("spell_cast_reversal")
                .expect("paid cast has a reversal descriptor");
            assert!(
                reversal.get("baseline").is_none(),
                "there is no nested game snapshot"
            );
            assert!(
                !reversal
                    .to_string()
                    .contains("Unrelated private hand marker 9751"),
                "the unrelated private card is not copied into the undo descriptor"
            );
            for edit in reversal["undo"].as_array().unwrap() {
                let data = edit.as_object().unwrap().values().next().unwrap();
                let path = data["path"].as_array().unwrap();
                assert!(
                    !(path.len() == 3 && path[0] == "players" && path[2] == "library"),
                    "ordinary payment does not retain library contents"
                );
                assert!(
                    path.len() > 1 || (path[0] != "objects" && path[0] != "players"),
                    "only changed members are retained, never the complete object or player store"
                );
            }
            runner
                .act(GameAction::SetPriorityPassingMode {
                    mode: engine::types::game_state::PriorityPassingMode::FullControl,
                })
                .expect("change an independent preference during payment");
            let order: Vec<_> = runner.state().players[0]
                .hand
                .iter()
                .rev()
                .copied()
                .collect();
            runner
                .act(GameAction::ReorderHand {
                    order: order.clone(),
                })
                .expect("reorder the current hand");
            let restored_order: Vec<_> = if discard {
                std::iter::once(card_to_discard).chain(order).collect()
            } else {
                order
            };
            assert_eq!(
                runner.state().players[0].hand.len(),
                hand_before.len() - usize::from(discard)
            );
            let selection =
                engine::game::mana_sources::activatable_mana_source_selections(runner.state(), P0)
                    .into_iter()
                    .find(|selection| selection.source.object_id == forest)
                    .expect("Forest mana selection during the payment window");
            let library_command_start = runner.state().resolved_rules_journal.entries().len();
            runner
                .act(GameAction::TapLandForMana { selection })
                .expect("activate Forest after earlier spell cost");
            assert!(runner.state().objects[&forest].tapped);
            assert_eq!(
                runner.state().players[0].mana_pool.total(),
                mana_before.total() + 1
            );
            if reload {
                let mut legacy = GameRunner::from_state(
                    serde_json::from_value(legacy_pending_save(runner.state()))
                        .expect("legacy determined mana total can resume"),
                );
                let before = serde_json::to_value(legacy.state()).unwrap();
                let error = legacy
                    .act(GameAction::CancelCast)
                    .expect_err("legacy save must not invent missing cost history");
                assert!(error.to_string().contains("cannot be canceled safely"));
                assert_eq!(serde_json::to_value(legacy.state()).unwrap(), before);
                legacy
                    .act(GameAction::PassPriority)
                    .expect("finish already determined legacy total");
                assert_eq!(legacy.state().objects[&linked].zone, Zone::Stack);
                assert_eq!(legacy.state().objects[&fodder].zone, Zone::Graveyard);
                assert_eq!(legacy.state().alt_cost_grant_permissions_used.len(), 1);
                let saved =
                    serde_json::to_string(runner.state()).expect("save paid unfinished cast");
                runner = GameRunner::from_state(
                    serde_json::from_str(&saved).expect("restore paid unfinished cast"),
                );
            }
            let names: std::sync::Arc<[String]> = vec!["Bitter Triumph".into()].into();
            runner.state_mut().all_card_names = names.clone();
            for viewer in [P0, P1] {
                let projected =
                    engine::game::visibility::filter_state_for_viewer(runner.state(), viewer);
                assert!(serde_json::to_value(projected)
                    .unwrap()
                    .get("spell_cast_reversal")
                    .is_none());
            }
            if library_disclosure {
                if library_operation == 2 {
                    assert!(runner.state().resolved_rules_journal.entries_since(library_command_start)
                        .iter().any(|entry| matches!(entry.command.as_ref(), Some(
                            engine::types::resolved_commands::ResolvedRulesCommand::LibraryShuffle(_)))),
                        "reach: the mana ability really shuffled the library");
                } else {
                    let top = *runner.state().players[0]
                        .library
                        .front()
                        .expect("library still has a card");
                    assert!(
                        runner.state().public_revealed_cards.contains(&top),
                        "reach: the mana ability revealed the library top"
                    );
                }
                let before_cancel = serde_json::to_value(runner.state()).unwrap();
                let error = runner
                    .act(GameAction::CancelCast)
                    .expect_err("cannot undo a library shuffle or disclosure");
                assert!(
                    error
                        .to_string()
                        .contains("Cannot undo a library disclosure"),
                    "{error:?}"
                );
                assert_eq!(
                    serde_json::to_value(runner.state()).unwrap(),
                    before_cancel,
                    "refused cancel leaves the legal payment procedure intact"
                );
                runner
                    .act(GameAction::PassPriority)
                    .expect("the cast can still finish");
                assert_eq!(runner.state().objects[&linked].zone, Zone::Stack);
                assert_eq!(runner.state().objects[&fodder].zone, Zone::Graveyard);
                continue;
            }
            let _result = runner
                .act(GameAction::CancelCast)
                .inspect(|result| {
                    events.extend(result.events.clone());
                })
                .expect("reverse whole cast");
            assert_eq!(runner.life(P0), life_before);
            assert_eq!(
                runner.state().players[0]
                    .hand
                    .iter()
                    .copied()
                    .collect::<Vec<_>>(),
                restored_order
            );
            assert_eq!(
                runner.state().priority_passing_mode(P0),
                engine::types::game_state::PriorityPassingMode::FullControl,
                "canceling a cast must retain a later actor-scoped preference"
            );
            assert_eq!(runner.state().objects[&card_to_discard].zone, Zone::Hand);
            assert_eq!(runner.state().players[0].mana_pool, mana_before);
            assert!(
                !runner.state().objects[&forest].tapped,
                "earlier-cost reversal also undoes legal mana"
            );
            assert_eq!(
                runner.state().all_card_names,
                names,
                "reload runtime metadata survives cancellation"
            );
            if !reload {
                let trace = engine::game::play_trace_view(runner.state()).expect("live play trace");
                assert!(trace.entries.iter().all(|entry| !matches!(&entry.kind,
                engine::game::EntryKind::Play { locus: engine::game::PlayLocus::Mana(id, _), .. } if *id == forest)),
                "the trace must not retain the undone Forest activation");
            }
            assert_eq!(runner.state().objects[&fodder].zone, Zone::Battlefield);
            assert_eq!(runner.state().objects[&linked].zone, Zone::Exile);
            assert!(runner.state().objects[&linked]
                .casting_permissions
                .is_empty());
            assert!(runner.state().pending_cast.is_none());
            assert!(runner
                .state()
                .stack
                .iter()
                .all(|entry| entry.id != linked && entry.source_id != saga));
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            assert!(runner.state().alt_cost_grant_permissions_used.is_empty());
            assert_eq!(cast_completions(&events, saga), 1);
        }
    }
}

/// CR 118.9d + CR 601.2b/f: the alternative declaration and the printed
/// choice/kicker are separate questions, through the real linked-Saga route.
#[test]
fn linked_alternative_costs_preserve_printed_choice_and_kicker() {
    for kicker in [false, true] {
        for pay_zero in [false, true] {
            for pay_extra in [false, true] {
                let card = OpponentCard {
                    name: "Additional declaration witness",
                    instant: true,
                    cost: ManaCost::generic(1),
                    text: if kicker {
                        "Kicker {1}\nYou gain 1 life."
                    } else {
                        "As an additional cost to cast this spell, discard a card or pay 3 life. You gain 1 life."
                    },
                };
                let Board {
                    mut runner,
                    sorcery: linked,
                    ..
                } = reach_chapter_two_on(VALIGARMANDA, GAIN_ONE, card, WarpedSpace::Unspent);
                let hand = runner.state().players[0].hand[0];
                let life = runner.life(P0);
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![linked],
                    })
                    .expect("pick linked card");
                assert!(matches!(
                    runner.state().waiting_for,
                    WaitingFor::OptionalCostChoice {
                        cost: AdditionalCost::Choice(..),
                        ..
                    }
                ));
                runner
                    .act(GameAction::DecideOptionalCost { pay: pay_zero })
                    .expect("elect alternative");
                assert!(
                    matches!(
                        runner.state().waiting_for,
                        WaitingFor::OptionalCostChoice { .. }
                    ),
                    "reach guard: printed additional obligation still offered"
                );
                runner
                    .act(GameAction::DecideOptionalCost { pay: pay_extra })
                    .expect("elect printed extra");
                if !kicker && pay_extra {
                    runner
                        .act(GameAction::SelectCards { cards: vec![hand] })
                        .expect("discard one card");
                }
                for _ in 0..8 {
                    match runner.state().waiting_for {
                        WaitingFor::ManaPayment { .. } => {
                            runner.act(GameAction::PassPriority).expect("commit mana");
                        }
                        _ => break,
                    }
                }
                assert_eq!(runner.state().objects[&linked].zone, Zone::Stack);
                assert_eq!(
                    runner.state().alt_cost_grant_permissions_used.len(),
                    usize::from(pay_zero)
                );
                let paid_mana = usize::from(!pay_zero) + usize::from(kicker && pay_extra);
                assert_eq!(runner.state().players[0].mana_pool.total(), 2 - paid_mana);
                if kicker {
                    let entry = runner
                        .state()
                        .stack
                        .iter()
                        .find(|entry| entry.id == linked)
                        .unwrap();
                    assert_eq!(
                        entry.ability().unwrap().context.kickers_paid.len(),
                        usize::from(pay_extra)
                    );
                } else {
                    assert_eq!(runner.life(P0), life - if pay_extra { 0 } else { 3 });
                    assert_eq!(
                        runner.state().objects[&hand].zone,
                        if pay_extra {
                            Zone::Graveyard
                        } else {
                            Zone::Hand
                        }
                    );
                }
            }
        }
    }
}

/// CR 118.9a/d: a selected free permission is already the alternative;
/// Warped Space must not replace it or spend a second grant's once-turn slot.
#[test]
fn a_selected_free_cast_keeps_its_required_cost_without_a_second_alternative() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_warped_space(&mut scenario);
    let caster = scenario.add_creature_from_oracle(P0, "Free cast grammar witness", 1, 1,
        "{T}: Exile target instant or sorcery card from your graveyard. You may cast that card without paying its mana cost.").id();
    let fodder = scenario.add_creature(P0, "Fling Fodder", 3, 3).id();
    let fling = scenario.add_spell_to_graveyard(P0, "Fling", true)
        .with_mana_cost(ManaCost::Cost { shards: vec![ManaCostShard::Red], generic: 1 })
        .from_oracle_text("As an additional cost to cast this spell, sacrifice a creature.\nFling deals damage equal to the sacrificed creature's power to any target.").id();
    let mut runner = scenario.build();
    seed_unlocked_warped_space(&mut runner);
    runner
        .act(GameAction::ActivateAbility {
            source_id: caster,
            ability_index: 0,
        })
        .expect("activate the free-cast instruction");
    runner.resolve_top();
    assert_eq!(
        runner.state().objects[&fling].zone,
        Zone::Exile,
        "the production instruction exiled Fling"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "the parsed free cast is offered as its granting ability resolves; got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("choose the free cast");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ));
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Player(P1)],
        })
        .expect("target P1");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::PayCost { .. }),
        "only Fling's required sacrifice remains; got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![fodder],
        })
        .expect("pay required sacrifice");
    if matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
        runner
            .act(GameAction::PassPriority)
            .expect("commit free cast plus sacrifice");
    }
    assert_eq!(runner.state().objects[&fling].zone, Zone::Stack);
    assert_eq!(runner.state().objects[&fodder].zone, Zone::Graveyard);
    assert!(runner.state().alt_cost_grant_permissions_used.is_empty());
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
}

/// CR 601.2f/h: even a replacement-ordering sacrifice must not remove a
/// cost-floor source before the spell's alternative plus kicker total is locked.
#[test]
fn a_linked_kicker_cast_locks_its_floor_before_replacement_ordering_sacrifices() {
    for (replacements, reload) in [(false, false), (true, false), (true, true)] {
        let mut ids = None;
        let Board { mut runner, sorcery: linked, .. } = reach_chapter_two_on_with(
            VALIGARMANDA, GAIN_ONE,
            OpponentCard { name: "Vines of Vastwood", instant: true,
                cost: ManaCost::Cost { shards: vec![ManaCostShard::Green], generic: 0 },
                text: "Kicker {G} (You may pay an additional {G} as you cast this spell.)\nTarget creature can't be the target of spells or abilities your opponents control this turn. If this spell was kicked, that creature gets +4/+4 until end of turn." },
            WarpedSpace::Unspent, |scenario| {
                scenario.add_creature_from_oracle(P0, "Anhelo, the Painter", 1, 3, ANHELO);
                scenario.add_enchantment_from_oracle(P0, "March of the Machines",
                    "Each noncreature artifact is an artifact creature with power and toughness each equal to its mana value. (Equipment that's a creature can't equip a creature.)")
                    .with_mana_cost(ManaCost::Cost { shards: vec![ManaCostShard::Blue], generic: 3 });
                let trinisphere = scenario.add_artifact_from_oracle(P0, "Trinisphere",
                    "As long as this artifact is untapped, each spell that would cost less than three mana to cast costs three mana to cast. (Additional mana in the cost may be paid with any color of mana or colorless mana. For example, a spell that would cost {1}{B} to cast costs {2}{B} to cast instead.)")
                    .with_mana_cost(ManaCost::generic(3)).id();
                let target = scenario.add_creature(P0, "Target", 3, 3).id();
                let forests: Vec<_> = (0..3).map(|_| scenario.add_basic_land(P0,
                    engine::types::mana::ManaColor::Green)).collect();
                if replacements {
                    scenario.add_enchantment_from_oracle(P1, "Leyline of the Void",
                        "If this card is in your opening hand, you may begin the game with it on the battlefield.\nIf a card would be put into an opponent's graveyard from anywhere, exile it instead.")
                        .with_mana_cost(ManaCost::Cost { shards: vec![ManaCostShard::Black, ManaCostShard::Black], generic: 2 });
                    scenario.add_creature_from_oracle(P1, "Dauthi Voidwalker", 3, 2,
                        "Shadow (This creature can block or be blocked by only creatures with shadow.)\nIf a card would be put into an opponent's graveyard from anywhere, instead exile it with a void counter on it.\n{T}, Sacrifice this creature: Choose an exiled card an opponent owns with a void counter on it. You may play it this turn without paying its mana cost.")
                        .with_mana_cost(ManaCost::Cost { shards: vec![ManaCostShard::Black, ManaCostShard::Black], generic: 0 });
                }
                ids = Some((trinisphere, target, forests));
            });
        let (trinisphere, target, forests) = ids.unwrap();
        assert_eq!(runner.state().objects[&trinisphere].power, Some(3));
        assert!(!runner.state().objects[&trinisphere].tapped);
        runner
            .act(GameAction::SelectCards {
                cards: vec![linked],
            })
            .expect("pick Vines");
        runner
            .act(GameAction::DecideOptionalCost { pay: true })
            .expect("elect Room zero");
        runner
            .act(GameAction::DecideOptionalCost { pay: true })
            .expect("elect casualty");
        runner
            .act(GameAction::SelectCards {
                cards: vec![trinisphere],
            })
            .expect("select animated floor source");
        assert_eq!(
            runner.state().objects[&trinisphere].zone,
            Zone::Battlefield,
            "selection must not pay a replacement-aware sacrifice"
        );
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(target)],
            })
            .expect("target the other creature");
        runner
            .act(GameAction::DecideOptionalCost { pay: true })
            .expect("elect kicker");
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ManaPayment { .. }
        ));
        assert_eq!(
            runner
                .state()
                .pending_cast
                .as_ref()
                .unwrap()
                .cost
                .mana_value(),
            3,
            "zero alternative plus one-mana kicker keeps the original three-mana floor"
        );
        for forest in forests {
            let selection =
                engine::game::mana_sources::activatable_mana_source_selections(runner.state(), P0)
                    .into_iter()
                    .find(|selection| selection.source.object_id == forest)
                    .expect("each untapped Forest is an authoritative mana source");
            runner
                .act(GameAction::TapLandForMana { selection })
                .expect("activate Forest for the locked total");
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pay the locked three mana");
        if replacements {
            assert!(
                matches!(
                    runner.state().waiting_for,
                    WaitingFor::ReplacementChoice { .. }
                ),
                "reach: two actual sacrifice replacements require ordering at payment"
            );
            if reload {
                runner = GameRunner::from_state(
                    serde_json::from_str(&serde_json::to_string(runner.state()).unwrap()).unwrap(),
                );
            }
            runner
                .act(GameAction::ChooseReplacement { index: 0 })
                .expect("resume the paid spell after replacement ordering");
        }
        assert_eq!(
            runner.state().objects[&trinisphere].zone,
            if replacements {
                Zone::Exile
            } else {
                Zone::Graveyard
            }
        );
        assert_eq!(runner.state().objects[&linked].zone, Zone::Stack);
        let entry = runner
            .state()
            .stack
            .iter()
            .find(|entry| entry.id == linked)
            .unwrap();
        let engine::types::game_state::StackEntryKind::Spell {
            actual_mana_spent, ..
        } = &entry.kind
        else {
            panic!("the original cast must be a spell");
        };
        assert_eq!(
            *actual_mana_spent, 3,
            "one exact mana payment across the pause"
        );
        assert_eq!(entry.ability().unwrap().context.kickers_paid.len(), 1);
        assert_eq!(runner.state().alt_cost_grant_permissions_used.len(), 1);
        assert!(runner.state().pending_cast.is_none());
        assert!(runner.state().pending_cost_move_resume.is_none());
    }
}

/// CR 733.1 + CR 800.4a: undo this spell's prior payment, while a different
/// player's independent concession remains settled. Both survive serde restore.
#[test]
fn canceling_a_paid_cast_preserves_another_players_concession() {
    for reload in [false, true] {
        let p2 = engine::types::player::PlayerId(2);
        let mut scenario = GameScenario::new_n_player(3, 9751);
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_land_to_hand(P0, "Forest");
        scenario.with_mana_pool(
            P0,
            vec![
                engine::types::mana::ManaUnit::new(
                    engine::types::mana::ManaType::Black,
                    ObjectId(0),
                    false,
                    vec![],
                ),
                engine::types::mana::ManaUnit::new(
                    engine::types::mana::ManaType::Black,
                    ObjectId(0),
                    false,
                    vec![],
                ),
            ],
        );
        let target = scenario.add_creature(P0, "Target", 3, 3).id();
        let spell = scenario.add_spell_to_hand(P0, "Bitter Triumph", true)
            .with_mana_cost(ManaCost::Cost { shards: vec![ManaCostShard::Black], generic: 1 })
            .from_oracle_text("As an additional cost to cast this spell, discard a card or pay 3 life.\nDestroy target creature or planeswalker.").id();
        let mut runner = scenario.build();
        let card_id = runner.state().objects[&spell].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: spell,
                card_id,
                targets: Vec::new(),
                payment_mode: engine::types::game_state::CastPaymentMode::Manual,
            })
            .expect("announce the printed spell");
        let WaitingFor::OptionalCostChoice { pending_cast, .. } = &runner.state().waiting_for
        else {
            panic!("the sole legal target leads to the printed cost choice");
        };
        assert_eq!(
            pending_cast.ability.targets,
            vec![TargetRef::Object(target)]
        );
        runner
            .act(GameAction::DecideOptionalCost { pay: false })
            .expect("pay three life");
        assert_eq!(
            runner.life(P0),
            17,
            "reach: an earlier spell payment was committed"
        );
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ManaPayment { .. }
        ));
        runner
            .act(GameAction::Concede { player_id: p2 })
            .expect("the third player concedes independently");
        assert!(runner.state().players[2].is_eliminated);
        if reload {
            runner = GameRunner::from_state(
                serde_json::from_str(&serde_json::to_string(runner.state()).unwrap()).unwrap(),
            );
        }
        runner
            .act(GameAction::CancelCast)
            .expect("cancel only the unfinished spell");
        assert_eq!(runner.life(P0), 20);
        assert!(
            runner.state().players[2].is_eliminated,
            "concession is not a casting payment"
        );
        assert_eq!(runner.state().objects[&spell].zone, Zone::Hand);
        assert!(runner.state().pending_cast.is_none());
        assert!(runner.state().stack.is_empty());
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ));
    }
}

/// CR 601.2g-h + CR 733.1: reserved casualty fodder cannot pay a second
/// sacrifice. A different Blood Vassal can legally pay its mana ability; cancel
/// returns that payment and the earlier life payment, while completion keeps both.
#[test]
fn canceling_a_linked_cast_refunds_prior_life_and_a_later_mana_sacrifice() {
    for (cancel_cast, reload) in [(true, false), (true, true), (false, false)] {
        let mut ids = None;
        let Board { mut runner, sorcery: linked, saga, .. } = reach_chapter_two_on_with(
            VALIGARMANDA, GAIN_ONE,
            OpponentCard { name: "Bitter Triumph", instant: true,
                cost: ManaCost::Cost { shards: vec![ManaCostShard::Black], generic: 1 },
                text: "As an additional cost to cast this spell, discard a card or pay 3 life.\nDestroy target creature or planeswalker." },
            WarpedSpace::Unspent, |scenario| {
                scenario.add_land_to_hand(P0, "Forest");
                scenario.add_creature_from_oracle(P0, "Anhelo, the Painter", 1, 1, ANHELO);
                let reserved = scenario.add_creature_from_oracle(P0, "Blood Vassal", 2, 2,
                    "Sacrifice this creature: Add {B}{B}.")
                    .with_mana_cost(ManaCost::Cost { shards: vec![ManaCostShard::Black], generic: 2 })
                    .with_subtypes(vec!["Thrull"]).id();
                let other = scenario.add_creature_from_oracle(P0, "Blood Vassal", 2, 2,
                    "Sacrifice this creature: Add {B}{B}.")
                    .with_mana_cost(ManaCost::Cost { shards: vec![ManaCostShard::Black], generic: 2 })
                    .with_subtypes(vec!["Thrull"]).id();
                let target = scenario.add_creature(P1, "Target", 3, 3).id();
                ids = Some((reserved, other, target));
            });
        let (reserved, other, target) = ids.unwrap();
        runner.state_mut().loop_detection =
            engine::types::game_state::LoopDetectionMode::Interactive;
        let mana_before = runner.state().players[0].mana_pool.clone();
        let mut events = Vec::new();
        for action in [
            GameAction::SelectCards {
                cards: vec![linked],
            },
            GameAction::DecideOptionalCost { pay: true },
            GameAction::DecideOptionalCost { pay: true },
            GameAction::SelectCards {
                cards: vec![reserved],
            },
            GameAction::SelectTargets {
                targets: vec![TargetRef::Object(target)],
            },
            GameAction::DecideOptionalCost { pay: false },
        ] {
            events.extend(
                runner
                    .act(action)
                    .expect("declare and pay the printed life choice")
                    .events,
            );
        }
        assert_eq!(runner.life(P0), 17, "reach: a prior action paid three life");
        assert_eq!(runner.state().objects[&reserved].zone, Zone::Battlefield);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ManaPayment { .. }
        ));
        let before_rejected = serde_json::to_value(runner.state()).unwrap();
        let error = runner
            .act(GameAction::ActivateAbility {
                source_id: reserved,
                ability_index: 0,
            })
            .expect_err("one permanent cannot pay both sacrifices");
        assert!(error
            .to_string()
            .contains("already committed to a spell sacrifice cost"));
        assert_eq!(
            serde_json::to_value(runner.state()).unwrap(),
            before_rejected
        );
        events.extend(
            runner
                .act(GameAction::ActivateAbility {
                    source_id: other,
                    ability_index: 0,
                })
                .expect("activate the other Blood Vassal during the mana window")
                .events,
        );
        assert_eq!(
            runner.state().objects[&other].zone,
            Zone::Graveyard,
            "reach: the legal mana ability really sacrificed its own source"
        );
        assert_eq!(
            runner.state().players[0].mana_pool.total(),
            mana_before.total() + 2
        );
        if reload {
            runner = GameRunner::from_state(
                serde_json::from_str(&serde_json::to_string(runner.state()).unwrap()).unwrap(),
            );
        }
        let action = if cancel_cast {
            GameAction::CancelCast
        } else {
            GameAction::PassPriority
        };
        events.extend(
            runner
                .act(action)
                .expect("cancel the paid cast or finish its legal total")
                .events,
        );
        if cancel_cast {
            assert_eq!(runner.life(P0), 20);
            assert_eq!(runner.state().players[0].mana_pool, mana_before);
            assert_eq!(runner.state().objects[&reserved].zone, Zone::Battlefield);
            assert_eq!(runner.state().objects[&other].zone, Zone::Battlefield);
            assert_eq!(runner.state().objects[&linked].zone, Zone::Exile);
            assert!(runner.state().objects[&linked]
                .casting_permissions
                .is_empty());
            assert!(runner.state().pending_cast.is_none());
            assert!(runner
                .state()
                .stack
                .iter()
                .all(|entry| entry.id != linked && entry.source_id != saga));
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            assert!(runner.state().alt_cost_grant_permissions_used.is_empty());
            assert_eq!(cast_completions(&events, saga), 1);
            if !reload {
                let trace = engine::game::play_trace_view(runner.state()).expect("live play trace");
                assert!(trace.entries.iter().all(|entry| !matches!(&entry.kind,
                    engine::game::EntryKind::Play { locus: engine::game::PlayLocus::Mana(id, _), .. } if *id == other)),
                    "the trace does not retain the reversed mana sacrifice");
            }
            let sources =
                engine::game::mana_sources::activatable_mana_source_selections(runner.state(), P0);
            assert!(
                sources
                    .iter()
                    .any(|selection| selection.source.object_id == other),
                "the returned creature keeps its printed mana ability"
            );
        } else {
            assert_eq!(runner.life(P0), 17);
            assert_eq!(runner.state().objects[&linked].zone, Zone::Stack);
            assert_eq!(runner.state().objects[&reserved].zone, Zone::Graveyard);
            assert_eq!(runner.state().objects[&other].zone, Zone::Graveyard);
            assert_eq!(runner.state().alt_cost_grant_permissions_used.len(), 1);
        }
    }
}

/// CR 118.9d + CR 733.1: synthetic no-mana-cost spell with one supported
/// life cost. The printed Room supplies its sole alternative and Dovin supplies
/// a mana increase, so the selecting input pays life and leaves payment open.
#[test]
fn canceling_a_linked_cast_preserves_its_parent_when_the_first_input_pays_life() {
    for (cancel, reload) in [(true, false), (true, true), (false, false)] {
        let Board {
            mut runner,
            sorcery: linked,
            saga,
            chapter_two_events,
            ..
        } = reach_chapter_two_on_with(
            VALIGARMANDA,
            GAIN_ONE,
            OpponentCard {
                name: "No printed cost life payment witness",
                instant: true,
                cost: ManaCost::NoCost,
                text: "As an additional cost to cast this spell, pay 3 life.\nYou gain 1 life.",
            },
            WarpedSpace::Unspent,
            |scenario| {
                scenario.add_planeswalker_from_oracle(P1, "Dovin, Hand of Control", "Dovin", 5,
                    "Artifact, instant, and sorcery spells your opponents cast cost {1} more to cast.\n[−1]: Until your next turn, prevent all damage that would be dealt to and dealt by target permanent an opponent controls.")
                    .with_mana_cost(ManaCost::Cost { shards: vec![ManaCostShard::WhiteBlue], generic: 2 });
            },
        );
        let mana_before = runner.state().players[0].mana_pool.clone();
        let mut events = chapter_two_events;
        events.extend(
            runner
                .act(GameAction::SelectCards {
                    cards: vec![linked],
                })
                .expect("elect the sole alternative and begin its additional cost")
                .events,
        );
        assert_eq!(
            runner.life(P0),
            17,
            "reach: the selecting input itself paid life"
        );
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
            "{:?}",
            runner.state().waiting_for
        );
        assert_eq!(
            runner
                .state()
                .pending_cast
                .as_ref()
                .unwrap()
                .cost
                .mana_value(),
            1,
            "the real cost increase leaves a nonzero mana component"
        );
        assert_eq!(
            cast_completions(&events, saga),
            1,
            "the shared instruction marker already exists while its cast waits for payment"
        );
        if reload {
            runner = GameRunner::from_state(
                serde_json::from_str(&serde_json::to_string(runner.state()).unwrap()).unwrap(),
            );
        }
        if cancel {
            events.extend(
                runner
                    .act(GameAction::CancelCast)
                    .expect("cancel the paid initial input")
                    .events,
            );
            assert_eq!(runner.life(P0), 20);
            assert_eq!(runner.state().objects[&linked].zone, Zone::Exile);
            assert!(runner.state().objects[&linked]
                .casting_permissions
                .is_empty());
            assert!(runner.state().pending_cast.is_none());
            assert!(runner
                .state()
                .stack
                .iter()
                .all(|entry| entry.id != linked && entry.source_id != saga));
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            assert_eq!(cast_completions(&events, saga), 1);
            assert!(runner.state().alt_cost_grant_permissions_used.is_empty());
            assert_eq!(runner.state().players[0].mana_pool, mana_before);
        } else {
            events.extend(
                runner
                    .act(GameAction::PassPriority)
                    .expect("pay the cost increase and complete")
                    .events,
            );
            assert_eq!(cast_completions(&events, saga), 1);
            assert_eq!(runner.life(P0), 17);
            assert_eq!(runner.state().objects[&linked].zone, Zone::Stack);
            assert_eq!(runner.state().alt_cost_grant_permissions_used.len(), 1);
            assert_eq!(
                runner.state().players[0].mana_pool.total(),
                mana_before.total() - 1
            );
        }
    }
}

/// Verbatim Oracle text (MTGJSON `text`): U+2014 row separators, U+2212 minus signs.
const COMET: &str = "[0]: Roll a six-sided die.\n\
1 or 2 \u{2014} [+2], then create two 1/1 green Squirrel creature tokens. They gain haste until end of turn.\n\
3 \u{2014} [\u{2212}1], then return a card with mana value 2 or less from your graveyard to your hand.\n\
4 or 5 \u{2014} Comet deals damage equal to the number of loyalty counters on him to a creature or player, then [\u{2212}2].\n\
6 \u{2014} [+1], and you may activate Comet's loyalty ability two more times this turn.";

/// Make the next six-sided roll land on `face`: the generator and draw
/// `roll_natural` uses, so the face is derived, not assumed.
fn force_next_d6(runner: &mut GameRunner, face: u8) {
    let seed = (0u64..10_000)
        .find(|&seed| {
            GameRng::seed_from_u64(seed)
                .draw(RandomDraw::Outcome)
                .random_range(1..=6u8)
                == face
        })
        .expect("a seed exists for every face");
    let state = runner.state_mut();
    state.rng_seed = seed;
    state.rng_word_pos = 0;
    state.rng = GameRng::seed_from_u64(seed);
}

/// CR 606.3 + CR 733.1: canceling a spell cannot give a planeswalker a
/// second loyalty activation after its unrelated earlier ability resolved,
/// and cannot take away Comet's two extra activations granted earlier.
#[test]
fn canceling_a_paid_cast_keeps_an_independent_loyalty_activation() {
    for cancel_cast in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P0, &["Forest", "Forest"]);
        scenario.with_library_top(P1, &["Forest", "Forest"]);
        let jace = scenario.add_planeswalker_from_oracle(P0, "Jace Beleren", "Jace", 3,
            "[+2]: Each player draws a card.\n[−1]: Target player draws a card.\n[−10]: Target player mills twenty cards.").id();
        let comet = scenario
            .add_planeswalker_from_oracle(P0, "Comet, Stellar Pup", "Comet", 5, COMET)
            .id();
        let blood = scenario
            .add_creature_from_oracle(
                P0,
                "Blood Vassal",
                2,
                2,
                "Sacrifice this creature: Add {B}{B}.",
            )
            .id();
        let target = scenario.add_creature(P1, "Target", 3, 3).id();
        let spell = scenario.add_spell_to_hand(P0, "Bitter Triumph", true)
            .with_mana_cost(ManaCost::Cost { shards: vec![ManaCostShard::Black], generic: 1 })
            .from_oracle_text("As an additional cost to cast this spell, discard a card or pay 3 life.\nDestroy target creature or planeswalker.").id();
        scenario.with_mana_pool(
            P0,
            (0..2)
                .map(|_| {
                    engine::types::mana::ManaUnit::new(
                        engine::types::mana::ManaType::Black,
                        ObjectId(0),
                        false,
                        vec![],
                    )
                })
                .collect(),
        );
        let mut runner = scenario.build();
        runner.activate(jace, 0).resolve();
        assert_eq!(
            runner.state().objects[&jace].loyalty_activations_this_turn,
            1
        );
        runner
            .act(GameAction::ActivateAbility {
                source_id: jace,
                ability_index: 0,
            })
            .expect_err("the earlier resolved activation has spent this turn's loyalty window");
        runner
            .act(GameAction::ActivateAbility {
                source_id: comet,
                ability_index: 0,
            })
            .expect("activate Comet's [0]");
        force_next_d6(&mut runner, 6);
        let mut rolled = Vec::new();
        while !runner.state().stack.is_empty() {
            rolled.extend(
                runner
                    .act(GameAction::PassPriority)
                    .expect("resolve Comet's roll")
                    .events,
            );
        }
        assert!(rolled.iter().any(|event| matches!(
            event,
            GameEvent::DieRolled {
                sides: 6,
                result: Some(6),
                ..
            }
        )));
        assert!(runner.state().stack.is_empty());
        let comet_rules = |runner: &GameRunner| {
            let object = &runner.state().objects[&comet];
            (
                object.loyalty_activations_this_turn,
                object.extra_loyalty_activations_this_turn,
            )
        };
        assert_eq!(comet_rules(&runner), (1, 2));
        let card_id = runner.state().objects[&spell].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: spell,
                card_id,
                targets: Vec::new(),
                payment_mode: engine::types::game_state::CastPaymentMode::Manual,
            })
            .expect("announce Bitter Triumph");
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(target)],
            })
            .expect("select the creature through the announcement prompt");
        runner
            .act(GameAction::DecideOptionalCost { pay: false })
            .expect("pay three life");
        assert_eq!(runner.life(P0), 17);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ManaPayment { .. }
        ));
        runner
            .act(GameAction::ActivateAbility {
                source_id: blood,
                ability_index: 0,
            })
            .expect("sacrifice Blood Vassal for mana during payment");
        assert_eq!(runner.state().objects[&blood].zone, Zone::Graveyard);
        runner
            .act(if cancel_cast {
                GameAction::CancelCast
            } else {
                GameAction::PassPriority
            })
            .expect("cancel or complete the spell");
        assert_eq!(
            runner.state().objects[&spell].zone,
            if cancel_cast { Zone::Hand } else { Zone::Stack }
        );
        assert_eq!(
            runner.state().objects[&blood].zone,
            if cancel_cast {
                Zone::Battlefield
            } else {
                Zone::Graveyard
            }
        );
        assert_eq!(runner.life(P0), if cancel_cast { 20 } else { 17 });
        assert_eq!(
            runner.state().objects[&jace].loyalty_activations_this_turn,
            1,
            "an unrelated resolved loyalty activation is not a casting payment"
        );
        assert_eq!(
            comet_rules(&runner),
            (1, 2),
            "Comet's earlier grant is not part of the reversed action"
        );
        if cancel_cast {
            // CR 606.3: with the stack empty again, Comet's grant still opens
            // his loyalty window; a still-pending spell would close it anyway.
            assert!(can_activate_loyalty_ability(runner.state(), comet, P0, 0));
            runner
                .act(GameAction::ActivateAbility {
                    source_id: jace,
                    ability_index: 0,
                })
                .expect_err("canceling another spell cannot reopen the loyalty window");
        }
    }
}

/// Synthetic review witness, not printed Oracle text. CR 605.1a/605.3b:
/// exile-to-stack does not disqualify a mana ability, but this inline resolver
/// cannot suspend for the cast. The parser must expose the whole ability as a
/// gap. With a library-moving activation cost, the same cast legitimately uses
/// the stack and remains supported (CR 608.2g).
fn check_linked_paid_cast_mana_boundary(route: &str) {
    for mana_first in [false, true] {
        for mana_ability in [false, true] {
            for accept in [false, true] {
                let mut scenario = GameScenario::new();
                scenario.at_phase(Phase::PreCombatMain);
                let cost = if mana_ability {
                    "{T}"
                } else {
                    "{T}, Mill a card"
                };
                let effects = if mana_first {
                    "Add {G}."
                } else {
                    "You gain 1 life. Add {G}."
                };
                let activated = format!(
                    "{cost}: {effects} You may cast an instant or sorcery card exiled with ~."
                );
                let body = match route {
                    "printed" => activated.to_string(),
                    "quoted" => format!("This artifact has \"{activated}\"."),
                    "station" => format!("0+ | {activated}"),
                    _ => unreachable!("fixed test routes"),
                };
                let text = format!(
                    "{{0}}: Exile target instant or sorcery card from a graveyard.\n{body}"
                );
                let source = scenario
                    .add_artifact_from_oracle(P0, "Linked cast boundary", &text)
                    .with_subtypes(vec!["Spacecraft"])
                    .id();
                let child = scenario
                    .add_spell_to_graveyard(P0, "Linked child", true)
                    .with_mana_cost(ManaCost::zero())
                    .from_oracle_text(GAIN_ONE)
                    .id();
                let mill = scenario
                    .add_spell_to_library_top(P0, "Mill cost", true)
                    .with_mana_cost(ManaCost::zero())
                    .from_oracle_text(GAIN_ONE)
                    .id();
                let mut runner = scenario.build();
                runner.activate(source, 0).target_object(child).resolve();
                assert_eq!(
                    runner.state().objects[&source].abilities.len(),
                    2,
                    "route {route}"
                );
                assert_eq!(runner.state().objects[&child].zone, Zone::Exile);
                let ability = &runner.state().objects[&source].abilities[1];
                if mana_ability {
                    assert!(
                        matches!(&*ability.effect, engine::types::ability::Effect::Unimplemented {name, ..}
                    if name == "linked_exile_mana_cast")
                    );
                    assert!(ability.sub_ability.is_none());
                    assert!(!engine::game::mana_abilities::is_mana_ability(ability));
                    assert!(
                        engine::game::mana_sources::activatable_mana_source_selections(
                            runner.state(),
                            P0
                        )
                        .iter()
                        .all(|selection| selection.source.object_id != source)
                    );
                    continue;
                }
                assert!(runner
                    .state()
                    .exile_links
                    .iter()
                    .any(|link| link.source_id == source && link.exiled_id == child));
                if mana_first {
                    assert!(matches!(
                        &*ability.effect,
                        engine::types::ability::Effect::Mana { .. }
                    ));
                } else {
                    assert!(matches!(
                        &*ability.effect,
                        engine::types::ability::Effect::GainLife { .. }
                    ));
                }
                assert!(!engine::game::mana_abilities::is_mana_ability(ability));
                runner
                    .act(GameAction::ActivateAbility {
                        source_id: source,
                        ability_index: 1,
                    })
                    .expect("activate the stack-using partner");
                assert_eq!(runner.state().objects[&mill].zone, Zone::Graveyard);
                settle(&mut runner);
                assert_eq!(pool_of(&runner), vec![child]);
                runner
                    .act(GameAction::SelectCards {
                        cards: if accept { vec![child] } else { vec![] },
                    })
                    .expect("choose zero or one linked spell");
                for _ in 0..8 {
                    if !matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
                        break;
                    }
                    runner.act(GameAction::PassPriority).expect("pay zero");
                }
                assert_eq!(
                    runner.state().objects[&child].zone,
                    if accept { Zone::Stack } else { Zone::Exile }
                );
                assert!(runner.state().objects[&child]
                    .casting_permissions
                    .is_empty());
                assert!(runner.state().pending_cast.is_none());
                assert!(matches!(
                    runner.state().waiting_for,
                    WaitingFor::Priority { .. }
                ));
            }
        }
    }
}

#[test]
fn linked_paid_casts_refuse_inline_mana_but_preserve_stack_resolution() {
    check_linked_paid_cast_mana_boundary("printed");
}

#[test]
fn linked_paid_casts_refuse_quoted_inline_mana_but_preserve_stack_resolution() {
    check_linked_paid_cast_mana_boundary("quoted");
}

#[test]
fn linked_paid_casts_refuse_station_inline_mana_but_preserve_stack_resolution() {
    check_linked_paid_cast_mana_boundary("station");
}

/// Synthetic review witness. CR 605.1b/605.4a: adding mana after a mana
/// activation must resolve immediately even when a later clause casts a card.
/// A life-gain trigger with the same body is an ordinary stack-using partner.
#[test]
fn linked_paid_casts_refuse_mana_triggers_but_preserve_ordinary_triggers() {
    for mana_trigger in [false, true] {
        for accept in [false, true] {
            let mut scenario = GameScenario::new();
            let text = if mana_trigger {
                "{0}: Exile target instant or sorcery card from a graveyard.\nWhenever you tap a land for mana, add {G}. You may cast an instant or sorcery card exiled with ~."
            } else {
                "{0}: Exile target instant or sorcery card from a graveyard.\nWhenever you gain life, add {G}. You may cast an instant or sorcery card exiled with ~."
            };
            let source = scenario
                .add_enchantment_from_oracle(P0, "Linked trigger boundary", text)
                .id();
            let child = scenario
                .add_spell_to_graveyard(P0, "Linked child", true)
                .with_mana_cost(ManaCost::zero())
                .from_oracle_text(GAIN_ONE)
                .id();
            let producer = scenario
                .add_spell_to_hand(P0, "Life gain producer", true)
                .with_mana_cost(ManaCost::zero())
                .from_oracle_text(GAIN_ONE)
                .id();
            let mut runner = scenario.build();
            assert_eq!(runner.state().objects[&source].trigger_definitions.len(), 1);
            runner.activate(source, 0).target_object(child).resolve();
            assert_eq!(runner.state().objects[&child].zone, Zone::Exile);
            let trigger = &runner.state().objects[&source].trigger_definitions[0].definition;
            let body = trigger.execute.as_deref().expect("parsed trigger body");
            if mana_trigger {
                assert_eq!(
                    trigger.mode,
                    engine::types::triggers::TriggerMode::TapsForMana
                );
                assert!(
                    matches!(&*body.effect, engine::types::ability::Effect::Unimplemented {name, ..}
                    if name == "linked_exile_mana_cast")
                );
                assert!(body.sub_ability.is_none());
                continue;
            }
            assert!(matches!(
                &*body.effect,
                engine::types::ability::Effect::Mana { .. }
            ));
            assert!(runner
                .state()
                .exile_links
                .iter()
                .any(|link| link.source_id == source && link.exiled_id == child));
            runner
                .act(GameAction::CastSpell {
                    object_id: producer,
                    card_id: runner.state().objects[&producer].card_id,
                    targets: vec![],
                    payment_mode: engine::types::game_state::CastPaymentMode::Auto,
                })
                .expect("cast the ordinary trigger's producer");
            settle(&mut runner);
            assert_eq!(pool_of(&runner), vec![child]);
            runner
                .act(GameAction::SelectCards {
                    cards: if accept { vec![child] } else { vec![] },
                })
                .expect("choose zero or one linked spell");
            for _ in 0..8 {
                if !matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
                    break;
                }
                runner.act(GameAction::PassPriority).expect("pay zero");
            }
            assert_eq!(
                runner.state().objects[&child].zone,
                if accept { Zone::Stack } else { Zone::Exile }
            );
            assert!(runner.state().objects[&child]
                .casting_permissions
                .is_empty());
            assert!(runner.state().pending_cast.is_none());
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
        }
    }
}
