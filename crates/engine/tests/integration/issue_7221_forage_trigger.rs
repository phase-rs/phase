use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const CORPSEBERRY_CULTIVATOR: &str = "At the beginning of combat on your turn, you may forage. \
(Exile three cards from your graveyard or sacrifice a Food.)\n\
Whenever you forage, put a +1/+1 counter on this creature.";

fn p1p1(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn resolve_until_counter(runner: &mut GameRunner, cultivator: ObjectId) {
    for _ in 0..200 {
        if p1p1(runner, cultivator) > 0 {
            return;
        }
        match &runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept the forage trigger");
            }
            WaitingFor::EffectZoneChoice { cards, count, .. } => {
                let cards = cards.iter().take(*count).copied().collect();
                runner
                    .act(GameAction::SelectCards { cards })
                    .expect("exile three cards to forage");
            }
            _ => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("advance the game");
            }
        }
    }
    panic!("forage trigger did not resolve");
}

#[test]
fn foraging_from_graveyard_triggers_corpseberry_cultivator() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cultivator = scenario
        .add_creature_from_oracle(P0, "Corpseberry Cultivator", 2, 3, CORPSEBERRY_CULTIVATOR)
        .id();
    for _ in 0..3 {
        scenario.add_creature_to_graveyard(P0, "Fodder", 1, 1);
    }

    let mut runner = scenario.build();
    resolve_until_counter(&mut runner, cultivator);

    assert_eq!(p1p1(&runner, cultivator), 1);
}
