//! CR 702.29a + CR 118.9: an alternative cost for "the cycling cost" replaces only the
//! cycling cost, never the "Discard this card" half of the activation. Gavi, Nest Warden
//! lets its controller pay {0} rather than the cycling cost of the first card they cycle
//! each turn; the card must still be discarded.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::AbilityCost;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const GAVI: &str = "You may pay {0} rather than pay the cycling cost of the first card you cycle each turn.\nWhenever you draw your second card each turn, create a 2/2 red and white Dinosaur Cat creature token.";
const FORGOTTEN_CAVE: &str =
    "This land enters tapped.\n{T}: Add {R}.\nCycling {R} ({R}, Discard this card: Draw a card.)";

fn activate_cycling(
    runner: &mut engine::game::scenario::GameRunner,
    card: ObjectId,
) -> Result<engine::types::game_state::ActionResult, engine::game::EngineError> {
    let ability_index = runner.state().objects[&card]
        .abilities
        .iter()
        .position(|ability| {
            ability.ability_tag == Some(engine::types::ability::AbilityTag::Cycling)
        })
        .expect("synthesized cycling ability");
    runner.act(GameAction::ActivateAbility {
        source_id: card,
        ability_index,
        payment_mode: CastPaymentMode::Auto,
    })
}

#[test]
fn alternative_cycling_cost_still_discards_and_is_first_per_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Gavi, Nest Warden", 2, 5, GAVI)
        .as_legendary();
    let first_cave = scenario
        .add_land_to_hand(P0, "Forgotten Cave")
        .from_oracle_text(FORGOTTEN_CAVE)
        .id();
    let second_cave = scenario
        .add_land_to_hand(P0, "Forgotten Cave")
        .from_oracle_text(FORGOTTEN_CAVE)
        .id();
    scenario.with_library_top(P0, &["First Draw", "Second Draw"]);

    let mut runner = scenario.build();

    // First cycle of the turn with no mana: the {R} branch is unaffordable, the {0}
    // branch is offered.
    let result = activate_cycling(&mut runner, first_cave).expect("cycling should be offered");
    let zero_branch = match result.waiting_for {
        WaitingFor::ActivationCostOneOfChoice { ref costs, .. } => costs
            .iter()
            .position(
                |cost| matches!(cost, AbilityCost::Mana { cost } if cost == &ManaCost::generic(0)),
            )
            .expect("the {0} cycling branch should be offered"),
        other => panic!("expected the alternative cost choice, got {other:?}"),
    };
    let chosen = runner
        .act(GameAction::ChooseActivationCostBranch { index: zero_branch })
        .expect("choosing the {0} branch should be accepted");
    let events = chosen.events;
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&first_cave].zone,
        Zone::Graveyard,
        "CR 702.29a: the {{0}} alternative replaces the cycling cost, not the discard"
    );
    let drawn = runner
        .state()
        .objects
        .values()
        .find(|o| o.name == "First Draw")
        .unwrap();
    assert_eq!(drawn.zone, Zone::Hand, "cycling must draw a card");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, GameEvent::Cycled { object_id, .. } if *object_id == first_cave)),
        "the card was cycled, so the Cycled event must be emitted: {events:?}"
    );

    // Second cycle this turn: the alternative is no longer offered; the ordinary {R}
    // cycling cost is what is paid (reach guard: the cycle completes).
    runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
        ManaType::Red,
        ObjectId(0),
        false,
        vec![],
    ));
    let second = activate_cycling(&mut runner, second_cave).expect("ordinary cycling is legal");
    assert!(
        !matches!(
            second.waiting_for,
            WaitingFor::ActivationCostOneOfChoice { .. }
        ),
        "only the first card cycled each turn may use {{0}}"
    );
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&second_cave].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "the second cycle pays its ordinary {{R}} cycling cost"
    );
}
