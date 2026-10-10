//! Combat relation damage prevention and trigger integration tests.
//!
//! CR 615.1a (damage prevention), CR 509.1g (creatures it's blocking / creatures blocking it),
//! CR 603.10a (LTB / dies combat history look-back), CR 608.2h / CR 113.7a (LKI target revalidation),
//! CR 301.5a (Aura/Equipment combat relation host rebinding).

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const WALL_OF_VAPOR_ORACLE: &str =
    "Defender (This creature can't attack.)\nPrevent all damage that would be dealt to this creature by creatures it's blocking.";
const DRY_SPELL_ORACLE: &str = "Dry Spell deals 1 damage to each creature and each player.";
const PRODIGAL_SORCERER_ORACLE: &str =
    "Vigilance\n{T}: Prodigal Sorcerer deals 1 damage to any target.";

#[test]
fn wall_of_vapor_takes_damage_from_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let wall = scenario
        .add_creature_from_oracle(P1, "Wall of Vapor", 0, 1, WALL_OF_VAPOR_ORACLE)
        .id();

    let dry_spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Dry Spell",
            /* is_instant */ false,
            DRY_SPELL_ORACLE,
        )
        .id();

    let mut runner = scenario.build();

    let outcome = runner.cast(dry_spell).resolve();

    // Wall of Vapor took 1 damage from Dry Spell (damage not prevented)
    // Since toughness is 1, 1 lethal damage causes it to die to state-based actions.
    outcome.assert_zone(&[wall], Zone::Graveyard);
}

#[test]
fn wall_of_vapor_prevents_combat_damage_from_blocked_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let wall = scenario
        .add_creature_from_oracle(P1, "Wall of Vapor", 0, 1, WALL_OF_VAPOR_ORACLE)
        .id();
    let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();

    let mut runner = scenario.build();

    // Advance to DeclareAttackers
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    // Pass priority to DeclareBlockers
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(wall, attacker)],
        })
        .expect("DeclareBlockers should succeed");

    // Pass priority through DeclareBlockers and combat damage
    runner.pass_both_players();

    // Combat damage was dealt. Wall of Vapor blocked the 2/2 attacker, so all damage
    // dealt to it by the blocked creature was prevented.
    // Wall of Vapor survives on the battlefield with 0 damage marked.
    assert_eq!(
        runner.state().objects.get(&wall).unwrap().damage_marked,
        0,
        "Combat damage from blocked attacker must be prevented"
    );
    assert_eq!(
        runner.state().objects.get(&wall).unwrap().zone,
        Zone::Battlefield,
        "Wall of Vapor must survive combat"
    );
}

#[test]
fn wall_of_vapor_prevents_noncombat_damage_from_blocked_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let wall = scenario
        .add_creature_from_oracle(P1, "Wall of Vapor", 0, 1, WALL_OF_VAPOR_ORACLE)
        .id();
    let pinger = scenario
        .add_creature_from_oracle(P0, "Prodigal Sorcerer", 1, 1, PRODIGAL_SORCERER_ORACLE)
        .id();

    let mut runner = scenario.build();

    // Advance to DeclareAttackers
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(pinger, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    // Pass priority to DeclareBlockers
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(wall, pinger)],
        })
        .expect("DeclareBlockers should succeed");

    // In DeclareBlockers step, P0 activates Prodigal Sorcerer targeting Wall of Vapor.
    runner.activate(pinger, 0).target_object(wall).resolve();

    // CR 615.1a: Wall of Vapor prevents all damage from creatures it's blocking,
    // including non-combat damage from an activated ability.
    assert_eq!(
        runner.state().objects.get(&wall).unwrap().damage_marked,
        0,
        "Noncombat damage from blocked creature must be prevented"
    );
    assert_eq!(
        runner.state().objects.get(&wall).unwrap().zone,
        Zone::Battlefield,
        "Wall of Vapor must survive noncombat damage from blocked creature"
    );
}

#[test]
fn wall_of_vapor_does_not_prevent_damage_from_unblocked_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let wall = scenario
        .add_creature_from_oracle(P1, "Wall of Vapor", 0, 1, WALL_OF_VAPOR_ORACLE)
        .id();
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let pinger = scenario
        .add_creature_from_oracle(P0, "Prodigal Sorcerer", 1, 1, PRODIGAL_SORCERER_ORACLE)
        .id();

    let mut runner = scenario.build();

    // Advance to DeclareAttackers
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(pinger, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    // Bear blocks pinger; Wall of Vapor does NOT block pinger.
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(bear, pinger)],
        })
        .expect("DeclareBlockers should succeed");

    // P0 activates Prodigal Sorcerer targeting Wall of Vapor.
    runner.activate(pinger, 0).target_object(wall).resolve();

    // Wall of Vapor is not blocking Prodigal Sorcerer, so damage is NOT prevented.
    // 1 lethal damage causes it to die to state-based actions.
    assert_eq!(
        runner.state().objects.get(&wall).unwrap().zone,
        Zone::Graveyard,
        "Damage from creature Wall of Vapor is not blocking must NOT be prevented"
    );
}

#[test]
fn armored_transport_prevents_combat_damage_from_blocking_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let transport = scenario
        .add_creature_from_oracle(
            P0,
            "Armored Transport",
            2,
            1,
            "Prevent all combat damage that would be dealt to this creature by creatures blocking it.",
        )
        .id();
    let blocker = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();

    // Advance to DeclareAttackers
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(transport, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    // Blocker blocks Armored Transport
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(blocker, transport)],
        })
        .expect("DeclareBlockers should succeed");

    // Pass through combat damage step
    runner.pass_both_players();

    // CR 615.1a + CR 509.1g: Armored Transport takes 0 damage (prevented) and survives.
    assert_eq!(
        runner
            .state()
            .objects
            .get(&transport)
            .unwrap()
            .damage_marked,
        0,
        "Combat damage from blocking creature must be prevented"
    );
    assert_eq!(
        runner.state().objects.get(&transport).unwrap().zone,
        Zone::Battlefield,
        "Armored Transport must survive combat"
    );
    // Blocker took 2 combat damage from Armored Transport and died
    assert_eq!(
        runner.state().objects.get(&blocker).unwrap().zone,
        Zone::Graveyard,
        "Blocker must be destroyed by combat damage"
    );
}

#[test]
fn wall_of_corpses_destroys_blocked_creature_with_lki() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P1,
        vec![ManaUnit::new(
            ManaType::Black,
            ObjectId(9_999),
            false,
            vec![],
        )],
    );

    let attacker = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
    let corpses = scenario
        .add_creature_from_oracle(
            P1,
            "Wall of Corpses",
            0,
            2,
            "Defender (This creature can't attack.)\n{B}, Sacrifice this creature: Destroy target creature this creature is blocking.",
        )
        .id();

    let mut runner = scenario.build();

    // Advance to DeclareAttackers
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    // Wall of Corpses blocks attacker
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(corpses, attacker)],
        })
        .expect("DeclareBlockers should succeed");

    // P0 passes priority in DeclareBlockers
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority");

    // CR 106.4: Step/phase transitions clear mana pools, so fund P1's pool directly in DeclareBlockers
    runner.state_mut().players[P1.0 as usize]
        .mana_pool
        .add(ManaUnit::new(
            ManaType::Black,
            ObjectId(9_999),
            false,
            vec![],
        ));

    // P1 activates Wall of Corpses sacrificing itself (ability index 1, following Defender keyword)
    runner
        .activate(corpses, 1)
        .pay_with(&[corpses])
        .target_object(attacker)
        .resolve();

    // Wall of Corpses was sacrificed
    assert_eq!(
        runner.state().objects.get(&corpses).unwrap().zone,
        Zone::Graveyard,
        "Wall of Corpses must be in graveyard after sacrifice cost"
    );

    // CR 113.7a / CR 608.2h: Attacker was destroyed by Wall of Corpses ability via LKI combat relation
    assert_eq!(
        runner.state().objects.get(&attacker).unwrap().zone,
        Zone::Graveyard,
        "Attacker must be destroyed by Wall of Corpses' activated ability"
    );
}

#[test]
fn baneclaw_marauder_triggers_when_blocking_creature_dies() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let marauder = scenario
        .add_creature_from_oracle(
            P0,
            "Baneclaw Marauder",
            3,
            4,
            "Whenever this creature becomes blocked, each creature blocking it gets -1/-1 until end of turn.\nWhenever a creature blocking this creature dies, that creature's controller loses 1 life.",
        )
        .id();
    let blocker = scenario.add_creature(P1, "Llanowar Elves", 1, 1).id();

    let mut runner = scenario.build();

    // Advance to DeclareAttackers
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(marauder, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    // Blocker blocks Marauder
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(blocker, marauder)],
        })
        .expect("DeclareBlockers should succeed");

    // Drain stack: BecomesBlocked trigger resolves (-1/-1), blocker dies to SBAs,
    // dies trigger fires and resolves (P1 loses 1 life)
    while !runner.state().stack.is_empty() {
        runner.pass_both_players();
    }

    assert_eq!(
        runner.state().objects.get(&blocker).unwrap().zone,
        Zone::Graveyard,
        "Blocker must have died from -1/-1"
    );
    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        19,
        "P1 must have lost 1 life from Baneclaw Marauder's dies trigger"
    );
}

#[test]
fn trailblazers_torch_deals_damage_to_blocking_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
    let torch = scenario
        .add_artifact_from_oracle(
            P0,
            "Trailblazer's Torch",
            "Whenever equipped creature becomes blocked, it deals 2 damage to each creature blocking it.",
        )
        .with_subtypes(vec!["Equipment"])
        .id();

    let blocker = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();

    // Attach Torch to attacker
    runner
        .state_mut()
        .objects
        .get_mut(&torch)
        .unwrap()
        .attached_to = Some(engine::game::game_object::AttachTarget::Object(attacker));
    runner
        .state_mut()
        .objects
        .get_mut(&attacker)
        .unwrap()
        .attachments
        .push(torch);
    engine::game::trigger_index::reindex_object_triggers(runner.state_mut(), torch);

    // Advance to DeclareAttackers
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    // Blocker blocks attacker
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(blocker, attacker)],
        })
        .expect("DeclareBlockers should succeed");

    // Drain stack: BecomesBlocked trigger fires and deals 2 damage to blocker
    while !runner.state().stack.is_empty() {
        runner.pass_both_players();
    }

    assert_eq!(
        runner.state().objects.get(&blocker).unwrap().zone,
        Zone::Graveyard,
        "Blocker must be destroyed by 2 damage from Trailblazer's Torch trigger"
    );
}

#[test]
fn abu_jafar_dies_trigger_destroys_combat_relation() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let abu = scenario
        .add_creature_from_oracle(
            P1,
            "Abu Ja'far",
            0,
            1,
            "When this creature dies, destroy all creatures blocking or blocked by it. They can't be regenerated.",
        )
        .id();
    let attacker = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();

    // Advance to DeclareAttackers
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    // Abu Ja'far blocks attacker
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(abu, attacker)],
        })
        .expect("DeclareBlockers should succeed");

    // Combat damage step: Attacker deals 2 damage to Abu Ja'far. Abu Ja'far dies.
    // Abu Ja'far dies trigger fires and goes to the stack.
    runner.pass_both_players();

    assert_eq!(
        runner.state().objects.get(&abu).unwrap().zone,
        Zone::Graveyard,
        "Abu Ja'far must be in graveyard from lethal combat damage"
    );

    // Resolve dies trigger from stack
    while !runner.state().stack.is_empty() {
        runner.pass_both_players();
    }

    // CR 603.10a + CR 608.2h: Abu Ja'far's LKI combat status identifies attacker as blocked creature,
    // destroying it.
    assert_eq!(
        runner.state().objects.get(&attacker).unwrap().zone,
        Zone::Graveyard,
        "Attacker must be destroyed by Abu Ja'far's dies trigger"
    );
}

#[test]
fn ib_halfheart_sacrifice_then_damage_destroys_blocker_of_sacrificed_goblin() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ib = scenario
        .add_creature_from_oracle(
            P0,
            "Ib Halfheart, Goblin Tactician",
            3,
            2,
            "Whenever another Goblin you control becomes blocked, sacrifice it. If you do, it deals 4 damage to each creature blocking it.",
        )
        .with_subtypes(vec!["Goblin"])
        .id();
    let goblin = scenario
        .add_creature(P0, "Goblin Raider", 2, 2)
        .with_subtypes(vec!["Goblin"])
        .id();
    let blocker = scenario.add_creature(P1, "Wall of Wood", 0, 3).id();

    let mut runner = scenario.build();

    // Advance to DeclareAttackers
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(goblin, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    // Blocker blocks Goblin
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(blocker, goblin)],
        })
        .expect("DeclareBlockers should succeed");

    // Drain stack: BecomesBlocked trigger resolves
    while !runner.state().stack.is_empty() {
        runner.pass_both_players();
    }

    // Goblin was sacrificed
    assert_eq!(
        runner.state().objects.get(&goblin).unwrap().zone,
        Zone::Graveyard,
        "Goblin must be sacrificed"
    );
    // Ib Halfheart is still on the battlefield
    assert_eq!(
        runner.state().objects.get(&ib).unwrap().zone,
        Zone::Battlefield,
        "Ib Halfheart must remain on battlefield"
    );
    // CR 509.3c + CR 113.7a: Sacrificed Goblin dealt 4 damage to Blocker via LKI, killing it
    assert_eq!(
        runner.state().objects.get(&blocker).unwrap().zone,
        Zone::Graveyard,
        "Blocker must be destroyed by 4 damage from sacrificed Goblin"
    );
}

#[test]
fn knight_of_dusk_fizzles_if_target_removed_from_combat_while_on_battlefield() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Black, ObjectId(9_998), false, vec![]),
            ManaUnit::new(ManaType::Black, ObjectId(9_999), false, vec![]),
        ],
    );

    let knight = scenario
        .add_creature_from_oracle(
            P0,
            "Knight of Dusk",
            2,
            2,
            "{B}{B}: Destroy target creature blocking this creature.",
        )
        .id();
    let blocker = scenario.add_creature(P1, "Hill Giant", 3, 3).id();

    let mut runner = scenario.build();

    // Advance to DeclareAttackers
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(knight, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    // Blocker blocks Knight of Dusk
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(blocker, knight)],
        })
        .expect("DeclareBlockers should succeed");

    // Add mana for P0 in DeclareBlockers step
    runner.state_mut().players[P0.0 as usize]
        .mana_pool
        .add(ManaUnit::new(
            ManaType::Black,
            ObjectId(9_998),
            false,
            vec![],
        ));
    runner.state_mut().players[P0.0 as usize]
        .mana_pool
        .add(ManaUnit::new(
            ManaType::Black,
            ObjectId(9_999),
            false,
            vec![],
        ));

    // P0 activates Knight of Dusk targeting blocker (ability index 0)
    runner
        .act(GameAction::ActivateAbility {
            source_id: knight,
            ability_index: 0,
        })
        .expect("ActivateAbility should succeed");

    if matches!(
        runner.state().waiting_for,
        engine::types::game_state::WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(engine::types::ability::TargetRef::Object(blocker)),
            })
            .expect("ChooseTarget should succeed");
    }

    if matches!(
        runner.state().waiting_for,
        engine::types::game_state::WaitingFor::ManaPayment { .. }
    ) {
        runner.act(GameAction::PassPriority).expect("Pay mana");
    }

    // Ability is on the stack. Now remove blocker from combat while it remains on the battlefield (CR 506.4).
    engine::game::effects::remove_from_combat::remove_object_from_combat(
        runner.state_mut(),
        blocker,
    );

    // Blocker is still on battlefield
    assert_eq!(
        runner.state().objects.get(&blocker).unwrap().zone,
        Zone::Battlefield,
        "Blocker remains on battlefield after being removed from combat"
    );

    // Pass priority to resolve ability
    runner.pass_both_players();

    // CR 506.4 + CR 608.2b: Target is still on battlefield but no longer blocking this creature;
    // LKI does NOT apply, target revalidation fails, ability fizzles.
    assert_eq!(
        runner.state().objects.get(&blocker).unwrap().zone,
        Zone::Battlefield,
        "Blocker must NOT be destroyed because it was removed from combat"
    );
}

#[test]
fn trailblazers_torch_trigger_maintains_host_even_if_equipment_moves_before_resolution() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let attacker_a = scenario.add_creature(P0, "Attacker A", 2, 2).id();
    let other_creature_b = scenario.add_creature(P0, "Other B", 2, 2).id();
    let torch = scenario
        .add_artifact_from_oracle(
            P0,
            "Trailblazer's Torch",
            "Whenever equipped creature becomes blocked, it deals 2 damage to each creature blocking it.",
        )
        .with_subtypes(vec!["Equipment"])
        .id();

    let blocker = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();

    // Attach Torch to Attacker A
    runner
        .state_mut()
        .objects
        .get_mut(&torch)
        .unwrap()
        .attached_to = Some(engine::game::game_object::AttachTarget::Object(attacker_a));
    runner
        .state_mut()
        .objects
        .get_mut(&attacker_a)
        .unwrap()
        .attachments
        .push(torch);
    engine::game::trigger_index::reindex_object_triggers(runner.state_mut(), torch);

    // Advance to DeclareAttackers
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker_a, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    // Blocker blocks Attacker A
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(blocker, attacker_a)],
        })
        .expect("DeclareBlockers should succeed");

    // Trigger is on the stack. Before it resolves, move Torch to Other B (e.g. Magnetic Theft).
    runner
        .state_mut()
        .objects
        .get_mut(&attacker_a)
        .unwrap()
        .attachments
        .retain(|&id| id != torch);
    runner
        .state_mut()
        .objects
        .get_mut(&other_creature_b)
        .unwrap()
        .attachments
        .push(torch);
    runner
        .state_mut()
        .objects
        .get_mut(&torch)
        .unwrap()
        .attached_to = Some(engine::game::game_object::AttachTarget::Object(
        other_creature_b,
    ));

    // Resolve trigger from stack
    while !runner.state().stack.is_empty() {
        runner.pass_both_players();
    }

    // CR 301.5a + Finding 4: The ability's host was bound to Attacker A when the trigger was created.
    // Moving the Equipment does not change the referent at resolution. Blocker takes 2 damage and dies.
    assert_eq!(
        runner.state().objects.get(&blocker).unwrap().zone,
        Zone::Graveyard,
        "Blocker must be destroyed by Trailblazer's Torch trigger even if Torch was moved before resolution"
    );
}

#[test]
fn baneclaw_marauder_does_not_trigger_if_blocker_removed_from_combat_before_death() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let marauder = scenario
        .add_creature_from_oracle(
            P0,
            "Baneclaw Marauder",
            3,
            4,
            "Whenever this creature becomes blocked, each creature blocking it gets -1/-1 until end of turn.\nWhenever a creature blocking this creature dies, that creature's controller loses 1 life.",
        )
        .id();
    let blocker = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();

    let bolt = scenario.add_bolt_to_hand(P0);

    let mut runner = scenario.build();

    // Advance to DeclareAttackers
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(marauder, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    // Blocker blocks Marauder
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(blocker, marauder)],
        })
        .expect("DeclareBlockers should succeed");

    // BecomesBlocked trigger resolves: Blocker gets -1/-1 (is now 1/1)
    while !runner.state().stack.is_empty() {
        runner.pass_both_players();
    }

    assert_eq!(
        runner.state().objects.get(&blocker).unwrap().zone,
        Zone::Battlefield,
        "Blocker survived -1/-1 as a 1/1"
    );

    // Remove blocker from combat while on the battlefield (CR 506.4)
    engine::game::effects::remove_from_combat::remove_object_from_combat(
        runner.state_mut(),
        blocker,
    );

    // Initial life of P1 is 20
    assert_eq!(runner.state().players[P1.0 as usize].life, 20);

    // Cast Lightning Bolt at instant speed targeting blocker.
    runner.cast(bolt).target_object(blocker).resolve();

    assert_eq!(
        runner.state().objects.get(&blocker).unwrap().zone,
        Zone::Graveyard,
        "Blocker died from Lightning Bolt damage"
    );

    // CR 603.10a / Finding 5: At the exact moment of zone departure, blocker was NOT blocking Marauder.
    // Therefore, Marauder's dies trigger must NOT fire. P1's life remains 20.
    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        20,
        "P1 must not lose life because the dying creature was no longer blocking Marauder"
    );
}

#[test]
fn departure_fallback_requires_from_battlefield() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let creature_in_hand = scenario
        .add_creature_to_hand(P0, "Hand Creature", 2, 2)
        .id();
    let _marauder = scenario
        .add_creature_from_oracle(
            P0,
            "Baneclaw Marauder",
            3,
            4,
            "Whenever a creature blocking this creature dies, that creature's controller loses 1 life.",
        )
        .id();

    let mut runner = scenario.build();

    // Discard creature directly from Hand to Graveyard (from_zone is Hand, not Battlefield)
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(
        runner.state_mut(),
        creature_in_hand,
        Zone::Graveyard,
        &mut events,
    );

    // Check that zone change record exists with from_zone == Hand
    let last_change = runner.state().zone_changes_this_turn.last().unwrap();
    assert_eq!(last_change.object_id, creature_in_hand);
    assert_eq!(last_change.from_zone, Some(Zone::Hand));

    // Life remains 20 (no triggers fired or matched)
    assert_eq!(runner.state().players[P0.0 as usize].life, 20);
    assert_eq!(runner.state().players[P1.0 as usize].life, 20);
}

#[test]
fn dwindle_blocking_destroys_enchanted_creature_and_leaves_attacker_alive() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
    let blocker = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let dwindle = scenario
        .add_enchantment_from_oracle(
            P0,
            "Dwindle",
            "Enchant creature\nEnchanted creature gets -6/-0.\nWhen enchanted creature blocks, destroy it. (The attacking creature remains blocked.)",
        )
        .with_subtypes(vec!["Aura"])
        .id();

    let mut runner = scenario.build();

    // Attach Dwindle to blocker
    runner
        .state_mut()
        .objects
        .get_mut(&dwindle)
        .unwrap()
        .attached_to = Some(engine::game::game_object::AttachTarget::Object(blocker));
    runner
        .state_mut()
        .objects
        .get_mut(&blocker)
        .unwrap()
        .attachments
        .push(dwindle);
    engine::game::trigger_index::reindex_object_triggers(runner.state_mut(), dwindle);

    // Advance to DeclareAttackers
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    // Blocker blocks attacker
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(blocker, attacker)],
        })
        .expect("DeclareBlockers should succeed");

    // Drain stack: Dwindle's trigger resolves
    while !runner.state().stack.is_empty() {
        runner.pass_both_players();
    }

    // Blocker is destroyed by Dwindle
    assert_eq!(
        runner.state().objects.get(&blocker).unwrap().zone,
        Zone::Graveyard,
        "Enchanted blocker must be destroyed by Dwindle trigger"
    );
    // Attacker remains alive on battlefield
    assert_eq!(
        runner.state().objects.get(&attacker).unwrap().zone,
        Zone::Battlefield,
        "Attacking creature must survive"
    );
}

#[test]
fn ashmouth_hound_attacking_deals_damage_to_counterpart_blocker() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let hound = scenario
        .add_creature_from_oracle(
            P0,
            "Ashmouth Hound",
            2,
            1,
            "Whenever this creature blocks or becomes blocked by a creature, this creature deals 1 damage to that creature.",
        )
        .id();
    let blocker = scenario.add_creature(P1, "Gunk Beetle", 1, 1).id();

    let mut runner = scenario.build();

    // Advance to DeclareAttackers
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(hound, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    // Blocker blocks Hound
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(blocker, hound)],
        })
        .expect("DeclareBlockers should succeed");

    // Drain stack: Ashmouth Hound trigger resolves and deals 1 damage to blocker
    while !runner.state().stack.is_empty() {
        runner.pass_both_players();
    }

    // Blocker dies to 1 damage before combat damage step
    assert_eq!(
        runner.state().objects.get(&blocker).unwrap().zone,
        Zone::Graveyard,
        "Blocker must die from 1 damage dealt by Ashmouth Hound trigger"
    );
    // Hound took no damage and survives
    assert_eq!(
        runner.state().objects.get(&hound).unwrap().zone,
        Zone::Battlefield,
        "Ashmouth Hound survives on battlefield"
    );
    assert_eq!(
        runner.state().objects.get(&hound).unwrap().damage_marked,
        0,
        "Hound took no damage"
    );
}

#[test]
fn ashmouth_hound_blocking_deals_damage_to_counterpart_attacker() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let attacker = scenario.add_creature(P0, "Gunk Beetle", 1, 1).id();
    let hound = scenario
        .add_creature_from_oracle(
            P1,
            "Ashmouth Hound",
            2,
            1,
            "Whenever this creature blocks or becomes blocked by a creature, this creature deals 1 damage to that creature.",
        )
        .id();

    let mut runner = scenario.build();

    // Advance to DeclareAttackers
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    // Hound blocks attacker
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(hound, attacker)],
        })
        .expect("DeclareBlockers should succeed");

    // Drain stack: Ashmouth Hound trigger resolves and deals 1 damage to attacker
    while !runner.state().stack.is_empty() {
        runner.pass_both_players();
    }

    // Attacker dies to 1 damage before combat damage step
    assert_eq!(
        runner.state().objects.get(&attacker).unwrap().zone,
        Zone::Graveyard,
        "Attacker must die from 1 damage dealt by Ashmouth Hound trigger"
    );
    // Hound survives with 0 damage marked
    assert_eq!(
        runner.state().objects.get(&hound).unwrap().zone,
        Zone::Battlefield,
        "Ashmouth Hound survives on battlefield"
    );
    assert_eq!(
        runner.state().objects.get(&hound).unwrap().damage_marked,
        0,
        "Hound took no damage"
    );
}

#[test]
fn abu_jafar_same_id_return_subject_still_destroys_attacker() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let abu = scenario
        .add_creature_from_oracle(
            P1,
            "Abu Ja'far",
            0,
            1,
            "When this creature dies, destroy all creatures blocking or blocked by it. They can't be regenerated.",
        )
        .id();
    let attacker = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();

    // Advance to DeclareAttackers
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    // Abu Ja'far blocks attacker
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(abu, attacker)],
        })
        .expect("DeclareBlockers should succeed");

    // Combat damage step: Attacker deals 2 damage to Abu Ja'far. Abu Ja'far dies.
    // Abu Ja'far dies trigger fires and goes to the stack.
    runner.pass_both_players();

    assert_eq!(
        runner.state().objects.get(&abu).unwrap().zone,
        Zone::Graveyard,
        "Abu Ja'far must be in graveyard from lethal combat damage"
    );

    // Abu Ja'far returns to the battlefield before dies trigger resolves (e.g. reanimation effect)
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), abu, Zone::Battlefield, &mut events);

    assert_eq!(
        runner.state().objects.get(&abu).unwrap().zone,
        Zone::Battlefield,
        "Abu Ja'far returned to battlefield"
    );

    // Resolve dies trigger from stack
    while !runner.state().stack.is_empty() {
        runner.pass_both_players();
    }

    // CR 603.10a + CR 608.2h: Abu Ja'far's LKI combat status identifies attacker as blocked creature,
    // destroying it even though Abu Ja'far returned with a new incarnation.
    assert_eq!(
        runner.state().objects.get(&attacker).unwrap().zone,
        Zone::Graveyard,
        "Attacker must be destroyed by Abu Ja'far's dies trigger"
    );
}

#[test]
fn abu_jafar_same_id_return_counterpart_survives() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let abu = scenario
        .add_creature_from_oracle(
            P1,
            "Abu Ja'far",
            0,
            1,
            "When this creature dies, destroy all creatures blocking or blocked by it. They can't be regenerated.",
        )
        .id();
    let attacker = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();

    // Advance to DeclareAttackers
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    // Abu Ja'far blocks attacker
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(abu, attacker)],
        })
        .expect("DeclareBlockers should succeed");

    // Combat damage step: Attacker deals 2 damage to Abu Ja'far. Abu Ja'far dies.
    // Abu Ja'far dies trigger fires and goes to the stack.
    runner.pass_both_players();

    assert_eq!(
        runner.state().objects.get(&abu).unwrap().zone,
        Zone::Graveyard,
        "Abu Ja'far must be in graveyard from lethal combat damage"
    );

    // Attacker is blinked (moved to exile and back to battlefield with a new incarnation)
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), attacker, Zone::Exile, &mut events);
    engine::game::zones::move_to_zone(runner.state_mut(), attacker, Zone::Battlefield, &mut events);

    assert_eq!(
        runner.state().objects.get(&attacker).unwrap().zone,
        Zone::Battlefield,
        "Attacker is on battlefield with a new incarnation"
    );

    // Resolve dies trigger from stack
    while !runner.state().stack.is_empty() {
        runner.pass_both_players();
    }

    // CR 400.7: Returning attacker has a new incarnation and is not the object that was blocked by Abu Ja'far.
    // It must survive.
    assert_eq!(
        runner.state().objects.get(&attacker).unwrap().zone,
        Zone::Battlefield,
        "New attacker incarnation must survive Abu Ja'far dies trigger"
    );
}

#[test]
fn ib_halfheart_pre_resolution_blink_prevents_sacrifice_and_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ib = scenario
        .add_creature_from_oracle(
            P0,
            "Ib Halfheart, Goblin Tactician",
            3,
            2,
            "Whenever another Goblin you control becomes blocked, sacrifice it. If you do, it deals 4 damage to each creature blocking it.",
        )
        .with_subtypes(vec!["Goblin"])
        .id();
    let goblin = scenario
        .add_creature(P0, "Goblin Raider", 2, 2)
        .with_subtypes(vec!["Goblin"])
        .id();
    let blocker = scenario.add_creature(P1, "Wall of Wood", 0, 3).id();

    let mut runner = scenario.build();

    // Advance to DeclareAttackers
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(goblin, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    // Blocker blocks Goblin
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(blocker, goblin)],
        })
        .expect("DeclareBlockers should succeed");

    // Trigger is now on stack. Blink the Goblin before resolution (Exile then Battlefield).
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), goblin, Zone::Exile, &mut events);
    engine::game::zones::move_to_zone(runner.state_mut(), goblin, Zone::Battlefield, &mut events);

    // Drain stack: BecomesBlocked trigger resolves
    while !runner.state().stack.is_empty() {
        runner.pass_both_players();
    }

    // CR 400.7: Goblin returned with a new incarnation, so it was NOT sacrificed.
    assert_eq!(
        runner.state().objects.get(&goblin).unwrap().zone,
        Zone::Battlefield,
        "Blinked Goblin must remain on battlefield (not sacrificed)"
    );
    // Ib Halfheart is still on the battlefield
    assert_eq!(
        runner.state().objects.get(&ib).unwrap().zone,
        Zone::Battlefield,
        "Ib Halfheart must remain on battlefield"
    );
    // Since Goblin was not sacrificed, sub-ability does not fire and Blocker took 0 damage.
    assert_eq!(
        runner.state().objects.get(&blocker).unwrap().zone,
        Zone::Battlefield,
        "Blocker survives on battlefield"
    );
    assert_eq!(
        runner.state().objects.get(&blocker).unwrap().damage_marked,
        0,
        "Blocker took 0 damage"
    );
}

#[test]
fn battle_scarred_goblin_removal_then_death_before_resolution_deals_no_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let goblin = scenario
        .add_creature_from_oracle(
            P0,
            "Battle-Scarred Goblin",
            2,
            2,
            "Whenever this creature becomes blocked, it deals 1 damage to each creature blocking it.",
        )
        .id();
    let blocker = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();

    // Advance to DeclareAttackers
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(goblin, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    // Blocker blocks Goblin
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(blocker, goblin)],
        })
        .expect("DeclareBlockers should succeed");

    // Trigger is on the stack.
    // Remove Goblin from combat while still on the battlefield (CR 506.4)
    engine::game::effects::remove_from_combat::remove_object_from_combat(
        runner.state_mut(),
        goblin,
    );

    // Destroy Goblin (move to graveyard) before trigger resolves
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), goblin, Zone::Graveyard, &mut events);

    assert_eq!(
        runner.state().objects.get(&goblin).unwrap().zone,
        Zone::Graveyard,
        "Goblin is in graveyard"
    );

    // Drain stack: BecomesBlocked trigger resolves
    while !runner.state().stack.is_empty() {
        runner.pass_both_players();
    }

    // CR 608.2h / Finding 3: Actual departure record shows Goblin had no blockers when it left the battlefield.
    // Zero damage is dealt to blocker.
    assert_eq!(
        runner.state().objects.get(&blocker).unwrap().zone,
        Zone::Battlefield,
        "Blocker survives on battlefield"
    );
    assert_eq!(
        runner.state().objects.get(&blocker).unwrap().damage_marked,
        0,
        "Blocker took 0 damage"
    );
}

#[test]
fn baneclaw_marauder_simultaneous_combat_death_order_attacker_first() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Attacker added first, so it precedes blocker in battlefield ordering.
    let marauder = scenario
        .add_creature_from_oracle(
            P0,
            "Baneclaw Marauder",
            3,
            4,
            "Whenever this creature becomes blocked, each creature blocking it gets -1/-1 until end of turn.\nWhenever a creature blocking this creature dies, that creature's controller loses 1 life.",
        )
        .id();
    let blocker = scenario.add_creature(P1, "Grizzly Bears", 5, 4).id();

    let mut runner = scenario.build();

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(marauder, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(blocker, marauder)],
        })
        .expect("DeclareBlockers should succeed");

    // BecomesBlocked trigger resolves: Blocker gets -1/-1 (is now 4/3).
    while !runner.state().stack.is_empty() {
        runner.pass_both_players();
    }

    // Advance to CombatDamage step
    runner.pass_both_players();

    // In combat damage:
    // Marauder deals 3 damage to Blocker (lethal, toughness 3).
    // Blocker deals 4 damage to Marauder (lethal, toughness 4).
    // CR 704.3: Both die simultaneously to SBAs.
    // Order in to_die is [marauder, blocker].
    // Simultaneous combat snapshots ensure blocker's zone change records it was blocking Marauder.
    assert_eq!(
        runner.state().objects.get(&marauder).unwrap().zone,
        Zone::Graveyard,
        "Marauder died in combat"
    );
    assert_eq!(
        runner.state().objects.get(&blocker).unwrap().zone,
        Zone::Graveyard,
        "Blocker died in combat"
    );

    // CR 603.10a: Marauder's dies trigger triggered and is on stack.
    while !runner.state().stack.is_empty() {
        runner.pass_both_players();
    }

    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        19,
        "P1 must lose 1 life when blocker dies simultaneously with Marauder (attacker-first delivery)"
    );
}

#[test]
fn baneclaw_marauder_simultaneous_combat_death_order_blocker_first() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Blocker added first, so it precedes attacker in battlefield ordering.
    let blocker = scenario.add_creature(P1, "Grizzly Bears", 5, 4).id();
    let marauder = scenario
        .add_creature_from_oracle(
            P0,
            "Baneclaw Marauder",
            3,
            4,
            "Whenever this creature becomes blocked, each creature blocking it gets -1/-1 until end of turn.\nWhenever a creature blocking this creature dies, that creature's controller loses 1 life.",
        )
        .id();

    let mut runner = scenario.build();

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(marauder, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(blocker, marauder)],
        })
        .expect("DeclareBlockers should succeed");

    // BecomesBlocked trigger resolves: Blocker gets -1/-1 (is now 4/3).
    while !runner.state().stack.is_empty() {
        runner.pass_both_players();
    }

    // Advance to CombatDamage step
    runner.pass_both_players();

    assert_eq!(
        runner.state().objects.get(&marauder).unwrap().zone,
        Zone::Graveyard,
        "Marauder died in combat"
    );
    assert_eq!(
        runner.state().objects.get(&blocker).unwrap().zone,
        Zone::Graveyard,
        "Blocker died in combat"
    );

    // CR 603.10a: Marauder's dies trigger triggered and is on stack.
    while !runner.state().stack.is_empty() {
        runner.pass_both_players();
    }

    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        19,
        "P1 must lose 1 life when blocker dies simultaneously with Marauder (blocker-first delivery)"
    );
}

#[test]
fn ib_halfheart_pre_resolution_blink_does_not_sacrifice_other_goblin() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ib = scenario
        .add_creature_from_oracle(
            P0,
            "Ib Halfheart, Goblin Tactician",
            3,
            2,
            "Whenever another Goblin you control becomes blocked, sacrifice it. If you do, it deals 4 damage to each creature blocking it.",
        )
        .with_subtypes(vec!["Goblin"])
        .id();
    let goblin_a = scenario
        .add_creature(P0, "Goblin Raider", 2, 2)
        .with_subtypes(vec!["Goblin"])
        .id();
    let goblin_b = scenario
        .add_creature(P0, "Goblin Piker", 2, 1)
        .with_subtypes(vec!["Goblin"])
        .id();
    let blocker = scenario.add_creature(P1, "Wall of Wood", 0, 3).id();

    let mut runner = scenario.build();

    // Advance to DeclareAttackers
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(goblin_a, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    // Blocker blocks Goblin A
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(blocker, goblin_a)],
        })
        .expect("DeclareBlockers should succeed");

    // Trigger watching Goblin A is now on stack. Blink Goblin A (new incarnation).
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), goblin_a, Zone::Exile, &mut events);
    engine::game::zones::move_to_zone(runner.state_mut(), goblin_a, Zone::Battlefield, &mut events);

    // Drain stack: BecomesBlocked trigger resolves
    while !runner.state().stack.is_empty() {
        runner.pass_both_players();
    }

    // CR 400.7 + CR 608.2c: Goblin A was not sacrificed because its incarnation changed.
    assert_eq!(
        runner.state().objects.get(&goblin_a).unwrap().zone,
        Zone::Battlefield,
        "Blinked Goblin A must remain on battlefield"
    );
    // Goblin B must NOT be sacrificed either (TriggeringSource hard no-op, no fallback to other permanents)
    assert_eq!(
        runner.state().objects.get(&goblin_b).unwrap().zone,
        Zone::Battlefield,
        "Unrelated Goblin B must NOT be sacrificed"
    );
    // Ib remains on battlefield
    assert_eq!(
        runner.state().objects.get(&ib).unwrap().zone,
        Zone::Battlefield,
        "Ib Halfheart remains on battlefield"
    );
    // Blocker took 0 damage
    assert_eq!(
        runner.state().objects.get(&blocker).unwrap().damage_marked,
        0,
        "Blocker took 0 damage"
    );
}

#[test]
fn ashmouth_hound_pre_resolution_blink_counterpart_deals_no_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let hound = scenario
        .add_creature_from_oracle(
            P0,
            "Ashmouth Hound",
            2,
            1,
            "Whenever this creature blocks or becomes blocked by a creature, this creature deals 1 damage to that creature.",
        )
        .id();
    let blocker = scenario.add_creature(P1, "Gunk Beetle", 1, 1).id();

    let mut runner = scenario.build();

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(hound, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(blocker, hound)],
        })
        .expect("DeclareBlockers should succeed");

    // Trigger is on stack with blocker as counterpart. Blink blocker before resolution.
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), blocker, Zone::Exile, &mut events);
    engine::game::zones::move_to_zone(runner.state_mut(), blocker, Zone::Battlefield, &mut events);

    // Drain stack: Hound trigger resolves
    while !runner.state().stack.is_empty() {
        runner.pass_both_players();
    }

    // CR 400.7: Blocker is a new object, not the triggering counterpart. Takes 0 damage.
    assert_eq!(
        runner.state().objects.get(&blocker).unwrap().zone,
        Zone::Battlefield,
        "Blinked blocker remains on battlefield"
    );
    assert_eq!(
        runner.state().objects.get(&blocker).unwrap().damage_marked,
        0,
        "Blinked blocker took no damage from Ashmouth Hound trigger"
    );
}

#[test]
fn trailblazers_torch_departed_equipped_creature_deals_damage_via_lki() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
    let torch = scenario
        .add_artifact_from_oracle(
            P0,
            "Trailblazer's Torch",
            "Whenever equipped creature becomes blocked, it deals 2 damage to each creature blocking it.",
        )
        .with_subtypes(vec!["Equipment"])
        .id();

    let blocker = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();

    // Attach Torch to attacker
    runner
        .state_mut()
        .objects
        .get_mut(&torch)
        .unwrap()
        .attached_to = Some(engine::game::game_object::AttachTarget::Object(attacker));
    runner
        .state_mut()
        .objects
        .get_mut(&attacker)
        .unwrap()
        .attachments
        .push(torch);
    engine::game::trigger_index::reindex_object_triggers(runner.state_mut(), torch);

    // Advance to DeclareAttackers
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    // Blocker blocks attacker
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(blocker, attacker)],
        })
        .expect("DeclareBlockers should succeed");

    // Torch trigger is on stack. Destroy attacker before trigger resolves.
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), attacker, Zone::Graveyard, &mut events);

    // Drain stack: Torch trigger resolves.
    // CR 113.7a + CR 608.2h: Departed equipped creature deals 2 damage via LKI.
    while !runner.state().stack.is_empty() {
        runner.pass_both_players();
    }

    assert_eq!(
        runner.state().objects.get(&blocker).unwrap().zone,
        Zone::Graveyard,
        "Blocker must be destroyed by 2 damage from departed equipped creature via LKI"
    );
}
