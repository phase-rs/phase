//! Regression for issue #4963: Charismatic Conqueror's optional tap belongs to
//! the player who controlled the permanent as it entered.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const CHARISMATIC_CONQUEROR_ORACLE: &str = "Whenever an artifact or creature an opponent controls enters untapped, they may tap that permanent. If they don't, you create a 1/1 white Vampire creature token with lifelink.";
const CONTROLLER_MAY_TAP_ORACLE: &str = "Whenever a creature enters, you may tap that permanent.";

fn scenario_with_optional_tapper(oracle: &str) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Charismatic Conqueror", 2, 2, oracle)
        .id();
    let entrant = scenario
        .add_creature_to_hand_from_oracle(P1, "Untapped Entrant", 1, 1, "")
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    (runner, entrant)
}

fn resolve_entrant_to_optional(runner: &mut GameRunner, entrant: ObjectId) {
    let card_id = runner.state().objects[&entrant].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: entrant,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast the zero-cost entrant through the production pipeline");
    runner.advance_until_stack_empty();
}

/// CR 603.2 + CR 603.6a + CR 608.2d: P1's untapped creature ETB triggers
/// Conqueror, but P1—not the Conqueror controller—makes the optional choice.
/// Accepting taps the entrant and suppresses the decline token branch.
#[test]
fn charismatic_conqueror_accept_prompts_entering_controller_and_taps_entrant() {
    let (mut runner, entrant) = scenario_with_optional_tapper(CHARISMATIC_CONQUEROR_ORACLE);
    resolve_entrant_to_optional(&mut runner, entrant);

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { player: P1, .. }
    ));
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("P1 accepts the optional tap");
    runner.advance_until_stack_empty();

    assert!(
        runner.state().objects[&entrant].tapped,
        "P1 accepted the tap"
    );
    assert!(
        !runner.state().objects.values().any(|object| {
            object.is_token && object.name == "Vampire" && object.controller == P0
        }),
        "accepting must not execute the 'If they don't' Vampire branch"
    );
}

/// CR 608.2c + CR 109.5: declining leaves P1's entrant untapped and creates
/// the Vampire for P0, the controller of Conqueror when its trigger fired.
#[test]
fn charismatic_conqueror_decline_keeps_entrant_untapped_and_creates_p0_vampire() {
    let (mut runner, entrant) = scenario_with_optional_tapper(CHARISMATIC_CONQUEROR_ORACLE);
    resolve_entrant_to_optional(&mut runner, entrant);

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { player: P1, .. }
    ));
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("P1 declines the optional tap");
    runner.advance_until_stack_empty();

    assert!(
        !runner.state().objects[&entrant].tapped,
        "declining must leave P1's entrant untapped"
    );
    let vampires: Vec<_> = runner
        .state()
        .objects
        .values()
        .filter(|object| object.is_token && object.name == "Vampire")
        .collect();
    assert_eq!(vampires.len(), 1, "declining creates one Vampire token");
    assert_eq!(vampires[0].controller, P0);
    assert!(vampires[0].keywords.contains(&Keyword::Lifelink));
}

/// CR 608.2d: A controller's "you may tap that permanent" uses the same
/// event-object referent but must prompt P0, proving the `they may` actor stamp
/// is not inferred from the lowered tap effect.
#[test]
fn controller_may_tap_that_permanent_prompts_ability_controller() {
    let (mut runner, entrant) = scenario_with_optional_tapper(CONTROLLER_MAY_TAP_ORACLE);
    resolve_entrant_to_optional(&mut runner, entrant);

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { player: P0, .. }
    ));
}
