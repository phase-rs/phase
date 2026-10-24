//! Integration tests for Adventure casting subsystem.
//!
//! Exercises all four building blocks together through the full engine pipeline:
//! - Adventure casting (face choice, exile-on-resolve, countered-to-graveyard)
//! - Casting creature from exile with AdventureCreature permission
//! - BecomesTarget trigger with event-context target resolution
//! - Damage prevention restriction (AddRestriction effect)
//! - Full Bonecrusher Giant lifecycle

use engine::ai_support::legal_actions;
use engine::game::game_object::BackFaceData;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, CastingPermission, Effect, GameRestriction, QuantityExpr,
    RestrictionExpiry, TargetFilter, TargetRef, TriggerDefinition,
};
use engine::types::actions::GameAction;
use engine::types::card::LayoutKind;
use engine::types::card_type::{CardType, CoreType};
use engine::types::game_state::{CastOfferKind, CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Add mana directly to a player's pool (bypasses land tapping).
fn add_mana(runner: &mut GameRunner, player: PlayerId, color: ManaType, count: usize) {
    let state = runner.state_mut();
    let player_data = state.players.iter_mut().find(|p| p.id == player).unwrap();
    for _ in 0..count {
        player_data
            .mana_pool
            .add(ManaUnit::new(color, ObjectId(0), false, Vec::new()));
    }
}

/// Build the Adventure back_face data for Stomp.
fn stomp_back_face() -> BackFaceData {
    BackFaceData {
        is_swap_snapshot: false,
        trigger_printed_origins: Vec::new(),
        name: "Stomp".to_string(),
        power: None,
        toughness: None,
        loyalty: None,
        printed_loyalty: None,
        defense: None,
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
                    amount: QuantityExpr::Fixed { value: 2 },
                    target: TargetFilter::Any,
                    damage_source: None,
                    excess: None,
                },
            ),
        ],
        trigger_definitions: Default::default(),
        replacement_definitions: Default::default(),
        static_definitions: Default::default(),
        color: vec![ManaColor::Red],
        printed_ref: None,
        modal: None,
        additional_cost: None,
        strive_cost: None,
        casting_restrictions: Vec::new(),
        casting_options: Vec::new(),
        layout_kind: Some(LayoutKind::Adventure),
        parse_warnings: vec![],
    }
}

/// Build the BecomesTarget trigger definition for Bonecrusher Giant.
fn bonecrusher_trigger() -> TriggerDefinition {
    TriggerDefinition::new(TriggerMode::BecomesTarget).execute(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 2 },
            target: TargetFilter::TriggeringSpellController,
            damage_source: None,
            excess: None,
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
    Arc::make_mut(&mut obj.base_trigger_definitions).push(trigger);
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
            object_id: obj_id,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast should succeed");

    assert!(
        matches!(
            result.waiting_for,
            WaitingFor::CastOffer {
                player,
                kind: CastOfferKind::Adventure { .. },
            } if player == P0
        ),
        "Expected AdventureCastChoice, got {:?}",
        result.waiting_for
    );
}

/// CR 715.3a + CR 118.6a: A land-front Adventure card can be cast as its
/// instant/sorcery face even though the land itself has no mana cost. Lindblum,
/// Industrial Regency // Mage Siege represents the Final Fantasy Town cycle.
#[test]
fn land_front_adventure_offers_only_its_castable_spell_face() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let object_id = scenario
        .add_land_to_hand(P0, "Lindblum, Industrial Regency")
        .id();
    let mut runner = scenario.build();

    let mut mage_siege = stomp_back_face();
    mage_siege.name = "Mage Siege".to_string();
    mage_siege.card_types.subtypes.push("Adventure".to_string());
    mage_siege.layout_kind = Some(LayoutKind::Adventure);
    runner
        .state_mut()
        .objects
        .get_mut(&object_id)
        .unwrap()
        .back_face = Some(mage_siege);
    add_mana(&mut runner, P0, ManaType::Red, 2);

    let card_id = runner.state().objects[&object_id].card_id;
    let cast = GameAction::CastSpell {
        object_id,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    };
    assert!(
        legal_actions(runner.state()).contains(&cast),
        "a land-front Adventure's castable spell face must reach legal actions"
    );

    runner
        .act(cast)
        .expect("Mage Siege should reach its Adventure cast offer");
    let offered_faces: Vec<bool> = legal_actions(runner.state())
        .into_iter()
        .filter_map(|action| match action {
            GameAction::ChooseAdventureFace { creature } => Some(creature),
            _ => None,
        })
        .collect();
    assert_eq!(
        offered_faces,
        vec![false],
        "the uncastable land face must not be offered as a spell"
    );

    let result = runner
        .act(GameAction::ChooseAdventureFace { creature: false })
        .expect("Mage Siege should cast as the Adventure face");
    if matches!(result.waiting_for, WaitingFor::TargetSelection { .. }) {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Player(P1)),
            })
            .expect("the chosen Adventure face should complete target selection");
    }
    assert!(matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0));
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
            object_id: obj_id,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
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
        obj.casting_permissions.iter().any(|permission| matches!(
            permission,
            CastingPermission::AdventureCreature {
                granted_to: Some(_),
                exile_incarnation: Some(_)
            }
        )),
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
            object_id: obj_id,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
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
        !obj.casting_permissions.iter().any(|permission| matches!(
            permission,
            CastingPermission::AdventureCreature {
                granted_to: Some(_),
                exile_incarnation: Some(_)
            }
        )),
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
            .push(CastingPermission::resolved_adventure(
                P0,
                engine::types::identifiers::ObjectIncarnationRef::from_object(obj),
            ));
    }

    let card_id = runner.state().objects[&obj_id].card_id;
    add_mana(&mut runner, P0, ManaType::Red, 3);

    // Should NOT prompt for face choice (from exile, always creature)
    let result = runner
        .act(GameAction::CastSpell {
            object_id: obj_id,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast from exile should succeed");

    assert!(
        !matches!(
            result.waiting_for,
            WaitingFor::CastOffer {
                kind: CastOfferKind::Adventure { .. },
                ..
            }
        ),
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
    let giant_id = scenario.add_creature(P0, "Bonecrusher Giant", 4, 3).id();

    // P1 has a Lightning Bolt targeting the Giant
    let bolt_id = scenario.add_bolt_to_hand(P1);

    let mut runner = scenario.build();

    // Add the BecomesTarget trigger via runner
    {
        let trigger = bonecrusher_trigger();
        let state = runner.state_mut();
        let obj = state.objects.get_mut(&giant_id).unwrap();
        obj.trigger_definitions.push(trigger.clone());
        Arc::make_mut(&mut obj.base_trigger_definitions).push(trigger);
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
            object_id: bolt_id,
            card_id: bolt_card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
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
            engine::types::game_state::StackEntryKind::KeywordAction { .. } => "KeywordAction",
            engine::types::game_state::StackEntryKind::CombatDamage { .. } => "CombatDamage",
        })
        .collect();

    // BecomesTarget trigger should be on the stack above the bolt
    assert!(
        stack_desc.contains(&"Triggered"),
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
            object_id: obj_id,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
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
            object_id: obj_id,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
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
    assert_eq!(
        obj.zone,
        Zone::Exile,
        "After Stomp resolves: should be in exile"
    );
    assert!(
        obj.casting_permissions.iter().any(|permission| matches!(
            permission,
            CastingPermission::AdventureCreature {
                granted_to: Some(_),
                exile_incarnation: Some(_)
            }
        )),
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
            object_id: obj_id,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast creature from exile should succeed");

    assert!(
        !matches!(
            result.waiting_for,
            WaitingFor::CastOffer {
                kind: CastOfferKind::Adventure { .. },
                ..
            }
        ),
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
        !obj.casting_permissions.iter().any(|permission| matches!(
            permission,
            CastingPermission::AdventureCreature {
                granted_to: Some(_),
                exile_incarnation: Some(_)
            }
        )),
        "AdventureCreature permission should be cleared after leaving exile"
    );
}

// ---------------------------------------------------------------------------
// Test 8: Adventure qualifier uses the cast-time spell record
// ---------------------------------------------------------------------------

/// CR 715.2a + CR 715.4: A creature spell cast from an Adventure card has the
/// Adventure card's alternative characteristics for the qualifier, even when
/// the creature face is the face cast. The ordinary-creature cast is the
/// negative control; the Adventure spell's exile-and-recast path is the
/// positive reach guard.
#[test]
fn garenbrig_squire_triggers_only_for_adventure_creature_casts() {
    const GARENBRIG_SQUIRE: &str =
        "Whenever you cast a creature spell that has an Adventure, Garenbrig Squire gets +1/+1 until end of turn.";

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let squire = scenario
        .add_creature_from_oracle(P0, "Garenbrig Squire", 2, 2, GARENBRIG_SQUIRE)
        .id();
    let ordinary_creature = scenario.add_creature_to_hand(P0, "Grizzly Bear", 2, 2).id();
    let adventure_creature = add_bonecrusher_to_hand(&mut scenario);

    let mut runner = scenario.build();
    setup_bonecrusher_adventure(&mut runner, adventure_creature);
    add_mana(&mut runner, P0, ManaType::Red, 5);

    runner.cast(ordinary_creature).resolve();
    let ordinary_record = runner
        .state()
        .spells_cast_this_turn_by_player
        .get(&P0)
        .and_then(|records| records.back())
        .expect("ordinary creature cast must be recorded");
    assert!(!ordinary_record.has_adventure);
    let squire_after_ordinary = runner.state().objects.get(&squire).unwrap();
    assert_eq!(squire_after_ordinary.power, Some(2));
    assert_eq!(squire_after_ordinary.toughness, Some(2));

    runner
        .cast(adventure_creature)
        .adventure_face(false)
        .target_player(P1)
        .resolve();
    let adventure_spell_record = runner
        .state()
        .spells_cast_this_turn_by_player
        .get(&P0)
        .and_then(|records| records.back())
        .expect("Adventure spell cast must be recorded");
    assert!(!adventure_spell_record.has_adventure);
    let squire_after_adventure_spell = runner.state().objects.get(&squire).unwrap();
    assert_eq!(squire_after_adventure_spell.power, Some(2));
    assert_eq!(squire_after_adventure_spell.toughness, Some(2));

    runner.cast(adventure_creature).resolve();
    let adventure_record = runner
        .state()
        .spells_cast_this_turn_by_player
        .get(&P0)
        .and_then(|records| records.back())
        .expect("Adventure creature cast must be recorded");
    assert!(adventure_record.has_adventure);
    let squire_after_adventure = runner.state().objects.get(&squire).unwrap();
    assert_eq!(squire_after_adventure.power, Some(3));
    assert_eq!(squire_after_adventure.toughness, Some(3));
}

// Complete fixed-pin records for the noncreature/land members of CR 715.
fn normal_permanent_adventure_db() -> engine::database::CardDatabase {
    let records = [
        serde_json::json!({"name":"Jidoor, Aristocratic Capital // Overture","faceName":"Jidoor, Aristocratic Capital","side":"a","layout":"adventure","manaValue":0,"types":["Land"],"type":"Land — Town","subtypes":["Town"],"supertypes":[],"colors":[],"colorIdentity":["U"],"text":"This land enters tapped.\n{T}: Add {U}.","identifiers":{"scryfallOracleId":"bd513d9d-5aa2-4860-bd86-8b5d9430f133"}}),
        serde_json::json!({"name":"Jidoor, Aristocratic Capital // Overture","faceName":"Overture","side":"b","layout":"adventure","manaCost":"{4}{U}{U}","manaValue":0,"types":["Sorcery"],"type":"Sorcery — Adventure","subtypes":["Adventure"],"supertypes":[],"colors":[],"colorIdentity":["U"],"text":"Target opponent mills half their library, rounded down. (Then exile this card. You may play the land later from exile.)","identifiers":{"scryfallOracleId":"bd513d9d-5aa2-4860-bd86-8b5d9430f133"}}),
        serde_json::json!({"name":"My Precious // Allure of Power","faceName":"My Precious","side":"a","layout":"adventure","manaCost":"{3}","manaValue":3,"types":["Artifact"],"type":"Legendary Artifact — Equipment","subtypes":["Equipment"],"supertypes":["Legendary"],"colors":[],"colorIdentity":["B"],"text":"Equipped creature has hexproof and can't be blocked.\nEquip—{2}, Pay 2 life.","identifiers":{"scryfallOracleId":"2e728381-6db0-4c66-883d-82d718fef833"}}),
        serde_json::json!({"name":"My Precious // Allure of Power","faceName":"Allure of Power","side":"b","layout":"adventure","manaCost":"{1}{B}","manaValue":3,"types":["Instant"],"type":"Instant — Adventure","subtypes":["Adventure"],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"text":"As an additional cost to cast this spell, sacrifice a creature.\nDraw two cards.","identifiers":{"scryfallOracleId":"2e728381-6db0-4c66-883d-82d718fef833"}}),
    ];
    let mut export = serde_json::Map::new();
    for (index, record) in records.into_iter().enumerate() {
        let card: engine::database::mtgjson::AtomicCard = serde_json::from_value(record).unwrap();
        let face = engine::database::synthesis::build_oracle_face_multi(
            &card,
            card.identifiers.scryfall_oracle_id.clone(),
        );
        assert!(engine::game::coverage::card_face_gaps(&face).is_empty());
        let mut value = serde_json::to_value(&face).unwrap();
        value["layout"] = serde_json::json!("adventure");
        value["face_index"] = serde_json::json!(index % 2);
        export.insert(face.name.to_lowercase(), value);
    }
    engine::database::CardDatabase::from_json_str(&serde_json::to_string(&export).unwrap()).unwrap()
}

#[test]
fn resolving_adventure_noncreature_additional_cost_and_normal_equipment_play() {
    use engine::game::scenario_db::GameScenarioDbExt;
    let db = normal_permanent_adventure_db();
    for free in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P0, &["Allure draw A", "Allure draw B", "Allure reserve"]);
        let precious = scenario.add_real_card(
            if free { P1 } else { P0 },
            "My Precious",
            if free { Zone::Library } else { Zone::Hand },
            &db,
        );
        let first = scenario
            .add_creature(P0, "First sacrifice candidate", 1, 1)
            .id();
        let chosen = scenario
            .add_creature(P0, "Nonfirst sacrifice candidate", 2, 2)
            .id();
        let goods = if free {
            Some(scenario.add_spell_to_hand_from_oracle(P0, "Stolen Goods", false, "Target opponent exiles cards from the top of their library until they exile a nonland card. Until end of turn, you may cast that card without paying its mana cost.").with_mana_cost(ManaCost::Cost { generic:3, shards:vec![ManaCostShard::Blue] }).id())
        } else {
            None
        };
        if free {
            scenario.add_artifact_from_oracle(
                P0,
                "Fist of Suns",
                "You may pay {W}{U}{B}{R}{G} rather than pay the mana cost for spells you cast.",
            );
        }
        let mut runner = scenario.build();
        add_mana(
            &mut runner,
            P0,
            if free {
                ManaType::Blue
            } else {
                ManaType::Black
            },
            if free { 4 } else { 2 },
        );
        if let Some(goods) = goods {
            runner
                .cast(goods)
                .target_player(P1)
                .resolve()
                .assert_zone(&[precious], Zone::Exile);
        }
        if free {
            for color in [
                ManaType::White,
                ManaType::Blue,
                ManaType::Black,
                ManaType::Red,
                ManaType::Green,
            ] {
                add_mana(&mut runner, P0, color, 1);
            }
        }
        runner
            .act(GameAction::CastSpell {
                object_id: precious,
                card_id: runner.state().objects[&precious].card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::CastOffer {
                player: P0,
                kind: CastOfferKind::Adventure { .. }
            }
        ));
        assert!(legal_actions(runner.state())
            .contains(&GameAction::ChooseAdventureFace { creature: false }));
        runner
            .act(GameAction::ChooseAdventureFace { creature: false })
            .unwrap();
        let WaitingFor::PayCost {
            player,
            choices,
            count,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("Allure's mandatory sacrifice must be paid even when its mana cost is free");
        };
        assert_eq!(*player, P0);
        assert_eq!(*count, 1);
        assert!(choices.contains(&first) && choices.contains(&chosen));
        runner
            .act(GameAction::SelectCards {
                cards: vec![chosen],
            })
            .unwrap();
        assert_eq!(runner.state().objects[&chosen].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&first].zone, Zone::Battlefield);
        assert_eq!(runner.state().objects[&precious].name, "Allure of Power");
        assert!(
            matches!(runner.state().stack.back().unwrap().kind, engine::types::game_state::StackEntryKind::Spell { actual_mana_spent,.. } if actual_mana_spent == if free {0} else {2})
        );
        runner.resolve_top();
        assert_eq!(runner.state().objects[&precious].zone, Zone::Exile);
        assert_eq!(runner.state().objects[&precious].name, "My Precious");
        assert!(runner.state().objects[&precious]
            .card_types
            .core_types
            .contains(&CoreType::Artifact));
        assert!(!runner.state().objects[&precious]
            .card_types
            .core_types
            .contains(&CoreType::Creature));
        assert_eq!(runner.state().players[0].hand.len(), 2);
        add_mana(&mut runner, P0, ManaType::Colorless, 3);
        let committed = runner.cast(precious).commit();
        assert!(matches!(
            committed.state().stack.back().unwrap().kind,
            engine::types::game_state::StackEntryKind::Spell {
                actual_mana_spent: 3,
                ..
            }
        ));
        let outcome = committed.resolve();
        outcome.assert_zone(&[precious], Zone::Battlefield);
        assert!(matches!(
            outcome.final_waiting_for(),
            WaitingFor::Priority { .. }
        ));
        assert!(outcome.state().stack.is_empty() && outcome.state().resolution_stack.is_empty());
    }
}

#[test]
fn resolving_adventure_land_front_plays_jidoor_without_a_spell_or_mana_payment() {
    use engine::game::scenario_db::GameScenarioDbExt;
    let db = normal_permanent_adventure_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(
        P1,
        &[
            "Mill A",
            "Mill B",
            "Mill C",
            "Retained D",
            "Retained E",
            "Retained F",
            "Retained G",
        ],
    );
    let jidoor = scenario.add_real_card(P0, "Jidoor, Aristocratic Capital", Zone::Hand, &db);
    let forest = scenario
        .add_land_to_hand(P0, "Forest")
        .with_subtypes(vec!["Forest"])
        .id();
    let timing_witness = scenario
        .add_creature_to_hand(P0, "Ordinary zero-cost stack witness", 1, 1)
        .id();
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, ManaType::Blue, 6);
    runner
        .act(GameAction::CastSpell {
            object_id: jidoor,
            card_id: runner.state().objects[&jidoor].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::CastOffer {
            player: P0,
            kind: CastOfferKind::Adventure { .. }
        }
    ));
    let issued = legal_actions(runner.state());
    assert!(issued.contains(&GameAction::ChooseAdventureFace { creature: false }));
    assert!(!issued.contains(&GameAction::ChooseAdventureFace { creature: true }));
    runner
        .act(GameAction::ChooseAdventureFace { creature: false })
        .unwrap();
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Player(P1)),
            })
            .unwrap();
    }
    assert_eq!(runner.state().objects[&jidoor].mana_cost.mana_value(), 6);
    assert!(matches!(
        runner.state().stack.back().unwrap().kind,
        engine::types::game_state::StackEntryKind::Spell {
            actual_mana_spent: 6,
            ..
        }
    ));
    runner.resolve_top();
    assert_eq!(runner.state().objects[&jidoor].zone, Zone::Exile);
    assert_eq!(runner.state().players[1].graveyard.len(), 3);
    assert_eq!(runner.state().players[1].library.len(), 4);
    assert_eq!(runner.state().objects[&jidoor].mana_cost.mana_value(), 0);
    assert!(!engine::game::casting::can_cast_object_now(
        runner.state(),
        P0,
        jidoor
    ));
    let play = GameAction::PlayLand {
        object_id: jidoor,
        card_id: runner.state().objects[&jidoor].card_id,
    };
    assert!(legal_actions(runner.state()).contains(&play));
    let mut used = GameRunner::from_state(runner.state().clone());
    used.act(GameAction::PlayLand {
        object_id: forest,
        card_id: used.state().objects[&forest].card_id,
    })
    .unwrap();
    assert_eq!(used.state().players[0].lands_played_this_turn, 1);
    assert!(!legal_actions(used.state()).contains(&play));
    assert!(used.act(play.clone()).is_err());
    let mut busy = GameRunner::from_state(runner.state().clone());
    busy.cast(timing_witness).commit();
    assert!(!legal_actions(busy.state()).contains(&play));
    assert!(busy.act(play.clone()).is_err());
    let mut combat = GameRunner::from_state(runner.state().clone());
    combat.advance_to_phase(Phase::BeginCombat);
    assert!(!legal_actions(combat.state()).contains(&play));
    assert!(combat.act(play.clone()).is_err());
    let mut opponent = GameRunner::from_state(runner.state().clone());
    opponent.advance_to_phase(Phase::End);
    opponent.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(opponent.state().active_player, P1);
    assert!(!legal_actions(opponent.state()).contains(&play));
    assert!(opponent.act(play.clone()).is_err());
    let result = runner.act(play).unwrap();
    assert_eq!(runner.state().objects[&jidoor].zone, Zone::Battlefield);
    assert!(runner.state().objects[&jidoor].tapped);
    assert_eq!(runner.state().players[0].lands_played_this_turn, 1);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    assert!(runner.state().objects[&jidoor]
        .casting_permissions
        .is_empty());
    assert!(!result
        .events
        .iter()
        .any(|event| matches!(event, engine::types::events::GameEvent::SpellCast { .. })));
    assert!(runner.state().stack.is_empty() && runner.state().resolution_stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn resolving_adventure_public_counter_and_illegal_target_do_not_earn_authority() {
    for response in ["Cancel", "Unsummon", "Resolved"] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let giant = add_bonecrusher_to_hand(&mut scenario);
        let untouched = scenario
            .add_creature(P1, "Stomp first unselected creature", 3, 3)
            .id();
        let victim = scenario.add_creature(P1, "Stomp target", 3, 3).id();
        let response_spell = if response == "Resolved" {
            None
        } else {
            Some(
                scenario
                    .add_spell_to_hand_from_oracle(
                        P1,
                        response,
                        true,
                        if response == "Cancel" {
                            "Counter target spell."
                        } else {
                            "Return target creature to its owner's hand."
                        },
                    )
                    .with_mana_cost(ManaCost::Cost {
                        generic: if response == "Cancel" { 1 } else { 0 },
                        shards: vec![ManaCostShard::Blue; if response == "Cancel" { 2 } else { 1 }],
                    })
                    .id(),
            )
        };
        let mut runner = scenario.build();
        setup_bonecrusher_adventure(&mut runner, giant);
        let parsed = engine::parser::parse_oracle_text(
            "Damage can't be prevented this turn. Stomp deals 2 damage to any target.",
            "Stomp",
            &[],
            &["Instant".into()],
            &["Adventure".into()],
        );
        runner
            .state_mut()
            .objects
            .get_mut(&giant)
            .unwrap()
            .back_face
            .as_mut()
            .unwrap()
            .abilities = parsed.abilities;
        add_mana(&mut runner, P0, ManaType::Red, 2);
        add_mana(&mut runner, P1, ManaType::Blue, 3);
        runner
            .cast(giant)
            .adventure_face(false)
            .target_object(victim)
            .commit();
        assert_eq!(runner.state().objects[&giant].name, "Stomp");
        if let Some(spell) = response_spell {
            runner.act(GameAction::PassPriority).unwrap();
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { player: P1 }
            ));
            let committed = runner
                .cast(spell)
                .target_object(if response == "Cancel" { giant } else { victim })
                .commit();
            assert!(
                matches!(committed.state().stack.back().unwrap().kind,engine::types::game_state::StackEntryKind::Spell {actual_mana_spent,..} if actual_mana_spent==if response=="Cancel" {3} else {1})
            );
            committed.resolve();
        } else {
            runner.resolve_top();
        }
        let object = &runner.state().objects[&giant];
        assert_eq!(
            object.zone,
            if response == "Resolved" {
                Zone::Exile
            } else {
                Zone::Graveyard
            }
        );
        assert_eq!(object.name, "Bonecrusher Giant");
        assert_eq!(
            object
                .casting_permissions
                .iter()
                .filter(|permission| matches!(
                    permission,
                    CastingPermission::AdventureCreature { .. }
                ))
                .count(),
            usize::from(response == "Resolved")
        );
        assert_eq!(runner.state().objects[&untouched].damage_marked, 0);
        if response == "Unsummon" {
            assert_eq!(runner.state().objects[&victim].zone, Zone::Hand);
        }
        if response == "Resolved" {
            assert_eq!(runner.state().objects[&victim].damage_marked, 2);
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert!(runner.state().stack.is_empty() && runner.state().resolution_stack.is_empty());
    }
}

#[test]
fn resolving_adventure_normal_cost_combines_fist_and_sphere_without_borrowing_a_free_sibling() {
    use engine::game::scenario_db::GameScenarioDbExt;
    let db = normal_permanent_adventure_db();
    for choose_fist in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P0, &["Draw first", "Draw second", "Reserve"]);
        let precious = scenario.add_real_card(P0, "My Precious", Zone::Hand, &db);
        let sacrifice = scenario.add_creature(P0, "Allure sacrifice", 1, 1).id();
        scenario.add_artifact_from_oracle(
            P0,
            "Sphere of Resistance",
            "Spells cost {1} more to cast.",
        );
        scenario.add_artifact_from_oracle(
            P0,
            "Fist of Suns",
            "You may pay {W}{U}{B}{R}{G} rather than pay the mana cost for spells you cast.",
        );
        let mut runner = scenario.build();
        add_mana(&mut runner, P0, ManaType::Black, 3);
        runner
            .cast(precious)
            .adventure_face(false)
            .decline_optional()
            .sacrifice_with(&[sacrifice])
            .resolve()
            .assert_zone(&[precious], Zone::Exile);
        if choose_fist {
            for color in [
                ManaType::White,
                ManaType::Blue,
                ManaType::Black,
                ManaType::Red,
                ManaType::Green,
                ManaType::Colorless,
            ] {
                add_mana(&mut runner, P0, color, 1);
            }
        } else {
            add_mana(&mut runner, P0, ManaType::Colorless, 4);
        }
        runner
            .act(GameAction::CastSpell {
                object_id: precious,
                card_id: runner.state().objects[&precious].card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
        if choose_fist {
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::OptionalCostChoice { player: P0, .. }
            ));
            runner
                .act(GameAction::DecideOptionalCost { pay: true })
                .unwrap();
        } else if matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalCostChoice { .. }
        ) {
            runner
                .act(GameAction::DecideOptionalCost { pay: false })
                .unwrap();
        }
        assert!(
            matches!(runner.state().stack.back().unwrap().kind,engine::types::game_state::StackEntryKind::Spell {actual_mana_spent,..} if actual_mana_spent==if choose_fist {6} else {4})
        );
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        runner.resolve_top();
        assert_eq!(runner.state().objects[&precious].zone, Zone::Battlefield);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }
}

#[test]
fn resolving_adventure_pull_and_crypt_clear_only_the_departed_incarnation() {
    use engine::game::scenario_db::GameScenarioDbExt;
    let db = normal_permanent_adventure_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Draw A", "Draw B", "Draw C", "Draw D", "Reserve"]);
    let first = scenario.add_real_card(P0, "My Precious", Zone::Hand, &db);
    let second = scenario.add_real_card(P0, "My Precious", Zone::Hand, &db);
    let sacrifices = [
        scenario.add_creature(P0, "First sacrifice", 1, 1).id(),
        scenario.add_creature(P0, "Second sacrifice", 1, 1).id(),
    ];
    let pull = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Pull from Eternity",
            true,
            "Put target face-up exiled card into its owner's graveyard.",
        )
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::White],
        })
        .id();
    let crypt = scenario
        .add_artifact_from_oracle(
            P0,
            "Tormod's Crypt",
            "{T}, Sacrifice this artifact: Exile target player's graveyard.",
        )
        .id();
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, ManaType::Black, 4);
    for (card, sacrifice) in [(first, sacrifices[0]), (second, sacrifices[1])] {
        runner
            .cast(card)
            .adventure_face(false)
            .sacrifice_with(&[sacrifice])
            .resolve()
            .assert_zone(&[card], Zone::Exile);
    }
    let old = runner.state().objects[&first].casting_permissions.clone();
    let old_incarnation = runner.state().objects[&first].incarnation;
    let second_permissions = runner.state().objects[&second].casting_permissions.clone();
    let loaded: engine::types::game_state::GameState =
        serde_json::from_str(&serde_json::to_string(runner.state()).unwrap()).unwrap();
    runner = GameRunner::from_state(loaded);
    add_mana(&mut runner, P0, ManaType::White, 1);
    runner
        .cast(pull)
        .target_object(first)
        .resolve()
        .assert_zone(&[first], Zone::Graveyard);
    assert!(runner.state().objects[&first]
        .casting_permissions
        .is_empty());
    runner
        .activate(crypt, 0)
        .target_player(P0)
        .resolve()
        .assert_zone(&[first, crypt], Zone::Exile);
    assert!(runner.state().objects[&first].incarnation > old_incarnation);
    assert!(runner.state().objects[&first]
        .casting_permissions
        .is_empty());
    assert_eq!(
        runner.state().objects[&second].casting_permissions,
        second_permissions
    );
    add_mana(&mut runner, P0, ManaType::Colorless, 3);
    assert!(engine::game::casting::can_cast_object_now(
        runner.state(),
        P0,
        second
    ));
    assert!(!engine::game::casting::can_cast_object_now(
        runner.state(),
        P0,
        first
    ));
    // Supplemental stale-value attack cannot promote the old stay to this birth.
    runner
        .state_mut()
        .objects
        .get_mut(&first)
        .unwrap()
        .casting_permissions = old;
    assert!(!engine::game::casting::can_cast_object_now(
        runner.state(),
        P0,
        first
    ));
    runner
        .cast(second)
        .resolve()
        .assert_zone(&[second], Zone::Battlefield);
    assert!(runner.state().stack.is_empty() && runner.state().resolution_stack.is_empty());
}

#[test]
fn allure_mandatory_sacrifice_cannot_be_skipped_by_paid_or_free_face_selection() {
    use engine::game::scenario_db::GameScenarioDbExt;
    let db = normal_permanent_adventure_db();
    for free in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P0, &["Draw first", "Draw second", "Reserve"]);
        let precious = scenario.add_real_card(
            if free { P1 } else { P0 },
            "My Precious",
            if free { Zone::Library } else { Zone::Hand },
            &db,
        );
        let goods = if free {
            Some(scenario.add_spell_to_hand_from_oracle(P0,"Stolen Goods",false,"Target opponent exiles cards from the top of their library until they exile a nonland card. Until end of turn, you may cast that card without paying its mana cost.").with_mana_cost(ManaCost::Cost {generic:3,shards:vec![ManaCostShard::Blue]}).id())
        } else {
            None
        };
        let mut runner = scenario.build();
        add_mana(
            &mut runner,
            P0,
            if free {
                ManaType::Blue
            } else {
                ManaType::Black
            },
            if free { 4 } else { 2 },
        );
        if let Some(goods) = goods {
            runner
                .cast(goods)
                .target_player(P1)
                .resolve()
                .assert_zone(&[precious], Zone::Exile);
        }
        assert!(!engine::game::casting::can_cast_adventure_face_now(
            runner.state(),
            P0,
            precious,
            false
        ));
        if free {
            runner
                .act(GameAction::CastSpell {
                    object_id: precious,
                    card_id: runner.state().objects[&precious].card_id,
                    targets: vec![],
                    payment_mode: CastPaymentMode::Auto,
                })
                .unwrap();
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::CastOffer {
                    player: P0,
                    kind: CastOfferKind::Adventure { .. }
                }
            ));
            assert!(!legal_actions(runner.state())
                .contains(&GameAction::ChooseAdventureFace { creature: false }));
            assert!(legal_actions(runner.state())
                .contains(&GameAction::ChooseAdventureFace { creature: true }));
            assert!(runner
                .act(GameAction::ChooseAdventureFace { creature: false })
                .is_err());
            runner
                .act(GameAction::ChooseAdventureFace { creature: true })
                .unwrap();
            assert!(matches!(
                runner.state().stack.back().unwrap().kind,
                engine::types::game_state::StackEntryKind::Spell {
                    actual_mana_spent: 0,
                    ..
                }
            ));
            runner.resolve_top();
            assert_eq!(runner.state().objects[&precious].zone, Zone::Battlefield);
        } else {
            assert!(runner
                .cast(precious)
                .adventure_face(false)
                .try_resolve()
                .is_err());
            assert_eq!(runner.state().objects[&precious].zone, Zone::Hand);
        }
        assert_eq!(runner.state().players[0].hand.len(), usize::from(!free));
        assert!(runner.state().stack.is_empty() && runner.state().resolution_stack.is_empty());
    }
}
