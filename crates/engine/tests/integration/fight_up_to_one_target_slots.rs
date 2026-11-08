//! CR 115.6 + CR 601.2c + CR 701.14a: "up to one" on a fight makes only the
//! creature being fought optional. The fighter is never optional.
//!
//! - Two declared slots, "target creature you control fights up to one target
//!   creature you don't control" (Favored Enemy, Arni Slays the Troll I, Amok
//!   Time I): the fighter slot stays required.
//! - One declared slot, "it fights up to one target creature" after a targeted
//!   pump (Bridgeworks Battle, Primal Might): the fight may be declined.
//!
//! Review finding on https://github.com/phase-rs/phase/pull/8820.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{TargetSelectionSlot, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Favored Enemy's enters trigger (its second line, verbatim). The first line's
/// "note the most prevalent creature type" is not needed here.
const FAVORED_ENEMY_ENTERS: &str = "When this enchantment enters, target creature you control fights up to one target creature you don't control.";

/// Bridgeworks Battle, front face (verbatim, reminder text dropped).
const BRIDGEWORKS_BATTLE: &str = "Target creature you control gets +2/+2 until end of turn. It fights up to one target creature you don't control.";

struct Board {
    runner: GameRunner,
    card: ObjectId,
    fighter: Option<ObjectId>,
    opponent: ObjectId,
}

/// A 3/3 for you (unless `with_fighter` is false), a 2/2 for the opponent, and
/// the card under test in your hand.
fn board(with_fighter: bool, name: &str, oracle: &str, enchantment: bool) -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let fighter = with_fighter.then(|| scenario.add_creature(P0, "Champion", 3, 3).id());
    let opponent = scenario.add_creature(P1, "Opponent Bear", 2, 2).id();
    let card = {
        let mut builder = scenario.add_spell_to_hand_from_oracle(P0, name, false, oracle);
        if enchantment {
            builder.as_enchantment();
        }
        builder.id()
    };
    Board {
        runner: scenario.build(),
        card,
        fighter,
        opponent,
    }
}

fn favored_enemy(with_fighter: bool) -> Board {
    board(with_fighter, "Favored Enemy", FAVORED_ENEMY_ENTERS, true)
}

/// Cast Favored Enemy and stop at its enters trigger's target prompt, returning
/// the slots. `None` when the stack empties without one surfacing.
fn cast_to_trigger_targets(board: &mut Board) -> Option<Vec<TargetSelectionSlot>> {
    let _commit = board.runner.cast(board.card).commit();
    for _ in 0..80 {
        match board.runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { target_slots, .. } => return Some(target_slots),
            WaitingFor::Priority { .. } if board.runner.state().stack.is_empty() => return None,
            WaitingFor::Priority { .. } => {
                board
                    .runner
                    .act(GameAction::PassPriority)
                    .expect("pass toward the enters trigger");
            }
            other => panic!("unexpected wait while casting Favored Enemy: {other:?}"),
        }
    }
    panic!("never reached the enters trigger's target prompt");
}

fn choose(board: &mut Board, target: Option<ObjectId>) {
    board
        .runner
        .act(GameAction::ChooseTarget {
            target: target.map(TargetRef::Object),
        })
        .expect("target choice accepted");
}

fn resolve_stack(board: &mut Board) {
    for _ in 0..80 {
        if board.runner.state().stack.is_empty() {
            return;
        }
        board
            .runner
            .act(GameAction::PassPriority)
            .expect("pass to resolve the stack");
    }
    panic!("stack did not resolve");
}

/// CR 601.2c: one slot per "target", announced in order. The fighter slot is
/// required; only the "up to one" slot may be declined.
#[test]
fn two_slot_fighter_required_fought_optional() {
    let mut board = favored_enemy(true);
    let slots = cast_to_trigger_targets(&mut board).expect("trigger asks for targets");
    assert_eq!(slots.len(), 2, "fighter + fought creature: {slots:?}");
    assert!(
        !slots[0].optional,
        "CR 115.6: 'up to one' must not make the fighter optional"
    );
    assert!(slots[1].optional, "the 'up to one' creature is optional");
    let fighter = board.fighter.unwrap();
    assert!(slots[0].legal_targets.contains(&TargetRef::Object(fighter)));
}

/// CR 701.14a: the normal selected pair fights.
#[test]
fn two_slot_selected_pair_fights() {
    let mut board = favored_enemy(true);
    let fighter = board.fighter.unwrap();
    let opponent = board.opponent;
    cast_to_trigger_targets(&mut board).expect("trigger asks for targets");
    choose(&mut board, Some(fighter));
    choose(&mut board, Some(opponent));
    resolve_stack(&mut board);

    let state = board.runner.state();
    assert_eq!(
        state.objects[&opponent].zone,
        Zone::Graveyard,
        "3/3 deals 3 to the 2/2"
    );
    assert_eq!(state.objects[&fighter].damage_marked, 2, "2/2 deals 2 back");
}

/// CR 115.6 + CR 701.14b: a chosen fighter with the opponent declined resolves
/// as no fight — the lone fighter is not reread as "~ fights".
#[test]
fn two_slot_declined_opponent_is_no_fight() {
    let mut board = favored_enemy(true);
    let fighter = board.fighter.unwrap();
    cast_to_trigger_targets(&mut board).expect("trigger asks for targets");
    choose(&mut board, Some(fighter));
    choose(&mut board, None);
    resolve_stack(&mut board);

    let state = board.runner.state();
    assert_eq!(state.objects[&fighter].damage_marked, 0);
    assert_eq!(state.objects[&board.opponent].damage_marked, 0);
    assert_eq!(state.objects[&board.opponent].zone, Zone::Battlefield);
}

/// CR 115.6: declining the required fighter slot is refused.
#[test]
fn two_slot_declining_the_fighter_is_rejected() {
    let mut board = favored_enemy(true);
    cast_to_trigger_targets(&mut board).expect("trigger asks for targets");
    let declined = board.runner.act(GameAction::ChooseTarget { target: None });
    assert!(
        declined.is_err(),
        "the fighter slot is required, declining it must fail: {declined:?}"
    );
}

/// CR 603.3d: with no creature to fight with, the required fighter slot has no
/// legal target, so the trigger is removed from the stack and nothing fights.
#[test]
fn two_slot_no_legal_fighter_removes_the_trigger() {
    let mut board = favored_enemy(false);
    let slots = cast_to_trigger_targets(&mut board);
    assert!(
        slots.is_none(),
        "no creature you control: the trigger must not prompt, got {slots:?}"
    );
    let state = board.runner.state();
    assert_eq!(state.objects[&board.card].zone, Zone::Battlefield);
    assert_eq!(state.objects[&board.opponent].damage_marked, 0);
}

/// CR 115.6: the single-slot form may decline the fight; the pump still
/// applies.
#[test]
fn one_slot_declined_fight_still_pumps() {
    let mut board = board(true, "Bridgeworks Battle", BRIDGEWORKS_BATTLE, false);
    let fighter = board.fighter.unwrap();
    let outcome = board
        .runner
        .cast(board.card)
        .target_objects(&[fighter])
        .resolve();

    let state = outcome.state();
    assert_eq!(state.objects[&fighter].power, Some(5), "+2/+2 applied");
    assert_eq!(state.objects[&fighter].damage_marked, 0);
    assert_eq!(state.objects[&board.opponent].damage_marked, 0);
    assert_eq!(state.objects[&board.opponent].zone, Zone::Battlefield);
}

/// CR 701.14a: the single-slot form with a chosen opponent fights with the
/// pumped creature.
#[test]
fn one_slot_selected_fight() {
    let mut board = board(true, "Bridgeworks Battle", BRIDGEWORKS_BATTLE, false);
    let fighter = board.fighter.unwrap();
    let opponent = board.opponent;
    let outcome = board
        .runner
        .cast(board.card)
        .target_objects(&[fighter, opponent])
        .resolve();

    let state = outcome.state();
    assert_eq!(
        state.objects[&opponent].zone,
        Zone::Graveyard,
        "5/5 deals 5"
    );
    assert_eq!(state.objects[&fighter].damage_marked, 2, "2/2 deals 2 back");
}
