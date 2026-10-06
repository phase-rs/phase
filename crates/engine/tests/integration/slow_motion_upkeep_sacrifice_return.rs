//! Tests for Slow Motion and recursive upkeep tax Auras.
//!
//! Oracle:
//! Enchant creature
//! At the beginning of the upkeep of enchanted creature's controller, that player sacrifices that creature unless they pay {2}.
//! When this Aura is put into a graveyard from the battlefield, return it to its owner's hand.
//!
//! CR 701.21a (Sacrifice) + CR 704.5m (Aura unattached state-based action) + CR 118.12 (Unless payment) + CR 109.5 ("You").

use engine::game::effects::attach::attach_to;
use engine::game::sba::check_state_based_actions;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::triggers::process_triggers;
use engine::types::ability::{ControllerRef, TargetFilter, TypedFilter};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const SLOW_MOTION_ORACLE: &str = "Enchant creature\n\
At the beginning of the upkeep of enchanted creature's controller, that player sacrifices that creature unless they pay {2}.\n\
When this Aura is put into a graveyard from the battlefield, return it to its owner's hand.";

fn generic_mana(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

fn enchant_creature() -> Keyword {
    Keyword::Enchant(TargetFilter::Typed(TypedFilter::creature()))
}

fn enchant_creature_you_control() -> Keyword {
    Keyword::Enchant(TargetFilter::Typed(
        TypedFilter::creature().controller(ControllerRef::You),
    ))
}

#[test]
fn slow_motion_oracle_parses() {
    let parsed = engine::parser::oracle::parse_oracle_text(
        SLOW_MOTION_ORACLE,
        "Slow Motion",
        &[],
        &["Enchantment".to_string()],
        &["Aura".to_string()],
    );
    assert_eq!(
        parsed.triggers.len(),
        2,
        "Slow Motion must parse 2 triggers"
    );

    let upkeep_trig = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::Phase)
        .expect("upkeep trigger must be present");
    assert_eq!(upkeep_trig.phase, Some(Phase::Upkeep));
    assert!(
        upkeep_trig.unless_pay.is_some(),
        "must have unless_pay modifier"
    );

    let dies_trig = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::ChangesZone)
        .expect("leaves-battlefield trigger must be present");
    assert_eq!(dies_trig.destination, Some(Zone::Graveyard));
}

#[test]
fn slow_motion_does_not_trigger_on_aura_controllers_upkeep() {
    // Negative test: Slow Motion only triggers on the upkeep of *enchanted creature's controller*.
    // When P0 controls Slow Motion attached to P1's creature, P0's upkeep must not trigger it.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Upkeep);

    let victim = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let slow_motion = scenario
        .add_enchantment_from_oracle(P0, "Slow Motion", SLOW_MOTION_ORACLE)
        .with_subtypes(vec!["Aura"])
        .with_keyword(enchant_creature())
        .id();

    let mut runner = scenario.build();
    attach_to(runner.state_mut(), slow_motion, victim);

    // P0's upkeep begins (Aura controller, but not enchanted creature's controller)
    runner.state_mut().active_player = P0;
    process_triggers(
        runner.state_mut(),
        &[GameEvent::PhaseChanged {
            phase: Phase::Upkeep,
        }],
    );

    assert!(
        runner.state().stack.is_empty(),
        "Slow Motion must not trigger during Aura controller's upkeep when attached to opponent's creature"
    );
}

#[test]
fn slow_motion_declining_cost_sacrifices_creature_and_returns_aura_to_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Upkeep);

    let victim = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let slow_motion = scenario
        .add_enchantment_from_oracle(P0, "Slow Motion", SLOW_MOTION_ORACLE)
        .with_subtypes(vec!["Aura"])
        .with_keyword(enchant_creature())
        .id();

    let mut runner = scenario.build();
    attach_to(runner.state_mut(), slow_motion, victim);

    // P1's upkeep begins
    runner.state_mut().active_player = P1;
    process_triggers(
        runner.state_mut(),
        &[GameEvent::PhaseChanged {
            phase: Phase::Upkeep,
        }],
    );

    // Positive trigger reach guard: trigger is on stack with P1 as scoped_player
    let stacked_trigger = runner
        .state()
        .stack
        .iter()
        .next()
        .expect("upkeep trigger must be placed on the stack");
    let ability = stacked_trigger
        .ability()
        .expect("stack entry must have an ability");
    assert_eq!(
        ability.scoped_player,
        Some(P1),
        "stacked trigger must bind P1 as the scoped player"
    );

    runner.advance_until_stack_empty();

    // P1 is prompted to pay {2}
    let waiting = runner.state().waiting_for.clone();
    assert!(
        matches!(waiting, WaitingFor::UnlessPayment { player, .. } if player == P1),
        "P1 must be prompted for unless payment, got {:?}",
        waiting
    );

    // P1 declines to pay
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("decline upkeep payment");
    runner.advance_until_stack_empty();

    // The creature was sacrificed to P1's graveyard
    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Graveyard,
        "enchanted creature must be sacrificed when cost is unpaid"
    );

    // Slow Motion was put into graveyard by SBA and returned to P0's hand by its trigger
    assert_eq!(
        runner.state().objects[&slow_motion].zone,
        Zone::Hand,
        "Slow Motion must return to its owner's hand after the creature is sacrificed"
    );
}

#[test]
fn slow_motion_paying_cost_keeps_creature_and_aura_on_battlefield() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Upkeep);
    scenario.with_mana_pool(P1, generic_mana(2));

    let victim = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let slow_motion = scenario
        .add_enchantment_from_oracle(P0, "Slow Motion", SLOW_MOTION_ORACLE)
        .with_subtypes(vec!["Aura"])
        .with_keyword(enchant_creature())
        .id();

    let mut runner = scenario.build();
    attach_to(runner.state_mut(), slow_motion, victim);

    // P1's upkeep begins
    runner.state_mut().active_player = P1;
    process_triggers(
        runner.state_mut(),
        &[GameEvent::PhaseChanged {
            phase: Phase::Upkeep,
        }],
    );
    runner.advance_until_stack_empty();

    // P1 pays {2}
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("pay upkeep cost");
    runner.advance_until_stack_empty();

    // The creature is still on the battlefield
    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Battlefield,
        "enchanted creature must remain on battlefield when cost is paid"
    );

    // Slow Motion is still attached to the creature on the battlefield
    assert_eq!(
        runner.state().objects[&slow_motion].zone,
        Zone::Battlefield,
        "Slow Motion must remain on battlefield when cost is paid"
    );
}

#[test]
fn slow_motion_aura_removed_while_upkeep_trigger_on_stack_still_sacrifices_creature() {
    // CR 113.7a + CR 608.2k: Removing the Aura source while its upkeep trigger is on the stack
    // does not prevent the ability from resolving against the enchanted creature.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Upkeep);

    let victim = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let slow_motion = scenario
        .add_enchantment_from_oracle(P0, "Slow Motion", SLOW_MOTION_ORACLE)
        .with_subtypes(vec!["Aura"])
        .with_keyword(enchant_creature())
        .id();

    let mut runner = scenario.build();
    attach_to(runner.state_mut(), slow_motion, victim);

    // P1's upkeep begins
    runner.state_mut().active_player = P1;
    process_triggers(
        runner.state_mut(),
        &[GameEvent::PhaseChanged {
            phase: Phase::Upkeep,
        }],
    );

    // Trigger is on the stack. Now destroy Slow Motion before resolving the upkeep trigger.
    // Move Slow Motion to graveyard (simulating Disenchant/Naturalize or destruction).
    // This triggers Slow Motion's leaves-battlefield trigger ("return it to its owner's hand").
    let mut destroy_events = Vec::new();
    engine::game::zones::move_to_zone(
        runner.state_mut(),
        slow_motion,
        Zone::Graveyard,
        &mut destroy_events,
    );
    process_triggers(runner.state_mut(), &destroy_events);

    // Advance stack - Slow Motion's leaves-battlefield trigger resolves and returns it to hand.
    // Then the upkeep trigger prompts P1 for payment.
    runner.advance_until_stack_empty();

    // P1 is prompted to pay {2} for the upkeep trigger
    let waiting = runner.state().waiting_for.clone();
    assert!(
        matches!(waiting, WaitingFor::UnlessPayment { player, .. } if player == P1),
        "P1 must be prompted for unless payment even if Slow Motion left the battlefield, got {:?}",
        waiting
    );

    // P1 declines to pay
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("decline upkeep payment");
    runner.advance_until_stack_empty();

    // The creature was sacrificed to P1's graveyard per CR 113.7a and CR 608.2k
    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Graveyard,
        "enchanted creature must be sacrificed even if Slow Motion left battlefield before resolution"
    );

    // Slow Motion is in P0's hand
    assert_eq!(
        runner.state().objects[&slow_motion].zone,
        Zone::Hand,
        "Slow Motion must be in hand from its dies trigger"
    );
}

#[test]
fn slow_motion_creature_control_change_before_resolution_prevents_sacrifice() {
    // CR 701.21a + CR 109.5: "that player sacrifices that creature unless they pay {2}."
    // Here "that player" is P1 (the player whose upkeep caused the trigger, captured as scoped_player).
    // If control of the creature changes to P0 before resolution, P1 cannot sacrifice P0's permanent.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Upkeep);

    let victim = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let slow_motion = scenario
        .add_enchantment_from_oracle(P0, "Slow Motion", SLOW_MOTION_ORACLE)
        .with_subtypes(vec!["Aura"])
        .with_keyword(enchant_creature())
        .id();

    let mut runner = scenario.build();
    attach_to(runner.state_mut(), slow_motion, victim);

    // P1's upkeep begins
    runner.state_mut().active_player = P1;
    process_triggers(
        runner.state_mut(),
        &[GameEvent::PhaseChanged {
            phase: Phase::Upkeep,
        }],
    );

    // Positive trigger reach guard: trigger is on stack with P1 as scoped_player
    let stacked_trigger = runner
        .state()
        .stack
        .iter()
        .next()
        .expect("upkeep trigger must be placed on the stack");
    let ability = stacked_trigger
        .ability()
        .expect("stack entry must have an ability");
    assert_eq!(
        ability.scoped_player,
        Some(P1),
        "stacked trigger must bind P1 as the scoped player"
    );

    // Change control of victim to P0 before resolution (e.g. Act of Treason / Control Magic)
    {
        let obj = runner.state_mut().objects.get_mut(&victim).unwrap();
        obj.base_controller = Some(P0);
        obj.controller = P0;
    }

    // Slow Motion enchants "creature" (not "creature you control"), so it remains legally attached
    let mut sba_events = Vec::new();
    check_state_based_actions(runner.state_mut(), &mut sba_events);
    assert_eq!(
        runner.state().objects[&slow_motion].zone,
        Zone::Battlefield,
        "Slow Motion remains on battlefield after host control change"
    );

    runner.advance_until_stack_empty();

    // Positive payer reach guard: P1 is still prompted for unless payment
    let waiting = runner.state().waiting_for.clone();
    assert!(
        matches!(waiting, WaitingFor::UnlessPayment { player, .. } if player == P1),
        "P1 must be prompted for unless payment, got {:?}",
        waiting
    );

    // P1 declines payment
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("decline upkeep payment");
    runner.advance_until_stack_empty();

    // Creature must NOT be sacrificed because P1 does not control it (CR 701.21a)
    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Battlefield,
        "creature must survive because P1 cannot sacrifice a permanent they do not control"
    );
    assert_eq!(
        runner.state().objects[&victim].controller,
        P0,
        "creature remains controlled by P0"
    );

    // Slow Motion remains on the battlefield attached to victim
    assert_eq!(
        runner.state().objects[&slow_motion].zone,
        Zone::Battlefield,
        "Slow Motion remains on battlefield attached to creature"
    );
}

const BREATH_OF_FURY_ORACLE: &str = "Enchant creature you control\n\
When enchanted creature deals combat damage to a player, sacrifice it and attach Breath of Fury to a creature you control. If you do, untap all creatures you control and after this phase, there is an additional combat phase.";

#[test]
fn breath_of_fury_oracle_parses() {
    let parsed = engine::parser::oracle::parse_oracle_text(
        BREATH_OF_FURY_ORACLE,
        "Breath of Fury",
        &[],
        &["Enchantment".to_string()],
        &["Aura".to_string()],
    );
    assert!(
        !parsed.triggers.is_empty(),
        "Breath of Fury must parse its combat damage trigger"
    );
}

#[test]
fn breath_of_fury_trigger_does_not_sacrifice_creature_if_control_changed() {
    // CR 109.5 + CR 701.21a + CR 303.4c + CR 704.5m:
    // When enchanted creature deals combat damage, P0's Breath of Fury trigger instructs P0 ("you")
    // to sacrifice it. If P1 gains control of the creature before the trigger resolves:
    // 1. CR 704.5m puts Breath of Fury into P0's graveyard by SBA (illegally enchanting creature P0 doesn't control).
    // 2. When the trigger resolves, P0 cannot sacrifice a creature they do not control (CR 701.21a).
    // 3. The "If you do" untap effect does not occur, leaving P0's other creatures tapped.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::CombatDamage);

    let creature = scenario.add_creature(P0, "Raging Goblin", 1, 1).id();
    let other_creature = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let breath_of_fury = scenario
        .add_enchantment_from_oracle(P0, "Breath of Fury", BREATH_OF_FURY_ORACLE)
        .with_subtypes(vec!["Aura"])
        .with_keyword(enchant_creature_you_control())
        .id();

    let mut runner = scenario.build();
    attach_to(runner.state_mut(), breath_of_fury, creature);

    // Tap other_creature so we can verify "If you do, untap all creatures you control" does NOT fire
    runner
        .state_mut()
        .objects
        .get_mut(&other_creature)
        .unwrap()
        .tapped = true;

    // Creature deals combat damage to P1, firing Breath of Fury's trigger
    process_triggers(
        runner.state_mut(),
        &[GameEvent::CombatDamageDealtToPlayer {
            player_id: P1,
            source_amounts: vec![(creature, 1)],
            total_damage: 1,
        }],
    );

    // Positive trigger reach guard: trigger is on stack controlled by P0
    let stacked_trigger = runner
        .state()
        .stack
        .iter()
        .next()
        .expect("combat damage trigger must be placed on the stack");
    assert_eq!(
        stacked_trigger.controller, P0,
        "stacked trigger must be controlled by P0"
    );

    // While trigger is on stack, P1 gains control of `creature` (e.g. Act of Treason / Control Magic)
    {
        let obj = runner.state_mut().objects.get_mut(&creature).unwrap();
        obj.base_controller = Some(P1);
        obj.controller = P1;
    }

    // CR 704.5m / CR 303.4c: Breath of Fury enchants "creature you control".
    // When the creature is controlled by P1, Breath of Fury is illegally attached and SBA moves it to graveyard.
    let mut sba_events = Vec::new();
    check_state_based_actions(runner.state_mut(), &mut sba_events);
    assert_eq!(
        runner.state().objects[&breath_of_fury].zone,
        Zone::Graveyard,
        "Breath of Fury must be put into owner's graveyard by SBA (CR 704.5m / CR 303.4c)"
    );

    // Advance stack to resolve Breath of Fury trigger
    runner.advance_until_stack_empty();

    // Creature must NOT have been sacrificed because P0 does not control it (CR 701.21a)
    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Battlefield,
        "P1's creature must not be sacrificed on P0's trigger"
    );
    assert_eq!(
        runner.state().objects[&creature].controller,
        P1,
        "creature is still controlled by P1"
    );

    // other_creature must remain tapped (the "if you do" failed)
    assert!(
        runner.state().objects[&other_creature].tapped,
        "P0's other creature must remain tapped because sacrifice was not performed"
    );
}
