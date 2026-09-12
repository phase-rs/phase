//! Issue #6876 — Agent Maria Hill fires only on a Teamwork additional-cost tap.
//!
//! Verbatim Oracle (Scryfall): "Whenever Agent Maria Hill becomes tapped to pay
//! a teamwork cost, put a +1/+1 counter on her and draw a card."
//!
//! C2.1: Teamwork payment awards +1/+1 and a card; non-Teamwork taps of the
//! same Hill award neither. Hostiles live in this file so negatives cannot
//! pass through an unparsed ability.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::ability::{AbilityCost, AbilityDefinition, AbilityKind, Effect};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const HILL: &str = "Whenever Agent Maria Hill becomes tapped to pay a teamwork cost, put a +1/+1 counter on her and draw a card.";
const TEAMWORK: &str = "Teamwork 1 (As an additional cost to cast this spell, you may tap any number of creatures you control with total power 1 or more.)\nYou gain 1 life.";

fn plus1(state: &GameState, id: ObjectId) -> u32 {
    *state
        .objects
        .get(&id)
        .and_then(|obj| obj.counters.get(&CounterType::Plus1Plus1))
        .unwrap_or(&0)
}

fn hand_count(state: &GameState, player: PlayerId) -> usize {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.hand.len())
        .expect("player exists")
}

#[test]
fn teamwork_pay_awards_plus1_and_draw() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["LibA", "LibB"]);
    let hill = scenario
        .add_creature_from_oracle(P0, "Agent Maria Hill", 2, 1, HILL)
        .id();
    let mut builder = scenario.add_spell_to_hand_from_oracle(P0, "Teamwork Probe", true, TEAMWORK);
    builder.from_oracle_text_with_keywords(&["teamwork:1"], TEAMWORK);
    builder.with_mana_cost(ManaCost::generic(0));
    let spell = builder.id();
    let mut runner = scenario.build();

    let outcome = runner
        .cast(spell)
        .accept_optional()
        .pay_cost_with(&[hill])
        .resolve();

    assert!(
        outcome.state().objects[&hill].tapped,
        "reach-guard: Teamwork payment must tap Hill"
    );
    assert_eq!(
        plus1(outcome.state(), hill),
        1,
        "Teamwork tap must put a +1/+1 counter on Hill"
    );
    outcome.assert_hand_drawn(P0, 1);
}

/// CR 508.1f: attacker-declaration tapping is not a cost. Load-bearing
/// AttackDeclaration hostile for the equality gate.
#[test]
fn attack_does_not_award_plus1_or_draw() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["LibA", "LibB"]);
    let hill = scenario
        .add_creature_from_oracle(P0, "Agent Maria Hill", 2, 1, HILL)
        .id();
    let mut runner = scenario.build();
    let hand_before = hand_count(runner.state(), P0);

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(hill, AttackTarget::Player(P1))])
        .expect("declare Hill as attacker");
    runner.advance_until_stack_empty();

    assert!(
        runner.state().objects[&hill].tapped,
        "reach-guard: attacking must tap Hill (CR 508.1f)"
    );
    assert_eq!(
        plus1(runner.state(), hill),
        0,
        "AttackDeclaration must not satisfy a Teamwork-qualified trigger"
    );
    assert_eq!(
        hand_count(runner.state(), P0),
        hand_before,
        "attack must not draw"
    );
}

#[test]
fn effect_tap_does_not_award_plus1_or_draw() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["LibA", "LibB"]);
    let hill = scenario
        .add_creature_from_oracle(P0, "Agent Maria Hill", 2, 1, HILL)
        .id();
    let mut tap =
        scenario.add_spell_to_hand_from_oracle(P0, "Tap Probe", true, "Tap target creature.");
    tap.with_mana_cost(ManaCost::generic(0));
    let spell = tap.id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_object(hill).resolve();

    assert!(
        outcome.state().objects[&hill].tapped,
        "reach-guard: the tap spell must tap Hill"
    );
    assert_eq!(plus1(outcome.state(), hill), 0);
    outcome.assert_hand_drawn(P0, 0);
}

/// CR 702.122b: crewing taps to pay a cost (`CrewFamily(Crew)`), not Teamwork.
#[test]
fn crew_does_not_award_plus1_or_draw() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["LibA", "LibB"]);
    let hill = scenario
        .add_creature_from_oracle(P0, "Agent Maria Hill", 2, 1, HILL)
        .id();
    let mut runner = scenario.build();
    let next_id = runner.state().next_object_id;
    let vehicle = create_object(
        runner.state_mut(),
        CardId(next_id),
        P0,
        "Test Vehicle".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = runner.state_mut().objects.get_mut(&vehicle).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.card_types.subtypes.push("Vehicle".to_string());
        obj.base_card_types = obj.card_types.clone();
        obj.keywords.push(Keyword::Crew {
            power: 1,
            once_per_turn: None,
        });
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
        obj.summoning_sick = false;
    }
    let hand_before = hand_count(runner.state(), P0);

    runner
        .act(GameAction::CrewVehicle {
            vehicle_id: vehicle,
            creature_ids: vec![],
        })
        .expect("enter crew mode");
    runner
        .act(GameAction::CrewVehicle {
            vehicle_id: vehicle,
            creature_ids: vec![hill],
        })
        .expect("pay crew with Hill");
    runner.advance_until_stack_empty();

    assert!(
        runner.state().objects[&hill].tapped,
        "reach-guard: crewing must tap Hill"
    );
    assert_eq!(plus1(runner.state(), hill), 0);
    assert_eq!(hand_count(runner.state(), P0), hand_before);
}

/// `{T}` activation is `TapSymbol`, not Teamwork. `with_ability_definition`
/// appends; the extra ability is `NoOp` so a false-positive trigger is the
/// only way a counter or card appears.
#[test]
fn tap_symbol_does_not_award_plus1_or_draw() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["LibA", "LibB"]);
    let mut hill_b = scenario.add_creature_from_oracle(P0, "Agent Maria Hill", 2, 1, HILL);
    hill_b.with_ability_definition(
        AbilityDefinition::new(AbilityKind::Activated, Effect::NoOp).cost(AbilityCost::Tap),
    );
    let hill = hill_b.id();
    let mut runner = scenario.build();
    let hand_before = hand_count(runner.state(), P0);

    runner.activate(hill, 0).resolve();

    assert!(
        runner.state().objects[&hill].tapped,
        "reach-guard: {{T}} must tap Hill"
    );
    assert_eq!(plus1(runner.state(), hill), 0);
    assert_eq!(hand_count(runner.state(), P0), hand_before);
}

#[test]
fn two_hills_only_tapped_selfref_is_rewarded() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["LibA", "LibB"]);
    let hill_a = scenario
        .add_creature_from_oracle(P0, "Agent Maria Hill", 2, 1, HILL)
        .id();
    let hill_b = scenario
        .add_creature_from_oracle(P0, "Agent Maria Hill", 2, 1, HILL)
        .id();
    let mut builder = scenario.add_spell_to_hand_from_oracle(P0, "Teamwork Probe", true, TEAMWORK);
    builder.from_oracle_text_with_keywords(&["teamwork:1"], TEAMWORK);
    builder.with_mana_cost(ManaCost::generic(0));
    let spell = builder.id();
    let mut runner = scenario.build();

    let outcome = runner
        .cast(spell)
        .accept_optional()
        .pay_cost_with(&[hill_a])
        .resolve();

    assert!(outcome.state().objects[&hill_a].tapped);
    assert!(!outcome.state().objects[&hill_b].tapped);
    assert_eq!(plus1(outcome.state(), hill_a), 1);
    assert_eq!(plus1(outcome.state(), hill_b), 0);
    outcome.assert_hand_drawn(P0, 1);
}

#[test]
fn declined_teamwork_leaves_hill_untapped() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["LibA", "LibB"]);
    let hill = scenario
        .add_creature_from_oracle(P0, "Agent Maria Hill", 2, 1, HILL)
        .id();
    let mut builder = scenario.add_spell_to_hand_from_oracle(P0, "Teamwork Probe", true, TEAMWORK);
    builder.from_oracle_text_with_keywords(&["teamwork:1"], TEAMWORK);
    builder.with_mana_cost(ManaCost::generic(0));
    let spell = builder.id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).resolve();

    assert!(
        !outcome.state().objects[&hill].tapped,
        "declining the optional Teamwork cost must not tap Hill"
    );
    assert_eq!(plus1(outcome.state(), hill), 0);
    outcome.assert_hand_drawn(P0, 0);
}

/// Optional control only — not a `tap_cause` discriminator. CR 702.20b:
/// attacking does not tap a vigilant creature, so no `PermanentTapped` event
/// is emitted. The load-bearing AttackDeclaration hostile is
/// `attack_does_not_award_plus1_or_draw`.
#[test]
fn vigilance_attack_does_not_tap_or_award() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["LibA", "LibB"]);
    let mut hill_b = scenario.add_creature_from_oracle(P0, "Agent Maria Hill", 2, 1, HILL);
    hill_b.with_keyword(Keyword::Vigilance);
    let hill = hill_b.id();
    let mut runner = scenario.build();
    let hand_before = hand_count(runner.state(), P0);

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(hill, AttackTarget::Player(P1))])
        .expect("declare vigilant Hill as attacker");
    runner.advance_until_stack_empty();

    assert!(
        !runner.state().objects[&hill].tapped,
        "CR 702.20b: vigilance means attacking does not tap"
    );
    assert_eq!(plus1(runner.state(), hill), 0);
    assert_eq!(hand_count(runner.state(), P0), hand_before);
}
