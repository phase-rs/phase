//! `WaitingFor::DeclareBlockers.must_be_blocked_targets` (CR 509.1c) — the
//! attacker-carried `MustBeBlocked` / `MustBeBlockedByAll` requirements on the
//! blocker prompt. Mirrors `deterministic_blocker_prompt_order.rs`'s driving
//! helpers.
//!
//! Oracle text verified verbatim against `data/mtgjson/AtomicCards.json`.

use engine::game::combat::{validate_blockers_for_player, AttackTarget};
use engine::game::scenario::{CastOutcome, GameRunner, GameScenario, P0, P1};
use engine::types::actions::{DebugAction, GameAction};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use std::collections::HashMap;

const P2: PlayerId = PlayerId(2);

const IRRESISTIBLE_PREY: &str = "Target creature must be blocked this turn if able.\nDraw a card.";
const OCHRAN_ASSASSIN: &str = "Deathtouch\nAll creatures able to block this creature do so.";

fn drive_to_declare_attackers(runner: &mut GameRunner) {
    for _ in 0..32 {
        match &runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority should reach declare attackers");
            }
            ref other => panic!("expected priority or declare attackers, got {other:?}"),
        }
    }
    panic!("did not reach declare attackers");
}

fn drive_to_declare_blockers(runner: &mut GameRunner) {
    for _ in 0..32 {
        match &runner.state().waiting_for {
            WaitingFor::DeclareBlockers { .. } => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority should reach declare blockers");
            }
            ref other => panic!("expected priority or declare blockers, got {other:?}"),
        }
    }
    panic!("did not reach declare blockers");
}

fn declare_attacks(runner: &mut GameRunner, attacks: Vec<(ObjectId, AttackTarget)>) {
    drive_to_declare_attackers(runner);
    runner
        .act(GameAction::DeclareAttackers {
            attacks,
            bands: vec![],
        })
        .expect("declaring attackers should succeed");
    drive_to_declare_blockers(runner);
}

/// Read `must_be_blocked_targets` off the current `DeclareBlockers` prompt.
fn must_be_blocked_targets(runner: &GameRunner) -> HashMap<ObjectId, Vec<ObjectId>> {
    match &runner.state().waiting_for {
        WaitingFor::DeclareBlockers {
            must_be_blocked_targets,
            ..
        } => must_be_blocked_targets.clone(),
        other => panic!("expected DeclareBlockers, got {other:?}"),
    }
}

/// Read `valid_block_targets` off the current `DeclareBlockers` prompt.
fn valid_block_targets(runner: &GameRunner) -> HashMap<ObjectId, Vec<ObjectId>> {
    match &runner.state().waiting_for {
        WaitingFor::DeclareBlockers {
            valid_block_targets,
            ..
        } => valid_block_targets.clone(),
        other => panic!("expected DeclareBlockers, got {other:?}"),
    }
}

/// P0 controls two identical 1/1 "Insect"s (`low` created first, `high`
/// second) and casts "Irresistible Prey" (verbatim) targeting `high`, with one
/// library card so the spell's draw does not lose the game (CR 104.3c). P1
/// controls `bear_count` untapped 2/2 "Bear"s. Both Insects attack P1, landing
/// on P1's `DeclareBlockers` prompt.
fn setup_prey_board(
    bear_count: usize,
) -> (GameRunner, ObjectId, ObjectId, Vec<ObjectId>, CastOutcome) {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let low = scenario.add_creature(P0, "Insect", 1, 1).id();
    let high = scenario.add_creature(P0, "Insect", 1, 1).id();
    let prey = scenario
        .add_spell_to_hand_from_oracle(P0, "Irresistible Prey", false, IRRESISTIBLE_PREY)
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario.with_library_top(P0, &["Library Card"]);
    let bears: Vec<ObjectId> = (0..bear_count)
        .map(|index| {
            scenario
                .add_creature(P1, &format!("Bear {index}"), 2, 2)
                .id()
        })
        .collect();

    let mut runner = scenario.build();
    let outcome = runner.cast(prey).target_object(high).resolve();

    declare_attacks(
        &mut runner,
        vec![
            (low, AttackTarget::Player(P1)),
            (high, AttackTarget::Player(P1)),
        ],
    );

    (runner, low, high, bears, outcome)
}

/// The initial prompt (producer: `turns.rs`) marks only the targeted member of
/// an otherwise-identical pair — the requirement's carrier is Prey's target,
/// not the lower id.
#[test]
fn initial_prompt_marks_only_the_targeted_member() {
    let (runner, low, high, bears, outcome) = setup_prey_board(1);
    let bear = bears[0];

    outcome.assert_hand_drawn(P0, 1);

    assert_eq!(
        must_be_blocked_targets(&runner),
        HashMap::from([(bear, vec![high])]),
        "only the targeted Insect is named"
    );

    let valid = valid_block_targets(&runner);
    assert_eq!(
        valid.get(&bear),
        Some(&vec![low, high]),
        "reach guard: low's absence above is not illegality \u{2014} both are \
         legal block targets for the Bear"
    );

    assert!(
        validate_blockers_for_player(runner.state(), P1, &[(bear, low)]).is_err(),
        "blocking the untargeted Insect leaves the named requirement unobeyed"
    );
    assert!(
        validate_blockers_for_player(runner.state(), P1, &[(bear, high)]).is_ok(),
        "blocking the targeted Insect obeys the requirement"
    );
}

/// The bare Lure form (`StaticMode::MustBeBlockedByAll`, Ochran Assassin,
/// verbatim) marks every able blocker toward the lure attacker only — a
/// second, untargeted attacker in the same combat is not named.
#[test]
fn lure_marks_every_able_blocker_toward_the_lure_attacker_only() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let ochran = scenario
        .add_creature_from_oracle(P0, "Ochran Assassin", 1, 1, OCHRAN_ASSASSIN)
        .id();
    let plain = scenario.add_creature(P0, "Insect", 1, 1).id();
    let b1 = scenario.add_creature(P1, "Bear", 2, 2).id();
    let b2 = scenario.add_creature(P1, "Bear", 2, 2).id();

    let mut runner = scenario.build();
    declare_attacks(
        &mut runner,
        vec![
            (ochran, AttackTarget::Player(P1)),
            (plain, AttackTarget::Player(P1)),
        ],
    );

    let valid = valid_block_targets(&runner);
    assert_eq!(
        valid.get(&b1),
        Some(&vec![ochran, plain]),
        "reach guard: both blockers can legally block both attackers"
    );
    assert_eq!(valid.get(&b2), Some(&vec![ochran, plain]));

    assert_eq!(
        must_be_blocked_targets(&runner),
        HashMap::from([(b1, vec![ochran]), (b2, vec![ochran])]),
        "every able blocker is named toward the lure attacker only"
    );
}

/// The multiplayer transition to the next defender (producer:
/// `engine_combat.rs::next_blocker_or_finish_declaration`) carries the
/// requirement only on the prompt of the defender the named attacker is
/// attacking; an earlier defender's prompt does not.
#[test]
fn next_defenders_prompt_carries_it_first_defenders_does_not() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let preyed = scenario.add_creature(P0, "Insect", 1, 1).id();
    let other_attacker = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
    let prey = scenario
        .add_spell_to_hand_from_oracle(P0, "Irresistible Prey", false, IRRESISTIBLE_PREY)
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario.with_library_top(P0, &["Library Card"]);
    let p1_bear = scenario.add_creature(P1, "Bear", 2, 2).id();
    let p2_bear = scenario.add_creature(P2, "Bear", 2, 2).id();

    let mut runner = scenario.build();
    runner.cast(prey).target_object(preyed).resolve();

    declare_attacks(
        &mut runner,
        vec![
            (other_attacker, AttackTarget::Player(P1)),
            (preyed, AttackTarget::Player(P2)),
        ],
    );

    match &runner.state().waiting_for {
        WaitingFor::DeclareBlockers { player, .. } => assert_eq!(*player, P1),
        other => panic!("expected DeclareBlockers for P1, got {other:?}"),
    }
    assert_eq!(
        valid_block_targets(&runner).get(&p1_bear),
        Some(&vec![other_attacker]),
        "reach guard: P1's Bear is a legal blocker"
    );
    assert!(
        must_be_blocked_targets(&runner).is_empty(),
        "P1's board carries no attacker-carried requirement: the named \
         attacker is attacking P2, not P1"
    );

    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![],
        })
        .expect("P1 declaring no blockers should advance to P2");

    match &runner.state().waiting_for {
        WaitingFor::DeclareBlockers { player, .. } => assert_eq!(*player, P2),
        other => panic!("expected DeclareBlockers for P2, got {other:?}"),
    }
    assert_eq!(
        must_be_blocked_targets(&runner).get(&p2_bear),
        Some(&vec![preyed]),
        "P2's prompt carries the requirement on the attacker aimed at them"
    );
}

/// A mid-prompt debug refresh (producer:
/// `combat.rs::refresh_combat_declaration_waiting_for`) recomputes the map:
/// a blocker that can no longer block loses its key, and an unaffected blocker
/// keeps its entry.
#[test]
fn mid_prompt_refresh_recomputes_it() {
    let (mut runner, _low, high, bears, _outcome) = setup_prey_board(2);
    let (bear_a, bear_b) = (bears[0], bears[1]);

    let before = must_be_blocked_targets(&runner);
    assert_eq!(
        before.get(&bear_a),
        Some(&vec![high]),
        "reach guard: both Bears list the targeted Insect before the refresh"
    );
    assert_eq!(before.get(&bear_b), Some(&vec![high]));

    runner.state_mut().debug_mode = true;
    runner
        .act(GameAction::Debug(DebugAction::SetTapped {
            object_id: bear_a,
            tapped: true,
        }))
        .expect("debug SetTapped should refresh the blocker prompt");

    let after = must_be_blocked_targets(&runner);
    assert!(
        !after.contains_key(&bear_a),
        "the tapped Bear can no longer block, so its key is gone"
    );
    assert_eq!(
        after.get(&bear_b),
        Some(&vec![high]),
        "the untapped Bear still lists the targeted Insect"
    );
}
