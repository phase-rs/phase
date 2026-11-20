//! CR 500.1 + CR 506.1: a bare "before <combat step>" window is a point in the turn — open from
//! the start of the turn until that step of the turn's first combat phase, while a combat phase is
//! still ahead (a skipped combat phase never occurs, CR 500.11) — while "during combat before
//! <combat step>" is measured in each combat phase.

use engine::game::combat::AttackTarget;
use engine::game::engine::EngineError;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::ObjectId;

const BERSERK: &str = "Cast this spell only before the combat damage step.
Target creature gains trample and gets +X/+0 until end of turn, where X is its power. At the beginning of the next end step, destroy that creature if it attacked this turn.";
const RAPID_FIRE: &str = "Cast this spell only before blockers are declared.
Target creature gains first strike until end of turn. If it doesn't have rampage, that creature gains rampage 2 until end of turn. (Whenever the creature becomes blocked, it gets +2/+2 until end of turn for each creature blocking it beyond the first.)";
const ANGUS: &str = "{G}{W}{U}, {T}: Prevent all combat damage that would be dealt this turn. Activate only before the combat damage step.";
const TALAS: &str =
    "{T}: Draw a card. Activate only during your turn, before attackers are declared.";
const SIRENS_CALL: &str = "Cast this spell only during an opponent's turn, before attackers are declared.
Creatures the active player controls attack this turn if able.
At the beginning of the next end step, destroy all non-Wall creatures that player controls that didn't attack this turn. Ignore this effect for each creature the player didn't control continuously since the beginning of the turn.";
const PANIC: &str = "Cast this spell only during combat before blockers are declared.
Target creature can't block this turn.
Draw a card at the beginning of the next turn's upkeep.";
const SAVE_POINT: &str = "When this enchantment enters, draw a card.
Sacrifice this enchantment: Remove each creature from combat and untap each creature that attacked this turn. There is an additional combat phase after this one. Activate only during combat before combat damage has been dealt.";
const RELENTLESS_ASSAULT: &str = "Untap all creatures that attacked this turn. After this main phase, there is an additional combat phase followed by an additional main phase.";
const MOMENT_OF_SILENCE: &str = "Target player skips their next combat phase this turn.";

fn assert_refused<T>(result: Result<T, EngineError>, restriction: &str) {
    match result {
        Err(EngineError::ActionNotAllowed(msg)) => assert!(
            msg.ends_with(&format!("not satisfied: {restriction}")),
            "refused for the wrong reason: {msg}"
        ),
        Err(other) => panic!("expected a {restriction} refusal, got {other:?}"),
        Ok(_) => panic!("expected a {restriction} refusal, but it was allowed"),
    }
}

/// Activates `source`'s first ability and resolves the stack.
fn activate(runner: &mut GameRunner, source: ObjectId) -> Result<(), EngineError> {
    runner.act(GameAction::ActivateAbility {
        source_id: source,
        ability_index: 0,
        payment_mode: CastPaymentMode::Auto,
    })?;
    while !runner.state().stack.is_empty() {
        runner
            .act(GameAction::PassPriority)
            .expect("the stack resolves");
    }
    Ok(())
}

/// Passes through the turn, declaring `attacks` and no blocks, until P0 holds priority on an
/// empty stack in a state satisfying `done`.
fn drive_until(
    runner: &mut GameRunner,
    attacks: &[(ObjectId, AttackTarget)],
    done: impl Fn(&GameState) -> bool,
) {
    for _ in 0..300 {
        let state = runner.state();
        if done(state)
            && state.stack.is_empty()
            && matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0)
        {
            return;
        }
        let action = match state.waiting_for {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: attacks.to_vec(),
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            _ => GameAction::PassPriority,
        };
        runner.act(action).expect("the turn advances");
    }
    panic!("the turn never reached the requested state");
}

/// Drives to the beginning of combat step of the turn's `n`th combat phase, declaring no attackers.
fn drive_to_beginning_of_combat(runner: &mut GameRunner, n: u32) {
    drive_until(runner, &[], |state| {
        state.phase == Phase::BeginCombat
            && state.steps_started_this_turn.count(Phase::BeginCombat) == n
    });
}

#[test]
fn bare_before_windows_are_open_before_the_first_combat() {
    for phase in [Phase::Upkeep, Phase::PreCombatMain] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(phase);
        scenario.with_mana_pool(
            P0,
            [ManaType::Green, ManaType::White, ManaType::Blue]
                .into_iter()
                .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
                .collect(),
        );
        let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let angus = scenario
            .add_creature_from_oracle(P0, "Angus Mackenzie", 2, 2, ANGUS)
            .id();
        let talas = scenario
            .add_creature_from_oracle(P0, "Talas Researcher", 1, 1, TALAS)
            .id();
        let berserk = scenario
            .add_spell_to_hand_from_oracle(P0, "Berserk", true, BERSERK)
            .id();
        let rapid_fire = scenario
            .add_spell_to_hand_from_oracle(P0, "Rapid Fire", true, RAPID_FIRE)
            .id();
        scenario.with_library_top(P0, &["Island", "Island"]);
        let mut runner = scenario.build();

        runner
            .cast(berserk)
            .target_object(bear)
            .try_resolve()
            .unwrap_or_else(|e| panic!("Berserk in {phase:?}: {e:?}"));
        assert!(
            runner.state().players[0].graveyard.contains(&berserk),
            "{phase:?}"
        );
        runner
            .cast(rapid_fire)
            .target_object(bear)
            .try_resolve()
            .unwrap_or_else(|e| panic!("Rapid Fire in {phase:?}: {e:?}"));
        activate(&mut runner, angus).unwrap_or_else(|e| panic!("Angus in {phase:?}: {e:?}"));
        let hand = runner.state().players[0].hand.len();
        activate(&mut runner, talas).unwrap_or_else(|e| panic!("Talas in {phase:?}: {e:?}"));
        assert_eq!(runner.state().players[0].hand.len(), hand + 1, "{phase:?}");
    }
}

/// A board whose turn gets a second combat phase from Relentless Assault cast in the precombat
/// main phase.
struct TwoCombats {
    runner: GameRunner,
    bear: ObjectId,
    talas: ObjectId,
    save_point: ObjectId,
    berserk: [ObjectId; 2],
    rapid_fire: ObjectId,
    panic: ObjectId,
}

fn two_combats() -> TwoCombats {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let talas = scenario
        .add_creature_from_oracle(P0, "Talas Researcher", 1, 1, TALAS)
        .id();
    let save_point = scenario
        .add_enchantment_from_oracle(P0, "Save Point", SAVE_POINT)
        .id();
    let relentless = scenario
        .add_spell_to_hand_from_oracle(P0, "Relentless Assault", false, RELENTLESS_ASSAULT)
        .id();
    let berserk = [(); 2].map(|()| {
        scenario
            .add_spell_to_hand_from_oracle(P0, "Berserk", true, BERSERK)
            .id()
    });
    let rapid_fire = scenario
        .add_spell_to_hand_from_oracle(P0, "Rapid Fire", true, RAPID_FIRE)
        .id();
    let panic = scenario
        .add_spell_to_hand_from_oracle(P0, "Panic", true, PANIC)
        .id();
    scenario.with_library_top(P0, &["Island", "Island", "Island", "Island"]);
    scenario.with_library_top(P1, &["Island", "Island"]);
    let mut runner = scenario.build();
    runner
        .cast(relentless)
        .try_resolve()
        .expect("Relentless Assault resolves");
    TwoCombats {
        runner,
        bear,
        talas,
        save_point,
        berserk,
        rapid_fire,
        panic,
    }
}

#[test]
fn bare_before_windows_close_after_the_first_combat() {
    let mut board = two_combats();
    let runner = &mut board.runner;

    // Panic is refused before combat: its window opens only during combat.
    assert_refused(
        runner
            .cast(board.panic)
            .target_object(board.bear)
            .try_resolve(),
        "DuringCombat",
    );

    drive_to_beginning_of_combat(runner, 1);
    runner
        .cast(board.berserk[0])
        .target_object(board.bear)
        .try_resolve()
        .expect("Berserk in the first combat");

    drive_to_beginning_of_combat(runner, 2);
    assert_refused(
        runner
            .cast(board.berserk[1])
            .target_object(board.bear)
            .try_resolve(),
        "BeforeCombatDamage",
    );
    assert_refused(
        runner
            .cast(board.rapid_fire)
            .target_object(board.bear)
            .try_resolve(),
        "BeforeBlockersDeclared",
    );
    assert_refused(activate(runner, board.talas), "BeforeAttackersDeclared");
}

#[test]
fn during_combat_windows_reopen_in_each_combat() {
    let mut board = two_combats();
    let runner = &mut board.runner;

    drive_to_beginning_of_combat(runner, 2);
    runner
        .cast(board.panic)
        .target_object(board.bear)
        .try_resolve()
        .expect("Panic in the second combat");
    activate(runner, board.save_point).expect("Save Point in the second combat");
}

#[test]
fn opponents_turn_before_attackers_window_opens_in_upkeep() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Upkeep);
    let own = scenario
        .add_spell_to_hand_from_oracle(P0, "Siren's Call", true, SIRENS_CALL)
        .id();
    let theirs = scenario
        .add_spell_to_hand_from_oracle(P1, "Siren's Call", true, SIRENS_CALL)
        .id();
    let mut runner = scenario.build();

    assert_refused(runner.cast(own).try_resolve(), "DuringOpponentsTurn");
    runner.act(GameAction::PassPriority).expect("P0 passes");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player } if player == P1
    ));
    runner
        .cast(theirs)
        .try_resolve()
        .expect("Siren's Call in the opponent's upkeep");
}

/// P0's precombat main phase with Moment of Silence and Relentless Assault in hand, and lands for
/// Angus Mackenzie.
struct SkippedCombat {
    runner: GameRunner,
    bear: ObjectId,
    angus: ObjectId,
    talas: ObjectId,
    berserk: ObjectId,
    rapid_fire: ObjectId,
    moment_of_silence: ObjectId,
    relentless: ObjectId,
}

fn skipped_combat() -> SkippedCombat {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for color in [ManaColor::Green, ManaColor::White, ManaColor::Blue] {
        scenario.add_basic_land(P0, color);
    }
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let angus = scenario
        .add_creature_from_oracle(P0, "Angus Mackenzie", 2, 2, ANGUS)
        .id();
    let talas = scenario
        .add_creature_from_oracle(P0, "Talas Researcher", 1, 1, TALAS)
        .id();
    let berserk = scenario
        .add_spell_to_hand_from_oracle(P0, "Berserk", true, BERSERK)
        .id();
    let rapid_fire = scenario
        .add_spell_to_hand_from_oracle(P0, "Rapid Fire", true, RAPID_FIRE)
        .id();
    let moment_of_silence = scenario
        .add_spell_to_hand_from_oracle(P0, "Moment of Silence", true, MOMENT_OF_SILENCE)
        .id();
    let relentless = scenario
        .add_spell_to_hand_from_oracle(P0, "Relentless Assault", false, RELENTLESS_ASSAULT)
        .id();
    scenario.with_library_top(P0, &["Island", "Island"]);
    let mut runner = scenario.build();
    runner
        .cast(moment_of_silence)
        .target_player(P0)
        .try_resolve()
        .expect("Moment of Silence resolves");
    SkippedCombat {
        runner,
        bear,
        angus,
        talas,
        berserk,
        rapid_fire,
        moment_of_silence,
        relentless,
    }
}

fn assert_windows_open(board: &mut SkippedCombat) {
    let runner = &mut board.runner;
    activate(runner, board.angus).expect("Angus Mackenzie");
    runner
        .cast(board.berserk)
        .target_object(board.bear)
        .try_resolve()
        .expect("Berserk");
    assert!(runner.state().players[0].graveyard.contains(&board.berserk));
    runner
        .cast(board.rapid_fire)
        .target_object(board.bear)
        .try_resolve()
        .expect("Rapid Fire");
    let hand = runner.state().players[0].hand.len();
    activate(runner, board.talas).expect("Talas Researcher");
    assert_eq!(runner.state().players[0].hand.len(), hand + 1);
}

fn no_combat_begun(state: &GameState) -> bool {
    state.phase == Phase::PostCombatMain
        && state.steps_started_this_turn.count(Phase::BeginCombat) == 0
}

#[test]
fn added_main_phase_before_the_first_actual_combat_keeps_bare_windows_open() {
    let mut board = skipped_combat();
    board
        .runner
        .cast(board.relentless)
        .try_resolve()
        .expect("Relentless Assault resolves");
    drive_until(&mut board.runner, &[], |state| {
        no_combat_begun(state) && !state.extra_phase_resume.is_empty()
    });
    assert_windows_open(&mut board);
    drive_to_beginning_of_combat(&mut board.runner, 1);
}

#[test]
fn postcombat_main_after_a_skipped_combat_closes_bare_windows() {
    let mut board = skipped_combat();
    let runner = &mut board.runner;
    drive_until(runner, &[], |state| {
        no_combat_begun(state) && state.extra_phase_resume.is_empty()
    });
    assert!(runner.state().players[0]
        .graveyard
        .contains(&board.moment_of_silence));
    assert_refused(activate(runner, board.angus), "BeforeCombatDamage");
    assert_refused(
        runner
            .cast(board.berserk)
            .target_object(board.bear)
            .try_resolve(),
        "BeforeCombatDamage",
    );
    assert_refused(
        runner
            .cast(board.rapid_fire)
            .target_object(board.bear)
            .try_resolve(),
        "BeforeBlockersDeclared",
    );
    assert_refused(activate(runner, board.talas), "BeforeAttackersDeclared");
}

#[test]
fn combat_added_after_a_skipped_combat_reopens_bare_windows() {
    let mut board = skipped_combat();
    drive_until(&mut board.runner, &[], |state| {
        no_combat_begun(state) && state.extra_phase_resume.is_empty()
    });
    board
        .runner
        .cast(board.relentless)
        .try_resolve()
        .expect("Relentless Assault resolves");
    assert_windows_open(&mut board);
    drive_to_beginning_of_combat(&mut board.runner, 1);
}

#[test]
fn added_main_phase_after_the_first_combat_closes_bare_windows() {
    let mut board = two_combats();
    let runner = &mut board.runner;
    drive_until(runner, &[], |state| {
        state.phase == Phase::PostCombatMain
            && state.steps_started_this_turn.count(Phase::BeginCombat) == 1
            && !state.extra_phase_resume.is_empty()
    });
    assert_refused(
        runner
            .cast(board.berserk[0])
            .target_object(board.bear)
            .try_resolve(),
        "BeforeCombatDamage",
    );
    assert_refused(
        runner
            .cast(board.rapid_fire)
            .target_object(board.bear)
            .try_resolve(),
        "BeforeBlockersDeclared",
    );
    assert_refused(activate(runner, board.talas), "BeforeAttackersDeclared");
}

#[test]
fn damage_step_of_an_added_first_combat_closes_bare_windows() {
    let mut board = two_combats();
    let runner = &mut board.runner;
    drive_until(runner, &[(board.bear, AttackTarget::Player(P1))], |state| {
        state.phase == Phase::CombatDamage
            && state.steps_started_this_turn.count(Phase::BeginCombat) == 1
            && !state.extra_phase_resume.is_empty()
    });
    assert_refused(
        runner
            .cast(board.berserk[0])
            .target_object(board.bear)
            .try_resolve(),
        "BeforeCombatDamage",
    );
    assert_refused(
        runner
            .cast(board.rapid_fire)
            .target_object(board.bear)
            .try_resolve(),
        "BeforeBlockersDeclared",
    );
}
