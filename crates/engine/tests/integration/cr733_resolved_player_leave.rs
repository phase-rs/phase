//! CR733 P2 coverage for the player-leave family.
//!
//! P1 defined `RulesExecutionNodeKind::PlayerLeave` and the matching node ref,
//! but nothing in the engine ever produced one — the CR 800.4 sweep marked
//! `is_eliminated` and appended to `eliminated_players` with raw writes, and
//! every mutation it performed downstream (owned-object exiles, control-effect
//! reversions) was attributed to whatever proposal happened to be live when the
//! state-based action fired.
//!
//! Two things are asserted here, and they are different claims:
//!
//! 1. The departure itself is journaled as one exact command. The two writes
//!    always move together, so they are one command rather than two.
//! 2. The departure opens its OWN execution node. That is what makes the sweep
//!    identifiable as a single causal unit on replay, instead of commands
//!    scattered under an unrelated cause.
//!
//! The test drives the REAL pipeline: a player is taken to 0 life, and the
//! CR 704.5a state-based action eliminates them.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::phase::Phase;
use engine::types::resolved_commands::{ResolvedRulesCommand, RulesExecutionNodeRef};

#[test]
fn losing_the_game_journals_an_exact_player_leave_under_its_own_node() {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let bolt = scenario.add_bolt_to_hand(P0);
    let mut runner = scenario.build();

    // Three players so the elimination does not immediately end the game — the
    // leave sweep must be observable while play continues.
    runner
        .state_mut()
        .players
        .iter_mut()
        .find(|player| player.id == P1)
        .expect("the victim is in the game")
        .life = 3;

    let journal_start = runner.state().resolved_rules_journal.entries().len();

    // CR 704.5a: a player with 0 or less life loses the game. A real burn spell
    // resolving at the victim takes them to 0 and makes the state-based action
    // fire through the production pipeline, rather than poking `is_eliminated`.
    runner.cast(bolt).target_player(P1).resolve();

    // CR 704.5a reach guard: the victim actually left. Without it the journal
    // assertions below could pass vacuously.
    let state = runner.state();
    assert!(
        state
            .players
            .iter()
            .find(|player| player.id == P1)
            .expect("the player record survives elimination")
            .is_eliminated,
        "CR 704.5a: a player at 0 life loses the game"
    );
    assert!(
        state.eliminated_players.contains(&P1),
        "the departure also appends to the eliminated list"
    );
    assert!(
        !state
            .players
            .iter()
            .find(|player| player.id == P0)
            .expect("the survivor is in the game")
            .is_eliminated,
        "only the player who lost leaves"
    );

    // The discriminating assertion: the departure is journaled as an exact
    // resolved command. Two raw field writes record nothing here.
    let leaves: Vec<_> = state
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(journal_start)
        .filter_map(|entry| entry.command.clone())
        .filter_map(|command| match command {
            ResolvedRulesCommand::PlayerLeave(command) if command.player == P1 => Some(command),
            _ => None,
        })
        .collect();
    assert_eq!(
        leaves.len(),
        1,
        "the elimination authority must journal exactly one resolved departure"
    );

    // The second claim: the departure runs under a PlayerLeave node, not under an
    // ambient proposal. This is what lets a replay recognize the CR 800.4 sweep as
    // one causal unit.
    assert!(
        matches!(leaves[0].cause, RulesExecutionNodeRef::PlayerLeave(_)),
        "CR 800.4: the departure opens its own execution node, got {:?}",
        leaves[0].cause
    );

    // Replay-exactness: the recorded departure reinstalls both writes, and is not
    // idempotent — a second application is a typed invariant failure rather than a
    // silent re-elimination.
    let mut replay = runner.state().clone();
    replay
        .players
        .iter_mut()
        .find(|player| player.id == P1)
        .expect("the player record survives elimination")
        .is_eliminated = false;
    replay.eliminated_players.retain(|player| *player != P1);

    engine::game::elimination::apply_resolved_player_leave(&mut replay, &leaves[0])
        .expect("the recorded departure must replay");
    assert!(
        replay
            .players
            .iter()
            .find(|player| player.id == P1)
            .expect("the player record survives elimination")
            .is_eliminated,
        "replay installs the recorded departure"
    );
    assert!(
        replay.eliminated_players.contains(&P1),
        "replay installs the eliminated-list append"
    );
    assert!(
        engine::game::elimination::apply_resolved_player_leave(&mut replay, &leaves[0]).is_err(),
        "a departure is not idempotent: re-applying it is a typed invariant failure"
    );
}
