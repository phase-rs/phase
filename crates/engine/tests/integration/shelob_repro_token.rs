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
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::process_triggers;
use engine::types::ability::{
    Effect, QuantityExpr, ReplacementMode, ResolvedAbility, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::keywords::Keyword;
use engine::types::proposed_event::ProposedEvent;
use engine::types::replacements::ReplacementEvent;
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
            excess: None,
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

fn shelob_food_copy_riot_choice() -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);

    scenario.add_creature_from_oracle(P0, "Shelob, Child of Ungoliant", 4, 4, SHELOB_DEATH_TRIGGER);
    let spider = scenario.add_creature(P0, "Acid Web Spider", 3, 3).id();
    let victim = scenario
        .add_creature(P1, "Riot Grizzly Bears", 2, 2)
        .from_oracle_text_with_keywords(&["riot"], "Riot")
        .id();

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&spider)
        .expect("spider exists")
        .card_types
        .subtypes
        .push("Spider".to_string());

    let victim_object = &runner.state().objects[&victim];
    assert!(
        victim_object.keywords.contains(&Keyword::Riot),
        "the victim must receive Riot from the production Oracle keyword path"
    );
    assert!(
        victim_object
            .replacement_definitions
            .iter_unchecked()
            .any(|replacement| {
                matches!(replacement.event, ReplacementEvent::Moved)
                    && replacement.destination_zone == Some(Zone::Battlefield)
                    && matches!(
                        &replacement.mode,
                        ReplacementMode::Optional { decline: Some(_) }
                    )
                    && replacement
                        .description
                        .as_deref()
                        .is_some_and(|description| description.contains("Riot"))
            }),
        "the victim's Riot must have a synthesized optional entry replacement"
    );

    let damage = ResolvedAbility::new(
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 2 },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        },
        vec![TargetRef::Object(victim)],
        spider,
        P0,
    );
    let mut events = Vec::new();
    deal_damage::resolve(runner.state_mut(), &damage, &mut events).expect("spider damage resolves");
    check_state_based_actions(runner.state_mut(), &mut events);
    assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);

    process_triggers(runner.state_mut(), &events);
    runner.advance_until_stack_empty();
    runner
}

fn assert_liminal_riot_choice(runner: &GameRunner) {
    let Some(pending) = runner.state().pending_replacement.as_ref() else {
        panic!(
            "the copied victim's synthesized Riot must park a replacement choice; waiting_for={:?}",
            runner.state().waiting_for
        );
    };
    assert!(
        matches!(&pending.proposed, ProposedEvent::TokenEntry { .. }),
        "Riot must apply to Shelob's liminal Food token entry, got {:?}",
        pending.proposed
    );
    assert!(
        !runner.state().liminal_entries.is_empty(),
        "the Food copy must still be liminal while Riot is awaiting its choice"
    );
    let WaitingFor::ReplacementChoice {
        candidate_count,
        candidates,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "Shelob's Food copy must reach the Riot replacement choice; waiting_for={:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*candidate_count, 2, "Riot must offer counter or haste");
    assert!(
        candidates
            .iter()
            .any(|candidate| candidate.description.contains("Riot")),
        "the pending token-entry replacement must be the synthesized Riot replacement: {candidates:?}"
    );
}

fn shelob_food_copy(runner: &GameRunner) -> &engine::game::game_object::GameObject {
    runner
        .state()
        .objects
        .values()
        .find(|object| {
            object.zone == Zone::Battlefield
                && object.is_token
                && object.controller == P0
                && object.name == "Riot Grizzly Bears"
                && object.card_types.core_types.contains(&CoreType::Artifact)
                && object
                    .card_types
                    .subtypes
                    .iter()
                    .any(|subtype| subtype == "Food")
        })
        .expect("Shelob must create its Food artifact copy token")
}

fn assert_clean_priority_state(runner: &GameRunner) {
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ),
        "the consumed Riot choice must settle at priority; waiting_for={:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().pending_replacement.is_none(),
        "the Riot replacement record must be consumed"
    );
    assert!(
        runner.state().pending_liminal_entry_resume.is_none(),
        "the liminal token resume must be consumed"
    );
    assert!(
        runner.state().liminal_entries.is_empty(),
        "the committed Food token must not leave a liminal entry behind"
    );
}

#[test]
fn shelob_food_copy_riot_decline_gains_haste_and_settles() {
    let mut runner = shelob_food_copy_riot_choice();
    assert_liminal_riot_choice(&runner);

    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("decline Riot counter for haste");

    let token = shelob_food_copy(&runner);
    assert!(token.keywords.contains(&Keyword::Haste));
    assert_eq!(
        token
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        0,
        "declining Riot must not add a +1/+1 counter"
    );
    assert_clean_priority_state(&runner);
}

#[test]
fn shelob_food_copy_riot_accept_gets_counter_without_haste() {
    let mut runner = shelob_food_copy_riot_choice();
    assert_liminal_riot_choice(&runner);

    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept Riot counter");

    let token = shelob_food_copy(&runner);
    assert_eq!(
        token
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        1,
        "accepting Riot must add exactly one +1/+1 counter"
    );
    assert!(
        !token.keywords.contains(&Keyword::Haste),
        "accepting Riot's counter must not grant haste"
    );
    assert_clean_priority_state(&runner);
}
