//! Issue #3791 — "sacrifice ~ unless you pay its mana cost" (Pendrell Flux,
//! Disruption Aura, Pendrell Mists).
//!
//! The unless-cost is the ability source's OWN printed mana cost — dynamic,
//! because the granting Aura can be attached to any permanent. The parser maps
//! "you pay its mana cost" to `AbilityCost::Mana { cost: ManaCost::SelfManaCost }`
//! and the unless-pay resolver materializes it to the source's `mana_cost` at
//! resolution time (mirroring the `ManaDynamic` resolution path).
//!
//! Runtime discriminator: a source with a NON-ZERO mana cost must stop at a real
//! `WaitingFor::UnlessPayment` prompt (NOT the CR 118.5 zero-cost short-circuit
//! that would fire if `SelfManaCost` wrongly resolved to {0}); declining then
//! sacrifices the permanent.
//!
//! CR 118.12 (unless-pay alternative) + CR 202.1 (mana cost characteristic).

use engine::game::scenario::{GameScenario, P0};
use engine::game::triggers::process_triggers;
use engine::types::ability::AbilityCost;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const UPKEEP_OWN_COST: &str =
    "At the beginning of your upkeep, sacrifice this creature unless you pay its mana cost.";

#[test]
fn unless_pay_its_mana_cost_parses_to_self_mana_cost() {
    let parsed = engine::parser::oracle::parse_oracle_text(
        UPKEEP_OWN_COST,
        "Test Upkeep Tax",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let trigger = parsed.triggers.first().expect("upkeep trigger must parse");
    let unless = trigger
        .unless_pay
        .as_ref()
        .expect("trigger must carry an unless_pay modifier");
    assert_eq!(
        unless.cost,
        AbilityCost::Mana {
            cost: ManaCost::SelfManaCost
        },
        "\"unless you pay its mana cost\" must lower to Mana{{SelfManaCost}}, got {:?}",
        unless.cost
    );
}

#[test]
fn declining_own_mana_cost_upkeep_tax_sacrifices_the_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Upkeep);

    // A 2/2 with a NON-ZERO printed cost ({2}{G}) carrying the upkeep own-cost
    // tax. SelfManaCost must resolve to {2}{G} — a real prompt, not {0}.
    let creature = scenario
        .add_creature_from_oracle(P0, "Test Upkeep Tax", 2, 2, UPKEEP_OWN_COST)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 2,
        })
        .id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;
    process_triggers(
        runner.state_mut(),
        &[GameEvent::PhaseChanged {
            phase: Phase::Upkeep,
        }],
    );
    runner.advance_until_stack_empty();

    // CR 118.5 discriminator: a real non-zero cost surfaces an UnlessPayment
    // prompt. If SelfManaCost had wrongly resolved to {0}, the zero-cost
    // short-circuit would skip the prompt and the creature would survive.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "own-mana-cost upkeep tax must stop at an UnlessPayment prompt, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("decline the upkeep payment");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Graveyard,
        "declining the own-mana-cost upkeep tax must sacrifice the source"
    );
}

#[test]
fn costless_source_upkeep_tax_is_unpayable_and_sacrificed() {
    // CR 118.6 + CR 202.1b: an object with no mana cost (token, land, or any
    // other costless permanent) creates an UNPAYABLE cost for "pay its mana
    // cost". The punishment must fire unconditionally — the player is never
    // offered a payment prompt (attempting to pay an unpayable cost is an
    // illegal action), so a costless permanent cannot dodge the sacrifice.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Upkeep);

    // A 2/2 with NO printed mana cost (as a token would have). SelfManaCost
    // resolves to ManaCost::NoCost, which must route to the unpayable branch —
    // NOT the {0} "always payable" short-circuit that would let it survive.
    let creature = scenario
        .add_creature_from_oracle(P0, "Test Upkeep Tax", 2, 2, UPKEEP_OWN_COST)
        .with_mana_cost(ManaCost::NoCost)
        .id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;
    process_triggers(
        runner.state_mut(),
        &[GameEvent::PhaseChanged {
            phase: Phase::Upkeep,
        }],
    );
    runner.advance_until_stack_empty();

    // CR 118.6 discriminator: no payment prompt is offered for an unpayable
    // cost. If NoCost had wrongly been treated as {0}/payable, the player would
    // get an UnlessPayment prompt and could keep the creature.
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "a costless source's unpayable upkeep tax must NOT offer a payment prompt, got {:?}",
        runner.state().waiting_for
    );

    // The punishment resolves unconditionally: the costless source is sacrificed.
    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Graveyard,
        "an unpayable own-mana-cost upkeep tax must sacrifice the costless source"
    );
}
