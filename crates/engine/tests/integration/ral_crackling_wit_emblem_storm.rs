//! Ral, Crackling Wit's −10 emblem: "You get an emblem with \"Instant and sorcery spells you cast
//! have storm.\"" CR 114.4: abilities of emblems function in the command zone, so the emblem's
//! storm grant declares the command zone as its only zone of function. CR 702.40a: storm copies
//! the spell for each other spell cast before it this turn, including spells cast before the
//! emblem existed. CR 707.10: a copy of a spell isn't cast, so Ral's "Whenever you cast a
//! noncreature spell" trigger sees the cast spell and not its copies.

use crate::support::shared_card_db;
use engine::game::printed_cards::rehydrate_game_from_card_db;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{AbilityCost, Effect, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

/// Verbatim MTGJSON Oracle text.
const RAL: &str = "Whenever you cast a noncreature spell, put a loyalty counter on Ral.\n[+1]: Create a 1/1 blue and red Otter creature token with prowess.\n[\u{2212}3]: Draw three cards, then discard two cards.\n[\u{2212}10]: Draw three cards. You get an emblem with \"Instant and sorcery spells you cast have storm.\" (Whenever you cast an instant or sorcery spell, copy it for each spell cast before it this turn.)";

struct Board {
    runner: GameRunner,
    ral: ObjectId,
    bolts: [ObjectId; 2],
    bears: [ObjectId; 2],
}

/// Ral at 10 loyalty, two Lightning Bolts and two Grizzly Bears in P0's hand, enough mana for
/// all four spells whatever color pays a generic cost, and a library deep enough for the −10's
/// three draws.
fn setup() -> Board {
    let db = shared_card_db().expect("the checked-in fixture contains Lightning Bolt");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ral = scenario
        .add_planeswalker_from_oracle(P0, "Ral, Crackling Wit", "Ral", 10, RAL)
        .id();
    for _ in 0..8 {
        scenario.add_card_to_library_top(P0, "Library Card");
        scenario.add_card_to_library_top(P1, "Library Card");
    }
    let bolts = [
        scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db),
        scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db),
    ];
    let bears = [
        scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db),
        scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db),
    ];
    let pool: Vec<ManaUnit> = [ManaType::Red; 4]
        .into_iter()
        .chain([ManaType::Green; 4])
        .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect();
    scenario.with_mana_pool(P0, pool);
    let mut runner = scenario.build();
    rehydrate_game_from_card_db(runner.state_mut(), db);
    Board {
        runner,
        ral,
        bolts,
        bears,
    }
}

fn cast(runner: &mut GameRunner, spell: ObjectId) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the spell can be cast");
}

/// CR 601.2c: Lightning Bolt's "any target" is P1.
fn target_p1(runner: &mut GameRunner) {
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "Lightning Bolt asks for its target, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .expect("P1 is a legal target");
}

/// Resolve the whole stack, keeping every storm copy's target (CR 707.10c), and return the
/// number of copies made. Any other prompt fails the test.
fn resolve_stack(runner: &mut GameRunner) -> usize {
    let mut copies = 0;
    for _ in 0..100 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return copies,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority can be passed");
            }
            WaitingFor::CopyRetarget { .. } => {
                copies += 1;
                runner
                    .act(GameAction::KeepAllCopyTargets)
                    .expect("the copy keeps its target");
            }
            other => panic!("unexpected prompt while resolving the stack: {other:?}"),
        }
    }
    panic!("the stack never emptied");
}

fn loyalty(runner: &GameRunner, ral: ObjectId) -> Option<u32> {
    runner.state().objects[&ral].loyalty
}

fn p1_life(runner: &GameRunner) -> i32 {
    runner.state().players[1].life
}

/// CR 603.3b: casting an instant while P0 has the emblem triggers Ral's cast trigger and the
/// spell's storm together; P0 orders them.
fn order_ral_and_storm_triggers(runner: &mut GameRunner, ral: ObjectId) {
    let WaitingFor::OrderTriggers { player, triggers } = &runner.state().waiting_for else {
        panic!(
            "Ral's trigger and storm must be ordered, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(triggers.len(), 2, "Ral's trigger and storm: {triggers:?}");
    assert_eq!(
        triggers
            .iter()
            .filter(|trigger| trigger.source_id == ral)
            .count(),
        1,
        "exactly one of the two triggers is Ral's: {triggers:?}"
    );
    runner
        .act(GameAction::OrderTriggers { order: vec![0, 1] })
        .expect("the triggers can be ordered");
}

/// Grizzly Bears, then Lightning Bolt (Ral 10 → 11), then Ral's −10 (Ral → 1, emblem created).
/// Both spells are cast before the emblem exists.
fn cast_two_spells_then_ultimate(board: &mut Board) {
    let Board {
        runner,
        ral,
        bolts,
        bears,
    } = board;
    let ral = *ral;

    cast(runner, bears[0]);
    assert_eq!(resolve_stack(runner), 0);
    assert_eq!(
        loyalty(runner, ral),
        Some(10),
        "a creature spell is not noncreature"
    );

    cast(runner, bolts[0]);
    target_p1(runner);
    assert_eq!(
        resolve_stack(runner),
        0,
        "no storm before the emblem exists"
    );
    assert_eq!(loyalty(runner, ral), Some(11));
    assert_eq!(p1_life(runner), 17);

    // The parsed −10 (third loyalty ability): draw three, then the emblem.
    let minus_ten = &runner.state().objects[&ral].abilities[2];
    assert_eq!(minus_ten.cost, Some(AbilityCost::Loyalty { amount: -10 }));
    let emblem_effect = minus_ten
        .sub_ability
        .as_ref()
        .map(|sub| sub.effect.as_ref());
    let Some(Effect::CreateEmblem { statics, triggers }) = emblem_effect else {
        panic!("Ral's −10 creates an emblem after drawing, got {emblem_effect:?}");
    };
    assert!(triggers.is_empty());
    assert_eq!(statics.len(), 1);
    // CR 114.4: the parsed emblem static functions from the command zone only.
    assert_eq!(statics[0].active_zones, vec![Zone::Command]);

    let hand_before = runner.state().players[0].hand.len();
    runner.activate(ral, 2).resolve();
    assert_eq!(loyalty(runner, ral), Some(1));
    assert_eq!(runner.state().players[0].hand.len(), hand_before + 3);
}

/// The emblem Ral's −10 created: exactly one, in the command zone, controlled by P0.
fn ral_emblem(runner: &GameRunner) -> ObjectId {
    let emblems: Vec<ObjectId> = runner
        .state()
        .command_zone
        .iter()
        .copied()
        .filter(|id| runner.state().objects[id].is_emblem)
        .collect();
    assert_eq!(emblems.len(), 1, "the −10 created one emblem");
    emblems[0]
}

#[test]
fn ral_emblem_grants_storm_counting_spells_cast_before_it() {
    let mut board = setup();
    cast_two_spells_then_ultimate(&mut board);
    let Board {
        mut runner,
        ral,
        bolts,
        ..
    } = board;

    let emblem_id = ral_emblem(&runner);
    let emblem = &runner.state().objects[&emblem_id];
    // CR 114.2: the emblem is owned and controlled by the player who gets it.
    assert_eq!(emblem.controller, P0);
    assert_eq!(emblem.static_definitions.len(), 1);
    let storm_grant = &emblem.static_definitions[0];
    assert_eq!(
        storm_grant.mode,
        StaticMode::CastWithKeyword {
            keyword: Keyword::Storm
        }
    );
    // CR 114.4: the emblem's static functions from the command zone and nowhere else.
    assert_eq!(storm_grant.active_zones, vec![Zone::Command]);

    let life_before = p1_life(&runner);
    cast(&mut runner, bolts[1]);
    target_p1(&mut runner);
    order_ral_and_storm_triggers(&mut runner, ral);
    // CR 702.40a: Grizzly Bears and the first Bolt were cast before this Bolt this turn.
    assert_eq!(resolve_stack(&mut runner), 2);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    // The Bolt and its two copies each deal 3.
    assert_eq!(life_before - p1_life(&runner), 9);
    // CR 707.10: the copies aren't cast, so Ral's trigger fired once (1 → 2).
    assert_eq!(loyalty(&runner, ral), Some(2));
}

#[test]
fn ral_emblem_storm_ignores_creature_spells() {
    let mut board = setup();
    cast_two_spells_then_ultimate(&mut board);
    let Board {
        mut runner,
        ral,
        bolts,
        bears,
    } = board;
    ral_emblem(&runner);

    // A creature spell gets no storm from the emblem (instant and sorcery spells only), and is
    // not a noncreature spell for Ral's trigger.
    cast(&mut runner, bears[1]);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "no trigger to order or target, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "only Grizzly Bears is on the stack: no storm trigger"
    );
    assert_eq!(resolve_stack(&mut runner), 0);
    assert_eq!(loyalty(&runner, ral), Some(1));

    // Paired positive: the next Bolt has storm and counts all three spells cast before it,
    // including the second Grizzly Bears (CR 702.40a).
    let life_before = p1_life(&runner);
    cast(&mut runner, bolts[1]);
    target_p1(&mut runner);
    order_ral_and_storm_triggers(&mut runner, ral);
    assert_eq!(resolve_stack(&mut runner), 3);
    assert!(runner.state().stack.is_empty());
    assert_eq!(life_before - p1_life(&runner), 12);
    assert_eq!(loyalty(&runner, ral), Some(2));
}
