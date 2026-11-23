//! CR 702.29a + CR 118.9: an alternative cost for "the cycling cost" replaces only the
//! cycling cost, never the "Discard this card" half of the activation. Gavi, Nest Warden
//! lets its controller pay {0} rather than the cycling cost of the first card they cycle
//! each turn; the card must still be discarded.

use engine::game::scenario::{GameScenario, P0, P1};
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

const NEW_PERSPECTIVES: &str = "When this enchantment enters, draw three cards.\nAs long as you have seven or more cards in hand, you may pay {0} rather than pay cycling costs.";

/// Two Forgotten Caves in hand under New Perspectives, padded with `fillers`
/// generic cards. The hand size at activation is `2 + fillers`.
fn new_perspectives_scenario(
    fillers: usize,
) -> (engine::game::scenario::GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "New Perspectives", NEW_PERSPECTIVES);
    let first_cave = scenario
        .add_land_to_hand(P0, "Forgotten Cave")
        .from_oracle_text(FORGOTTEN_CAVE)
        .id();
    let second_cave = scenario
        .add_land_to_hand(P0, "Forgotten Cave")
        .from_oracle_text(FORGOTTEN_CAVE)
        .id();
    for _ in 0..fillers {
        scenario.add_card_to_hand(P0, "Filler Card");
    }
    scenario.with_library_top(P0, &["First Draw", "Second Draw"]);
    (scenario.build(), first_cave, second_cave)
}

/// Picks the {0} branch of a cycling cost choice and runs it to completion.
fn cycle_through_zero_branch(runner: &mut engine::game::scenario::GameRunner, card: ObjectId) {
    let result = activate_cycling(runner, card).expect("cycling should be offered");
    let zero_branch = match result.waiting_for {
        WaitingFor::ActivationCostOneOfChoice { ref costs, .. } => costs
            .iter()
            .position(
                |cost| matches!(cost, AbilityCost::Mana { cost } if cost == &ManaCost::generic(0)),
            )
            .expect("the {0} cycling branch should be offered"),
        other => panic!("expected the alternative cost choice, got {other:?}"),
    };
    runner
        .act(GameAction::ChooseActivationCostBranch { index: zero_branch })
        .expect("choosing the {0} branch should be accepted");
    runner.advance_until_stack_empty();
}

/// CR 702.29a + CR 118.9: with seven or more cards in hand, New Perspectives lets
/// every card cycled (not just the first each turn) use {0}, and each is still
/// discarded.
#[test]
fn new_perspectives_offers_zero_cycling_to_every_cycle_with_seven_cards() {
    let (mut runner, first_cave, second_cave) = new_perspectives_scenario(5);
    assert_eq!(runner.state().players[0].hand.len(), 7);

    cycle_through_zero_branch(&mut runner, first_cave);
    assert_eq!(runner.state().objects[&first_cave].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().players[0].hand.len(),
        7,
        "the cycled card is replaced by the drawn card, so the gate still holds"
    );

    cycle_through_zero_branch(&mut runner, second_cave);
    assert_eq!(
        runner.state().objects[&second_cave].zone,
        Zone::Graveyard,
        "the second cycle also used {{0}} and still discarded the card"
    );
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
}

/// The grant is gated on "seven or more cards in hand": with six, no alternative
/// branch is offered and the ordinary {R} cycling cost is paid. Same fixture as the
/// seven-card test above, one filler fewer.
#[test]
fn new_perspectives_offers_nothing_below_seven_cards() {
    let (mut runner, first_cave, _second_cave) = new_perspectives_scenario(4);
    assert_eq!(runner.state().players[0].hand.len(), 6);
    runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
        ManaType::Red,
        ObjectId(0),
        false,
        vec![],
    ));

    let result = activate_cycling(&mut runner, first_cave).expect("ordinary cycling is legal");
    assert!(
        !matches!(
            result.waiting_for,
            WaitingFor::ActivationCostOneOfChoice { .. }
        ),
        "no {{0}} branch below seven cards in hand"
    );
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&first_cave].zone,
        Zone::Graveyard,
        "reach guard: the cycle completed through the ordinary cost"
    );
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "the ordinary {{R}} cycling cost was paid"
    );
}

const CENSOR: &str = "Counter target spell unless its controller pays {1}.\nCycling {U} ({U}, Discard this card: Draw a card.)";

/// CR 109.5 + CR 118.9 + CR 702.29a: Gavi's "the first card you cycle each turn"
/// counts only its controller's own cycling. An opponent who cycles first during
/// the controller's turn (cycling is instant speed) does not consume P0's {0}.
#[test]
fn opponents_earlier_cycle_does_not_consume_gavis_zero_cycling() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Gavi, Nest Warden", 2, 5, GAVI)
        .as_legendary();
    let p0_cave = scenario
        .add_land_to_hand(P0, "Forgotten Cave")
        .from_oracle_text(FORGOTTEN_CAVE)
        .id();
    let censor = scenario
        .add_spell_to_hand_from_oracle(P1, "Censor", true, CENSOR)
        .id();
    scenario.with_library_top(P0, &["P0 Draw"]);
    scenario.with_library_top(P1, &["P1 Draw"]);
    let mut runner = scenario.build();

    // CR 117.3d: P0 passes with an empty stack, so P1 receives priority during
    // P0's turn and cycles Censor for {U}.
    runner
        .act(GameAction::PassPriority)
        .expect("P0 may pass priority");
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 },
        "reach guard: P1 received priority"
    );
    runner.state_mut().players[1].mana_pool.add(ManaUnit::new(
        ManaType::Blue,
        ObjectId(0),
        false,
        vec![],
    ));
    let p1_cycle = activate_cycling(&mut runner, censor).expect("P1's cycling is legal");
    assert!(
        !matches!(
            p1_cycle.waiting_for,
            WaitingFor::ActivationCostOneOfChoice { .. }
        ),
        "Gavi belongs to P0, so P1 is offered no {{0}} branch"
    );
    resolve_stack_by_passing(&mut runner);
    assert_eq!(
        runner.state().objects[&censor].zone,
        Zone::Graveyard,
        "reach guard: P1's Censor was cycled first this turn"
    );
    // CR 117.3b: P0, the active player, receives priority after the ability resolves.
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 },
        "P0 naturally regains priority"
    );

    // P0 has no mana, so the only affordable branch is the {0} alternative.
    let result = activate_cycling(&mut runner, p0_cave)
        .expect("P0's first cycle is payable through Gavi's {0}");
    let zero_branch = match result.waiting_for {
        WaitingFor::ActivationCostOneOfChoice { ref costs, .. } => costs
            .iter()
            .position(
                |cost| matches!(cost, AbilityCost::Mana { cost } if cost == &ManaCost::generic(0)),
            )
            .expect("the {0} cycling branch should still be offered to P0"),
        other => panic!("expected the alternative cost choice, got {other:?}"),
    };
    runner
        .act(GameAction::ChooseActivationCostBranch { index: zero_branch })
        .expect("choosing the {0} branch should be accepted");
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&p0_cave].zone, Zone::Graveyard);
}

/// CR 117.4 + CR 608.1: with an ability on the stack, both players pass in
/// turn until it resolves.
fn resolve_stack_by_passing(runner: &mut engine::game::scenario::GameRunner) {
    for _ in 0..8 {
        if runner.state().stack.is_empty() {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority resolves the stack");
    }
    panic!("stack did not resolve: {:?}", runner.state().stack);
}
