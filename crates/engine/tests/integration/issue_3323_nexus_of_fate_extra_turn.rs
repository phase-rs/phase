//! Issue #3323 — Nexus of Fate must grant its caster an extra turn even when
//! cast on an opponent's turn.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P2: PlayerId = PlayerId(2);

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

#[test]
fn nexus_of_fate_grants_caster_extra_turn_on_opponents_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let nexus = scenario
        .add_spell_to_hand_from_oracle(P0, "Nexus of Fate", true, NEXUS_OF_FATE)
        .id();
    scenario.with_mana_pool(P0, floating_mana(7, ManaType::Blue));

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    grant_priority(&mut runner, P0);

    runner.cast(nexus).resolve();

    assert!(
        runner.state().extra_turns.contains(&P0),
        "Nexus caster must receive an extra turn when cast on the opponent's turn, got {:?}",
        runner.state().extra_turns
    );
    assert!(
        !runner.state().extra_turns.contains(&P1),
        "active player must not receive the extra turn, got {:?}",
        runner.state().extra_turns
    );
}

#[test]
fn nexus_of_fate_grants_caster_extra_turn_on_own_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let nexus = scenario
        .add_spell_to_hand_from_oracle(P0, "Nexus of Fate", true, NEXUS_OF_FATE)
        .id();
    scenario.with_mana_pool(P0, floating_mana(7, ManaType::Blue));

    let mut runner = scenario.build();
    runner.cast(nexus).resolve();

    assert_eq!(
        runner.state().extra_turns,
        vec![P0],
        "Nexus caster must receive an extra turn on their own turn"
    );
}

#[test]
fn nexus_of_fate_extra_turn_is_taken_after_opponents_turn_ends() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let nexus = scenario
        .add_spell_to_hand_from_oracle(P0, "Nexus of Fate", true, NEXUS_OF_FATE)
        .id();
    scenario.with_mana_pool(P0, floating_mana(7, ManaType::Blue));

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    assert_eq!(
        engine::game::players::next_player(runner.state(), P1),
        P2,
        "three-player setup must make P2 the natural next turn after P1"
    );
    grant_priority(&mut runner, P0);
    runner.cast(nexus).resolve();

    for _ in 0..128 {
        if runner.state().active_player != P1 {
            break;
        }
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority pass while ending opponent's turn");
    }

    assert_eq!(
        runner.state().active_player,
        P0,
        "Nexus caster must take the granted extra turn after the opponent's turn ends"
    );
    assert!(
        runner.state().extra_turns.is_empty(),
        "consumed extra turn must be removed from the queue"
    );
}
