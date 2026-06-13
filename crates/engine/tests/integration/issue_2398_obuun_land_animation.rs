//! Issue #2398 — Obuun, Mul Daya Ancestor: begin-combat animation must set the
//! target land's P/T to Obuun's power (X/X), not 0/0.
//!
//! https://github.com/phase-rs/phase/issues/2398

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn power_toughness(runner: &GameRunner, id: ObjectId) -> (i32, i32) {
    let obj = runner.state().objects.get(&id).expect("object present");
    (obj.power.unwrap_or(0), obj.toughness.unwrap_or(0))
}

const OBUUN_ORACLE: &str = "At the beginning of combat on your turn, up to one target land you control becomes an X/X Elemental creature with trample and haste until end of turn, where X is Obuun's power. It's still a land.";

#[test]
fn issue_2398_obuun_animates_land_to_source_power() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Obuun, Mul Daya Ancestor", 3, 3, OBUUN_ORACLE);
    let forest = scenario.add_basic_land(P0, ManaColor::Green);

    let mut runner = scenario.build();
    runner.pass_both_players();
    assert_eq!(runner.state().phase, Phase::BeginCombat);

    if matches!(
        runner.state().waiting_for,
        engine::types::game_state::WaitingFor::TriggerTargetSelection { .. }
    ) {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(forest)],
            })
            .expect("select land target for Obuun");
    }
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&forest].zone,
        Zone::Battlefield,
        "animated land must survive as a creature (Obuun's power is 3, not 0/0)"
    );
    assert_eq!(
        power_toughness(&runner, forest),
        (3, 3),
        "land must become X/X where X is Obuun's power"
    );
    let land = &runner.state().objects[&forest];
    assert!(land.has_keyword(&Keyword::Trample));
    assert!(land.has_keyword(&Keyword::Haste));
}
