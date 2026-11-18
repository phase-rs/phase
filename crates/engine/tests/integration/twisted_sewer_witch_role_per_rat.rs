//! Runtime regression for Twisted Sewer-Witch's per-Rat Role creation.
//!
//! CR 608.2c: "Then for each Rat you control, create a Wicked Role token
//! attached to that Rat" is evaluated after the Rat token exists, and each
//! iteration's "that Rat" is the member being iterated. CR 701.7a + CR 303.7:
//! each Role token is created attached to its own Rat. A single Role on the
//! wrong object (or on a parent token) is the defect this test discriminates.

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameScenario, P0};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Verbatim printed Oracle text of Twisted Sewer-Witch.
const TWISTED_SEWER_WITCH: &str = "When this creature enters, create a 1/1 black Rat creature token with \"This creature can't block.\" Then for each Rat you control, create a Wicked Role token attached to that Rat. (If you control another Role on it, put that one into the graveyard. Enchanted creature gets +1/+1. When this token is put into a graveyard, each opponent loses 1 life.)";

/// Verbatim printed Oracle text of Rat Colony.
const RAT_COLONY: &str = "This creature gets +1/+0 for each other Rat you control.\nA deck can have any number of cards named Rat Colony.";

#[test]
fn twisted_sewer_witch_attaches_one_wicked_role_to_each_rat() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let colony = scenario
        .add_creature_from_oracle(P0, "Rat Colony", 2, 1, RAT_COLONY)
        .with_subtypes(vec!["Rat"])
        .id();
    let witch = scenario
        .add_creature_to_hand_from_oracle(P0, "Twisted Sewer-Witch", 3, 4, TWISTED_SEWER_WITCH)
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(witch).free_cast().resolve();

    // Reach-guard: the Witch resolved and its enters trigger created the Rat token.
    outcome
        .find_object(|o| o.name == "Twisted Sewer-Witch" && o.zone == Zone::Battlefield)
        .expect("the Witch resolves onto the battlefield");
    let state = outcome.state();
    let rat_token: ObjectId = *state
        .battlefield
        .iter()
        .find(|id| {
            let obj = &state.objects[*id];
            obj.is_token && obj.name == "Rat"
        })
        .expect("the enters trigger creates a Rat token");

    let roles: Vec<_> = state
        .battlefield
        .iter()
        .filter(|id| {
            let obj = &state.objects[*id];
            obj.is_token && obj.name == "Wicked Role"
        })
        .collect();
    assert_eq!(roles.len(), 2, "one Wicked Role per Rat you control");

    let attached: Vec<_> = roles
        .iter()
        .map(|id| state.objects[*id].attached_to)
        .collect();
    assert!(
        attached.contains(&Some(AttachTarget::Object(colony))),
        "a Role is attached to the pre-existing Rat: {attached:?}"
    );
    assert!(
        attached.contains(&Some(AttachTarget::Object(rat_token))),
        "a Role is attached to the new Rat token: {attached:?}"
    );
}
