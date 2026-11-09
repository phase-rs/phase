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
use engine::types::ability::{EffectKind, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{TargetSelectionSlot, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost};
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

/// Boxing Ring (verbatim, both lines). Its mana value is 2.
const BOXING_RING: &str = "Whenever a creature you control enters, it fights up to one target creature you don't control with the same mana value.\n{T}: Create a Treasure token. Activate only if you control a creature that fought this turn.";

/// A Boxing Ring board. `board.card` is the entering 3/3 in your hand (also
/// `board.fighter`), and `board.opponent` is the opponent's mana-value-3 2/2.
struct BoxingRingBoard {
    board: Board,
    opponent_mv2: ObjectId,
    opponent_mv4: ObjectId,
}

/// Boxing Ring (mana value 2) on your battlefield; the opponent controls 2/2s
/// with mana values 2, 3 and 4; a 3/3 with mana value `entering_mana_value` is
/// in your hand with enough Forests to cast it.
fn boxing_ring_board(entering_mana_value: u32) -> BoxingRingBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_artifact_from_oracle(P0, "Boxing Ring", BOXING_RING)
        .with_mana_cost(ManaCost::generic(2));
    let opponent_mv2 = scenario
        .add_creature(P1, "Opponent Two", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let opponent_mv3 = scenario
        .add_creature(P1, "Opponent Three", 2, 2)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let opponent_mv4 = scenario
        .add_creature(P1, "Opponent Four", 2, 2)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    for _ in 0..entering_mana_value {
        scenario.add_basic_land(P0, ManaColor::Green);
    }
    let entering = scenario
        .add_creature_to_hand(P0, "Entering Brawler", 3, 3)
        .with_mana_cost(ManaCost::generic(entering_mana_value))
        .id();
    BoxingRingBoard {
        board: Board {
            runner: scenario.build(),
            card: entering,
            fighter: Some(entering),
            opponent: opponent_mv3,
        },
        opponent_mv2,
        opponent_mv4,
    }
}

/// CR 202.3 + CR 608.2k: "with the same mana value" names no comparand, so it
/// compares with the object the trigger condition named — the entering
/// creature "it". Only the opponent creature whose mana value equals the
/// entering creature's (3) may be targeted; the one matching Boxing Ring's own
/// mana value (2) and the mana-value-4 one may not. CR 701.14a: the chosen
/// pair then fights.
#[test]
fn boxing_ring_offers_only_the_same_mana_value_creature() {
    let mut ring = boxing_ring_board(3);
    let entering = ring.board.card;
    let opponent_mv3 = ring.board.opponent;
    let slots = cast_to_trigger_targets(&mut ring.board).expect("trigger asks for a target");
    assert_eq!(
        slots.len(),
        1,
        "\"it\" is the entering creature, not a slot"
    );
    assert_eq!(
        slots[0].legal_targets,
        vec![TargetRef::Object(opponent_mv3)],
        "only the opponent creature with the entering creature's mana value is legal"
    );

    choose(&mut ring.board, Some(opponent_mv3));
    resolve_stack(&mut ring.board);

    let state = ring.board.runner.state();
    assert_eq!(state.objects[&opponent_mv3].zone, Zone::Graveyard);
    assert_eq!(
        state.objects[&entering].damage_marked, 2,
        "2/2 deals 2 back"
    );
    for untouched in [ring.opponent_mv2, ring.opponent_mv4] {
        assert_eq!(state.objects[&untouched].zone, Zone::Battlefield);
        assert_eq!(state.objects[&untouched].damage_marked, 0);
    }
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
}

/// Cast the Boxing Ring board's entering creature and drive toward its enters
/// trigger, collecting every event. Returns the trigger's target slots if a
/// prompt surfaces, `None` once the stack empties without one.
fn cast_boxing_ring_entrant(
    board: &mut Board,
) -> (Option<Vec<TargetSelectionSlot>>, Vec<GameEvent>) {
    let mut events = board.runner.cast(board.card).commit().events().to_vec();
    for _ in 0..80 {
        match board.runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { target_slots, .. } => {
                return (Some(target_slots), events)
            }
            WaitingFor::Priority { .. } if board.runner.state().stack.is_empty() => {
                return (None, events)
            }
            WaitingFor::Priority { .. } => {
                let result = board
                    .runner
                    .act(GameAction::PassPriority)
                    .expect("pass toward the enters trigger");
                events.extend(result.events);
            }
            other => panic!("unexpected wait while casting the entering creature: {other:?}"),
        }
    }
    panic!("the stack never emptied");
}

/// CR 115.6 + CR 701.14a: with no opponent creature of the entering creature's
/// mana value (5), the "up to one" target has no legal choice, so the trigger
/// is not prompted; it resolves with zero targets and nobody fights.
#[test]
fn boxing_ring_without_a_same_mana_value_creature_fights_nothing() {
    let mut ring = boxing_ring_board(5);
    let entering = ring.board.card;
    let (slots, events) = cast_boxing_ring_entrant(&mut ring.board);
    assert!(
        slots.is_none(),
        "no opponent creature has mana value 5, so there is nothing to choose: {slots:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::Fight,
                    ..
                }
            ))
            .count(),
        1,
        "the trigger resolves once with zero targets: {events:?}"
    );

    let state = ring.board.runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(state.objects[&entering].zone, Zone::Battlefield);
    assert_eq!(state.objects[&entering].damage_marked, 0);
    for untouched in [ring.opponent_mv2, ring.board.opponent, ring.opponent_mv4] {
        assert_eq!(state.objects[&untouched].zone, Zone::Battlefield);
        assert_eq!(state.objects[&untouched].damage_marked, 0);
    }
}
