#![allow(unused_imports)]
use super::*;

/// CR 510.1: Unblocked attacker deals combat damage to defending player
#[test]
fn unblocked_attacker_deals_damage_to_player() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker_id = scenario.add_creature(P0, "Bear", 2, 2).id();
    let mut runner = scenario.build();

    run_combat(&mut runner, vec![attacker_id], vec![]);

    let state = runner.state();
    let p1_life = state.players.iter().find(|p| p.id == P1).unwrap().life;
    assert_eq!(
        p1_life, 18,
        "Defending player should take 2 damage from unblocked 2/2"
    );
}

/// CR 510.1c: Blocked creature and blocker exchange damage
#[test]
fn blocked_creature_and_blocker_exchange_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker_id = scenario.add_creature(P0, "Centaur", 3, 3).id();
    let blocker_id = scenario.add_creature(P1, "Bear", 2, 2).id();
    let mut runner = scenario.build();

    run_combat(
        &mut runner,
        vec![attacker_id],
        vec![(blocker_id, attacker_id)],
    );

    let state = runner.state();
    // Blocker (2/2) took 3 damage (lethal) -- should be in graveyard after SBAs
    assert!(
        !state.battlefield.contains(&blocker_id),
        "2/2 blocker should die to 3 damage"
    );
    // Attacker (3/3) took 2 damage -- survives
    let attacker = &state.objects[&attacker_id];
    assert_eq!(
        attacker.damage_marked, 2,
        "3/3 attacker should have 2 damage marked"
    );
    assert!(
        state.battlefield.contains(&attacker_id),
        "3/3 attacker should survive with 2 damage"
    );
}

/// CR 702.45a: Bushido pumps the Bushido creature when it becomes blocked.
#[test]
fn bushido_becomes_blocked_pumps_attacker_not_blocker() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker_id = scenario
        .add_creature(P0, "Ronin", 2, 2)
        .from_oracle_text_with_keywords(&["bushido"], "Bushido 2")
        .id();
    let blocker_id = scenario.add_creature(P1, "Bear", 2, 2).id();
    let mut runner = scenario.build();

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker_id, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("Bushido creature should be able to attack");
    // CR 508.2: Active player gets priority after attackers before blockers.
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(blocker_id, attacker_id)],
        })
        .expect("blocker should be able to block the Bushido creature");

    assert_eq!(
        runner.state().stack.len(),
        1,
        "becomes-blocked Bushido trigger should be on the stack"
    );
    runner.resolve_top();

    let state = runner.state();
    assert_eq!(state.objects[&attacker_id].power, Some(4));
    assert_eq!(state.objects[&attacker_id].toughness, Some(4));
    assert_eq!(state.objects[&blocker_id].power, Some(2));
    assert_eq!(state.objects[&blocker_id].toughness, Some(2));
}

/// CR 509.3c: "Whenever this creature becomes blocked" triggers ONLY ONCE per
/// combat, even when multiple creatures block it. A Bushido 2 creature that is
/// double-blocked must end at +2/+2 (→ 4/4), not +4/+4 (→ 6/6) from firing once
/// per blocker.
#[test]
fn bushido_becomes_blocked_fires_once_when_double_blocked() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker_id = scenario
        .add_creature(P0, "Ronin", 2, 2)
        .from_oracle_text_with_keywords(&["bushido"], "Bushido 2")
        .id();
    let blocker_a = scenario.add_creature(P1, "Bear A", 2, 2).id();
    let blocker_b = scenario.add_creature(P1, "Bear B", 2, 2).id();
    let mut runner = scenario.build();

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker_id, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("Bushido creature should be able to attack");
    // CR 508.2: Active player gets priority after attackers before blockers.
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(blocker_a, attacker_id), (blocker_b, attacker_id)],
        })
        .expect("both blockers should be able to block the Bushido creature");

    // CR 509.3c: exactly one becomes-blocked trigger, regardless of blocker count.
    assert_eq!(
        runner.state().stack.len(),
        1,
        "becomes-blocked Bushido trigger fires once per combat, not once per blocker"
    );
    runner.resolve_top();

    let state = runner.state();
    assert_eq!(state.objects[&attacker_id].power, Some(4));
    assert_eq!(state.objects[&attacker_id].toughness, Some(4));
}

/// CR 509.3d: "Whenever this creature becomes blocked by a creature" triggers
/// once for each creature that blocks it.
#[test]
fn becomes_blocked_by_creature_fires_for_each_blocker() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker_id = scenario
        .add_creature(P0, "Acolyte of the Inferno", 2, 2)
        .from_oracle_text(
            "Whenever Acolyte of the Inferno becomes blocked by a creature, \
             Acolyte of the Inferno deals 2 damage to that creature.",
        )
        .id();
    let blocker_a = scenario.add_creature(P1, "Bear A", 3, 3).id();
    let blocker_b = scenario.add_creature(P1, "Bear B", 3, 3).id();
    let mut runner = scenario.build();

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker_id, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("trigger source should be able to attack");
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(blocker_a, attacker_id), (blocker_b, attacker_id)],
        })
        .expect("both blockers should be able to block the trigger source");

    match &runner.state().waiting_for {
        WaitingFor::OrderTriggers { player, triggers } => {
            assert_eq!(*player, P0);
            assert_eq!(
                triggers.len(),
                2,
                "CR 509.3d: by-a-creature trigger fires once for each blocker"
            );
        }
        other => panic!("expected CR 603.3b OrderTriggers for two blocker triggers, got {other:?}"),
    }

    runner
        .act(GameAction::OrderTriggers { order: vec![0, 1] })
        .expect("submitting trigger order should succeed");
    runner.advance_until_stack_empty();

    let state = runner.state();
    assert_eq!(state.objects[&blocker_a].damage_marked, 2);
    assert_eq!(state.objects[&blocker_b].damage_marked, 2);
}

/// CR 509.1h + CR 509.3d + CR 603.7: Venom's "blocks or becomes blocked by a
/// non-Wall creature, destroy the other creature at end of combat" — the
/// originally reported bug (a compound blocks-or-becomes-blocked trigger with a
/// blocker filter silently dropping "or becomes blocked", and, independently,
/// the runtime resolving "the other creature" to the wrong object).
///
/// Reverting the compound-mode dispatch (`oracle_trigger.rs`) → the trigger
/// parses as plain `Blocks` and never fires when Venom's host is blocked as an
/// attacker, so no delayed trigger is scheduled (first assert flips). Reverting
/// the `blocked_attacker_from_event` leading arm (`targeting.rs`) → "the other
/// creature" resolves to Venom's host instead of the blocker, so the delayed
/// trigger targets the wrong object and the wrong creature dies (both zone
/// asserts flip).
///
/// Sizing: attacker and blocker are 1/5 so both survive combat damage — the
/// blocker's death must come from Venom's end-of-combat destroy, not combat.
///
/// (The real card is an Aura using "enchanted creature"; the scenario harness
/// has no aura-attach helper, so the runtime paths — compound matcher,
/// per-blocker filtered event emission, ParentTarget resolution, delayed-trigger
/// scheduling — are exercised via the self-referential `SelfRef` form. The
/// verbatim "enchanted creature" (`AttachedTo`) parse and the delayed-destroy
/// AST shape are covered by the parser test
/// `trigger_blocks_or_becomes_blocked_venom_parses_delayed_destroy`.)
#[test]
fn venom_destroys_the_creature_that_blocked_it_at_end_of_combat() {
    use engine::types::ability::{DelayedTriggerCondition, Effect};

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario
        .add_creature(P0, "Venom", 1, 5)
        .from_oracle_text(
            "Whenever Venom blocks or becomes blocked by a non-Wall creature, \
             destroy the other creature at end of combat.",
        )
        .id();
    let blocker = scenario.add_creature(P1, "Bear", 1, 5).id();
    let mut runner = scenario.build();

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("Venom creature should be able to attack");
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(blocker, attacker)],
        })
        .expect("Bear should be able to block");
    runner.advance_until_stack_empty();

    // CR 603.7: resolving the compound trigger creates a delayed "at end of
    // combat, destroy the other creature" trigger. Its presence proves the
    // compound mode fired from the becomes-blocked event and was NOT silently
    // dropped: reverting the compound-mode dispatch parses Venom as plain
    // `Blocks`, whose matcher requires the source to be the *blocker* — Venom's
    // host is the attacker, so it never fires and no delayed trigger exists.
    // ("The other creature" is lowered to a tracked-set target; the object it
    // actually resolves to is proven by the graveyard assertions below.)
    let scheduled_destroy = runner.state().delayed_triggers.iter().any(|dt| {
        matches!(
            dt.condition,
            DelayedTriggerCondition::AtNextPhase {
                phase: Phase::EndCombat
            }
        ) && matches!(&dt.ability.effect, Effect::Destroy { .. })
    });
    assert!(
        scheduled_destroy,
        "Venom must schedule an end-of-combat destroy (compound mode must fire, not be \
         dropped to plain Blocks); delayed_triggers = {:?}",
        runner.state().delayed_triggers
    );

    // Advance through combat damage into End of Combat so the delayed trigger
    // fires. Both creatures are 1/5, so neither dies to combat damage — the
    // blocker's death must come from Venom's end-of-combat destroy. Pass
    // priority (resolving the delayed trigger when it lands on the stack) until
    // the blocker leaves the battlefield or the loop bound is hit.
    for _ in 0..40 {
        if runner.state().objects[&blocker].zone == Zone::Graveyard {
            break;
        }
        if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
            runner.advance_until_stack_empty();
        } else if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }

    assert_eq!(
        runner.state().objects.get(&blocker).map(|o| o.zone),
        Some(Zone::Graveyard),
        "the blocker must be destroyed at end of combat"
    );
    assert_eq!(
        runner.state().objects.get(&attacker).map(|o| o.zone),
        Some(Zone::Battlefield),
        "the Venom host (attacker) must survive — Venom destroys the OTHER creature, \
         never its own host"
    );
}

/// CR 509.3d + CR 608.2k: Quagmire Lamprey's "becomes blocked by a creature,
/// put a -1/-1 counter on that creature" — the pre-existing bug where the
/// counter landed on Quagmire itself (its own host / the attacker) instead of
/// the blocker. Fixed as an in-scope byproduct of re-typing the per-blocker
/// event so `blocked_attacker_from_event` returns the blocker.
///
/// Reverting the `blocked_attacker_from_event` leading arm → the -1/-1 counter
/// lands on Quagmire (attacker) instead of the Bear (blocker): both asserts flip.
#[test]
fn quagmire_lamprey_puts_minus_counter_on_blocker_not_itself() {
    use engine::types::counter::CounterType;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario
        .add_creature(P0, "Quagmire Lamprey", 2, 2)
        .from_oracle_text(
            "Whenever Quagmire Lamprey becomes blocked by a creature, \
             put a -1/-1 counter on that creature.",
        )
        .id();
    let blocker = scenario.add_creature(P1, "Bear", 3, 3).id();
    let mut runner = scenario.build();

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("Quagmire Lamprey should be able to attack");
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(blocker, attacker)],
        })
        .expect("Bear should be able to block");
    runner.advance_until_stack_empty();

    let state = runner.state();
    assert_eq!(
        state.objects[&blocker]
            .counters
            .get(&CounterType::Minus1Minus1)
            .copied(),
        Some(1),
        "the -1/-1 counter must land on the blocker (Bear)"
    );
    assert_eq!(
        state.objects[&attacker]
            .counters
            .get(&CounterType::Minus1Minus1)
            .copied(),
        None,
        "Quagmire Lamprey (the attacker/host) must NOT receive its own -1/-1 counter"
    );
}

#[test]
fn decayed_attacker_sacrifices_at_end_of_combat() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker_id = scenario
        .add_creature(P0, "Decayed Zombie", 2, 2)
        .with_keyword(Keyword::Decayed)
        .id();
    let mut runner = scenario.build();

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker_id, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("decayed creature should be able to attack");

    assert!(
        runner.state().stack.len() == 1,
        "decayed attack trigger should be on the stack"
    );

    for _ in 0..40 {
        if runner.state().objects[&attacker_id].zone == Zone::Graveyard {
            break;
        }
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { .. }
        ) {
            runner
                .act(GameAction::DeclareBlockers {
                    assignments: vec![],
                })
                .expect("declaring no blockers should succeed");
        } else if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }

    assert_eq!(runner.state().objects[&attacker_id].zone, Zone::Graveyard);
}

/// CR 510.1b: First strike damage resolves before regular damage
#[test]
fn first_strike_kills_before_regular_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker_id = {
        let mut b = scenario.add_creature(P0, "Knight", 2, 2);
        b.first_strike();
        b.id()
    };
    let blocker_id = scenario.add_creature(P1, "Bear", 3, 2).id();
    let mut runner = scenario.build();

    run_combat(
        &mut runner,
        vec![attacker_id],
        vec![(blocker_id, attacker_id)],
    );

    let state = runner.state();
    // First strike 2/2 deals 2 to blocker with toughness 2 = lethal.
    // Blocker dies before dealing regular damage.
    assert!(
        !state.battlefield.contains(&blocker_id),
        "Blocker should die to first strike damage before dealing regular damage"
    );
    assert_eq!(
        state.objects[&attacker_id].damage_marked, 0,
        "First strike attacker should take 0 damage (blocker died before regular step)"
    );

    // Snapshot for regression anchoring
    insta::assert_json_snapshot!(
        "combat_first_strike_kills_before_regular",
        runner.snapshot()
    );
}

/// CR 510.1c: Double strike deals damage in both steps
#[test]
fn double_strike_deals_damage_in_both_steps() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker_id = {
        let mut b = scenario.add_creature(P0, "Champion", 3, 3);
        b.double_strike();
        b.id()
    };
    let blocker_id = scenario.add_creature(P1, "Rhino", 5, 5).id();
    let mut runner = scenario.build();

    run_combat(
        &mut runner,
        vec![attacker_id],
        vec![(blocker_id, attacker_id)],
    );

    let state = runner.state();
    // Double strike 3/3 deals 3 in first strike step + 3 in regular step = 6 total
    // 6 >= 5 toughness = lethal, blocker should die
    assert!(
        !state.battlefield.contains(&blocker_id),
        "5/5 blocker should die to 6 total damage from double strike 3/3"
    );
}

/// CR 702.2b: Defender can't attack
#[test]
fn defender_cannot_attack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wall_id = {
        let mut b = scenario.add_creature(P0, "Wall", 0, 4);
        b.defender();
        b.id()
    };
    let mut runner = scenario.build();

    // Pass priority to get to DeclareAttackers
    runner.pass_both_players();

    // Trying to declare a defender as attacker should fail
    let result = runner.act(GameAction::DeclareAttackers {
        attacks: vec![(wall_id, AttackTarget::Player(P1))],
        bands: vec![],
    });
    assert!(
        result.is_err(),
        "Creature with Defender should not be able to attack"
    );
}

/// CR 510.1: Multiple attackers and blockers resolve correctly
#[test]
fn multiple_attackers_mixed_blocking() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker1 = scenario.add_creature(P0, "Centaur", 3, 3).id();
    let attacker2 = scenario.add_creature(P0, "Bear", 2, 2).id();
    let blocker = scenario.add_creature(P1, "Guard", 2, 2).id();
    let mut runner = scenario.build();

    // One blocker blocks attacker1, attacker2 is unblocked
    run_combat(
        &mut runner,
        vec![attacker1, attacker2],
        vec![(blocker, attacker1)],
    );

    // Unblocked attacker2 (2/2) deals 2 damage to P1
    assert_eq!(
        runner.life(P1),
        18,
        "Unblocked 2/2 should deal 2 damage to defending player"
    );

    // Blocked exchange: 3/3 vs 2/2 -- blocker dies, attacker takes 2 damage
    let state = runner.state();
    assert!(
        !state.battlefield.contains(&blocker),
        "2/2 blocker should die to 3/3 attacker"
    );
    assert_eq!(
        state.objects[&attacker1].damage_marked, 2,
        "3/3 attacker should have 2 damage from blocker"
    );

    // Snapshot for regression anchoring
    insta::assert_json_snapshot!(
        "combat_multiple_attackers_mixed_blocking",
        runner.snapshot()
    );
}

/// CR 510.1: Attacker taps when attacking (no vigilance)
#[test]
fn attacker_taps_when_attacking() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker_id = scenario.add_creature(P0, "Bear", 2, 2).id();
    let mut runner = scenario.build();

    // Pass priority to get to DeclareAttackers
    runner.pass_both_players();

    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker_id, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    assert!(
        runner.state().objects[&attacker_id].tapped,
        "Attacker without vigilance should be tapped after declaring attack"
    );
}

/// CR 603.2 + CR 704.3: DamageReceived triggers fire even when the source creature
/// dies from the same combat damage (triggers are collected before SBAs destroy it).
/// Regression test for Jackal Pup / Boros Reckoner pattern.
#[test]
fn damage_received_trigger_fires_when_creature_dies() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, QuantityExpr, QuantityRef, TargetFilter,
        TriggerDefinition,
    };
    use engine::types::triggers::TriggerMode;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P0 attacks with a vanilla 1/1 — it will die to the blocker
    let attacker_id = scenario.add_creature(P0, "Goblin", 1, 1).id();

    // P1 blocks with a "Jackal Pup" — 2/1 with DamageReceived trigger that deals
    // that much damage to its controller (P1).
    let pup_trigger = TriggerDefinition::new(TriggerMode::DamageReceived)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DealDamage {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
                target: TargetFilter::Controller,
                damage_source: None,
                excess: None,
            },
        ))
        .valid_card(TargetFilter::SelfRef)
        .trigger_zones(vec![Zone::Battlefield]);

    let pup_id = {
        let mut b = scenario.add_creature(P1, "Jackal Pup", 2, 1);
        b.with_trigger_definition(pup_trigger);
        b.id()
    };

    let mut runner = scenario.build();

    run_combat(&mut runner, vec![attacker_id], vec![(pup_id, attacker_id)]);

    // After combat damage, both creatures die (1 toughness each).
    // The trigger should be on the stack — resolve it.
    runner.resolve_top();

    // Jackal Pup took 1 damage from the 1/1 attacker, so its trigger should deal
    // 1 damage to P1 (its controller).
    assert_eq!(
        runner.life(P1),
        19,
        "Jackal Pup's DamageReceived trigger should deal 1 damage to its controller"
    );

    // Verify both creatures died
    assert!(
        !runner.state().battlefield.contains(&attacker_id),
        "1/1 attacker should die to 2 damage from Jackal Pup"
    );
    assert!(
        !runner.state().battlefield.contains(&pup_id),
        "Jackal Pup (2/1) should die to 1 damage from attacker"
    );
}

/// CR 603.10a: Dies triggers (leaves-the-battlefield) fire from graveyard scan
/// after combat damage. The ZoneChanged events from SBAs are processed by
/// run_post_action_pipeline when auto_advance returns Priority after CombatDamage.
#[test]
fn dies_trigger_fires_from_combat_damage() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter, TriggerDefinition,
    };
    use engine::types::triggers::TriggerMode;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let attacker_id = scenario.add_creature(P0, "Bear", 3, 3).id();

    // P1 creature with "When this creature dies, you gain 3 life."
    let dies_trigger = TriggerDefinition::new(TriggerMode::ChangesZone)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: engine::types::ability::TargetFilter::Controller,
            },
        ))
        .valid_card(TargetFilter::SelfRef)
        .origin(Zone::Battlefield)
        .destination(Zone::Graveyard)
        .trigger_zones(vec![Zone::Graveyard]);

    let blocker_id = {
        let mut b = scenario.add_creature(P1, "Doomed Traveler", 1, 1);
        b.with_trigger_definition(dies_trigger);
        b.id()
    };

    let mut runner = scenario.build();

    run_combat(
        &mut runner,
        vec![attacker_id],
        vec![(blocker_id, attacker_id)],
    );

    // CR 510.4: After combat damage, players receive priority. The dies trigger
    // is placed on the stack by run_post_action_pipeline processing ZoneChanged events.
    // Resolve the trigger by passing priority.
    runner.resolve_top();

    assert!(
        !runner.state().battlefield.contains(&blocker_id),
        "1/1 blocker should die to 3 damage"
    );

    // P1 started at 20, blocker died → trigger grants 3 life → 23
    assert_eq!(
        runner.life(P1),
        23,
        "Dies trigger should fire and grant 3 life to controller"
    );
}

// ---------------------------------------------------------------------------
// CR 508.1d + CR 508.1h + CR 509.1c + CR 509.1d: Combat tax (UnlessPay) family
// ---------------------------------------------------------------------------

use engine::parser::oracle_static::parse_static_line;
use engine::types::card_type::CoreType as Core;
use engine::types::game_state::CombatTaxContext;
use engine::types::mana::{ManaColor, ManaCostShard};

fn add_ghostly_prison(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    // Ghostly Prison is an Enchantment (no P/T). Use a 2/2 creature shell only
    // so `add_creature` gives us a live permanent without SBAs killing it
    // (a 0/0 creature dies to CR 704.5f on the first state-based check after
    // entering). The test asserts only on the Prison's static-driven tax
    // behavior, not the source's card type.
    let def = parse_static_line(
        "Creatures can't attack you unless their controller pays {2} for each creature they control that's attacking you.",
    )
    .expect("Ghostly Prison should parse");
    let mut builder = scenario.add_creature(player, "Ghostly Prison", 2, 2);
    builder.with_static_definition(def);
    builder.id()
}

/// CR 508.1d + CR 508.1h: Ghostly Prison on defender's side with two attackers
/// computes a {4} total tax (two creatures × {2}). Accepting pays the mana and
/// completes the attack.
#[test]
fn ghostly_prison_accept_pays_tax_and_attacks_proceed() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Defender controls Ghostly Prison.
    let _prison = add_ghostly_prison(&mut scenario, P1);
    // Attacker has two bears.
    let a1 = scenario.add_creature(P0, "Bear 1", 2, 2).id();
    let a2 = scenario.add_creature(P0, "Bear 2", 2, 2).id();
    // Attacker has 4 Plains for the tax.
    for _ in 0..4 {
        scenario.add_basic_land(P0, ManaColor::White);
    }
    let mut runner = scenario.build();
    runner.pass_both_players();

    let attacks = vec![
        (a1, AttackTarget::Player(P1)),
        (a2, AttackTarget::Player(P1)),
    ];
    runner
        .act(GameAction::DeclareAttackers {
            attacks,
            bands: vec![],
        })
        .expect("DeclareAttackers should pause with CombatTaxPayment");

    // Verify we're paused with the right total ({4}) and two per-creature entries.
    match &runner.state().waiting_for {
        WaitingFor::CombatTaxPayment {
            player,
            context,
            total_cost,
            per_creature,
            ..
        } => {
            assert_eq!(*player, P0, "active player owes the tax");
            assert!(matches!(context, CombatTaxContext::Attacking));
            assert_eq!(total_cost.mana_value(), 4);
            assert_eq!(per_creature.len(), 2);
        }
        other => panic!("expected CombatTaxPayment, got {other:?}"),
    }

    // Tap four Plains for mana (ManaPayment is not required here — pay_unless_cost
    // goes through the unified mana-payment pipeline).
    // Simpler path: accept and let the engine draw from the mana pool. We need
    // mana available — tap the lands by activating their mana abilities.
    let plains: Vec<ObjectId> = runner
        .state()
        .battlefield
        .iter()
        .filter(|&&id| {
            let obj = runner.state().objects.get(&id).unwrap();
            obj.controller == P0 && obj.card_types.core_types.contains(&Core::Land)
        })
        .copied()
        .collect();
    for land in plains {
        runner
            .act(GameAction::TapLandForMana { object_id: land })
            .ok();
    }

    // Accept the tax.
    runner
        .act(GameAction::PayCombatTax { accept: true })
        .expect("PayCombatTax accept should succeed");

    // The attack should now be declared — attackers are tapped (unless vigilance).
    let state = runner.state();
    assert!(
        state.combat.is_some(),
        "Combat state must be populated after tax paid"
    );
    let combat = state.combat.as_ref().unwrap();
    assert_eq!(combat.attackers.len(), 2);
}

fn add_sphere_of_safety(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    let def = parse_static_line(
        "Creatures can't attack you or planeswalkers you control unless their controller pays {X} for each of those creatures, where X is the number of enchantments you control.",
    )
    .expect("Sphere of Safety should parse");
    let mut builder = scenario.add_creature(player, "Sphere of Safety", 2, 2);
    builder.as_enchantment().with_static_definition(def);
    builder.id()
}

fn add_enchantment(scenario: &mut GameScenario, player: PlayerId, name: &str) -> ObjectId {
    scenario
        .add_creature(player, name, 2, 2)
        .as_enchantment()
        .id()
}

/// CR 508.1h + CR 202.3e: Sphere of Safety — {X} must be concretized from
/// enchantment count before computing attack tax (issue #3865).
#[test]
fn sphere_of_safety_attack_tax_scales_with_enchantment_count() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let _sphere = add_sphere_of_safety(&mut scenario, P1);
    let _other = add_enchantment(&mut scenario, P1, "Other Aura");
    let attacker = scenario.add_creature(P0, "Bear", 2, 2).id();
    for _ in 0..2 {
        scenario.add_basic_land(P0, ManaColor::White);
    }

    let mut runner = scenario.build();
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("Sphere of Safety should pause for combat tax");

    match &runner.state().waiting_for {
        WaitingFor::CombatTaxPayment { total_cost, .. } => {
            assert_eq!(total_cost.mana_value(), 2, "two enchantments → X=2 tax");
        }
        other => panic!("expected CombatTaxPayment, got {other:?}"),
    }
}

/// CR 508.1d + CR 509.1c: Declining the tax drops the taxed attackers. With
/// Ghostly Prison on defender and only two taxed attackers, decline → zero
/// attackers → combat ends (CR 508.8).
#[test]
fn ghostly_prison_decline_removes_taxed_attackers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let _prison = add_ghostly_prison(&mut scenario, P1);
    let a1 = scenario.add_creature(P0, "Bear 1", 2, 2).id();
    let a2 = scenario.add_creature(P0, "Bear 2", 2, 2).id();
    let mut runner = scenario.build();
    runner.pass_both_players();

    let attacks = vec![
        (a1, AttackTarget::Player(P1)),
        (a2, AttackTarget::Player(P1)),
    ];
    runner
        .act(GameAction::DeclareAttackers {
            attacks,
            bands: vec![],
        })
        .expect("DeclareAttackers should pause with CombatTaxPayment");

    // Decline the tax.
    runner
        .act(GameAction::PayCombatTax { accept: false })
        .expect("PayCombatTax decline should succeed");

    // CR 508.8: No attackers remain → combat ends.
    let state = runner.state();
    assert!(
        state.combat.is_none() || state.combat.as_ref().unwrap().attackers.is_empty(),
        "After declining the tax, no attackers should remain"
    );
    // The attackers should not be tapped (tap is only applied after tax is paid
    // per CR 508.1f).
    let a1_obj = &state.objects[&a1];
    let a2_obj = &state.objects[&a2];
    assert!(
        !a1_obj.tapped && !a2_obj.tapped,
        "declined attackers stay untapped"
    );
}

/// CR 508.1d + issue #1303: Summon: Yojimbo chapters II/III grant a transient
/// combat tax via `GrantStaticAbility`. The tax must reach `compute_combat_tax`
/// when attackers declare against the saga's controller.
#[test]
fn issue_1303_yojimbo_chapter_combat_tax_requires_payment() {
    use engine::game::effects::effect::resolve;
    use engine::game::layers::evaluate_layers;
    use engine::parser::oracle_effect::parse_effect;
    use engine::types::ability::{Duration, Effect, PlayerScope, ResolvedAbility};

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let saga = scenario.add_creature(P1, "Summon: Yojimbo", 1, 1).id();
    let attacker = scenario.add_creature(P0, "Bear", 2, 2).id();
    for _ in 0..2 {
        scenario.add_basic_land(P0, ManaColor::White);
    }

    let effect = parse_effect(
        "Until your next turn, creatures can't attack you unless their controller pays {2} for each of those creatures.",
    );
    assert!(
        matches!(effect, Effect::GenericEffect { .. }),
        "Yojimbo chapter tax must parse to GenericEffect, got {effect:?}"
    );

    let ability =
        ResolvedAbility::new(effect, vec![], saga, P1).duration(Duration::UntilNextTurnOf {
            player: PlayerScope::Controller,
        });

    let mut runner = scenario.build();
    resolve(runner.state_mut(), &ability, &mut Vec::new()).expect("resolve tax grant");
    evaluate_layers(runner.state_mut());
    runner.pass_both_players();

    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("attack declaration should pause for combat tax");

    match &runner.state().waiting_for {
        WaitingFor::CombatTaxPayment {
            player,
            total_cost,
            per_creature,
            ..
        } => {
            assert_eq!(*player, P0);
            assert_eq!(total_cost.mana_value(), 2);
            assert_eq!(per_creature.len(), 1);
        }
        other => panic!("expected CombatTaxPayment, got {other:?}"),
    }
}

/// CR 508.1h: Two Ghostly Prisons stacked aggregate to {4} per attacker.
#[test]
fn two_prisons_stack_tax() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let _p1 = add_ghostly_prison(&mut scenario, P1);
    let _p2 = add_ghostly_prison(&mut scenario, P1);
    let a1 = scenario.add_creature(P0, "Bear", 2, 2).id();
    let mut runner = scenario.build();
    runner.pass_both_players();

    let attacks = vec![(a1, AttackTarget::Player(P1))];
    runner
        .act(GameAction::DeclareAttackers {
            attacks,
            bands: vec![],
        })
        .expect("DeclareAttackers should pause with CombatTaxPayment");

    match &runner.state().waiting_for {
        WaitingFor::CombatTaxPayment { total_cost, .. } => {
            // 1 attacker × {2} × 2 prisons = {4}.
            assert_eq!(total_cost.mana_value(), 4);
        }
        other => panic!("expected CombatTaxPayment, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// CR 508.1d + CR 702.36 + CR 117.5: Norn's Annex regression (L9-52).
// User-reported deadlock when Norn's Annex is in play. Class covers ~5
// Phyrexian-cost combat-tax statics (Norn's Annex specifically). The end-to-end
// flow MUST yield WaitingFor::CombatTaxPayment, accept the {W/P} cost via the
// shared mana-payment pipeline (auto-deciding mana-vs-life), and complete the
// attack without entering an infinite loop or returning a non-progress state.
// ---------------------------------------------------------------------------

fn add_norns_annex(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    // Norn's Annex is an Artifact (no P/T). Mirrors `add_ghostly_prison` —
    // use a 2/2 creature shell so SBAs (CR 704.5f) don't kill it. Test asserts
    // only on the Annex's static-driven Phyrexian tax, not on its card type.
    let def = parse_static_line(
        "Creatures can't attack you or planeswalkers you control unless their controller pays {W/P} for each of those creatures.",
    )
    .expect("Norn's Annex should parse");
    let mut builder = scenario.add_creature(player, "Norn's Annex", 2, 2);
    builder.with_static_definition(def);
    builder.id()
}

/// CR 508.1d + CR 702.36: Norn's Annex with one attacker — engine pauses with a
/// {W/P}-cost CombatTaxPayment. Accepting auto-pays a Plains (CR 107.4f auto-
/// decide path: prefer mana). The attack proceeds and the engine yields a
/// non-deadlock waiting state.
#[test]
fn norns_annex_accept_pays_phyrexian_with_mana() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Defender (P1) controls Norn's Annex.
    let _annex = add_norns_annex(&mut scenario, P1);
    // Active player has one attacker plus a Plains for the {W/P}-as-mana payment.
    let attacker = scenario.add_creature(P0, "Bear", 2, 2).id();
    scenario.add_basic_land(P0, ManaColor::White);
    let mut runner = scenario.build();
    runner.pass_both_players();

    let attacks = vec![(attacker, AttackTarget::Player(P1))];
    runner
        .act(GameAction::DeclareAttackers {
            attacks,
            bands: vec![],
        })
        .expect("DeclareAttackers should pause with CombatTaxPayment");

    // Verify the engine paused with the right Phyrexian-cost tax (mana_value 1).
    match &runner.state().waiting_for {
        WaitingFor::CombatTaxPayment {
            player,
            context,
            total_cost,
            per_creature,
            ..
        } => {
            assert_eq!(*player, P0, "active player owes the tax");
            assert!(matches!(context, CombatTaxContext::Attacking));
            // CR 202.3g: {W/P} contributes mana_value 1.
            assert_eq!(total_cost.mana_value(), 1);
            assert_eq!(per_creature.len(), 1);
        }
        other => panic!("expected CombatTaxPayment, got {other:?}"),
    }

    // Tap the Plains so the auto-decide path prefers mana (CR 107.4f).
    let plains: Vec<ObjectId> = runner
        .state()
        .battlefield
        .iter()
        .filter(|&&id| {
            let obj = runner.state().objects.get(&id).unwrap();
            obj.controller == P0 && obj.card_types.core_types.contains(&Core::Land)
        })
        .copied()
        .collect();
    for land in plains {
        runner
            .act(GameAction::TapLandForMana { object_id: land })
            .ok();
    }

    runner
        .act(GameAction::PayCombatTax { accept: true })
        .expect("PayCombatTax accept must succeed (engine must not deadlock)");

    // CR 508.1f: After tax is paid, the attack is finalized.
    let state = runner.state();
    assert!(
        state.combat.is_some(),
        "Combat state must be populated after Norn's Annex tax paid"
    );
    let combat = state.combat.as_ref().unwrap();
    assert_eq!(combat.attackers.len(), 1);
    // CR 117.5: Engine must yield a progress-capable WaitingFor (not a deadlock).
    assert!(
        !matches!(state.waiting_for, WaitingFor::CombatTaxPayment { .. }),
        "engine must advance past CombatTaxPayment after acceptance, got {:?}",
        state.waiting_for
    );
}

/// CR 508.1d + CR 702.36 + CR 118.3: Norn's Annex accept path with no white
/// mana — the auto-decide path falls back to paying 2 life per Phyrexian shard
/// (CR 107.4f). Engine must not deadlock; life is deducted; attack finalizes.
#[test]
fn norns_annex_accept_pays_phyrexian_with_life_when_no_mana() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let _annex = add_norns_annex(&mut scenario, P1);
    // Single attacker, no Plains — life-payment fallback path.
    let attacker = scenario.add_creature(P0, "Bear", 2, 2).id();
    let mut runner = scenario.build();
    runner.pass_both_players();

    let life_before = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .unwrap()
        .life;

    let attacks = vec![(attacker, AttackTarget::Player(P1))];
    runner
        .act(GameAction::DeclareAttackers {
            attacks,
            bands: vec![],
        })
        .expect("DeclareAttackers should pause with CombatTaxPayment");

    runner
        .act(GameAction::PayCombatTax { accept: true })
        .expect("PayCombatTax accept must succeed via life payment (no deadlock)");

    let state = runner.state();
    let life_after = state.players.iter().find(|p| p.id == P0).unwrap().life;
    // CR 107.4f + CR 118.3b: One {W/P} paid as life ⇒ 2 life lost.
    assert_eq!(
        life_after,
        life_before - 2,
        "Phyrexian shard auto-pays 2 life when mana unavailable"
    );
    assert!(state.combat.is_some());
    assert_eq!(state.combat.as_ref().unwrap().attackers.len(), 1);
    assert!(
        !matches!(state.waiting_for, WaitingFor::CombatTaxPayment { .. }),
        "engine must advance past CombatTaxPayment after acceptance, got {:?}",
        state.waiting_for
    );
}

/// CR 508.1d + CR 702.36: Norn's Annex decline path — drop the taxed attacker.
/// Mirrors `ghostly_prison_decline_removes_taxed_attackers` for Phyrexian costs.
#[test]
fn norns_annex_decline_drops_taxed_attackers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let _annex = add_norns_annex(&mut scenario, P1);
    let attacker = scenario.add_creature(P0, "Bear", 2, 2).id();
    let mut runner = scenario.build();
    runner.pass_both_players();

    let attacks = vec![(attacker, AttackTarget::Player(P1))];
    runner
        .act(GameAction::DeclareAttackers {
            attacks,
            bands: vec![],
        })
        .expect("DeclareAttackers should pause with CombatTaxPayment");

    runner
        .act(GameAction::PayCombatTax { accept: false })
        .expect("PayCombatTax decline must succeed");

    let state = runner.state();
    assert!(
        state.combat.is_none() || state.combat.as_ref().unwrap().attackers.is_empty(),
        "After declining the Norn's Annex tax, no attackers should remain"
    );
    assert!(
        !state.objects[&attacker].tapped,
        "declined attacker stays untapped"
    );
}

// ---------------------------------------------------------------------------
// CR 508.1d + CR 508.1h + CR 611.3a + CR 118.12a: Archangel of Tithes (#309)
// and Propaganda multiplayer (#302) — end-to-end regression coverage.
//
// These integration tests exercise the full DeclareAttackers → CombatTaxPayment
// → PayCombatTax pipeline using the real parsed Oracle text for each card.
// Unit-level coverage of `compute_attack_tax` lives in
// `crates/engine/src/game/combat.rs`; these tests verify wiring between the
// parser, runtime, and waiting-for state machine.
// ---------------------------------------------------------------------------

/// Build an Archangel of Tithes with both verified Oracle statics attached.
///
/// Verified Oracle text (client/public/card-data.json, 2026-05-10):
/// > Flying
/// > As long as this creature is untapped, creatures can't attack you or
/// > planeswalkers you control unless their controller pays {1} for each of
/// > those creatures.
/// > As long as this creature is attacking, creatures can't block unless their
/// > controller pays {1} for each of those creatures.
fn add_archangel_of_tithes(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    let attack_tax = parse_static_line(
        "As long as this creature is untapped, creatures can't attack you or planeswalkers you control unless their controller pays {1} for each of those creatures.",
    )
    .expect("Archangel of Tithes attack-tax static should parse");
    let block_tax = parse_static_line(
        "As long as this creature is attacking, creatures can't block unless their controller pays {1} for each of those creatures.",
    )
    .expect("Archangel of Tithes block-tax static should parse");
    let mut builder = scenario.add_creature(player, "Archangel of Tithes", 3, 5);
    builder.with_static_definition(attack_tax);
    builder.with_static_definition(block_tax);
    builder.id()
}

/// CR 508.1d + CR 508.1h + CR 118.12a: Issue #309 regression — Archangel of
/// Tithes' first static taxes opponent attacks against its controller while it
/// is untapped. Engine must pause with a `CombatTaxPayment` of `{1}` per
/// attacker, scoped to attacks against the Archangel's controller.
#[test]
fn archangel_of_tithes_untapped_taxes_opponent_attacks() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // P1 controls an untapped Archangel of Tithes.
    let _archangel = add_archangel_of_tithes(&mut scenario, P1);
    // P0 (active player) attacks P1 with a single bear.
    let attacker = scenario.add_creature(P0, "Bear", 2, 2).id();
    let mut runner = scenario.build();
    runner.pass_both_players();

    let attacks = vec![(attacker, AttackTarget::Player(P1))];
    runner
        .act(GameAction::DeclareAttackers {
            attacks,
            bands: vec![],
        })
        .expect("DeclareAttackers should pause with CombatTaxPayment (#309)");

    match &runner.state().waiting_for {
        WaitingFor::CombatTaxPayment {
            player,
            context,
            total_cost,
            per_creature,
            ..
        } => {
            assert_eq!(*player, P0, "active player owes the tax");
            assert!(matches!(context, CombatTaxContext::Attacking));
            assert_eq!(total_cost.mana_value(), 1, "{{1}} per attacker");
            assert_eq!(per_creature.len(), 1);
        }
        other => panic!("expected CombatTaxPayment, got {other:?}"),
    }
}

/// CR 611.3a + CR 118.12a: Tapped Archangel of Tithes' attack-tax gate
/// (`Not(SourceIsTapped)`) fails, so the tax is dormant and the attack
/// proceeds without pausing. Mirrors the unit test
/// `compute_attack_tax_archangel_of_tithes_gated_by_untapped` at the
/// integration level.
#[test]
fn archangel_of_tithes_tapped_does_not_tax() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let archangel = add_archangel_of_tithes(&mut scenario, P1);
    let attacker = scenario.add_creature(P0, "Bear", 2, 2).id();
    let mut runner = scenario.build();
    // Tap the Archangel — gate fails, tax is dormant. The state mutation must
    // happen on the runner (post-build) so the tap survives any builder-side
    // re-derivation.
    runner
        .state_mut()
        .objects
        .get_mut(&archangel)
        .unwrap()
        .tapped = true;
    runner.pass_both_players();

    let attacks = vec![(attacker, AttackTarget::Player(P1))];
    runner
        .act(GameAction::DeclareAttackers {
            attacks,
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed without tax pause");

    // Attack proceeds directly — no CombatTaxPayment pause.
    let state = runner.state();
    assert!(
        !matches!(state.waiting_for, WaitingFor::CombatTaxPayment { .. }),
        "tapped Archangel must not pause for tax, got {:?}",
        state.waiting_for
    );
    assert!(state.combat.is_some());
    assert_eq!(state.combat.as_ref().unwrap().attackers.len(), 1);
}

/// CR 109.5 + CR 508.1d: "you" on Archangel of Tithes refers to its
/// controller, and the static's `Opponent` affected filter excludes that
/// controller's own creatures. The Archangel's controller can attack their
/// own opponent without paying the tax.
#[test]
fn archangel_of_tithes_controller_can_attack_own_creatures_without_tax() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // P0 (active player) controls an untapped Archangel of Tithes AND a Bear.
    let _archangel = add_archangel_of_tithes(&mut scenario, P0);
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    let mut runner = scenario.build();
    runner.pass_both_players();

    // P0 attacks P1 with their own Bear. The Archangel's tax is scoped to
    // creatures controlled by *opponents of the Archangel's controller* — the
    // Bear is controlled by the Archangel's controller, so no tax.
    let attacks = vec![(bear, AttackTarget::Player(P1))];
    runner
        .act(GameAction::DeclareAttackers {
            attacks,
            bands: vec![],
        })
        .expect("Owner of Archangel should attack without paying their own tax");

    let state = runner.state();
    assert!(
        !matches!(state.waiting_for, WaitingFor::CombatTaxPayment { .. }),
        "controller's own attack must not pause for tax, got {:?}",
        state.waiting_for
    );
    assert!(state.combat.is_some());
    assert_eq!(state.combat.as_ref().unwrap().attackers.len(), 1);
}

/// Build a Propaganda with its verified Oracle static attached.
///
/// Verified Oracle text (client/public/card-data.json, 2026-05-10):
/// > Creatures can't attack you unless their controller pays {2} for each
/// > creature they control that's attacking you.
fn add_propaganda(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    let def = parse_static_line(
        "Creatures can't attack you unless their controller pays {2} for each creature they control that's attacking you.",
    )
    .expect("Propaganda should parse");
    let mut builder = scenario.add_creature(player, "Propaganda", 2, 2);
    builder.with_static_definition(def);
    builder.id()
}

/// Build a 3-player scenario where P1 is the active attacker, Propaganda is on
/// `propaganda_owner`'s battlefield, and P1 controls a single Bear ready to
/// attack. The runner is parked in `WaitingFor::DeclareAttackers` so
/// `GameAction::DeclareAttackers` fires immediately.
fn build_3p_propaganda_scenario(propaganda_owner: PlayerId) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    let _propaganda = add_propaganda(&mut scenario, propaganda_owner);
    let attacker = scenario.add_creature(P1, "Bear", 2, 2).id();
    let mut runner = scenario.build();
    // Active attacker is P1; jump straight into the declare-attackers waiting
    // state so the test exercises only the tax pause path. The valid_*
    // collections are advisory legality hints; the engine re-validates on
    // submission via `validate_attackers`.
    let state = runner.state_mut();
    state.active_player = P1;
    state.priority_player = P1;
    state.phase = Phase::DeclareAttackers;
    state.turn_number = 2;
    state.waiting_for = WaitingFor::DeclareAttackers {
        player: P1,
        valid_attacker_ids: vec![attacker],
        valid_attack_targets: vec![AttackTarget::Player(P0), AttackTarget::Player(PlayerId(2))],
        valid_attack_targets_by_attacker: None,
        attacker_constraints: Default::default(),
    };
    (runner, attacker)
}

/// CR 508.1d + CR 109.5: Issue #302 regression — in a 3-player game, Player A
/// controls Propaganda, Player B (active) attacks Player C. Propaganda's
/// `defended` filter (its own controller, Player A) does NOT match Player C,
/// so the tax must NOT fire.
#[test]
fn propaganda_does_not_tax_attacks_against_other_opponents_3p() {
    const P2: PlayerId = PlayerId(2);

    let (mut runner, attacker) = build_3p_propaganda_scenario(P0);

    let attacks = vec![(attacker, AttackTarget::Player(P2))];
    runner
        .act(GameAction::DeclareAttackers {
            attacks,
            bands: vec![],
        })
        .expect("DeclareAttackers against P2 must not pause for P0's Propaganda (#302)");

    let state = runner.state();
    assert!(
        !matches!(state.waiting_for, WaitingFor::CombatTaxPayment { .. }),
        "Propaganda must not tax attacks against players other than its controller (#302), got {:?}",
        state.waiting_for
    );
    assert!(state.combat.is_some());
    assert_eq!(state.combat.as_ref().unwrap().attackers.len(), 1);
}

/// CR 508.1a–e + Decision 2: an ILLEGAL declaration that would otherwise incur a
/// tax must be REJECTED outright. Strict validation runs BEFORE the tax is quoted,
/// so no `CombatTaxPayment` prompt opens and nothing is tapped/committed.
///
/// Revert guard: the old order quoted the tax first and returned
/// `CombatTaxPayment` even for an invalid declaration — reverting to that makes
/// `result.is_err()` and the "no tax prompt" assertion both fail.
#[test]
fn invalid_taxed_declaration_is_rejected_before_tax_prompt() {
    let (mut runner, attacker) = build_3p_propaganda_scenario(P0);
    // CR 508.1a: a tapped creature can't be declared as an attacker — make the
    // declaration illegal while keeping it a tax target (it still attacks P0).
    runner
        .state_mut()
        .objects
        .get_mut(&attacker)
        .unwrap()
        .tapped = true;

    let result = runner.act(GameAction::DeclareAttackers {
        attacks: vec![(attacker, AttackTarget::Player(P0))],
        bands: vec![],
    });

    assert!(
        result.is_err(),
        "a tapped attacker declaration must be rejected, never taxed"
    );
    let state = runner.state();
    assert!(
        !matches!(state.waiting_for, WaitingFor::CombatTaxPayment { .. }),
        "an illegal declaration must never open a tax prompt, got {:?}",
        state.waiting_for
    );
    assert!(
        state.combat.is_none(),
        "no combat state may be created when the declaration is rejected"
    );
}

/// Paired positive reach-guard: the SAME taxed attack, when LEGAL, DOES open the
/// tax prompt — proving the input reaches the tax seam so the negative above is
/// not passing vacuously on an unrelated short-circuit.
#[test]
fn valid_taxed_declaration_opens_tax_prompt() {
    let (mut runner, attacker) = build_3p_propaganda_scenario(P0);
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P0))],
            bands: vec![],
        })
        .expect("a legal taxed declaration should pause for payment, not error");
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::CombatTaxPayment { .. }),
        "a legal attack against Propaganda's controller must open the tax prompt, got {:?}",
        state.waiting_for
    );
    assert!(
        state.combat.is_none(),
        "the tax pause must not tap or commit before payment (CR 508.1f deferred)"
    );
}

/// CR 508.1d + CR 508.1h: Sanity companion to #302 — in the same 3-player
/// setup, when Player B attacks Player A (Propaganda's controller), the tax
/// DOES fire and the engine pauses with `CombatTaxPayment` of `{2}`.
#[test]
fn propaganda_taxes_attacks_against_its_controller_3p() {
    let (mut runner, attacker) = build_3p_propaganda_scenario(P0);

    let attacks = vec![(attacker, AttackTarget::Player(P0))];
    runner
        .act(GameAction::DeclareAttackers {
            attacks,
            bands: vec![],
        })
        .expect("DeclareAttackers against P0 must pause with CombatTaxPayment");

    match &runner.state().waiting_for {
        WaitingFor::CombatTaxPayment {
            player, total_cost, ..
        } => {
            assert_eq!(*player, P1, "attacker (P1) owes the tax");
            assert_eq!(total_cost.mana_value(), 2, "{{2}} per attacker");
        }
        other => panic!("expected CombatTaxPayment, got {other:?}"),
    }
}

// ===========================================================================
// Step G — engine-owned declare-attackers legality regression matrix.
//
// Every test drives the production `GameAction::DeclareAttackers` /
// `GameAction::PayCombatTax` path through `GameRunner`; the engine re-derives
// legality from the live `AttackDeclarationConstraints` model on each
// submission (the `valid_*` hints on the manually-parked `WaitingFor` are
// advisory only). Every negative is paired with a positive reach-guard in the
// same test so no assertion passes vacuously on an upstream short-circuit.
// ===========================================================================

use engine::types::ability::StaticDefinition;
use engine::types::identifiers::{ObjectIncarnationRef, LEGACY_INCARNATION};
use engine::types::statics::{
    AttackDefenderScope, CombatAloneAction, CombatAloneRequirement, StaticMode,
};

const P2: PlayerId = PlayerId(2);

/// Park a freshly-built 3-player runner in P0's declare-attackers step so
/// `GameAction::DeclareAttackers` fires immediately. Mirrors
/// `build_3p_propaganda_scenario`; the `valid_*` hints are advisory (the engine
/// re-validates from the live constraints model).
fn park_3p_declare(runner: &mut GameRunner, attackers: &[ObjectId]) {
    let state = runner.state_mut();
    state.active_player = P0;
    state.priority_player = P0;
    state.phase = Phase::DeclareAttackers;
    state.turn_number = 2;
    state.waiting_for = WaitingFor::DeclareAttackers {
        player: P0,
        valid_attacker_ids: attackers.to_vec(),
        valid_attack_targets: vec![AttackTarget::Player(P1), AttackTarget::Player(P2)],
        valid_attack_targets_by_attacker: None,
        attacker_constraints: Default::default(),
    };
}

/// Tap every land P0 controls for mana (used to fund a combat-tax accept).
fn tap_all_p0_lands(runner: &mut GameRunner) {
    let lands: Vec<ObjectId> = runner
        .state()
        .battlefield
        .iter()
        .filter(|&&id| {
            let obj = runner.state().objects.get(&id).unwrap();
            obj.controller == P0 && obj.card_types.core_types.contains(&Core::Land)
        })
        .copied()
        .collect();
    for land in lands {
        runner
            .act(GameAction::TapLandForMana { object_id: land })
            .ok();
    }
}

/// CR 508.1d flagship: two INCOMPATIBLE `MustAttackPlayer` requirements on one
/// creature (it can attack only one player). The maximum attainable requirement
/// count is 1, so a declaration attacking one required player is legal even
/// though the other requirement is unmet — this is exactly the incompatible-
/// requirements bug the CR 508.1d solver fixes.
///
/// Revert guard: the old validator required EVERY `MustAttackPlayer` target
/// individually, so attacking only P1 was rejected (P2's lure unmet). Reverting
/// `score >= max_no_payment` flips `attack_one_is_legal` back to an error.
#[test]
fn incompatible_must_attack_player_accepts_max_score_declaration() {
    fn setup() -> (GameRunner, ObjectId) {
        let mut scenario = GameScenario::new_n_player(3, 42);
        let attacker = {
            let mut b = scenario.add_creature(P0, "Doubly Lured Bear", 2, 2);
            b.with_static_definition(StaticDefinition::new(StaticMode::MustAttackPlayer {
                player: P1,
            }));
            b.with_static_definition(StaticDefinition::new(StaticMode::MustAttackPlayer {
                player: P2,
            }));
            b.id()
        };
        let mut runner = scenario.build();
        park_3p_declare(&mut runner, &[attacker]);
        (runner, attacker)
    }

    // Flagship positive: satisfying the maximum attainable count (1) by attacking
    // ONE required player is legal, even though the other lure is unmet.
    let (mut legal, attacker) = setup();
    legal
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("CR 508.1d: attacking one of two incompatible required players is legal");
    assert!(
        legal.state().combat.is_some(),
        "the max-score declaration must commit"
    );

    // Paired negative reach-guard: declaring ZERO attackers scores 0 < max (1),
    // so the requirement is still enforced — the fix did not simply drop it.
    let (mut illegal, _attacker) = setup();
    let empty = illegal.act(GameAction::DeclareAttackers {
        attacks: vec![],
        bands: vec![],
    });
    assert!(
        empty.is_err(),
        "a lower-score (empty) declaration must be rejected — the requirement still binds"
    );
}

/// CR 508.1c + CR 508.5: a global `MaxAttackersEachCombat { max: 1 }` cap rejects
/// a two-attacker declaration and accepts a one-attacker declaration.
#[test]
fn global_attacker_cap_rejects_over_cap_accepts_at_cap() {
    fn setup() -> (GameRunner, ObjectId, ObjectId) {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        {
            let mut b = scenario.add_creature(P1, "Cap Enchantment", 2, 2);
            b.as_enchantment();
            b.with_static_definition(StaticDefinition::new(StaticMode::MaxAttackersEachCombat {
                max: 1,
                defender: None,
            }));
            b.id();
        }
        let a1 = scenario.add_creature(P0, "Bear 1", 2, 2).id();
        let a2 = scenario.add_creature(P0, "Bear 2", 2, 2).id();
        let mut runner = scenario.build();
        runner.pass_both_players();
        (runner, a1, a2)
    }

    // Positive reach-guard: one attacker is within the cap → legal and committed.
    // `combat` is already `Some` at the DeclareAttackers step (set at BeginCombat),
    // so committal is proven by an attacker landing in `combat.attackers`, not by
    // `combat.is_some()` (which would be vacuously true here).
    let (mut ok, a1, _a2) = setup();
    ok.act(GameAction::DeclareAttackers {
        attacks: vec![(a1, AttackTarget::Player(P1))],
        bands: vec![],
    })
    .expect("one attacker is within the global cap");
    assert_eq!(
        ok.state()
            .combat
            .as_ref()
            .map(|c| c.attackers.len())
            .unwrap_or(0),
        1,
        "the within-cap attacker must commit"
    );

    // Negative: two attackers exceed the global cap of 1 → rejected, nothing commits.
    let (mut bad, a1, a2) = setup();
    let res = bad.act(GameAction::DeclareAttackers {
        attacks: vec![
            (a1, AttackTarget::Player(P1)),
            (a2, AttackTarget::Player(P1)),
        ],
        bands: vec![],
    });
    assert!(res.is_err(), "two attackers exceed a global cap of 1");
    assert!(
        bad.state()
            .combat
            .as_ref()
            .is_none_or(|c| c.attackers.is_empty()),
        "a rejected over-cap declaration commits no attackers"
    );
}

/// CR 508.5 + CR 802.1: a per-defender cap (`defender: Some(Controller)`) on P1
/// limits only attacks against P1. Two creatures may not both attack P1, but one
/// attacking P1 while the other attacks P2 is legal.
#[test]
fn per_defender_cap_limits_only_that_defender() {
    fn setup() -> (GameRunner, ObjectId, ObjectId) {
        let mut scenario = GameScenario::new_n_player(3, 42);
        {
            let mut b = scenario.add_creature(P1, "Judoon Enforcers", 2, 2);
            b.with_static_definition(StaticDefinition::new(StaticMode::MaxAttackersEachCombat {
                max: 1,
                defender: Some(AttackDefenderScope::Controller),
            }));
            b.id();
        }
        let a1 = scenario.add_creature(P0, "Bear 1", 2, 2).id();
        let a2 = scenario.add_creature(P0, "Bear 2", 2, 2).id();
        let mut runner = scenario.build();
        park_3p_declare(&mut runner, &[a1, a2]);
        (runner, a1, a2)
    }

    // Positive reach-guard: split across the two defenders → each defender sees
    // one attacker, within P1's per-defender cap.
    let (mut ok, a1, a2) = setup();
    ok.act(GameAction::DeclareAttackers {
        attacks: vec![
            (a1, AttackTarget::Player(P1)),
            (a2, AttackTarget::Player(P2)),
        ],
        bands: vec![],
    })
    .expect("one attacker each against P1 and P2 respects P1's per-defender cap");
    assert!(ok.state().combat.is_some());

    // Negative: both against P1 exceed P1's cap of 1 (P2's freedom is irrelevant).
    let (mut bad, a1, a2) = setup();
    let res = bad.act(GameAction::DeclareAttackers {
        attacks: vec![
            (a1, AttackTarget::Player(P1)),
            (a2, AttackTarget::Player(P1)),
        ],
        bands: vec![],
    });
    assert!(
        res.is_err(),
        "two attackers against P1 exceed P1's per-defender cap"
    );
}

/// Build a 2-player runner parked at DeclareAttackers with a `CombatAlone`
/// attacker plus a vanilla companion.
fn setup_combat_alone(req: CombatAloneRequirement) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sole = {
        let mut b = scenario.add_creature(P0, "Combat-Alone Creature", 2, 2);
        b.with_static_definition(StaticDefinition::new(StaticMode::CombatAlone {
            action: CombatAloneAction::Attack,
            requirement: req,
        }));
        b.id()
    };
    let companion = scenario.add_creature(P0, "Companion", 2, 2).id();
    let mut runner = scenario.build();
    runner.pass_both_players();
    (runner, sole, companion)
}

/// CR 508.5: `CombatAlone { MustBeSole }` (Master of Cruelties class) — the
/// creature may attack alone but not alongside a companion.
#[test]
fn combat_alone_must_be_sole_rejects_companion() {
    // Positive reach-guard: attacking alone is legal.
    let (mut ok, sole, _c) = setup_combat_alone(CombatAloneRequirement::MustBeSole);
    ok.act(GameAction::DeclareAttackers {
        attacks: vec![(sole, AttackTarget::Player(P1))],
        bands: vec![],
    })
    .expect("a MustBeSole creature may attack alone");
    assert_eq!(ok.state().combat.as_ref().unwrap().attackers.len(), 1);

    // Negative: attacking with a companion violates MustBeSole.
    let (mut bad, sole, companion) = setup_combat_alone(CombatAloneRequirement::MustBeSole);
    let res = bad.act(GameAction::DeclareAttackers {
        attacks: vec![
            (sole, AttackTarget::Player(P1)),
            (companion, AttackTarget::Player(P1)),
        ],
        bands: vec![],
    });
    assert!(
        res.is_err(),
        "MustBeSole forbids attacking alongside a companion"
    );
}

/// CR 508.5: `CombatAlone { NeedsCompanion }` ("can't attack alone") — the
/// creature may attack only alongside at least one other attacker.
#[test]
fn combat_alone_needs_companion_rejects_solo_attack() {
    // Negative: attacking alone is illegal.
    let (mut bad, sole, _c) = setup_combat_alone(CombatAloneRequirement::NeedsCompanion);
    let res = bad.act(GameAction::DeclareAttackers {
        attacks: vec![(sole, AttackTarget::Player(P1))],
        bands: vec![],
    });
    assert!(res.is_err(), "a NeedsCompanion creature can't attack alone");

    // Positive reach-guard: attacking WITH a companion is legal — proves the
    // creature can otherwise attack, so the solo rejection isn't vacuous.
    let (mut ok, sole, companion) = setup_combat_alone(CombatAloneRequirement::NeedsCompanion);
    ok.act(GameAction::DeclareAttackers {
        attacks: vec![
            (sole, AttackTarget::Player(P1)),
            (companion, AttackTarget::Player(P1)),
        ],
        bands: vec![],
    })
    .expect("a NeedsCompanion creature may attack alongside a companion");
    assert_eq!(ok.state().combat.as_ref().unwrap().attackers.len(), 2);
}

/// CR 506.2 + CR 508.1a: a scoped `CantAttack` "can't attack its owner" bars only
/// the owner as a defender; an allowed sibling target (another opponent) stays
/// reachable and a legal declaration against it is accepted.
#[test]
fn scoped_cant_attack_owner_bars_only_the_owner() {
    fn setup() -> (GameRunner, ObjectId) {
        let mut scenario = GameScenario::new_n_player(3, 42);
        // Owned by P1, controlled by P0 (a Xantcha-style loaned attacker) with a
        // "can't attack its owner" restriction on itself.
        let attacker = {
            let mut b = scenario.add_creature(P1, "Owner-Restricted Bear", 2, 2);
            b.with_static_definition(
                StaticDefinition::new(StaticMode::CantAttack)
                    .affected(engine::types::ability::TargetFilter::SelfRef)
                    .attack_defended(Some(engine::types::triggers::AttackTargetFilter::Owner)),
            );
            b.id()
        };
        let mut runner = scenario.build();
        // Direct field assignment (not a control-change event) so the creature
        // keeps its non-summoning-sick status while its owner stays P1.
        runner
            .state_mut()
            .objects
            .get_mut(&attacker)
            .unwrap()
            .controller = P0;
        park_3p_declare(&mut runner, &[attacker]);
        (runner, attacker)
    }

    // Negative: attacking the owner (P1) is barred by the scoped restriction.
    let (mut bad, attacker) = setup();
    let res = bad.act(GameAction::DeclareAttackers {
        attacks: vec![(attacker, AttackTarget::Player(P1))],
        bands: vec![],
    });
    assert!(
        res.is_err(),
        "the loaned creature can't attack its owner (P1)"
    );

    // Positive reach-guard: the allowed sibling target (non-owner P2) is legal —
    // proving the restriction is scoped to the owner, not a blanket CantAttack.
    let (mut ok, attacker) = setup();
    ok.act(GameAction::DeclareAttackers {
        attacks: vec![(attacker, AttackTarget::Player(P2))],
        bands: vec![],
    })
    .expect("attacking a non-owner opponent (P2) is allowed");
    assert!(ok.state().combat.is_some());
}

/// Build a 2-player runner paused at `CombatTaxPayment` for a single Ghostly
/// Prison-taxed attacker (tax {2}); returns the paused runner + attacker.
fn paused_single_taxed_attacker() -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let _prison = add_ghostly_prison(&mut scenario, P1);
    let attacker = scenario.add_creature(P0, "Bear", 2, 2).id();
    for _ in 0..2 {
        scenario.add_basic_land(P0, ManaColor::White);
    }
    let mut runner = scenario.build();
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("a legal taxed attack must pause for payment");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::CombatTaxPayment { .. }
        ),
        "reach-guard: the declaration must reach the tax-pause seam"
    );
    (runner, attacker)
}

/// CR 400.7 + CR 508.1k identity: a creature that leaves and re-enters the
/// battlefield during the tax pause is a NEW object. On accept, only refs whose
/// incarnation still matches the live object become attackers — the stale
/// snapshot is dropped, so the reincarnated creature does not attack.
///
/// Revert guard: without the `obj.incarnation == ref.incarnation` check in
/// `commit_attack_declaration_from_snapshot`, the stale snapshot would still
/// commit and the (bumped) attacker would attack — flipping both branches below.
#[test]
fn reincarnated_attacker_dropped_on_tax_accept_but_stable_ref_commits() {
    // Positive control: with NO reincarnation, the stable ref commits normally.
    let (mut stable, attacker) = paused_single_taxed_attacker();
    tap_all_p0_lands(&mut stable);
    stable
        .act(GameAction::PayCombatTax { accept: true })
        .expect("accept pays the tax");
    assert!(
        stable
            .state()
            .combat
            .as_ref()
            .is_some_and(|c| c.attackers.iter().any(|a| a.object_id == attacker)),
        "an unchanged-incarnation attacker must attack after paying the tax"
    );

    // Negative: bump the incarnation (simulate leave+re-enter, CR 400.7) before
    // accepting — the snapshot ref no longer matches, so the creature is dropped.
    let (mut reincarnated, attacker) = paused_single_taxed_attacker();
    reincarnated
        .state_mut()
        .objects
        .get_mut(&attacker)
        .unwrap()
        .incarnation += 1;
    tap_all_p0_lands(&mut reincarnated);
    reincarnated
        .act(GameAction::PayCombatTax { accept: true })
        .expect("accept still succeeds even though the stale ref is dropped");
    let state = reincarnated.state();
    let is_attacking = state
        .combat
        .as_ref()
        .is_some_and(|c| c.attackers.iter().any(|a| a.object_id == attacker));
    assert!(
        !is_attacking,
        "CR 508.1k: a reincarnated attacker must NOT attack from the old snapshot"
    );
    assert!(
        !state.objects[&attacker].tapped,
        "a dropped attacker is never tapped (commit skips it)"
    );
}

/// CR 508.1d + Decision 2: declining an attack tax discards the whole proposal
/// and rebuilds a fresh `DeclareAttackers` prompt — nothing is tapped, no combat
/// is committed, and no attacker is silently retained.
///
/// Revert guard: the old decline path filtered taxed attackers and continued
/// combat; the fresh-prompt assertion fails if that behavior is restored.
#[test]
fn decline_attack_tax_returns_fresh_declare_attackers_prompt() {
    let (mut runner, attacker) = paused_single_taxed_attacker();
    runner
        .act(GameAction::PayCombatTax { accept: false })
        .expect("declining the tax must succeed");
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::DeclareAttackers { .. }),
        "declining must rebuild a fresh DeclareAttackers prompt, got {:?}",
        state.waiting_for
    );
    // `combat` remains `Some` (set at BeginCombat, and the fresh prompt is still in
    // the combat phase); the contract is that NO attacker was committed on decline.
    assert!(
        state.combat.as_ref().is_none_or(|c| c.attackers.is_empty()),
        "no attacker may be committed when the tax is declined"
    );
    assert!(
        !state.objects[&attacker].tapped,
        "a declined attacker stays untapped (CR 508.1f deferred, then abandoned)"
    );
}

/// Payload contract serde: the new `valid_attack_targets_by_attacker` field is
/// `None` for legacy saves (absent → fallback) and `Some(_)` when authoritative
/// (explicit empty is distinct from absent, per the field contract).
#[test]
fn valid_attack_targets_by_attacker_absent_is_none_present_empty_is_authoritative() {
    use std::collections::HashMap;

    // None: `skip_serializing_if` omits the field; a legacy save (field absent)
    // deserializes back to None (consumers fall back to the aggregate).
    let none_wf = WaitingFor::DeclareAttackers {
        player: P0,
        valid_attacker_ids: vec![],
        valid_attack_targets: vec![],
        valid_attack_targets_by_attacker: None,
        attacker_constraints: Default::default(),
    };
    let none_json = serde_json::to_string(&none_wf).unwrap();
    assert!(
        !none_json.contains("valid_attack_targets_by_attacker"),
        "None must be omitted for legacy compatibility: {none_json}"
    );
    match serde_json::from_str::<WaitingFor>(&none_json).unwrap() {
        WaitingFor::DeclareAttackers {
            valid_attack_targets_by_attacker,
            ..
        } => assert!(
            valid_attack_targets_by_attacker.is_none(),
            "an absent field must deserialize to None (legacy fallback)"
        ),
        other => panic!("expected DeclareAttackers, got {other:?}"),
    }

    // Some(empty): serialized (authoritative) and distinct from None.
    let empty_wf = WaitingFor::DeclareAttackers {
        player: P0,
        valid_attacker_ids: vec![],
        valid_attack_targets: vec![],
        valid_attack_targets_by_attacker: Some(HashMap::new()),
        attacker_constraints: Default::default(),
    };
    let empty_json = serde_json::to_string(&empty_wf).unwrap();
    assert!(
        empty_json.contains("valid_attack_targets_by_attacker"),
        "Some(empty) must be serialized (authoritative), not omitted: {empty_json}"
    );
    match serde_json::from_str::<WaitingFor>(&empty_json).unwrap() {
        WaitingFor::DeclareAttackers {
            valid_attack_targets_by_attacker,
            ..
        } => {
            let map = valid_attack_targets_by_attacker
                .expect("an explicit empty map must deserialize to Some, not None");
            assert!(map.is_empty(), "the authoritative empty map has no keys");
        }
        other => panic!("expected DeclareAttackers, got {other:?}"),
    }
}

/// CR 400.7 legacy mid-tax save: a pre-migration `CombatTaxPending::Attack`
/// stored bare `ObjectId` numbers. The `ObjectIncarnationRefCompat` shim maps a
/// legacy bare id to `LEGACY_INCARNATION`, which can never match a live
/// incarnation — so those attackers are safely dropped on resume. A full
/// `{ object_id, incarnation }` record round-trips unchanged.
#[test]
fn legacy_object_incarnation_ref_deserializes_to_sentinel() {
    // Pre-migration bare-id record.
    let legacy: ObjectIncarnationRef = serde_json::from_str("7").unwrap();
    assert_eq!(legacy.object_id, ObjectId(7));
    assert_eq!(
        legacy.incarnation, LEGACY_INCARNATION,
        "a legacy bare-id ref must bind the sentinel so it never matches a live incarnation"
    );

    // New full record round-trips with its real incarnation.
    let full: ObjectIncarnationRef =
        serde_json::from_str(r#"{"object_id":7,"incarnation":3}"#).unwrap();
    assert_eq!(full.object_id, ObjectId(7));
    assert_eq!(full.incarnation, 3);
    assert_ne!(
        full.incarnation, LEGACY_INCARNATION,
        "a full record keeps its real incarnation, distinct from the legacy sentinel"
    );
}

/// CR 508.1d (final sentence): "If a creature can't attack unless a player pays a
/// cost, that player is not required to pay that cost." A creature whose only way
/// to satisfy its `MustAttackPlayer` lure is a TAXED attack does not raise the
/// no-payment maximum — so declaring NO attackers is legal (the player is never
/// forced to pay the tax to satisfy the requirement).
///
/// Revert guard: if `max_no_payment` counted taxed attacks (or the old validator
/// required the lure regardless of tax), the empty declaration would score 0 < 1
/// and be rejected — flipping `empty_is_legal`.
#[test]
fn must_attack_whose_only_target_is_taxed_does_not_force_payment() {
    fn setup() -> (GameRunner, ObjectId) {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        // P1 controls Ghostly Prison, so any attack against P1 is taxed {2}.
        let _prison = add_ghostly_prison(&mut scenario, P1);
        // The attacker is lured to attack P1 — but P1 is the only legal target
        // and it is taxed, so no FREE declaration satisfies the lure.
        let attacker = {
            let mut b = scenario.add_creature(P0, "Lured Bear", 2, 2);
            b.with_static_definition(StaticDefinition::new(StaticMode::MustAttackPlayer {
                player: P1,
            }));
            b.id()
        };
        let mut runner = scenario.build();
        runner.pass_both_players();
        (runner, attacker)
    }

    // Flagship: declaring NO attackers is legal — the max attainable no-payment
    // requirement count is 0, because the only satisfying attack is taxed.
    let (mut empty, _attacker) = setup();
    empty
        .act(GameAction::DeclareAttackers {
            attacks: vec![],
            bands: vec![],
        })
        .expect("CR 508.1d: a lure satisfiable only by a taxed attack must not force payment");

    // Positive reach-guard: the lured attack against P1 genuinely IS taxed — it
    // opens the tax prompt. This proves the requirement's only satisfaction is
    // taxed, so `max_no_payment == 0` is correct and the empty pass isn't vacuous.
    let (mut taxed, attacker) = setup();
    taxed
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("the lured attack should pause for the tax it is voluntarily paying");
    assert!(
        matches!(
            taxed.state().waiting_for,
            WaitingFor::CombatTaxPayment { .. }
        ),
        "the only way to obey the lure is a taxed attack (reach-guard for the empty case)"
    );
}

/// CR 701.15b/c: two DISTINCT goaders create two INDEPENDENT `Goad` requirements
/// ("attack a player other than the goader"), not one collapsed requirement. In a
/// 4-player game a creature goaded by P1 and P2 obeys the most requirements
/// (generic + both goads = 3) only by attacking the non-goader P3; attacking a
/// goader obeys just 2, below the attainable maximum, so it is illegal.
///
/// Revert guard: if the two goaders collapsed into a single `Goad`, the max would
/// be 2 and attacking P1 would tie it and be wrongly accepted. `attack_goader`'s
/// `is_err()` flips when goad scoring is not per-goader.
#[test]
fn multiple_goaders_score_independently() {
    const P3: PlayerId = PlayerId(3);

    fn setup() -> (GameRunner, ObjectId) {
        let mut scenario = GameScenario::new_n_player(4, 42);
        let attacker = scenario.add_creature(P0, "Twice-Goaded Ogre", 3, 3).id();
        let mut runner = scenario.build();
        {
            // Goaded by two distinct players → two independent Goad requirements.
            let obj = runner.state_mut().objects.get_mut(&attacker).unwrap();
            obj.goaded_by.insert(P1);
            obj.goaded_by.insert(P2);
        }
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.phase = Phase::DeclareAttackers;
        state.turn_number = 2;
        state.waiting_for = WaitingFor::DeclareAttackers {
            player: P0,
            valid_attacker_ids: vec![attacker],
            valid_attack_targets: vec![
                AttackTarget::Player(P1),
                AttackTarget::Player(P2),
                AttackTarget::Player(P3),
            ],
            valid_attack_targets_by_attacker: None,
            attacker_constraints: Default::default(),
        };
        (runner, attacker)
    }

    // Positive: attacking the non-goader P3 obeys generic + BOTH goads = 3 (max).
    let (mut ok, attacker) = setup();
    ok.act(GameAction::DeclareAttackers {
        attacks: vec![(attacker, AttackTarget::Player(P3))],
        bands: vec![],
    })
    .expect("attacking the non-goader obeys the maximum requirement count (CR 701.15c)");
    assert!(
        ok.state().combat.is_some(),
        "the max-score declaration must commit"
    );

    // Negative: attacking a goader (P1) obeys only generic + Goad{P2} = 2 < 3,
    // below the attainable maximum (P3 is a legal non-goader target) → illegal.
    let (mut bad, attacker) = setup();
    let res = bad.act(GameAction::DeclareAttackers {
        attacks: vec![(attacker, AttackTarget::Player(P1))],
        bands: vec![],
    });
    assert!(
        res.is_err(),
        "attacking a goader scores below the max — both goad requirements bind independently"
    );
    assert!(
        bad.state().combat.is_none(),
        "a rejected sub-max declaration commits no combat"
    );
}

/// Decision 1 / Finding 2 termination guard: a wide goaded board coupled by a
/// single `NeedsCompanion` creature (requirements non-empty + coupling static +
/// NO caps) is exactly the case the old plain-backtracking `best_free_declaration`
/// enumerated as `(1 + targets)^N`. With 15 creatures × 2 targets that is ~14M
/// nodes — a multi-second hang; the memoized, dominance-pruned DP solves it in a
/// handful of states. A max-score human submission must be accepted essentially
/// instantly; reverting to the naive solver regresses this to a timeout.
#[test]
fn wide_goaded_coupled_board_declares_without_blowup() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    // One creature that can't attack alone (couples the whole board), plus 14
    // goaded go-wide creatures. All are goaded by P1, so every free declaration is
    // scored and the DP's coupled path (not the separable fast path) runs.
    let lonely = {
        let mut b = scenario.add_creature(P0, "Needs A Friend", 2, 2);
        b.with_static_definition(StaticDefinition::new(StaticMode::CombatAlone {
            action: CombatAloneAction::Attack,
            requirement: CombatAloneRequirement::NeedsCompanion,
        }));
        b.id()
    };
    let mut wide: Vec<ObjectId> = vec![lonely];
    for i in 0..14 {
        wide.push(scenario.add_creature(P0, &format!("Goaded {i}"), 2, 2).id());
    }
    let mut runner = scenario.build();
    {
        for &id in &wide {
            runner
                .state_mut()
                .objects
                .get_mut(&id)
                .unwrap()
                .goaded_by
                .insert(P1);
        }
    }
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.phase = Phase::DeclareAttackers;
        state.turn_number = 2;
        state.waiting_for = WaitingFor::DeclareAttackers {
            player: P0,
            valid_attacker_ids: wide.clone(),
            valid_attack_targets: vec![AttackTarget::Player(P1), AttackTarget::Player(P2)],
            valid_attack_targets_by_attacker: None,
            attacker_constraints: Default::default(),
        };
    }

    // Max-score declaration: every goaded creature attacks the non-goader P2
    // (obeys generic + goad); the NeedsCompanion creature has 14 companions.
    let attacks: Vec<(ObjectId, AttackTarget)> = wide
        .iter()
        .map(|&id| (id, AttackTarget::Player(P2)))
        .collect();
    runner
        .act(GameAction::DeclareAttackers {
            attacks,
            bands: vec![],
        })
        .expect(
            "the maximum-requirement wide declaration must validate without exponential blowup",
        );
    assert!(
        runner.state().combat.is_some(),
        "the wide coupled declaration commits combat"
    );
}

/// CR 508.1d + Decision 4 (affected-ness independence): a SHARED, quantity-scaled
/// tax (Sphere of Safety — {X} per creature, X = the defender's enchantment count)
/// taxes EACH attacker independently. With two creatures both lured to attack the
/// protected player, no FREE declaration satisfies either lure (both attacks are
/// taxed), so `max_no_payment == 0` and declaring zero attackers is legal — the
/// shared tax is NOT "used up" by the first affected creature.
///
/// Revert guard: if the tax were attributed only to the FIRST `per_creature` row
/// (the artifact Decision 4 eliminates), the SECOND creature would read as free,
/// `max_no_payment` would be ≥ 1, and the empty declaration would be REJECTED.
/// `empty.is_ok()` flips under that bug; the two per-attacker reach-guards prove
/// the empty pass is not vacuous (each lured attack really is taxed).
#[test]
fn shared_scaled_tax_taxes_each_attacker_independently() {
    fn setup() -> (GameRunner, ObjectId, ObjectId) {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        // Sphere of Safety on P1: attacking P1 costs {X} per creature, X = P1's
        // enchantment count (≥ 1, since the Sphere itself counts), so every attack
        // against P1 is taxed.
        let _sphere = add_sphere_of_safety(&mut scenario, P1);
        let lure = |scenario: &mut GameScenario, name: &str| {
            let mut b = scenario.add_creature(P0, name, 2, 2);
            b.with_static_definition(StaticDefinition::new(StaticMode::MustAttackPlayer {
                player: P1,
            }));
            b.id()
        };
        let c1 = lure(&mut scenario, "Lured Bear 1");
        let c2 = lure(&mut scenario, "Lured Bear 2");
        // Fund a potential tax so the pause seam is reachable (reach-guards below).
        for _ in 0..4 {
            scenario.add_basic_land(P0, ManaColor::White);
        }
        let mut runner = scenario.build();
        runner.pass_both_players();
        (runner, c1, c2)
    }

    // Flagship: BOTH lures are only satisfiable by a taxed attack, so
    // max_no_payment == 0 and declaring zero attackers is legal.
    let (mut empty, _c1, _c2) = setup();
    empty
        .act(GameAction::DeclareAttackers {
            attacks: vec![],
            bands: vec![],
        })
        .expect("CR 508.1d: a shared scaled tax on both lured attackers must not force payment");

    // Reach-guard 1: C1 attacking the protected player IS taxed (opens the prompt).
    let (mut one, c1, _c2) = setup();
    one.act(GameAction::DeclareAttackers {
        attacks: vec![(c1, AttackTarget::Player(P1))],
        bands: vec![],
    })
    .expect("a single lured attack is legal (voluntarily taxed)");
    assert!(
        matches!(one.state().waiting_for, WaitingFor::CombatTaxPayment { .. }),
        "C1 attacking the protected player is taxed"
    );

    // Reach-guard 2 (independence): C2 attacking ALONE is ALSO taxed — the
    // per-pairing verdict does not depend on which creature or on any co-attacker.
    let (mut two, _c1, c2) = setup();
    two.act(GameAction::DeclareAttackers {
        attacks: vec![(c2, AttackTarget::Player(P1))],
        bands: vec![],
    })
    .expect("a single lured attack is legal (voluntarily taxed)");
    assert!(
        matches!(two.state().waiting_for, WaitingFor::CombatTaxPayment { .. }),
        "C2 is independently taxed (not freed by C1's absence) — affected-ness independence"
    );
}

/// CR 805.10a: during a Two-Headed Giant turn the active team is the attacking
/// team, so a teammate's creature is a legal attacker in the same declaration and
/// caps/requirements apply to the COMBINED attacker set. The team-bug fix
/// (`team_eligible_attacker_ids`) lets the active player declare its teammate's
/// creature; an opposing-team creature is never eligible.
///
/// Revert guard: the pre-fix `controller == active_player` filter excluded the
/// teammate's creature, so declaring it errored. `ok`'s two-attacker commit flips
/// to an error if the team generalization is reverted.
#[test]
fn two_headed_giant_teammate_creature_can_attack_in_combined_declaration() {
    use engine::types::format::FormatConfig;
    const P3: PlayerId = PlayerId(3);

    fn setup() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
        // 2HG teams: {P0, P1} vs {P2, P3}. P0 is active; P1 is its teammate.
        let mut scenario = GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 42);
        let own = scenario.add_creature(P0, "Own Ogre", 3, 3).id();
        let teammate = scenario.add_creature(P1, "Teammate Bear", 2, 2).id();
        let opponent = scenario.add_creature(P2, "Opposing Wall", 0, 4).id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.phase = Phase::DeclareAttackers;
        state.turn_number = 2;
        state.waiting_for = WaitingFor::DeclareAttackers {
            player: P0,
            valid_attacker_ids: vec![own, teammate],
            valid_attack_targets: vec![AttackTarget::Player(P2), AttackTarget::Player(P3)],
            valid_attack_targets_by_attacker: None,
            attacker_constraints: Default::default(),
        };
        (runner, own, teammate, opponent)
    }

    // Positive: the active player's OWN creature and its TEAMMATE's creature attack
    // an opposing-team player together in one combined declaration.
    let (mut ok, own, teammate, _opp) = setup();
    ok.act(GameAction::DeclareAttackers {
        attacks: vec![
            (own, AttackTarget::Player(P2)),
            (teammate, AttackTarget::Player(P2)),
        ],
        bands: vec![],
    })
    .expect("CR 805.10a: a teammate's creature is a legal attacker on the active team's turn");
    assert!(
        ok.state()
            .combat
            .as_ref()
            .is_some_and(|c| c.attackers.len() == 2),
        "both team creatures attack in the combined declaration"
    );

    // Negative: an opposing-team creature is never eligible on the active team's turn.
    let (mut bad, _own, _teammate, opponent) = setup();
    let res = bad.act(GameAction::DeclareAttackers {
        attacks: vec![(opponent, AttackTarget::Player(P2))],
        bands: vec![],
    });
    assert!(
        res.is_err(),
        "an opposing-team creature cannot attack on the active team's turn"
    );
}

/// CR 400.7 + CR 508.1k + legacy compat: a pre-migration save stored the paused
/// attack snapshot with bare `ObjectId`s, which deserialize through
/// `ObjectIncarnationRefCompat::Legacy` to `LEGACY_INCARNATION` (`u64::MAX`). That
/// sentinel can never match a live object's incarnation, so on tax-accept the
/// legacy attacker is dropped — it does not attack from the stale snapshot. This is
/// the end-to-end accept-path counterpart to the sentinel-serde unit test.
///
/// Revert guard: without the `obj.incarnation == ref.incarnation` check in
/// `commit_attack_declaration_from_snapshot`, the `LEGACY_INCARNATION` ref would
/// still commit and the attacker would attack. `!is_attacking` flips.
#[test]
fn legacy_injected_combat_tax_snapshot_drops_attacker_on_accept() {
    use engine::types::game_state::CombatTaxPending;

    // Positive control: a live-incarnation ref commits normally (paired baseline).
    let (mut live, attacker) = paused_single_taxed_attacker();
    tap_all_p0_lands(&mut live);
    live.act(GameAction::PayCombatTax { accept: true })
        .expect("accept pays the tax");
    assert!(
        live.state()
            .combat
            .as_ref()
            .is_some_and(|c| c.attackers.iter().any(|a| a.object_id == attacker)),
        "a live-incarnation snapshot commits the attacker"
    );

    // Legacy injection: rewrite the paused snapshot ref to the LEGACY sentinel that
    // a pre-migration bare-`ObjectId` save deserializes to.
    let (mut legacy, attacker) = paused_single_taxed_attacker();
    match &mut legacy.state_mut().waiting_for {
        WaitingFor::CombatTaxPayment {
            pending: CombatTaxPending::Attack { attacks, .. },
            ..
        } => {
            for (ref_mut, _target) in attacks.iter_mut() {
                *ref_mut = ObjectIncarnationRef::of(attacker, LEGACY_INCARNATION);
            }
        }
        other => panic!("expected a paused attack tax, got {other:?}"),
    }
    tap_all_p0_lands(&mut legacy);
    legacy
        .act(GameAction::PayCombatTax { accept: true })
        .expect("accept still succeeds even though the legacy ref is dropped");
    let state = legacy.state();
    assert!(
        !state
            .combat
            .as_ref()
            .is_some_and(|c| c.attackers.iter().any(|a| a.object_id == attacker)),
        "CR 400.7/508.1k: a LEGACY_INCARNATION snapshot ref never matches a live object and is dropped"
    );
    assert!(
        !state.objects[&attacker].tapped,
        "a dropped attacker is never tapped (commit skips it)"
    );
}

/// CR 508.1c + CR 109.5: a temporary attack prohibition ("Players can't attack you
/// this turn", a `ProhibitActivity::Attack` restriction sourced from the protected
/// player's permanent) bars attacking the protected player but leaves other
/// opponents attackable — it is a per-target hard restriction, scoped by the
/// `defended` filter relative to the source controller, not a blanket ban.
///
/// Revert guard: the negative (attacking P1 is `is_err()`) flips if
/// `attack_passes_temporary_prohibition` stops consulting `state.restrictions`;
/// the positive (attacking P2 commits) guards against an over-broad ban.
#[test]
fn temporary_attack_prohibition_bars_only_the_protected_player() {
    use engine::types::ability::{
        GameRestriction, ProhibitedActivity, RestrictionExpiry, RestrictionPlayerScope,
    };
    use engine::types::triggers::AttackTargetFilter;

    fn setup() -> (GameRunner, ObjectId) {
        let mut scenario = GameScenario::new_n_player(3, 42);
        // P1 controls the permanent that sources the prohibition, so P1 is the
        // protected player.
        let source = scenario.add_creature(P1, "Ward Totem", 0, 3).id();
        let attacker = scenario.add_creature(P0, "Bear", 2, 2).id();
        let mut runner = scenario.build();
        runner
            .state_mut()
            .restrictions
            .push(GameRestriction::ProhibitActivity {
                source,
                affected_players: RestrictionPlayerScope::AllPlayers,
                expiry: RestrictionExpiry::EndOfTurn,
                activity: ProhibitedActivity::Attack {
                    defended: AttackTargetFilter::PlayerOrPlaneswalker,
                },
            });
        park_3p_declare(&mut runner, &[attacker]);
        (runner, attacker)
    }

    // Negative: P0's creature can't attack the protected player P1.
    let (mut barred, attacker) = setup();
    let res = barred.act(GameAction::DeclareAttackers {
        attacks: vec![(attacker, AttackTarget::Player(P1))],
        bands: vec![],
    });
    assert!(
        res.is_err(),
        "a temporary attack prohibition bars attacking the protected player"
    );
    assert!(barred.state().combat.is_none());

    // Positive reach-guard: the SAME creature may attack a DIFFERENT opponent (P2),
    // proving the prohibition is scoped to the protected player, not a blanket ban.
    let (mut ok, attacker) = setup();
    ok.act(GameAction::DeclareAttackers {
        attacks: vec![(attacker, AttackTarget::Player(P2))],
        bands: vec![],
    })
    .expect("the prohibition protects only P1 — attacking P2 is legal");
    assert!(ok.state().combat.is_some());
}
