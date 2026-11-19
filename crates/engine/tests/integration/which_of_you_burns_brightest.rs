//! Which of You Burns Brightest?: pay X, burn a target, sweep their creatures.
//!
//! Oracle: "When you set this scheme in motion, you may pay {X}. If you do,
//! this scheme deals X damage to target opponent or planeswalker and each
//! creature that player or that planeswalker's controller controls."
//!
//! CR 601.2c + CR 603.3d: the opponent-or-planeswalker target is announced
//! when the trigger goes on the stack, before the may-pay resolves.
//! CR 608.2c: the sweep leg inherits that bound target and reads its
//! controller back branch-correctly — a player answers itself, a
//! planeswalker answers its controller (`parent_target_controller`, LKI
//! when the primary already destroyed it).
//!
//! Revert-to-red: drop the sweep-leg synthesis — the document gaps
//! (scheme_face asserts). Bind the sweep to `TargetPlayer` instead of
//! `ParentTargetController` — the planeswalker branch sweeps nothing (its
//! creature survives). Drop target propagation — both sweeps fizzle.

use engine::database::synthesis::synthesize_archenemy;
use engine::game::deck_loading::create_scheme_deck_card;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::parser::parse_oracle_text;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::format::FormatConfig;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const WHICH_OF_YOU_BURNS_BRIGHTEST: &str = "When you set this scheme in motion, you may pay {X}. If you do, this scheme deals X damage to target opponent or planeswalker and each creature that player or that planeswalker's controller controls.";

const P2: PlayerId = PlayerId(2);

fn scheme_face(name: &str, oracle: &str) -> CardFace {
    let parsed = parse_oracle_text(oracle, name, &[], &["Scheme".to_string()], &[]);
    assert!(
        !format!("{parsed:?}").contains("Unimplemented"),
        "{name}: document is gapped"
    );
    let mut face = CardFace {
        name: name.to_string(),
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        ..Default::default()
    };
    face.card_type.core_types.push(CoreType::Scheme);
    synthesize_archenemy(&mut face);
    let gaps = engine::game::coverage::card_face_gaps(&face);
    assert!(gaps.is_empty(), "{name}: gaps: {gaps:?}");
    face
}

/// Drive to `target`, declaring no attackers/blockers through combat and
/// passing priority. Panics on any other prompt.
fn drive_to_phase(runner: &mut GameRunner, target: Phase) {
    for _ in 0..64 {
        runner.advance_to_phase(target);
        if runner.state().phase == target {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                runner.declare_attackers(&[]).expect("declare no attackers");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner.declare_blockers(&[]).expect("declare no blockers");
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
                runner.act(GameAction::PassPriority).expect("pass priority");
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            other => panic!("unexpected prompt driving to {target:?}: {other:?}"),
        }
    }
    panic!("never reached {target:?}");
}

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn life_of(runner: &GameRunner, player: PlayerId) -> i32 {
    runner.state().players[player.0 as usize].life
}

fn loyalty_of(runner: &GameRunner, id: ObjectId) -> Option<u32> {
    runner.state().objects[&id].loyalty
}

struct BurnsBoard {
    p1_creature: ObjectId,
    p2_creature: ObjectId,
    p2_walker: ObjectId,
}

/// Build the 3-player game: a 2/2 for each opponent, a 5-loyalty walker for
/// P2. Returns the runner parked at precombat main with the scheme fired.
fn setup_burns() -> (GameRunner, BurnsBoard) {
    let mut format = FormatConfig::archenemy();
    format.archenemy_player = Some(P0);
    let mut scenario = GameScenario::new_with_format(format, 3, 42);
    scenario.at_phase(Phase::Draw);
    for i in 0..10 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {i}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Library {i}"));
        scenario.add_card_to_library_top(P2, &format!("P2 Library {i}"));
    }
    let p1_creature = scenario.add_vanilla(P1, 2, 2);
    let p2_creature = scenario.add_vanilla(P2, 2, 2);
    let p2_walker = scenario
        .add_planeswalker_from_oracle(P2, "P2 Walker", "Test", 5, "+1: You gain 1 life.")
        .id();
    let mut runner = scenario.build();

    let face = scheme_face(
        "Which of You Burns Brightest?",
        WHICH_OF_YOU_BURNS_BRIGHTEST,
    );
    let scheme = create_scheme_deck_card(runner.state_mut(), &face, P0);
    runner.state_mut().archenemy = Some(P0);
    runner.state_mut().active_player = P0;

    drive_to_phase(&mut runner, Phase::PreCombatMain);
    assert!(
        runner.state().command_zone.contains(&scheme) && !runner.state().objects[&scheme].face_down,
        "reach guard: turn 1 must set the scheme in motion"
    );
    // The pool empties across phases, so fund the may-pay after the drive.
    for _ in 0..6 {
        let _ = runner.state_mut().add_mana_to_pool(
            P0,
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        );
    }
    (
        runner,
        BurnsBoard {
            p1_creature,
            p2_creature,
            p2_walker,
        },
    )
}

#[derive(Default)]
struct BurnsPrompts {
    targets: u32,
    may_decisions: u32,
    pay_offers: u32,
    x_choices: u32,
    mana_payments: u32,
}

/// Resolve the scheme: announce `target`, answer the may-pay with `pay`,
/// and pay `x` when accepted. Panics on any other prompt.
fn resolve_burns(runner: &mut GameRunner, target: TargetRef, pay: bool, x: u32) -> BurnsPrompts {
    let mut prompts = BurnsPrompts::default();
    for _ in 0..64 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::TriggerTargetSelection { selection, .. }
            | WaitingFor::TargetSelection { selection, .. } => {
                assert!(
                    selection.current_legal_targets.contains(&target),
                    "reach guard: the scripted target must be legal, legals: {:?}",
                    selection.current_legal_targets
                );
                prompts.targets += 1;
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(target.clone()),
                    })
                    .expect("target must be accepted");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                prompts.may_decisions += 1;
                runner
                    .act(GameAction::DecideOptionalEffect { accept: pay })
                    .expect("may decision must be accepted");
            }
            WaitingFor::OptionalCostChoice { .. } => {
                prompts.pay_offers += 1;
                runner
                    .act(GameAction::DecideOptionalCost {
                        index: if pay { 0 } else { 1 },
                    })
                    .expect("pay decision must be accepted");
            }
            WaitingFor::ChooseXValue { .. } => {
                assert!(pay, "X must only be chosen on the pay path");
                prompts.x_choices += 1;
                runner
                    .act(GameAction::ChooseX { value: x })
                    .expect("X must be accepted");
            }
            WaitingFor::PayAmountChoice { .. } => {
                assert!(pay, "X must only be paid on the pay path");
                prompts.x_choices += 1;
                runner
                    .act(GameAction::SubmitPayAmount { amount: x })
                    .expect("X payment must be accepted");
            }
            WaitingFor::ManaPayment { .. } => {
                prompts.mana_payments += 1;
                runner.act(GameAction::PassPriority).expect("auto-pay");
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt resolving the scheme: {other:?}"),
        }
    }
    assert!(
        runner.state().stack.is_empty(),
        "all triggers must have resolved"
    );
    prompts
}

#[test]
fn burns_brightest_player_branch_burns_player_and_sweeps_their_creatures() {
    let (mut runner, board) = setup_burns();
    let p0_life = life_of(&runner, P0);
    let p1_life = life_of(&runner, P1);
    let p2_life = life_of(&runner, P2);

    let prompts = resolve_burns(&mut runner, TargetRef::Player(P1), true, 3);
    runner.advance_until_stack_empty();

    assert_eq!(prompts.targets, 1, "one target announcement");
    assert_eq!(prompts.may_decisions, 1, "one may decision");
    assert_eq!(
        prompts.pay_offers, 0,
        "single-step: the amount prompt follows the may directly"
    );
    assert_eq!(prompts.x_choices, 1, "one X choice");
    // The primary: P1 takes 3.
    assert_eq!(life_of(&runner, P1), p1_life - 3, "P1 takes the X");
    // The sweep: P1's 2/2 takes 3 and dies; P2's board is untouched.
    assert_eq!(
        zone_of(&runner, board.p1_creature),
        Zone::Graveyard,
        "P1's creature must be swept"
    );
    assert_eq!(
        zone_of(&runner, board.p2_creature),
        Zone::Battlefield,
        "P2's creature must survive a P1-aimed sweep"
    );
    assert_eq!(loyalty_of(&runner, board.p2_walker), Some(5));
    assert_eq!(life_of(&runner, P2), p2_life, "P2 takes nothing");
    assert_eq!(life_of(&runner, P0), p0_life, "P0 takes nothing");
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the chain must resolve back to priority with an empty stack"
    );
}

#[test]
fn burns_brightest_planeswalker_branch_burns_walker_and_sweeps_controller_creatures() {
    let (mut runner, board) = setup_burns();
    let p0_life = life_of(&runner, P0);
    let p1_life = life_of(&runner, P1);
    let p2_life = life_of(&runner, P2);

    let prompts = resolve_burns(&mut runner, TargetRef::Object(board.p2_walker), true, 2);
    runner.advance_until_stack_empty();

    assert_eq!(prompts.targets, 1, "one target announcement");
    assert_eq!(prompts.may_decisions, 1, "one may decision");
    assert_eq!(
        prompts.pay_offers, 0,
        "single-step: the amount prompt follows the may directly"
    );
    assert_eq!(prompts.x_choices, 1, "one X choice");
    // The primary: the walker loses 2 loyalty.
    assert_eq!(
        loyalty_of(&runner, board.p2_walker),
        Some(3),
        "the walker takes the X"
    );
    // The sweep: its controller's 2/2 takes 2 and dies; P1 is untouched.
    assert_eq!(
        zone_of(&runner, board.p2_creature),
        Zone::Graveyard,
        "the walker's controller's creature must be swept"
    );
    assert_eq!(
        zone_of(&runner, board.p1_creature),
        Zone::Battlefield,
        "P1's creature must survive a walker-aimed sweep"
    );
    assert_eq!(life_of(&runner, P1), p1_life, "P1 takes nothing");
    assert_eq!(life_of(&runner, P2), p2_life, "P2 takes nothing");
    assert_eq!(life_of(&runner, P0), p0_life, "P0 takes nothing");
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the chain must resolve back to priority with an empty stack"
    );
}

#[test]
fn burns_brightest_declined_pay_deals_no_damage() {
    let (mut runner, board) = setup_burns();
    let p0_life = life_of(&runner, P0);
    let p1_life = life_of(&runner, P1);
    let p2_life = life_of(&runner, P2);

    let prompts = resolve_burns(&mut runner, TargetRef::Player(P1), false, 0);
    runner.advance_until_stack_empty();

    assert_eq!(prompts.targets, 1, "the target is still announced");
    assert_eq!(prompts.may_decisions, 1, "the may is still offered");
    assert_eq!(prompts.pay_offers, 0, "no pay offer on the decline path");
    assert_eq!(prompts.x_choices, 0, "no X choice on the decline path");
    assert_eq!(life_of(&runner, P0), p0_life);
    assert_eq!(life_of(&runner, P1), p1_life);
    assert_eq!(life_of(&runner, P2), p2_life);
    assert_eq!(loyalty_of(&runner, board.p2_walker), Some(5));
    assert_eq!(zone_of(&runner, board.p1_creature), Zone::Battlefield);
    assert_eq!(zone_of(&runner, board.p2_creature), Zone::Battlefield);
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the chain must resolve back to priority with an empty stack"
    );
}
