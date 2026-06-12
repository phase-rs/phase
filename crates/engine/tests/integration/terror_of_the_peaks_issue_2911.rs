//! Issue #2911 — Terror of the Peaks imposes an additional 3 life cost on
//! opponent spells that target it (ward-like, but not the Ward keyword).

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::game_state::WaitingFor;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

const TERROR_OF_THE_PEAKS: &str = "Flying\nSpells your opponents cast that target this creature cost an additional 3 life to cast.\nWhenever another creature you control enters, this creature deals damage equal to that creature's power to any target.";

fn floating_mana(generic: usize, red: usize) -> Vec<ManaUnit> {
    let mut pool = Vec::new();
    for _ in 0..generic {
        pool.push(ManaUnit::new(
            ManaType::Colorless,
            engine::types::identifiers::ObjectId(0),
            false,
            vec![],
        ));
    }
    for _ in 0..red {
        pool.push(ManaUnit::new(
            ManaType::Red,
            engine::types::identifiers::ObjectId(0),
            false,
            vec![],
        ));
    }
    pool
}

#[test]
fn terror_of_the_peaks_charges_three_life_when_targeted_by_opponent_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let terror = scenario
        .add_creature_from_oracle(P0, "Terror of the Peaks", 5, 4, TERROR_OF_THE_PEAKS)
        .id();
    let bolt = scenario.add_bolt_to_hand(P1);
    scenario.with_mana_pool(P1, floating_mana(0, 1));

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }

    let outcome = runner.cast(bolt).target_object(terror).resolve();

    outcome.assert_life_delta(P1, -3);
    outcome.assert_life_delta(P0, 0);
}

#[test]
fn terror_of_the_peaks_does_not_tax_spells_not_targeting_it() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let _terror = scenario
        .add_creature_from_oracle(P0, "Terror of the Peaks", 5, 4, TERROR_OF_THE_PEAKS)
        .id();
    let bolt = scenario.add_bolt_to_hand(P1);
    scenario.with_mana_pool(P1, floating_mana(0, 1));

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }

    let outcome = runner.cast(bolt).target_player(P0).resolve();

    outcome.assert_life_delta(P1, 0);
    outcome.assert_life_delta(P0, -3);
}
