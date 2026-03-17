//! Integration tests for Adventure casting subsystem.
//!
//! Exercises all four building blocks together through the full engine pipeline:
//! - Adventure casting (face choice, exile-on-resolve, countered-to-graveyard)
//! - Casting creature from exile with AdventureCreature permission
//! - BecomesTarget trigger with event-context target resolution
//! - Damage prevention restriction (AddRestriction effect)
//! - Full Bonecrusher Giant lifecycle

use engine::game::game_object::BackFaceData;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, CastingPermission, DamageAmount, Effect, GameRestriction,
    RestrictionExpiry, TargetFilter, TargetRef, TriggerDefinition,
};
use engine::types::actions::GameAction;
use engine::types::card_type::{CardType, CoreType};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Add mana directly to a player's pool (bypasses land tapping).
fn add_mana(runner: &mut GameRunner, player: PlayerId, color: ManaType, count: usize) {
    let state = runner.state_mut();
    let player_data = state.players.iter_mut().find(|p| p.id == player).unwrap();
    for _ in 0..count {
        player_data.mana_pool.add(ManaUnit {
            color,
            source_id: ObjectId(0),
            snow: false,
            restrictions: Vec::new(),
        });
    }
}

/// Build the Adventure back_face data for Stomp.
fn stomp_back_face() -> BackFaceData {
    BackFaceData {
        name: "Stomp".to_string(),
        power: None,
        toughness: None,
        loyalty: None,
        card_types: {
            let mut ct = CardType::default();
            ct.core_types.push(CoreType::Instant);
            ct
        },
        mana_cost: ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 1,
        },
        keywords: Vec::new(),
        abilities: vec![
            // Stomp effect 1: AddRestriction (damage can't be prevented this turn)
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::AddRestriction {
                    restriction: GameRestriction::DamagePreventionDisabled {
                        source: ObjectId(0), // placeholder, filled at resolution
                        expiry: RestrictionExpiry::EndOfTurn,
                        scope: None,
                    },
                },
            ),
            // Stomp effect 2: DealDamage 2 to any target
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::DealDamage {
                    amount: DamageAmount::Fixed(2),
                    target: TargetFilter::Any,
                },
            ),
        ],
        trigger_definitions: Vec::new(),
        replacement_definitions: Vec::new(),
        static_definitions: Vec::new(),
        color: vec![ManaColor::Red],
        printed_ref: None,
        modal: None,
        additional_cost: None,
        casting_restrictions: Vec::new(),
        casting_options: Vec::new(),
    }
}

/// Build the BecomesTarget trigger definition for Bonecrusher Giant.
fn bonecrusher_trigger() -> TriggerDefinition {
    TriggerDefinition::new(TriggerMode::BecomesTarget).execute(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::DealDamage {
            amount: DamageAmount::Fixed(2),
            target: TargetFilter::TriggeringSpellController,
        },
    ))
}

/// Create a Bonecrusher Giant Adventure card in a player's hand via GameScenario.
/// Returns the ObjectId. Must be called before `scenario.build()`.
fn add_bonecrusher_to_hand(scenario: &mut GameScenario) -> ObjectId {
    let obj_id = scenario
        .add_creature_to_hand(P0, "Bonecrusher Giant", 4, 3)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 2,
        })
        .id();
    obj_id
}

/// Attach Adventure back_face and BecomesTarget trigger to a Bonecrusher Giant object.
/// Must be called on the `GameRunner` after `scenario.build()`.
fn setup_bonecrusher_adventure(runner: &mut GameRunner, obj_id: ObjectId) {
    let state = runner.state_mut();
    let obj = state.objects.get_mut(&obj_id).unwrap();
    obj.back_face = Some(stomp_back_face());

    let trigger = bonecrusher_trigger();
    obj.trigger_definitions.push(trigger.clone());
    obj.base_trigger_definitions.push(trigger);
}

// ---------------------------------------------------------------------------
// Test 1: Adventure cast choice from hand
// ---------------------------------------------------------------------------

/// CR 715.3a: Casting an Adventure card from hand should prompt the player
/// to choose between creature face and Adventure face.
#[test]
fn adventure_cast_stomp_from_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let obj_id = add_bonecrusher_to_hand(&mut scenario);

    let mut runner = scenario.build();
    setup_bonecrusher_adventure(&mut runner, obj_id);
    let card_id = runner.state().objects[&obj_id].card_id;
    add_mana(&mut runner, P0, ManaType::Red, 3);

    let result = runner
        .act(GameAction::CastSpell {
            card_id,
            targets: vec![],
        })
        .expect("cast should succeed");

    assert!(
        matches!(
            result.waiting_for,
            WaitingFor::AdventureCastChoice {
                player,
                card_id: _,
                ..
            } if player == P0
        ),
        "Expected AdventureCastChoice, got {:?}",
        result.waiting_for
    );
}

// ---------------------------------------------------------------------------
// Test 2: Adventure spell resolves to exile
// ---------------------------------------------------------------------------

/// CR 715.4: When an Adventure spell resolves, the card goes to exile
/// (not graveyard) with AdventureCreature permission.
#[test]
fn adventure_exile_on_resolve() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let obj_id = add_bonecrusher_to_hand(&mut scenario);

    let mut runner = scenario.build();
    setup_bonecrusher_adventure(&mut runner, obj_id);
    let card_id = runner.state().objects[&obj_id].card_id;
    add_mana(&mut runner, P0, ManaType::Red, 3);

    // Cast the spell
    runner
        .act(GameAction::CastSpell {
            card_id,
            targets: vec![],
        })
        .expect("cast should succeed");

    // Choose Adventure face
    let result = runner
        .act(GameAction::ChooseAdventureFace { creature: false })
        .expect("choose adventure face should succeed");

    // Handle target selection if prompted
    if matches!(result.waiting_for, WaitingFor::TargetSelection { .. }) {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Player(P1)),
            })
            .expect("target selection should succeed");
    }

    // Resolve the spell (pass priority for both players)
    runner.resolve_top();

    // Card should be in exile
    let obj = runner.state().objects.get(&obj_id).unwrap();
    assert_eq!(obj.zone, Zone::Exile, "Adventure should resolve to exile");

    // Should have AdventureCreature permission
    assert!(
        obj.casting_permissions
            .contains(&CastingPermission::AdventureCreature),
        "Should have AdventureCreature permission after Adventure resolves"
    );

    // Name should be restored to creature face
    assert_eq!(
        obj.name, "Bonecrusher Giant",
        "Name should be restored to creature face after exile"
    );
}

// ---------------------------------------------------------------------------
// Test 3: Countered Adventure goes to graveyard
// ---------------------------------------------------------------------------

/// CR 715.4: If an Adventure spell is countered (moved from stack to graveyard
/// without resolving), it goes to graveyard (not exile) and does NOT get
/// AdventureCreature permission.
///
/// Uses direct zone manipulation to simulate counter effect, avoiding full
/// two-player counter spell interaction complexity.
#[test]
fn adventure_countered_to_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let obj_id = add_bonecrusher_to_hand(&mut scenario);

    let mut runner = scenario.build();
    setup_bonecrusher_adventure(&mut runner, obj_id);
    let card_id = runner.state().objects[&obj_id].card_id;
    add_mana(&mut runner, P0, ManaType::Red, 3);

    // Cast as Adventure (Stomp)
    runner
        .act(GameAction::CastSpell {
            card_id,
            targets: vec![],
        })
        .expect("cast should succeed");

    runner
        .act(GameAction::ChooseAdventureFace { creature: false })
        .expect("choose adventure should succeed");

    // Handle target selection for Stomp if needed
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Player(P1)),
            })
            .expect("target selection should succeed");
    }

    // Simulate the counter effect: remove from stack, move to graveyard
    // (This is what the counter effect handler does)
    {
        let state = runner.state_mut();
        state.stack.retain(|e| e.id != obj_id);
        zones::move_to_zone(state, obj_id, Zone::Graveyard, &mut Vec::new());
    }

    // Card should be in graveyard, NOT exile
    let obj = runner.state().objects.get(&obj_id).unwrap();
    assert_eq!(
        obj.zone,
        Zone::Graveyard,
        "Countered Adventure should go to graveyard"
    );
    assert!(
        !obj.casting_permissions
            .contains(&CastingPermission::AdventureCreature),
        "Countered spell should NOT get AdventureCreature permission"
    );
}

// ---------------------------------------------------------------------------
// Test 4: Cast creature from exile
// ---------------------------------------------------------------------------

/// CR 715.5: A card in exile with AdventureCreature permission can be cast
/// as a creature from exile. No face choice is prompted (always creature).
#[test]
fn adventure_cast_creature_from_exile() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Create a Bonecrusher Giant in hand first
    let obj_id = scenario
        .add_creature_to_hand(P0, "Bonecrusher Giant", 4, 3)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 2,
        })
        .id();

    let mut runner = scenario.build();

    // Move to exile and add casting permission via runner
    {
        let state = runner.state_mut();
        zones::move_to_zone(state, obj_id, Zone::Exile, &mut Vec::new());
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.casting_permissions
            .push(CastingPermission::AdventureCreature);
    }

    let card_id = runner.state().objects[&obj_id].card_id;
    add_mana(&mut runner, P0, ManaType::Red, 3);

    // Should NOT prompt for face choice (from exile, always creature)
    let result = runner
        .act(GameAction::CastSpell {
            card_id,
            targets: vec![],
        })
        .expect("cast from exile should succeed");

    assert!(
        !matches!(result.waiting_for, WaitingFor::AdventureCastChoice { .. }),
        "Casting from exile should NOT prompt for face choice, got {:?}",
        result.waiting_for
    );
}

// ---------------------------------------------------------------------------
// Test 5: BecomesTarget trigger fires and deals damage
// ---------------------------------------------------------------------------

/// Bonecrusher Giant on the battlefield: when it becomes the target of a spell,
/// its trigger fires dealing 2 damage to that spell's controller.
#[test]
fn bonecrusher_becomes_target_trigger() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Put Bonecrusher Giant on P0's battlefield
    let giant_id = scenario
        .add_creature(P0, "Bonecrusher Giant", 4, 3)
        .id();

    // P1 has a Lightning Bolt targeting the Giant
    let bolt_id = scenario.add_bolt_to_hand(P1);

    let mut runner = scenario.build();

    // Add the BecomesTarget trigger via runner
    {
        let trigger = bonecrusher_trigger();
        let state = runner.state_mut();
        let obj = state.objects.get_mut(&giant_id).unwrap();
        obj.trigger_definitions.push(trigger.clone());
        obj.base_trigger_definitions.push(trigger);
    }

    let bolt_card_id = runner.state().objects[&bolt_id].card_id;
    add_mana(&mut runner, P1, ManaType::Red, 1);

    let initial_p1_life = runner.life(P1);

    // P0 passes priority
    runner
        .act(GameAction::PassPriority)
        .expect("P0 pass should succeed");

    // P1 casts Lightning Bolt
    runner
        .act(GameAction::CastSpell {
            card_id: bolt_card_id,
            targets: vec![],
        })
        .expect("cast bolt should succeed");

    // Handle target selection for bolt
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(giant_id)),
            })
            .expect("target selection should succeed");
    }

    // Check stack state after target selection -- trigger should have fired
    let stack_desc: Vec<_> = runner
        .state()
        .stack
        .iter()
        .map(|e| match &e.kind {
            engine::types::game_state::StackEntryKind::Spell { .. } => "Spell",
            engine::types::game_state::StackEntryKind::ActivatedAbility { .. } => "Activated",
            engine::types::game_state::StackEntryKind::TriggeredAbility { .. } => "Triggered",
        })
        .collect();

    // BecomesTarget trigger should be on the stack above the bolt
    assert!(
        stack_desc.iter().any(|s| *s == "Triggered"),
        "BecomesTarget trigger should be on stack. Stack: {:?}",
        stack_desc
    );

    // Resolve everything (trigger resolves first, then bolt)
    for _ in 0..30 {
        if runner.state().stack.is_empty() {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }

    // P1 should have taken 2 damage from the BecomesTarget trigger
    // (TriggeringSpellController resolves to P1 since P1 cast the bolt)
    assert!(
        runner.life(P1) <= initial_p1_life - 2,
        "P1 should have taken at least 2 damage from BecomesTarget trigger, life: {} (was {})",
        runner.life(P1),
        initial_p1_life
    );
}

// ---------------------------------------------------------------------------
// Test 6: Stomp damage prevention disabled
// ---------------------------------------------------------------------------

/// Stomp adds a DamagePreventionDisabled restriction when it resolves.
#[test]
fn stomp_damage_prevention_disabled() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let obj_id = add_bonecrusher_to_hand(&mut scenario);

    let mut runner = scenario.build();
    setup_bonecrusher_adventure(&mut runner, obj_id);
    let card_id = runner.state().objects[&obj_id].card_id;
    add_mana(&mut runner, P0, ManaType::Red, 3);

    // Cast as Adventure (Stomp)
    runner
        .act(GameAction::CastSpell {
            card_id,
            targets: vec![],
        })
        .expect("cast should succeed");

    runner
        .act(GameAction::ChooseAdventureFace { creature: false })
        .expect("choose adventure should succeed");

    // Handle target selection for Stomp
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Player(P1)),
            })
            .expect("target selection should succeed");
    }

    // Resolve Stomp
    runner.resolve_top();

    // Should have DamagePreventionDisabled restriction
    assert!(
        runner
            .state()
            .restrictions
            .iter()
            .any(|r| matches!(r, GameRestriction::DamagePreventionDisabled { .. })),
        "Stomp should add DamagePreventionDisabled restriction, restrictions: {:?}",
        runner.state().restrictions
    );
}

// ---------------------------------------------------------------------------
// Test 7: Full Bonecrusher Giant lifecycle
// ---------------------------------------------------------------------------

/// End-to-end: Cast Stomp (Adventure) from hand -> resolves to exile ->
/// cast Bonecrusher Giant (creature) from exile -> enters battlefield.
#[test]
fn bonecrusher_full_flow() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let obj_id = add_bonecrusher_to_hand(&mut scenario);

    let mut runner = scenario.build();
    setup_bonecrusher_adventure(&mut runner, obj_id);
    let card_id = runner.state().objects[&obj_id].card_id;

    // --- Turn 1: Cast Stomp ---
    add_mana(&mut runner, P0, ManaType::Red, 2);

    runner
        .act(GameAction::CastSpell {
            card_id,
            targets: vec![],
        })
        .expect("cast should succeed");

    // Choose Adventure face
    runner
        .act(GameAction::ChooseAdventureFace { creature: false })
        .expect("choose adventure should succeed");

    // Handle target selection for Stomp
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Player(P1)),
            })
            .expect("target selection should succeed");
    }

    // Resolve Stomp
    runner.resolve_top();

    // Verify: card in exile with AdventureCreature permission
    let obj = runner.state().objects.get(&obj_id).unwrap();
    assert_eq!(obj.zone, Zone::Exile, "After Stomp resolves: should be in exile");
    assert!(
        obj.casting_permissions
            .contains(&CastingPermission::AdventureCreature),
        "After Stomp resolves: should have AdventureCreature permission"
    );
    assert_eq!(
        obj.name, "Bonecrusher Giant",
        "After Stomp resolves: name should be creature face"
    );

    // --- Turn 2: Cast creature from exile ---
    add_mana(&mut runner, P0, ManaType::Red, 3);

    // Should not prompt for face choice from exile
    let result = runner
        .act(GameAction::CastSpell {
            card_id,
            targets: vec![],
        })
        .expect("cast creature from exile should succeed");

    assert!(
        !matches!(result.waiting_for, WaitingFor::AdventureCastChoice { .. }),
        "Should not prompt for face choice from exile"
    );

    // Resolve the creature spell
    runner.resolve_top();

    // Verify: creature on the battlefield
    let obj = runner.state().objects.get(&obj_id).unwrap();
    assert_eq!(
        obj.zone,
        Zone::Battlefield,
        "Bonecrusher Giant should be on battlefield"
    );
    assert_eq!(obj.power, Some(4), "Should have 4 power");
    assert_eq!(obj.toughness, Some(3), "Should have 3 toughness");

    // AdventureCreature permission should be cleared (cleared on zone change from exile)
    assert!(
        !obj.casting_permissions
            .contains(&CastingPermission::AdventureCreature),
        "AdventureCreature permission should be cleared after leaving exile"
    );
}
