//! Cross-target constraints survive an X announcement that sets the target count.
//!
//! CR 601.2b + CR 601.2c: when an announced X determines how many targets a
//! spell or ability has, X is announced first and the targets are chosen
//! after it, and the chosen set must satisfy every cross-target requirement
//! ("from a single graveyard"). CR 602.2b applies the same steps to an
//! activated ability. CR 107.3a: the announced X is the value every X takes.
//!
//! One row per route that defers target selection behind an X announcement:
//! a non-mana announcing activation cost (Discard X), a mana `{X}` activation
//! cost, and a spell's X-bearing required additional cost. The texts are
//! synthetic: no printed card combines one of these routes with a cross-target
//! constraint today. Martyr of Bones joins route (a) once its reveal-X cost
//! announces X.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const DISCARD_X_UP_TO: &str =
    "Discard X cards: Exile up to X target cards from a single graveyard.";
const DISCARD_X_EXACT: &str = "Discard X cards: Exile X target cards from a single graveyard.";
const MANA_X_UP_TO: &str = "{X}, {T}: Exile up to X target cards from a single graveyard.";
const MANA_X_EXACT: &str = "{X}, {T}: Exile X target cards from a single graveyard.";
const REQUIRED_X_UP_TO: &str = "As an additional cost to cast this spell, discard X cards.\nExile up to X target cards from a single graveyard.";
const REQUIRED_X_EXACT: &str = "As an additional cost to cast this spell, discard X cards.\nExile X target cards from a single graveyard.";
/// Rats' Feast ({X}{B}, sorcery): the spell mana-`{X}` reference route.
const RATS_FEAST: &str = "Exile X target cards from a single graveyard.";

fn colorless(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

fn obj(id: ObjectId) -> TargetRef {
    TargetRef::Object(id)
}

/// The route under test, with the source it starts from.
#[derive(Clone, Copy)]
enum Route {
    /// (a) a non-mana announcing activation cost: an artifact on P0's battlefield.
    DiscardXActivation,
    /// (b) a mana `{X}` activation cost: an artifact on P0's battlefield.
    ManaXActivation,
    /// (c) a spell's X-bearing required additional cost: a {0} sorcery in P0's hand.
    RequiredXSpell,
    /// Reference: a spell whose mana cost has `{X}` (Rats' Feast, {X}{B}).
    ManaXSpell,
}

struct Board {
    runner: GameRunner,
    source: ObjectId,
    fodder: Vec<ObjectId>,
    own_grave: ObjectId,
    other_graves: Vec<ObjectId>,
}

/// P0 has two cards in hand (the X discards), one card in its own graveyard;
/// P1 has `other_grave_count` cards in its graveyard.
fn board(route: Route, text: &str, other_grave_count: usize) -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = match route {
        Route::DiscardXActivation => scenario
            .add_artifact_from_oracle(P0, "Ossuary Lens", text)
            .id(),
        Route::ManaXActivation => {
            scenario.with_mana_pool(P0, colorless(3));
            scenario
                .add_artifact_from_oracle(P0, "Ossuary Lens", text)
                .id()
        }
        Route::RequiredXSpell => scenario
            .add_spell_to_hand_from_oracle(P0, "Grave Tithe", false, text)
            .with_mana_cost(ManaCost::zero())
            .id(),
        Route::ManaXSpell => {
            let mut pool = colorless(3);
            pool.push(ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]));
            scenario.with_mana_pool(P0, pool);
            scenario
                .add_spell_to_hand_from_oracle(P0, "Rats' Feast", false, text)
                .with_mana_cost(ManaCost::Cost {
                    shards: vec![ManaCostShard::X, ManaCostShard::Black],
                    generic: 0,
                })
                .id()
        }
    };
    let fodder = match route {
        Route::ManaXActivation | Route::ManaXSpell => Vec::new(),
        Route::DiscardXActivation | Route::RequiredXSpell => vec![
            scenario.add_card_to_hand(P0, "Fodder One"),
            scenario.add_card_to_hand(P0, "Fodder Two"),
        ],
    };
    let own_grave = scenario.add_spell_to_graveyard(P0, "Own Grave", true).id();
    let other_graves = (0..other_grave_count)
        .map(|i| {
            scenario
                .add_spell_to_graveyard(P1, &format!("Other Grave {i}"), true)
                .id()
        })
        .collect();
    Board {
        runner: scenario.build(),
        source,
        fodder,
        own_grave,
        other_graves,
    }
}

/// Starts the activation or cast and returns the X announcement's bounds.
fn begin(board: &mut Board, route: Route) -> (u32, u32) {
    let action = match route {
        Route::DiscardXActivation | Route::ManaXActivation => GameAction::ActivateAbility {
            source_id: board.source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        },
        Route::RequiredXSpell | Route::ManaXSpell => GameAction::CastSpell {
            object_id: board.source,
            card_id: board.runner.state().objects[&board.source].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        },
    };
    board
        .runner
        .act(action)
        .expect("begin the activation or cast");
    let WaitingFor::ChooseXValue {
        player, min, max, ..
    } = board.runner.state().waiting_for.clone()
    else {
        panic!(
            "X is announced before targets (CR 601.2b), got {:?}",
            board.runner.state().waiting_for
        );
    };
    assert_eq!(player, P0);
    (min, max)
}

fn target_slot_count(runner: &GameRunner) -> usize {
    let WaitingFor::TargetSelection { target_slots, .. } = &runner.state().waiting_for else {
        panic!(
            "targets are chosen after X (CR 601.2c), got {:?}",
            runner.state().waiting_for
        );
    };
    target_slots.len()
}

/// Pays whatever the route still owes once targets are chosen (mana from the
/// pool; the X discards), then resolves.
fn pay_and_resolve(board: &mut Board) {
    for _ in 0..4 {
        match board.runner.state().waiting_for.clone() {
            WaitingFor::ManaPayment { .. } => {
                board
                    .runner
                    .act(GameAction::PassPriority)
                    .expect("pay the mana from the pool");
            }
            WaitingFor::PayCost {
                kind: PayCostKind::Discard,
                count,
                ..
            } => {
                assert_eq!(count, board.fodder.len(), "the cost discards X cards");
                board
                    .runner
                    .act(GameAction::SelectCards {
                        cards: board.fodder.clone(),
                    })
                    .expect("discard the X cards");
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected prompt while paying: {other:?}"),
        }
    }
    assert_eq!(
        board.runner.state().stack.len(),
        1,
        "the ability or spell is on the stack"
    );
    board.runner.advance_until_stack_empty();
    assert!(board.runner.state().stack.is_empty());
    assert!(matches!(
        board.runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// "Up to X target cards from a single graveyard", X = 2: two cards from one
/// graveyard are accepted as a whole declaration, a card from a second
/// graveyard is refused both as a whole declaration and slot by slot, and two
/// cards from one graveyard chosen slot by slot are accepted and exiled.
fn targets_come_from_a_single_graveyard(route: Route, text: &str) {
    // Positive reach guard on a fresh board: a whole declaration of two cards
    // from the same graveyard is accepted, so the mixed-graveyard rejection
    // below is the constraint's, not `SelectTargets` refused in this state.
    let mut accepted = board(route, text, 2);
    begin(&mut accepted, route);
    accepted
        .runner
        .act(GameAction::ChooseX { value: 2 })
        .expect("announce X = 2");
    assert_eq!(target_slot_count(&accepted.runner), 2);
    let (first, second) = (accepted.other_graves[0], accepted.other_graves[1]);
    accepted
        .runner
        .act(GameAction::SelectTargets {
            targets: vec![obj(first), obj(second)],
        })
        .expect("a declaration from a single graveyard is legal");
    assert!(!matches!(
        accepted.runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ));
    pay_and_resolve(&mut accepted);
    let zone = |id: ObjectId| accepted.runner.state().objects[&id].zone;
    assert_eq!(zone(first), Zone::Exile);
    assert_eq!(zone(second), Zone::Exile);
    assert_eq!(zone(accepted.own_grave), Zone::Graveyard);

    let mut board = board(route, text, 2);
    let (min, max) = begin(&mut board, route);
    assert_eq!(min, 0);
    assert!(max >= 2, "X = 2 is offered (max {max})");
    board
        .runner
        .act(GameAction::ChooseX { value: 2 })
        .expect("announce X = 2");
    // CR 601.2c: X = 2 announces two optional ("up to") target slots.
    assert_eq!(target_slot_count(&board.runner), 2);

    let (first, second) = (board.other_graves[0], board.other_graves[1]);
    // CR 601.2c: the whole declaration is checked against the constraint.
    assert!(
        board
            .runner
            .act(GameAction::SelectTargets {
                targets: vec![obj(first), obj(board.own_grave)],
            })
            .is_err(),
        "a declaration spanning two graveyards is illegal"
    );
    assert_eq!(target_slot_count(&board.runner), 2);

    board
        .runner
        .act(GameAction::ChooseTarget {
            target: Some(obj(first)),
        })
        .expect("the first card is legal");
    // Reach guard: the walk is still on the second slot.
    assert_eq!(target_slot_count(&board.runner), 2);
    assert!(
        board
            .runner
            .act(GameAction::ChooseTarget {
                target: Some(obj(board.own_grave)),
            })
            .is_err(),
        "a second card from another graveyard is illegal"
    );
    assert_eq!(target_slot_count(&board.runner), 2);
    board
        .runner
        .act(GameAction::ChooseTarget {
            target: Some(obj(second)),
        })
        .expect("a second card from the same graveyard is legal");
    assert!(!matches!(
        board.runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ));

    pay_and_resolve(&mut board);
    let zone = |id: ObjectId| board.runner.state().objects[&id].zone;
    assert_eq!(zone(first), Zone::Exile);
    assert_eq!(zone(second), Zone::Exile);
    assert_eq!(zone(board.own_grave), Zone::Graveyard);
}

/// "X target cards from a single graveyard" with one card in each of two
/// graveyards: X = 2 has no legal target assignment and is refused before it
/// is locked in; X = 1 is accepted.
fn x_without_a_single_graveyard_assignment_is_refused(route: Route, text: &str) {
    let mut board = board(route, text, 1);
    let (_, max) = begin(&mut board, route);
    assert!(max >= 2, "X = 2 is offered (max {max})");
    assert!(
        board.runner.act(GameAction::ChooseX { value: 2 }).is_err(),
        "no single graveyard holds two cards"
    );
    // Reach guard: still announcing X.
    assert!(matches!(
        board.runner.state().waiting_for,
        WaitingFor::ChooseXValue { .. }
    ));
    board
        .runner
        .act(GameAction::ChooseX { value: 1 })
        .expect("X = 1 has a legal target");
    assert_eq!(target_slot_count(&board.runner), 1);
    let other = board.other_graves[0];
    board
        .runner
        .act(GameAction::ChooseTarget {
            target: Some(obj(other)),
        })
        .expect("choose the one target");
    board.fodder.truncate(1);
    pay_and_resolve(&mut board);
    assert_eq!(board.runner.state().objects[&other].zone, Zone::Exile);
    assert_eq!(
        board.runner.state().objects[&board.own_grave].zone,
        Zone::Graveyard
    );
}

#[test]
fn discard_x_activation_targets_come_from_a_single_graveyard() {
    targets_come_from_a_single_graveyard(Route::DiscardXActivation, DISCARD_X_UP_TO);
}

#[test]
fn discard_x_activation_refuses_an_x_no_single_graveyard_can_supply() {
    x_without_a_single_graveyard_assignment_is_refused(Route::DiscardXActivation, DISCARD_X_EXACT);
}

#[test]
fn mana_x_activation_targets_come_from_a_single_graveyard() {
    targets_come_from_a_single_graveyard(Route::ManaXActivation, MANA_X_UP_TO);
}

#[test]
fn mana_x_activation_refuses_an_x_no_single_graveyard_can_supply() {
    x_without_a_single_graveyard_assignment_is_refused(Route::ManaXActivation, MANA_X_EXACT);
}

#[test]
fn required_x_cost_spell_targets_come_from_a_single_graveyard() {
    targets_come_from_a_single_graveyard(Route::RequiredXSpell, REQUIRED_X_UP_TO);
}

#[test]
fn required_x_cost_spell_refuses_an_x_no_single_graveyard_can_supply() {
    x_without_a_single_graveyard_assignment_is_refused(Route::RequiredXSpell, REQUIRED_X_EXACT);
}

/// Reference reading (positive control): the spell mana-`{X}` route carries
/// the constraint at base.
#[test]
fn rats_feast_refuses_an_x_no_single_graveyard_can_supply() {
    x_without_a_single_graveyard_assignment_is_refused(Route::ManaXSpell, RATS_FEAST);
}
