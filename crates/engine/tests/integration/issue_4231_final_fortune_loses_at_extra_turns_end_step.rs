//! Issue #4231 — Final Fortune: "Take an extra turn after this one. At the
//! beginning of that turn's end step, you lose the game."
//!
//! CR 500.7 + CR 603.7a: "that turn" is the extra turn the first sentence
//! grants, so the delayed loss must wait for that turn's end step. Before the
//! fix it fired at the end step of the turn Final Fortune was cast in, so the
//! caster lost before the extra turn was ever taken.
//!
//! The loss is bound to the SPECIFIC granted turn (`ExtraTurnId`): a skipped
//! grant retires it (CR 614.10a, both live skip paths), a LIFO-reordered queue
//! fires it at its own turn (CR 500.7), and a grant to another player fires at
//! that player's turn. "That turn" in an unrelated chain (Oracle en-Vec) is not
//! rewritten to the extra-turn rule.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::game_state::{
    GameState, PersistedGameState, PersistedRestoreFinalization, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const FINAL_FORTUNE: &str = "Take an extra turn after this one. At the beginning of that turn's end step, you lose the game.";

fn floating_mana(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

/// Walk the real priority loop until `stop` holds or the game ends. Uses
/// `PassPriority` rather than `advance_to_phase`, which re-runs
/// beginning-of-step triggers when priority is already open.
fn pass_priority_until(runner: &mut GameRunner, stop: impl Fn(&GameRunner) -> bool) {
    for _ in 0..200 {
        if stop(runner) || matches!(runner.state().waiting_for, WaitingFor::GameOver { .. }) {
            return;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner.declare_attackers(&[]).expect("declare no attackers");
            }
            other => panic!("unexpected prompt while advancing: {other:?}"),
        }
    }
    panic!("stop condition never reached");
}

fn in_turn_after_its_end_step(runner: &GameRunner, turn: u32) -> bool {
    runner.state().turn_number > turn
}

fn assert_still_in_game(runner: &GameRunner, when: &str) {
    let state = runner.state();
    assert!(
        !matches!(state.waiting_for, WaitingFor::GameOver { .. })
            && !state.players[0].is_eliminated,
        "Final Fortune's caster must still be in the game {when}; waiting_for = {:?}",
        state.waiting_for
    );
}

fn assert_caster_lost(runner: &GameRunner) {
    let state = runner.state();
    assert!(
        state.players[0].is_eliminated,
        "the caster must lose at the extra turn's end step; waiting_for = {:?}",
        state.waiting_for
    );
    assert_eq!(
        state.waiting_for,
        WaitingFor::GameOver { winner: Some(P1) },
        "the opponent wins once the delayed loss resolves"
    );
}

/// Cast Final Fortune on `caster`'s behalf while `active` holds the turn.
fn cast_final_fortune(active: PlayerId) -> (GameRunner, u32) {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Lib A", "Lib B", "Lib C"]);
    scenario.with_library_top(P1, &["Opp A", "Opp B", "Opp C"]);
    let final_fortune = scenario
        .add_spell_to_hand_from_oracle(P0, "Final Fortune", true, FINAL_FORTUNE)
        .id();
    scenario.with_mana_pool(P0, floating_mana(2, ManaType::Red));

    let mut runner = scenario.build();
    if active != P0 {
        let state = runner.state_mut();
        state.active_player = active;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    let cast_turn = runner.state().turn_number;
    runner.cast(final_fortune).resolve();

    // Reach-guards: the spell granted exactly one extra turn and scheduled one
    // delayed trigger, so every later assertion is about that trigger's timing.
    assert_eq!(
        runner.state().extra_turns.len(),
        1,
        "Final Fortune must grant one extra turn"
    );
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "Final Fortune must schedule its delayed loss"
    );
    (runner, cast_turn)
}

#[test]
fn final_fortune_cast_on_your_turn_loses_at_the_extra_turns_end_step_not_this_one() {
    let (mut runner, cast_turn) = cast_final_fortune(P0);

    // Through this turn's end step and cleanup, into the extra turn.
    pass_priority_until(&mut runner, |r| in_turn_after_its_end_step(r, cast_turn));
    assert_still_in_game(&runner, "after the turn Final Fortune was cast in");
    assert_eq!(
        runner.state().active_player,
        P0,
        "the extra turn is the caster's"
    );
    assert_eq!(runner.state().turn_number, cast_turn + 1);

    // Into the extra turn's end step, then let the delayed loss resolve.
    pass_priority_until(&mut runner, |r| r.state().turn_number > cast_turn + 1);
    assert_caster_lost(&runner);
}

#[test]
fn final_fortune_cast_on_an_opponents_turn_loses_at_the_extra_turns_end_step() {
    // Instant speed on the opponent's turn: "this one" is P1's turn, and the
    // extra turn is P0's. The opponent's end step must not fire the loss.
    let (mut runner, cast_turn) = cast_final_fortune(P1);

    pass_priority_until(&mut runner, |r| in_turn_after_its_end_step(r, cast_turn));
    assert_still_in_game(&runner, "after the opponent's turn it was cast in");
    assert_eq!(
        runner.state().active_player,
        P0,
        "the extra turn is the caster's"
    );

    pass_priority_until(&mut runner, |r| r.state().turn_number > cast_turn + 1);
    assert_caster_lost(&runner);
}

const TIME_WARP: &str = "Target player takes an extra turn after this one.";
const MEDITATE: &str = "Draw four cards. You skip your next turn.";
const TROUBLE_IN_PAIRS: &str = "If an opponent would begin an extra turn, that player skips that turn instead.\nWhenever an opponent attacks you with two or more creatures, draws their second card each turn, or casts their second spell each turn, you draw a card.";
/// Synthetic: Final Fortune's loss clause behind a Time Warp-style grant to
/// another player (nearest printed analogues: Final Fortune, Time Warp).
const GRANT_TO_OPPONENT_THEN_LOSE: &str = "Target player takes an extra turn after this one. At the beginning of that turn's end step, you lose the game.";
/// Synthetic: two grants, so "that turn" has no single antecedent (nearest
/// printed analogue: Final Fortune; Time Stretch grants two turns).
const TWO_GRANTS_THEN_LOSE: &str = "Take two extra turns after this one. At the beginning of that turn's end step, you lose the game.";
const ORACLE_EN_VEC: &str = "{T}: Target opponent chooses any number of creatures they control. During that player's next turn, the chosen creatures attack if able, and other creatures can't attack. At the beginning of that turn's end step, destroy each of the chosen creatures that didn't attack this turn. Activate only during your turn.";

fn library(scenario: &mut GameScenario) {
    scenario.with_library_top(
        P0,
        &["L1", "L2", "L3", "L4", "L5", "L6", "L7", "L8", "L9", "L10"],
    );
    scenario.with_library_top(
        P1,
        &["O1", "O2", "O3", "O4", "O5", "O6", "O7", "O8", "O9", "O10"],
    );
}

/// CR 614.10a + CR 603.7a: Meditate's "you skip your next turn" (the
/// `turns_to_skip` counter path) skips the granted extra turn, so its end step
/// never occurs and the loss is retired; P0 survives the later natural turns.
#[test]
fn final_fortune_loss_is_retired_when_the_granted_turn_is_skipped_by_a_turn_skip() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    library(&mut scenario);
    let ff = scenario
        .add_spell_to_hand_from_oracle(P0, "Final Fortune", true, FINAL_FORTUNE)
        .id();
    let meditate = scenario
        .add_spell_to_hand_from_oracle(P0, "Meditate", true, MEDITATE)
        .id();
    scenario.with_mana_pool(P0, floating_mana(8, ManaType::Red));
    let mut runner = scenario.build();
    let cast_turn = runner.state().turn_number;
    runner.cast(ff).resolve();
    runner.cast(meditate).resolve();
    assert_eq!(runner.state().turns_to_skip[0], 1, "Meditate arms the skip");
    assert_eq!(runner.state().delayed_triggers.len(), 1);

    // Through the skipped extra turn, P1's turn, and P0's next natural turn.
    pass_priority_until(&mut runner, |r| r.state().turn_number > cast_turn + 2);
    assert_still_in_game(&runner, "after the granted turn was skipped");
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "the delayed loss bound to the skipped turn must be retired"
    );
    assert_eq!(runner.state().turns_to_skip[0], 0);
}

/// CR 614.10 + CR 614.10a: Trouble in Pairs's replacement (the `BeginTurn`
/// replacement path) skips the granted extra turn the same way.
#[test]
fn final_fortune_loss_is_retired_when_the_granted_turn_is_skipped_by_a_replacement() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    library(&mut scenario);
    scenario.add_enchantment_from_oracle(P1, "Trouble in Pairs", TROUBLE_IN_PAIRS);
    let ff = scenario
        .add_spell_to_hand_from_oracle(P0, "Final Fortune", true, FINAL_FORTUNE)
        .id();
    scenario.with_mana_pool(P0, floating_mana(4, ManaType::Red));
    let mut runner = scenario.build();
    let cast_turn = runner.state().turn_number;
    runner.cast(ff).resolve();
    assert_eq!(runner.state().delayed_triggers.len(), 1);

    pass_priority_until(&mut runner, |r| r.state().turn_number > cast_turn + 2);
    assert_still_in_game(&runner, "after the replacement skipped the granted turn");
    assert!(runner.state().delayed_triggers.is_empty());
}

/// CR 500.7: a later grant is taken first (LIFO). Final Fortune's loss belongs
/// to ITS turn, which is taken second, not to the Time Warp turn taken first.
#[test]
fn final_fortune_loss_fires_at_its_own_turn_when_a_later_grant_is_taken_first() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    library(&mut scenario);
    let ff = scenario
        .add_spell_to_hand_from_oracle(P0, "Final Fortune", true, FINAL_FORTUNE)
        .id();
    let warp = scenario
        .add_spell_to_hand_from_oracle(P0, "Time Warp", true, TIME_WARP)
        .id();
    scenario.with_mana_pool(P0, floating_mana(8, ManaType::Red));
    let mut runner = scenario.build();
    let cast_turn = runner.state().turn_number;
    runner.cast(ff).resolve();
    runner.cast(warp).target_player(P0).resolve();
    assert_eq!(runner.state().extra_turns.len(), 2);

    // Turn T+1 is the Time Warp turn: its end step must not lose the game.
    pass_priority_until(&mut runner, |r| r.state().turn_number > cast_turn + 1);
    assert_still_in_game(&runner, "after the later (Time Warp) grant's turn");
    // Turn T+2 is Final Fortune's own turn.
    pass_priority_until(&mut runner, |r| r.state().turn_number > cast_turn + 2);
    assert_caster_lost(&runner);
}

/// CR 500.7 + CR 603.7a: "that turn" is the granted turn whoever takes it. The
/// caster ("you") loses at the end step of the opponent's extra turn.
#[test]
fn delayed_loss_fires_at_a_turn_granted_to_another_player() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    library(&mut scenario);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Granting Fortune", true, GRANT_TO_OPPONENT_THEN_LOSE)
        .id();
    scenario.with_mana_pool(P0, floating_mana(4, ManaType::Red));
    let mut runner = scenario.build();
    let cast_turn = runner.state().turn_number;
    runner.cast(spell).target_player(P1).resolve();
    assert_eq!(runner.state().delayed_triggers.len(), 1);

    pass_priority_until(&mut runner, |r| r.state().turn_number > cast_turn);
    assert_still_in_game(&runner, "after the turn it was cast in");
    assert_eq!(runner.state().active_player, P1, "the extra turn is P1's");
    pass_priority_until(&mut runner, |r| r.state().turn_number > cast_turn + 1);
    assert_caster_lost(&runner);
}

/// CR 608.2c: two grants leave "that turn" without a single antecedent, so no
/// delayed trigger is created (the same rule as one added phase for "that
/// combat").
#[test]
fn no_delayed_loss_is_created_when_the_antecedent_grants_two_turns() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    library(&mut scenario);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Double Fortune", true, TWO_GRANTS_THEN_LOSE)
        .id();
    scenario.with_mana_pool(P0, floating_mana(4, ManaType::Red));
    let mut runner = scenario.build();
    runner.cast(spell).resolve();
    assert_eq!(runner.state().extra_turns.len(), 2);
    assert!(runner.state().delayed_triggers.is_empty());
}

/// Oracle en-Vec's "that turn" is "that player's next turn", not a granted
/// extra turn: the extra-turn reading must not apply to it.
#[test]
fn that_turns_end_step_without_a_granted_turn_is_not_rewritten_to_the_extra_turn_rule() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let en_vec = scenario
        .add_creature_from_oracle(P0, "Oracle en-Vec", 1, 1, ORACLE_EN_VEC)
        .id();
    let runner = scenario.build();
    let object = &runner.state().objects[&en_vec];
    let mut delayed = 0;
    let mut unsupported_that_turn = 0;
    for ability in object.abilities.iter() {
        let mut next = Some(ability);
        while let Some(def) = next {
            match &*def.effect {
                Effect::CreateDelayedTrigger { .. } => delayed += 1,
                Effect::Unimplemented { description, .. }
                    if description.as_deref().is_some_and(|text| {
                        text.starts_with("At the beginning of that turn's end step")
                    }) =>
                {
                    unsupported_that_turn += 1
                }
                _ => {}
            }
            next = def.sub_ability.as_deref();
        }
    }
    // The referent ("that player's next turn") is not proven, so the clause is
    // kept as an honest unsupported form instead of a guessed delayed trigger.
    assert_eq!(
        delayed, 0,
        "no delayed trigger may be guessed for \"that turn\""
    );
    assert_eq!(unsupported_that_turn, 1);
}

/// Round-trips `state` through the persistence boundary (the trusted capture
/// and the raw form), the way a saved game is resumed.
fn save_and_restore(state: &GameState, trusted: bool) -> GameState {
    let persisted = if trusted {
        PersistedGameState::capture(state.clone())
    } else {
        PersistedGameState::Raw(Box::new(state.clone()))
    };
    serde_json::from_value::<PersistedGameState>(
        serde_json::to_value(&persisted).expect("the state serializes"),
    )
    .expect("the save decodes")
    .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
    .expect("the save restores")
    .finalize_after_rehydration(|_| Ok(()))
    .expect("the save rehydrates")
}

/// CR 500.7 + CR 603.7a: a game saved with Final Fortune's turn still queued
/// resumes with the loss bound to that same turn. A grant made after the
/// restore is minted a fresh identity (the allocator is persisted), so it does
/// not take over the bound turn's loss: both later grants are taken first
/// (LIFO) and survive their end steps.
#[test]
fn restored_game_keeps_the_loss_on_its_queued_turn_and_mints_fresh_ids() {
    for trusted in [false, true] {
        let mut scenario = GameScenario::new_n_player(2, 42);
        scenario.at_phase(Phase::PreCombatMain);
        library(&mut scenario);
        let ff = scenario
            .add_spell_to_hand_from_oracle(P0, "Final Fortune", true, FINAL_FORTUNE)
            .id();
        let warp_a = scenario
            .add_spell_to_hand_from_oracle(P0, "Time Warp", true, TIME_WARP)
            .id();
        let warp_b = scenario
            .add_spell_to_hand_from_oracle(P0, "Time Warp", true, TIME_WARP)
            .id();
        scenario.with_mana_pool(P0, floating_mana(8, ManaType::Red));
        let mut runner = scenario.build();
        let cast_turn = runner.state().turn_number;
        runner.cast(ff).resolve();
        runner.cast(warp_a).target_player(P0).resolve();

        let mut runner = GameRunner::from_state(save_and_restore(runner.state(), trusted));
        runner.cast(warp_b).target_player(P0).resolve();
        let ids: Vec<_> = runner.state().extra_turns.iter().map(|t| t.id).collect();
        assert_eq!(ids.len(), 3);
        assert!(
            ids.iter().collect::<std::collections::HashSet<_>>().len() == 3,
            "every granted turn has a distinct identity after the restore: {ids:?}"
        );

        pass_priority_until(&mut runner, |r| r.state().turn_number > cast_turn + 1);
        assert_still_in_game(&runner, "after the first later grant's turn");
        pass_priority_until(&mut runner, |r| r.state().turn_number > cast_turn + 2);
        assert_still_in_game(&runner, "after the second later grant's turn");
        pass_priority_until(&mut runner, |r| r.state().turn_number > cast_turn + 3);
        assert_caster_lost(&runner);
    }
}

/// CR 500.7 + CR 603.7a: a game saved while the granted turn is in progress
/// resumes with that turn still identified, so its loss fires at its end step.
#[test]
fn restored_mid_turn_game_still_loses_at_the_granted_turns_end_step() {
    for trusted in [false, true] {
        let (mut runner, cast_turn) = cast_final_fortune(P0);
        pass_priority_until(&mut runner, |r| in_turn_after_its_end_step(r, cast_turn));
        assert!(
            serde_json::to_value(runner.state()).expect("serializes")["current_extra_turn"]
                .is_number(),
            "reach: the granted turn is in progress"
        );

        let mut runner = GameRunner::from_state(save_and_restore(runner.state(), trusted));
        assert_still_in_game(&runner, "after the restore");
        pass_priority_until(&mut runner, |r| r.state().turn_number > cast_turn + 1);
        assert_caster_lost(&runner);
    }
}
