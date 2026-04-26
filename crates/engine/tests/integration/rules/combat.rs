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
                player: engine::types::ability::GainLifePlayer::Controller,
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
        .act(GameAction::DeclareAttackers { attacks })
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
        .act(GameAction::DeclareAttackers { attacks })
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
        .act(GameAction::DeclareAttackers { attacks })
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
        .act(GameAction::DeclareAttackers { attacks })
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
        .act(GameAction::DeclareAttackers { attacks })
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
        .act(GameAction::DeclareAttackers { attacks })
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
