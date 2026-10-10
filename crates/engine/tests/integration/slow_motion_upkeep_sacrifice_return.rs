//! Tests for Slow Motion and recursive upkeep tax Auras.
//!
//! Oracle:
//! Enchant creature
//! At the beginning of the upkeep of enchanted creature's controller, that player sacrifices that creature unless they pay {2}.
//! When this Aura is put into a graveyard from the battlefield, return it to its owner's hand.
//!
//! CR 701.21a (Sacrifice) + CR 704.5m (Aura unattached state-based action) + CR 118.12 (Unless payment) + CR 109.5 ("You").

use engine::game::effects::attach::attach_to;
use engine::game::game_object::AttachTarget;
use engine::game::sba::check_state_based_actions;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::triggers::{drain_order_triggers_with_identity, process_triggers};
use engine::types::ability::{ControllerRef, EffectKind, TargetFilter, TypedFilter};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;
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
            source_incarnations: vec![],
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

const BREATH_OF_FURY_VERBATIM: &str = "Enchant creature you control\n\
When enchanted creature deals combat damage to a player, sacrifice it and attach this Aura to a creature you control. If you do, untap all creatures you control and after this phase, there is an additional combat phase.";

#[derive(Debug)]
struct BreathOfFuryOutcome {
    goblin_zone: Zone,
    host: Option<ObjectId>,
    bears_tapped: Vec<bool>,
    extra_phases: usize,
    choice_offered: bool,
}

/// Breath of Fury on a goblin that deals combat damage, with `bears` tapped
/// Bears; any attach choice picks the first Bear.
fn breath_of_fury_combat_damage(cant_be_sacrificed: bool, bears: usize) -> BreathOfFuryOutcome {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::CombatDamage);
    let mut goblin_builder = scenario.add_creature(P0, "Raging Goblin", 1, 1);
    if cant_be_sacrificed {
        goblin_builder.with_static(StaticMode::Other("CantBeSacrificed".to_string()));
    }
    let goblin = goblin_builder.id();
    let bear_ids: Vec<ObjectId> = (0..bears)
        .map(|i| scenario.add_creature(P0, &format!("Bear {i}"), 2, 2).id())
        .collect();
    let breath_of_fury = scenario
        .add_enchantment_from_oracle(P0, "Breath of Fury", BREATH_OF_FURY_VERBATIM)
        .with_subtypes(vec!["Aura"])
        .with_keyword(enchant_creature_you_control())
        .id();
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), breath_of_fury, goblin);
    for bear in &bear_ids {
        runner.state_mut().objects.get_mut(bear).unwrap().tapped = true;
    }
    process_triggers(
        runner.state_mut(),
        &[GameEvent::CombatDamageDealtToPlayer {
            player_id: P1,
            source_amounts: vec![(goblin, 1)],
            total_damage: 1,
            source_incarnations: vec![],
        }],
    );
    assert!(!runner.state().stack.is_empty(), "trigger must be stacked");

    let mut choice_offered = false;
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::EffectZoneChoice {
                effect_kind: EffectKind::Attach,
                ..
            } => {
                choice_offered = true;
                let pick = bear_ids.first().copied().unwrap_or(goblin);
                runner
                    .act(GameAction::SelectCards { cards: vec![pick] })
                    .expect("attach choice accepted");
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    let state = runner.state();
    BreathOfFuryOutcome {
        goblin_zone: state.objects[&goblin].zone,
        host: match state.objects[&breath_of_fury].attached_to {
            Some(AttachTarget::Object(id)) => Some(id),
            _ => None,
        },
        bears_tapped: bear_ids.iter().map(|b| state.objects[b].tapped).collect(),
        extra_phases: state.extra_phases.len(),
        choice_offered,
    }
}

/// CR 118.12: "If you do" after "sacrifice it and attach this Aura" needs the attach.
#[test]
fn breath_of_fury_if_you_do_needs_the_attach() {
    let attached = breath_of_fury_combat_damage(false, 1);
    assert_eq!(attached.goblin_zone, Zone::Graveyard);
    assert!(attached.host.is_some(), "{attached:?}");
    assert_eq!(attached.bears_tapped, vec![false], "{attached:?}");
    assert_eq!(attached.extra_phases, 1, "{attached:?}");

    let no_host = breath_of_fury_combat_damage(false, 0);
    assert_eq!(no_host.goblin_zone, Zone::Graveyard);
    assert_eq!(no_host.host, None);
    assert_eq!(no_host.extra_phases, 0, "{no_host:?}");
}

/// CR 118.12: "If you do" after "sacrifice it and attach this Aura" needs the sacrifice.
#[test]
fn breath_of_fury_if_you_do_needs_the_sacrifice() {
    let sacrificed = breath_of_fury_combat_damage(false, 1);
    assert_eq!(sacrificed.bears_tapped, vec![false], "{sacrificed:?}");
    assert_eq!(sacrificed.extra_phases, 1, "{sacrificed:?}");

    let refused = breath_of_fury_combat_damage(true, 1);
    assert_eq!(refused.goblin_zone, Zone::Battlefield);
    assert!(refused.choice_offered, "{refused:?}");
    assert_eq!(refused.bears_tapped, vec![true], "{refused:?}");
    assert_eq!(refused.extra_phases, 0, "{refused:?}");
}

/// CR 608.2c: a host chosen after the trigger paused still lets the rider read the attach.
#[test]
fn breath_of_fury_chosen_host_runs_the_if_you_do_rider() {
    let outcome = breath_of_fury_combat_damage(false, 2);
    assert!(outcome.choice_offered, "{outcome:?}");
    assert_eq!(outcome.goblin_zone, Zone::Graveyard);
    assert_eq!(outcome.bears_tapped, vec![false, false], "{outcome:?}");
    assert_eq!(outcome.extra_phases, 1, "{outcome:?}");
}

/// CR 400.7: a Breath of Fury that went to the graveyard is a new object and is not attached.
#[test]
fn breath_of_fury_in_graveyard_is_not_attached() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::CombatDamage);
    let creature = scenario.add_creature(P0, "Raging Goblin", 1, 1).id();
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let breath_of_fury = scenario
        .add_enchantment_from_oracle(P0, "Breath of Fury", BREATH_OF_FURY_VERBATIM)
        .with_subtypes(vec!["Aura"])
        .with_keyword(enchant_creature_you_control())
        .id();
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), breath_of_fury, creature);
    process_triggers(
        runner.state_mut(),
        &[GameEvent::CombatDamageDealtToPlayer {
            player_id: P1,
            source_amounts: vec![(creature, 1)],
            total_damage: 1,
            source_incarnations: vec![],
        }],
    );
    assert!(
        !runner.state().stack.is_empty(),
        "reach-guard: Breath of Fury's combat damage trigger is on the stack"
    );
    {
        let obj = runner.state_mut().objects.get_mut(&creature).unwrap();
        obj.base_controller = Some(P1);
        obj.controller = P1;
    }
    let mut sba_events = Vec::new();
    check_state_based_actions(runner.state_mut(), &mut sba_events);
    assert_eq!(
        runner.state().objects[&breath_of_fury].zone,
        Zone::Graveyard
    );

    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty(), "the trigger resolved");

    let state = runner.state();
    assert_ne!(
        state.objects[&breath_of_fury].attached_to,
        Some(AttachTarget::Object(bear))
    );
    assert!(!state.objects[&bear].attachments.contains(&breath_of_fury));
}

const LAT_NAMS_LEGACY_ORACLE: &str = "Shuffle a card from your hand into your library. If you do, draw two cards at the beginning of the next turn's upkeep.";

/// Casts Lat-Nam's Legacy with `hand` other cards in hand; returns (delayed
/// triggers, card choices offered).
fn cast_lat_nams_legacy(hand: usize) -> (usize, usize) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for i in 0..hand {
        scenario.add_card_to_hand(P0, &format!("Card {i}"));
    }
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Lat-Nam's Legacy", false, LAT_NAMS_LEGACY_ORACLE)
        .id();
    scenario.with_mana_pool(
        P0,
        [ManaType::Colorless, ManaType::Blue]
            .into_iter()
            .map(|ty| ManaUnit::new(ty, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast");
    let mut offered = 0;
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            WaitingFor::EffectZoneChoice { cards, .. } => {
                offered += 1;
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![cards[0]],
                    })
                    .expect("pick");
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    (runner.state().delayed_triggers.len(), offered)
}

/// CR 118.12: a shuffle-in that waits on the card choice still counts once it is performed.
#[test]
fn lat_nams_legacy_chosen_card_schedules_the_draw() {
    assert_eq!(cast_lat_nams_legacy(0), (0, 0), "empty hand");
    assert_eq!(cast_lat_nams_legacy(2), (1, 1), "two cards");
    assert_eq!(cast_lat_nams_legacy(3), (1, 1), "three cards");
}
