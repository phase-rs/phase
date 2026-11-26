//! Mog, Moogle Warrior — probe.
use engine::game::scenario::{GameScenario, P0};
use engine::types::phase::Phase;

const MOG: &str = "Lifelink\nDance — At the beginning of your end step, each player may discard a card. Each player who discarded a card this way draws a card. If a creature card was discarded this way, you create a 1/2 white Moogle creature token with lifelink. Then if a noncreature card was discarded this way, put a +1/+1 counter on each Moogle you control.";

#[test]
fn probe_mog_parse() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mog = scenario
        .add_creature(P0, "Mog, Moogle Warrior", 1, 2)
        .from_oracle_text(MOG)
        .id();
    let runner = scenario.build();
    let obj = runner.state().objects.get(&mog).unwrap();
    println!("{:#?}", obj.trigger_definitions);
}
