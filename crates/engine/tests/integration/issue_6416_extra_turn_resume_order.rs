//! Issue #6416 — Extra turn taken out of sequence must resume the original
//! turn order after the *specified* turn (CR 500.7), not after the beneficiary.
//!
//! Reported sequence (4p A,B,C,D): during C's turn A takes an extra turn via
//! Nexus of Fate (miracle / Molecule Man). Expected after Extra A: D, then A.
//! Bug: Extra A was followed by B (as if A had just finished a natural turn).

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

const NEXUS_OF_FATE: &str = "Take an extra turn after this one.\n\
    If Nexus of Fate would be put into a graveyard from anywhere, reveal Nexus of Fate and \
    shuffle it into its owner's library instead.";

fn floating_mana(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

fn grant_priority(runner: &mut engine::game::scenario::GameRunner, player: PlayerId) {
    let state = runner.state_mut();
    state.priority_player = player;
    state.waiting_for = WaitingFor::Priority { player };
}

fn pass_until_active_changes(
    runner: &mut engine::game::scenario::GameRunner,
    from: PlayerId,
) -> bool {
    for _ in 0..128 {
        if runner.state().active_player != from {
            return true;
        }
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            return false;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority pass while ending turn");
    }
    false
}

/// 4-player FFA: during C's turn A casts Nexus → Extra A → resume with D.
#[test]
fn extra_turn_out_of_sequence_resumes_after_specified_turn() {
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let nexus = scenario
        .add_spell_to_hand_from_oracle(P0, "Nexus of Fate", true, NEXUS_OF_FATE)
        .id();
    scenario.with_mana_pool(P0, floating_mana(7, ManaType::Blue));

    let mut runner = scenario.build();
    // C's turn (P2). Natural next after C is D (P3).
    runner.state_mut().active_player = P2;
    assert_eq!(
        engine::game::players::next_player(runner.state(), P2),
        P3,
        "four-player setup must make P3 the natural next turn after P2"
    );
    grant_priority(&mut runner, P0);

    runner.cast(nexus).resolve();

    // Reach guard: queue is anchored to C, beneficiary A.
    assert!(
        runner
            .state()
            .extra_turns
            .iter()
            .any(|et| et.player == P0 && et.anchor == P2),
        "Nexus must enqueue ExtraTurn {{ player: A, anchor: C }}, got {:?}",
        runner.state().extra_turns
    );

    assert!(
        pass_until_active_changes(&mut runner, P2),
        "must leave C's turn"
    );
    assert_eq!(
        runner.state().active_player,
        P0,
        "reach guard: A must take the granted extra turn after C ends"
    );
    assert!(
        runner.state().extra_turns.is_empty(),
        "extra turn must be consumed when A becomes active"
    );

    assert!(
        pass_until_active_changes(&mut runner, P0),
        "must leave A's extra turn"
    );
    assert_eq!(
        runner.state().active_player,
        P3,
        "CR 500.7: after OOS extra turn, resume with D (next after specified turn C), not B"
    );
    assert!(
        runner.state().extra_turn_sequence_anchor.is_none(),
        "resume latch must clear after natural order resumes"
    );
}

/// Sibling: in-sequence Nexus on own turn still advances to the next seat after
/// the extra is consumed. (Beneficiary == active, so both natural and extra
/// turns keep `active_player == A` until the sequence resumes.)
#[test]
fn extra_turn_on_own_turn_resumes_natural_next() {
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let nexus = scenario
        .add_spell_to_hand_from_oracle(P0, "Nexus of Fate", true, NEXUS_OF_FATE)
        .id();
    scenario.with_mana_pool(P0, floating_mana(7, ManaType::Blue));

    let mut runner = scenario.build();
    assert_eq!(runner.state().active_player, P0);
    runner.cast(nexus).resolve();

    assert!(
        runner
            .state()
            .extra_turns
            .iter()
            .any(|et| et.player == P0 && et.anchor == P0),
        "in-sequence Nexus must enqueue ExtraTurn {{ player: A, anchor: A }}, got {:?}",
        runner.state().extra_turns
    );
    let turns_taken_at_cast = runner.state().players[0].turns_taken;

    // Pass through A's remaining natural turn and the following extra turn.
    assert!(
        pass_until_active_changes(&mut runner, P0),
        "must leave A's natural+extra turn sequence"
    );
    assert_eq!(
        runner.state().active_player,
        P1,
        "in-sequence extra: resume after A → B"
    );
    assert!(
        runner.state().extra_turns.is_empty(),
        "extra turn must be consumed"
    );
    assert!(
        runner.state().players[0].turns_taken > turns_taken_at_cast,
        "reach guard: A must have begun the granted extra turn (turns_taken increased)"
    );
}

/// CR 500.7: nested extras through the production ExtraTurn resolver.
/// During C, A casts Nexus; during A's extra, B casts Nexus. Order must be
/// C → Extra A → Extra B → D (outer C anchor latched, not overwritten by A).
#[test]
fn extra_turn_nested_via_resolver_preserves_outer_anchor() {
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let nexus_a = scenario
        .add_spell_to_hand_from_oracle(P0, "Nexus of Fate", true, NEXUS_OF_FATE)
        .id();
    let nexus_b = scenario
        .add_spell_to_hand_from_oracle(P1, "Nexus of Fate", true, NEXUS_OF_FATE)
        .id();
    scenario.with_mana_pool(P0, floating_mana(7, ManaType::Blue));
    scenario.with_mana_pool(P1, floating_mana(7, ManaType::Blue));

    let mut runner = scenario.build();
    runner.state_mut().active_player = P2;
    grant_priority(&mut runner, P0);

    runner.cast(nexus_a).resolve();
    assert!(
        runner
            .state()
            .extra_turns
            .iter()
            .any(|et| et.player == P0 && et.anchor == P2),
        "A's Nexus during C must enqueue ExtraTurn {{ player: A, anchor: C }}, got {:?}",
        runner.state().extra_turns
    );

    assert!(
        pass_until_active_changes(&mut runner, P2),
        "must leave C's turn"
    );
    assert_eq!(runner.state().active_player, P0, "Extra A after C");
    assert_eq!(
        runner.state().extra_turn_sequence_anchor,
        Some(P2),
        "first extra must latch specified turn C"
    );

    // During A's extra: B casts Nexus via the production ExtraTurn resolver.
    // Resolver anchors to active_player (A); latch must keep outer C.
    grant_priority(&mut runner, P1);
    runner.cast(nexus_b).resolve();
    assert!(
        runner
            .state()
            .extra_turns
            .iter()
            .any(|et| et.player == P1 && et.anchor == P0),
        "B's Nexus during A must enqueue ExtraTurn {{ player: B, anchor: A }}, got {:?}",
        runner.state().extra_turns
    );

    assert!(
        pass_until_active_changes(&mut runner, P0),
        "must leave A's extra turn"
    );
    assert_eq!(runner.state().active_player, P1, "Extra B after A");
    assert_eq!(
        runner.state().extra_turn_sequence_anchor,
        Some(P2),
        "nested grant must not overwrite outer C latch"
    );

    assert!(
        pass_until_active_changes(&mut runner, P1),
        "must leave B's extra turn"
    );
    assert_eq!(
        runner.state().active_player,
        P3,
        "after nested extras drain, resume after original specified turn C → D"
    );
    assert!(
        runner.state().extra_turn_sequence_anchor.is_none(),
        "resume latch must clear after natural order resumes"
    );
}
