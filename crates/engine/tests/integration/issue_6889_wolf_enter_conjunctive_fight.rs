//! Regression for issue #6889: Tolsimir, Friend to Wolves' Wolf-enter trigger
//! must split "you gain 3 life and that creature fights up to one target …"
//! and have the *entering Wolf* fight (not Tolsimir).
//!
//! https://github.com/phase-rs/phase/issues/6889

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{EffectKind, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Verbatim both sentences (Scryfall / MTGJSON).
const TOLSIMIR_ORACLE: &str = "When Tolsimir enters, create Voja, Friend to Elves, a legendary 3/3 green and white Wolf creature token.\nWhenever a Wolf you control enters, you gain 3 life and that creature fights up to one target creature you don't control.";

const BOUNCE_ORACLE: &str = "Return target creature to its owner's hand.";

struct Board {
    runner: GameRunner,
    tolsimir: ObjectId,
    wolf: ObjectId,
    opponent: ObjectId,
}

fn board_with_bounce(include_bounce: bool) -> (Board, Option<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tolsimir = scenario
        .add_creature_from_oracle(P0, "Tolsimir, Friend to Wolves", 3, 3, TOLSIMIR_ORACLE)
        .id();
    let opponent = scenario.add_creature(P1, "Opponent Beast", 4, 4).id();
    let wolf = scenario
        .add_creature_to_hand(P0, "Lone Wolf", 1, 1)
        .with_subtypes(vec!["Wolf"])
        .id();
    let bounce = if include_bounce {
        Some(
            scenario
                .add_spell_to_hand_from_oracle(P0, "Unsummon", true, BOUNCE_ORACLE)
                .id(),
        )
    } else {
        None
    };
    (
        Board {
            runner: scenario.build(),
            tolsimir,
            wolf,
            opponent,
        },
        bounce,
    )
}

/// Drive the Wolf onto the stack, resolve it, and stop at the fight
/// `TriggerTargetSelection` (do not use `SpellCast::resolve()` — it would
/// auto-answer the wait and keep resolving).
fn drive_wolf_to_fight_target_prompt(board: &mut Board) {
    let _commit = board.runner.cast(board.wolf).commit();
    for _ in 0..80 {
        match board.runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                return;
            }
            WaitingFor::OrderTriggers { .. } => {
                board
                    .runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .expect("order the single Wolf-enter trigger");
            }
            WaitingFor::Priority { .. } => {
                if board.runner.state().stack.is_empty() {
                    panic!(
                        "stack emptied before TriggerTargetSelection; fight slot never surfaced"
                    );
                }
                board
                    .runner
                    .act(GameAction::PassPriority)
                    .expect("pass toward the Wolf-enter trigger");
            }
            other => panic!("unexpected wait while driving the Wolf cast: {other:?}"),
        }
    }
    panic!("did not reach TriggerTargetSelection after casting the Wolf");
}

/// CR 119.3 + CR 701.14a: choosing the 4/4 makes the 1/1 Wolf fight it.
/// Revert of the Fight node → +3 only, 4/4 undamaged. Revert of TriggeringSource
/// → Tolsimir (3/3) dies, Wolf lives, 4/4 marked 3.
#[test]
fn wolf_enter_choose_target_wolf_fights_not_tolsimir() {
    let (mut board, _) = board_with_bounce(false);
    let outcome = board
        .runner
        .cast(board.wolf)
        .target_objects(&[board.opponent])
        .resolve();

    outcome.assert_life_delta(P0, 3);
    outcome.assert_zone(&[board.wolf], Zone::Graveyard);
    outcome.assert_zone(&[board.tolsimir], Zone::Battlefield);
    assert_eq!(
        outcome.state().objects[&board.opponent].damage_marked,
        1,
        "1/1 Wolf deals 1 to the 4/4"
    );
    assert_eq!(
        outcome.state().objects[&board.tolsimir].damage_marked,
        0,
        "Tolsimir must not be a fighter"
    );
}

/// CR 115.6: decline zero targets — still gain 3, no fight. Driver must see
/// `slot.optional` (else it panics on the undeclared required slot).
#[test]
fn wolf_enter_decline_zero_targets_gains_life_no_fight() {
    let (mut board, _) = board_with_bounce(false);
    let outcome = board.runner.cast(board.wolf).resolve();

    outcome.assert_life_delta(P0, 3);
    outcome.assert_zone(&[board.wolf], Zone::Battlefield);
    outcome.assert_zone(&[board.opponent], Zone::Battlefield);
    assert_eq!(outcome.state().objects[&board.wolf].damage_marked, 0);
    assert_eq!(outcome.state().objects[&board.opponent].damage_marked, 0);
    assert_eq!(outcome.state().objects[&board.tolsimir].damage_marked, 0);
    assert!(
        outcome.events().iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::Fight,
                subject: None,
                ..
            }
        )),
        "optional no-fight must emit EffectResolved Fight with subject None; \
         swallowed MissingParam does not. events: {:?}",
        outcome.events()
    );
}

/// CR 115.6 + CR 603.3d: no legal fight target still gains 3 (optional slot).
#[test]
fn wolf_enter_no_opponent_creatures_gains_life_no_fight() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Tolsimir, Friend to Wolves", 3, 3, TOLSIMIR_ORACLE)
        .id();
    let wolf = scenario
        .add_creature_to_hand(P0, "Lone Wolf", 1, 1)
        .with_subtypes(vec!["Wolf"])
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(wolf).resolve();

    outcome.assert_life_delta(P0, 3);
    outcome.assert_zone(&[wolf], Zone::Battlefield);
}

/// CR 608.2b: a chosen fight target that leaves before resolution fizzles the
/// whole ability — no life, no fight. Must not use `.resolve()` on the Wolf
/// (no hook after ChooseTarget).
#[test]
fn wolf_enter_chosen_target_illegal_fizzles_whole_ability() {
    let (mut board, bounce) = board_with_bounce(true);
    let bounce = bounce.expect("Unsummon in hand");
    let life_before = board.runner.state().players[0].life;

    drive_wolf_to_fight_target_prompt(&mut board);
    board
        .runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(board.opponent)),
        })
        .expect("choose the 4/4 as the fight target");
    assert!(
        !board.runner.state().stack.is_empty(),
        "reach-guard: trigger is on the stack after ChooseTarget"
    );

    board
        .runner
        .cast(bounce)
        .target_objects(&[board.opponent])
        .resolve();

    assert_eq!(
        board.runner.state().players[0].life,
        life_before,
        "CR 608.2b: illegal chosen target fizzles the whole ability, including GainLife"
    );
    assert_eq!(
        board.runner.state().objects[&board.wolf].zone,
        Zone::Battlefield
    );
    assert_eq!(board.runner.state().objects[&board.wolf].damage_marked, 0);
    assert_eq!(
        board.runner.state().objects[&board.opponent].zone,
        Zone::Hand,
        "reach-guard: the 4/4 left before the trigger resolved"
    );
}

/// CR 701.14b: entering Wolf gone, chosen target still legal → +3 life, no fight.
#[test]
fn wolf_enter_wolf_gone_gains_life_no_fight() {
    let (mut board, bounce) = board_with_bounce(true);
    let bounce = bounce.expect("Unsummon in hand");
    let life_before = board.runner.state().players[0].life;

    drive_wolf_to_fight_target_prompt(&mut board);
    board
        .runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(board.opponent)),
        })
        .expect("choose the 4/4 as the fight target");
    assert!(
        !board.runner.state().stack.is_empty(),
        "reach-guard: trigger is on the stack after ChooseTarget"
    );

    board
        .runner
        .cast(bounce)
        .target_objects(&[board.wolf])
        .resolve();

    assert_eq!(
        board.runner.state().players[0].life,
        life_before + 3,
        "CR 701.14b: GainLife still runs when the untargeted Wolf has left"
    );
    assert_eq!(
        board.runner.state().objects[&board.opponent].damage_marked,
        0,
        "no fight when the Wolf is gone"
    );
    assert_eq!(
        board.runner.state().objects[&board.tolsimir].damage_marked,
        0,
        "Tolsimir must not fight in the Wolf's place"
    );
    assert_eq!(
        board.runner.state().objects[&board.opponent].zone,
        Zone::Battlefield,
        "reach-guard: the chosen target stayed legal (contrast 608.2b row)"
    );
    assert_eq!(board.runner.state().objects[&board.wolf].zone, Zone::Hand);
}
