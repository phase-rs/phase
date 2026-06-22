//! Repro: Shelob, Child of Ungoliant must create a Food copy token when a
//! creature dealt damage this turn by a Spider you control dies.
//!
//! Unlike the PR #3184 end-to-end test (which manually pushes a DamageRecord and
//! only asserts the trigger reaches the stack), this drives the REAL damage
//! pipeline (`deal_damage::resolve` populates `damage_dealt_this_turn`) and
//! asserts the observable result: a Food artifact copy token of the dead
//! creature exists under Shelob's controller.

use engine::game::effects::deal_damage;
use engine::game::sba::check_state_based_actions;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::triggers::process_triggers;
use engine::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef};
use engine::types::card_type::CoreType;
use engine::types::zones::Zone;

const SHELOB_DEATH_TRIGGER: &str = "Whenever another creature dealt damage this turn by a Spider you controlled dies, create a token that's a copy of that creature, except it's a Food artifact with \"{2}, {T}, Sacrifice ~: You gain 3 life,\" and it loses all other card types.";

#[test]
fn shelob_creates_food_copy_token_via_real_damage_pipeline() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);

    let _shelob = scenario
        .add_creature_from_oracle(P0, "Shelob, Child of Ungoliant", 4, 4, SHELOB_DEATH_TRIGGER)
        .id();

    // A Spider you control deals the damage.
    let spider = scenario.add_creature(P0, "Acid Web Spider", 3, 3).id();
    // An opponent's creature that will be dealt lethal damage by the Spider.
    let victim = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&spider)
        .unwrap()
        .card_types
        .subtypes
        .push("Spider".to_string());

    // Deal lethal damage from the Spider through the production effect path so the
    // per-turn damage ledger is populated with the real source snapshot.
    let damage = ResolvedAbility::new(
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 2 },
            target: TargetFilter::Any,
            damage_source: None,
        },
        vec![TargetRef::Object(victim)],
        spider,
        P0,
    );
    let mut events = Vec::new();
    deal_damage::resolve(runner.state_mut(), &damage, &mut events).expect("spider damage resolves");

    // SBA destroys the victim (lethal damage), producing the death event.
    check_state_based_actions(runner.state_mut(), &mut events);
    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Graveyard,
        "victim must die from the Spider's lethal damage"
    );

    process_triggers(runner.state_mut(), &events);
    runner.advance_until_stack_empty();

    // The observable effect: a Food artifact copy token of the dead creature,
    // under Shelob's controller.
    let token = runner.state().objects.values().find(|o| {
        o.zone == Zone::Battlefield
            && o.is_token
            && o.controller == P0
            && o.name == "Grizzly Bears"
            && o.card_types.core_types.contains(&CoreType::Artifact)
            && o.card_types.subtypes.iter().any(|s| s == "Food")
    });

    assert!(
        token.is_some(),
        "Shelob must create a Food copy token of the dead creature. \
         Battlefield tokens: {:?}",
        runner
            .state()
            .objects
            .values()
            .filter(|o| o.zone == Zone::Battlefield && o.is_token)
            .map(|o| (
                o.name.clone(),
                o.card_types.core_types.clone(),
                o.card_types.subtypes.clone()
            ))
            .collect::<Vec<_>>()
    );
}
