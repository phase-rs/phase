//! Runtime regression for issue #1981 (Mogg War Marshal) — a creature's "dies"
//! trigger must fire when it is sacrificed for an unpaid echo cost.
//!
//! Mogg War Marshal: "Echo {1}{R}" + "When this creature enters or dies, create
//! a 1/1 red Goblin creature token." The compound "enters or dies" clause is
//! split by the parser into two independent triggers; this test exercises the
//! *dies* half.
//!
//! CR 702.30a (Echo): "At the beginning of your upkeep, if this permanent came
//! under your control since the beginning of your last upkeep, sacrifice it
//! unless you pay its echo cost." Declining the payment sacrifices the
//! permanent.
//!
//! CR 700.4 + CR 603.6c: A permanent moving from the battlefield to a graveyard
//! "dies", and a "when this dies" leaves-the-battlefield trigger triggers from
//! that zone change — regardless of whether the move was a sacrifice as a cost,
//! a sacrifice as an effect, or destruction.
//!
//! Root cause (pre-fix): the echo unpaid-sacrifice branch in
//! `engine_payment_choices::handle_unless_payment` resolves the `Sacrifice`
//! effect (emitting the battlefield → graveyard `ZoneChanged` event) and then
//! returns directly, bypassing `run_post_action_pipeline`. Because nothing scans
//! the freshly emitted events with `triggers::process_triggers`, the dies
//! trigger was never collected — the Goblin token was never created.

use engine::database::synthesis::KeywordTriggerInstaller;
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, PtValue, QuantityExpr, TargetFilter, TriggerDefinition,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

/// Echo {1}{R}, matching Mogg War Marshal's printed echo cost.
fn echo_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Red],
        generic: 1,
    }
}

/// "When this creature dies, create a 1/1 red Goblin creature token." — the
/// dies half of Mogg War Marshal's compound "enters or dies" trigger.
///
/// CR 603.6c + CR 700.4: a battlefield → graveyard move is a "dies" event; the
/// trigger fires from that zone change.
fn dies_create_goblin_trigger() -> TriggerDefinition {
    let create_goblin = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Token {
            name: "Goblin".to_string(),
            power: PtValue::Fixed(1),
            toughness: PtValue::Fixed(1),
            types: vec!["Creature".to_string()],
            colors: vec![ManaColor::Red],
            keywords: vec![],
            tapped: false,
            count: QuantityExpr::Fixed { value: 1 },
            owner: TargetFilter::Controller,
            attach_to: None,
            enters_attacking: false,
            supertypes: vec![],
            static_abilities: vec![],
            enter_with_counters: vec![],
        },
    );

    TriggerDefinition::new(TriggerMode::ChangesZone)
        .valid_card(TargetFilter::SelfRef)
        .origin(Zone::Battlefield)
        .destination(Zone::Graveyard)
        .trigger_zones(vec![Zone::Battlefield])
        .execute(create_goblin)
        .description("When this creature dies, create a 1/1 red Goblin creature token.".to_string())
}

/// Count 1/1 red Goblin tokens controlled by `P0` on the battlefield.
fn goblin_tokens(state: &engine::types::game_state::GameState) -> usize {
    state
        .battlefield
        .iter()
        .filter(|id| {
            state.objects.get(id).is_some_and(|obj| {
                obj.is_token
                    && obj.controller == P0
                    && obj.name == "Goblin"
                    && obj.card_types.core_types.contains(&CoreType::Creature)
            })
        })
        .count()
}

/// Build a creature on P0's battlefield carrying the synthesized echo upkeep
/// trigger and the dies → Goblin trigger, with `echo_due` set so the echo
/// trigger's intervening-if (`TriggerCondition::EchoDue`) is satisfied.
fn build_echo_creature(scenario: &mut GameScenario) -> engine::types::identifiers::ObjectId {
    let echo_trigger = KeywordTriggerInstaller::triggers_for(&Keyword::Echo(echo_cost()))
        .into_iter()
        .next()
        .expect("echo keyword synthesizes one upkeep trigger");

    scenario
        .add_creature(P0, "Mogg War Marshal", 1, 1)
        .with_keyword(Keyword::Echo(echo_cost()))
        .with_trigger_definition(echo_trigger)
        .with_trigger_definition(dies_create_goblin_trigger())
        .id()
}

/// CR 702.30a + CR 700.4 — declining the echo payment sacrifices the creature,
/// and the battlefield → graveyard move fires its dies trigger, creating the
/// Goblin token.
///
/// Pre-fix this asserted-fails: the unpaid-echo sacrifice path returned without
/// running the trigger scan, so no Goblin token was created.
#[test]
fn unpaid_echo_sacrifice_fires_dies_trigger() {
    let mut scenario = GameScenario::new();
    let creature = build_echo_creature(&mut scenario);

    let mut runner = scenario.build();

    // Pre-existing battlefield permanent that just changed controller: it is
    // P0's upkeep and the creature is echo-due (CR 702.30a).
    runner.state_mut().turn_number = 2;
    runner.state_mut().phase = Phase::Untap;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    runner
        .state_mut()
        .objects
        .get_mut(&creature)
        .unwrap()
        .echo_due = true;

    assert_eq!(
        goblin_tokens(runner.state()),
        0,
        "precondition: no Goblin tokens before upkeep"
    );

    // Drive Untap → Upkeep. The echo trigger lands on the stack and resolves,
    // surfacing the "sacrifice unless you pay {1}{R}" prompt.
    runner.auto_advance_to_main_phase();
    runner.advance_until_stack_empty();

    let WaitingFor::UnlessPayment { .. } = runner.state().waiting_for else {
        panic!(
            "echo upkeep trigger should surface an UnlessPayment prompt, got {:?}",
            runner.state().waiting_for
        );
    };

    // P0 declines to pay the echo cost → the creature is sacrificed.
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("decline echo payment");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects.get(&creature).unwrap().zone,
        Zone::Graveyard,
        "declining echo must sacrifice the creature (CR 702.30a)"
    );
    assert_eq!(
        goblin_tokens(runner.state()),
        1,
        "the unpaid-echo sacrifice is a death (CR 700.4); the dies trigger must \
         create one 1/1 red Goblin token"
    );
}

/// Control: a death by destruction (not echo) fires the same dies trigger.
/// Establishes that the trigger itself is wired correctly, isolating the bug to
/// the echo-sacrifice resolution path.
#[test]
fn ordinary_death_fires_dies_trigger() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = build_echo_creature(&mut scenario);
    let mut runner = scenario.build();

    runner.state_mut().turn_number = 2;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    assert_eq!(goblin_tokens(runner.state()), 0, "precondition: no tokens");

    // Move the creature to the graveyard directly through the zone-change
    // primitive, then run the trigger scan — the same battlefield → graveyard
    // event the echo sacrifice produces.
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), creature, Zone::Graveyard, &mut events);
    engine::game::triggers::process_triggers(runner.state_mut(), &events);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects.get(&creature).unwrap().zone,
        Zone::Graveyard,
        "control: creature is in the graveyard"
    );
    assert_eq!(
        goblin_tokens(runner.state()),
        1,
        "control: a normal death fires the dies trigger and creates the Goblin"
    );
}
