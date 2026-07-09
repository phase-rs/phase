//! Runtime coverage for GitHub issue #5341: Dreadhorde Invasion's upkeep
//! trigger must both lose 1 life AND amass Zombies 1.
//!
//! Coverage previously claimed support for the compound clause while the
//! bare-" and " splitter omitted `amass`, so LoseLife swallowed the remainder
//! and the Army never arrived. Parser + resolver pins here cover the class
//! ("lose N life and amass [subtype] N"), not only this card.
//!
//! CR ANCHORS (verified against docs/MagicCompRules.txt):
//!   * CR 701.47a — Amass [subtype] N creates an Army token / puts counters /
//!     adds the subtype.
//!   * CR 603.1 — Triggered ability places on the stack and resolves its
//!     effect.
//!   * CR 608.2c — Instructions are followed in written order (LoseLife then
//!     Amass).
//!
//! CARD TEXT (verified from client/public/card-data.json):
//! "At the beginning of your upkeep, you lose 1 life and amass Zombies 1.
//! (Put a +1/+1 counter on an Army you control. It's also a Zombie. If you
//! don't control an Army, create a 0/0 black Zombie Army creature token
//! first.)"

use engine::game::scenario::{GameScenario, P0};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::phase::Phase;

const DREADHORDE_INVASION_ORACLE: &str = "At the beginning of your upkeep, you lose 1 life and amass Zombies 1. (Put a +1/+1 counter on an Army you control. It's also a Zombie. If you don't control an Army, create a 0/0 black Zombie Army creature token first.)\nWhenever a Zombie token you control with power 6 or greater attacks, it gains lifelink until end of turn.";

#[test]
fn dreadhorde_invasion_upkeep_amasses_zombie_army() {
    let mut scenario = GameScenario::new();
    // Start at Untap so advancing into Upkeep fires the synthesized phase
    // trigger (CR 503.1a + CR 603.2).
    scenario.at_phase(Phase::Untap);
    scenario.with_life(P0, 20);
    scenario
        .add_creature(P0, "Dreadhorde Invasion", 0, 0)
        .as_enchantment()
        .from_oracle_text(DREADHORDE_INVASION_ORACLE);

    let mut runner = scenario.build();
    runner.advance_to_upkeep();
    // Resolve the upkeep trigger (LoseLife → Amass).
    runner.resolve_top();

    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        19,
        "controller must lose 1 life from the upkeep trigger"
    );

    let armies: Vec<_> = runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|obj| {
            obj.controller == P0
                && obj.card_types.core_types.contains(&CoreType::Creature)
                && obj.card_types.subtypes.iter().any(|s| s == "Army")
        })
        .collect();
    assert_eq!(
        armies.len(),
        1,
        "amass with no existing Army must create a Zombie Army token"
    );
    let army = armies[0];
    assert!(army.is_token, "amassed Army must be a token");
    assert!(
        army.card_types.subtypes.iter().any(|s| s == "Zombie"),
        "amassed Army must be a Zombie subtype"
    );
    assert_eq!(
        army.counters.get(&CounterType::Plus1Plus1).copied(),
        Some(1),
        "amass Zombies 1 must put one +1/+1 counter on the Army"
    );
    assert_eq!(army.power, Some(1));
    assert_eq!(army.toughness, Some(1));
}
