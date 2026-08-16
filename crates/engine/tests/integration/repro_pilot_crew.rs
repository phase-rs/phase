//! Regression: a Pilot token created "with" a crew-contribution ability (e.g.
//! Shorikai, Genesis Engine's "This token crews Vehicles as though its power
//! were 2 greater.") must carry that static exactly once and apply it when
//! crewing.
//!
//! CR 111.4: A token's abilities are defined by the effect that creates it. The
//! catalog preset for Shorikai's Pilot token mirrors the same printed text, so
//! the catalog `rules_text` fallback used to re-inject a second, identical
//! `CrewContribution` static on top of the one from the `with "..."` clause —
//! stacking the +2 delta to +4 and making a 1/1 Pilot contribute 5 power toward
//! a crew cost instead of 3.

use engine::game::casting::activated_ability_definitions;
use engine::game::scenario::{GameScenario, P0};
use engine::game::zones::create_object;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::statics::CrewAction;
use engine::types::zones::Zone;

const SHORIKAI: &str = "{1}, {T}: Draw two cards, then discard a card. Create a 1/1 colorless Pilot creature token with \"This token crews Vehicles as though its power were 2 greater.\"";

fn colorless_mana(count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

fn resolve_stack(runner: &mut engine::game::scenario::GameRunner) {
    for _ in 0..40 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner.resolve_top();
    }
}

/// Activate Shorikai's ability and resolve it, returning the created Pilot token id.
fn create_shorikai_pilot(
    runner: &mut engine::game::scenario::GameRunner,
    shorikai: ObjectId,
) -> ObjectId {
    let ability_index = activated_ability_definitions(runner.state(), shorikai)
        .into_iter()
        .next()
        .expect("Shorikai activated ability")
        .0;
    runner.activate(shorikai, ability_index).resolve();

    if let engine::types::game_state::WaitingFor::DiscardChoice { .. } = runner.state().waiting_for
    {
        let hand_card = runner.state().players[P0.0 as usize].hand[0];
        runner
            .act(GameAction::SelectCards {
                cards: vec![hand_card],
            })
            .expect("discard for Shorikai");
    }

    resolve_stack(runner);

    *runner
        .state()
        .last_created_token_ids
        .first()
        .expect("Pilot token created")
}

#[test]
fn shorikai_pilot_token_crews_with_plus_two() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Library A", "Library B", "Library C", "Library D"]);
    let shorikai = scenario
        .add_creature(P0, "Shorikai, Genesis Engine", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Vehicle"])
        .from_oracle_text(SHORIKAI)
        .id();
    let _hand_card = scenario
        .add_creature_to_hand(P0, "Discard Fodder", 1, 1)
        .id();
    scenario.with_mana_pool(P0, colorless_mana(1));

    let mut runner = scenario.build();
    let token_id = create_shorikai_pilot(&mut runner, shorikai);

    // CR 702.122a: exactly one +2 crew-contribution static — the catalog preset
    // must not double the `with "..."` grant. A 1/1 Pilot therefore contributes
    // 1 (base power) + 2 (delta) = 3 toward a crew cost.
    let contribution = engine::game::static_abilities::object_crew_power_contribution(
        runner.state(),
        token_id,
        CrewAction::Crew,
    );
    assert_eq!(
        contribution, 3,
        "1/1 Pilot with a single +2 crew delta must contribute 3 power toward crew"
    );
}

/// Add a Crew-`power` Vehicle controlled by P0 to `runner`'s state.
fn add_crew_vehicle(runner: &mut engine::game::scenario::GameRunner, power: u32) -> ObjectId {
    let vehicle = create_object(
        runner.state_mut(),
        CardId(0),
        P0,
        format!("Test Crew-{power} Vehicle"),
        Zone::Battlefield,
    );
    let obj = runner.state_mut().objects.get_mut(&vehicle).unwrap();
    obj.card_types.core_types.push(CoreType::Artifact);
    obj.card_types.subtypes.push("Vehicle".to_string());
    let crew = Keyword::Crew {
        power,
        once_per_turn: None,
    };
    obj.base_keywords.push(crew.clone());
    obj.keywords.push(crew);
    obj.base_power = Some(5);
    obj.base_toughness = Some(5);
    obj.power = Some(5);
    obj.toughness = Some(5);
    vehicle
}

/// Build a fresh game with Shorikai's Pilot token and a Crew-`power` Vehicle,
/// then run the full two-step crew flow (activate → select crewers) tapping ONLY
/// the lone Pilot. Returns whether the paying step was accepted.
fn lone_pilot_crews(power: u32) -> bool {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Library A", "Library B", "Library C", "Library D"]);
    let shorikai = scenario
        .add_creature(P0, "Shorikai, Genesis Engine", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Vehicle"])
        .from_oracle_text(SHORIKAI)
        .id();
    let _hand_card = scenario
        .add_creature_to_hand(P0, "Discard Fodder", 1, 1)
        .id();
    scenario.with_mana_pool(P0, colorless_mana(1));

    let mut runner = scenario.build();
    let token_id = create_shorikai_pilot(&mut runner, shorikai);
    let vehicle = add_crew_vehicle(&mut runner, power);

    // Step 1 activates the Vehicle's crew ability. CR 702.122a: it is rejected up
    // front if no subset of eligible creatures can meet the cost, so a failure
    // here already means "the lone Pilot can't crew this". Step 2 pays by tapping
    // only the Pilot, the step that binds the chosen crewers to the cost.
    if runner
        .act(GameAction::CrewVehicle {
            vehicle_id: vehicle,
            creature_ids: vec![],
        })
        .is_err()
    {
        return false;
    }
    runner
        .act(GameAction::CrewVehicle {
            vehicle_id: vehicle,
            creature_ids: vec![token_id],
        })
        .is_ok()
}

/// End-to-end: the created Pilot token, alone, must satisfy Crew 3 (its base
/// power 1 plus the +2 delta equals 3) but NOT Crew 4. This exercises the full
/// `CrewVehicle` announcement plus payment validation — the user-facing path
/// where "the Pilot crews for just 1" would show — and pins the contribution to
/// exactly 3.
#[test]
fn shorikai_pilot_token_alone_crews_a_crew_three_vehicle() {
    // CR 702.122a: adjusted crew power 3 satisfies Crew 3 ...
    assert!(
        lone_pilot_crews(3),
        "a lone 1/1 Pilot (+2 crew delta = 3 power) must satisfy Crew 3"
    );
    // ... but NOT Crew 4 — the +2 delta is applied exactly once (not zero, which
    // would read as "crews for 1", and not twice, which would over-satisfy).
    assert!(
        !lone_pilot_crews(4),
        "a lone 1/1 Pilot contributes exactly 3 and must NOT satisfy Crew 4"
    );
}
