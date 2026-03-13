#![allow(unused_imports)]
use super::*;

use engine::types::ability::{DamageAmount, Effect, TargetFilter, TargetRef, TypedFilter};
use engine::types::game_state::StackEntryKind;
use engine::types::identifiers::{CardId, ObjectId};

/// CR 608.2b: Spell with no legal targets on resolution fizzles.
///
/// Cast a bolt targeting a creature. Remove the creature before resolution
/// (via another bolt). When the first bolt resolves, it should fizzle
/// because its target is no longer legal.
#[test]
fn spell_fizzles_when_target_removed() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // A 2/2 creature (will be destroyed before first bolt resolves)
    let bear_id = scenario.add_creature(P1, "Bear", 2, 2).id();

    // Two bolts
    let bolt1_id = scenario.add_bolt_to_hand(P0);
    let bolt2_id = scenario.add_bolt_to_hand(P0);

    let mut runner = scenario.build();

    // Cast bolt 1 targeting the bear
    let bolt1_card_id = runner.state().objects[&bolt1_id].card_id;
    let result1 = runner
        .act(GameAction::CastSpell {
            card_id: bolt1_card_id,
            targets: vec![],
        })
        .expect("cast bolt 1 should succeed");

    // Handle target selection for bolt 1
    if matches!(result1.waiting_for, WaitingFor::TargetSelection { .. }) {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(bear_id)],
            })
            .expect("select target for bolt 1");
    }

    // Cast bolt 2 also targeting the bear (this will go on top of stack)
    let bolt2_card_id = runner.state().objects[&bolt2_id].card_id;
    let result2 = runner
        .act(GameAction::CastSpell {
            card_id: bolt2_card_id,
            targets: vec![],
        })
        .expect("cast bolt 2 should succeed");

    if matches!(result2.waiting_for, WaitingFor::TargetSelection { .. }) {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(bear_id)],
            })
            .expect("select target for bolt 2");
    }

    // Stack: [bolt1 (bottom, targeting bear), bolt2 (top, targeting bear)]
    assert_eq!(runner.state().stack.len(), 2, "Two bolts on stack");

    // Resolve bolt 2 (top of stack) -- kills the bear (3 damage >= 2 toughness)
    runner.resolve_top();

    // Bear should be in graveyard now (SBAs destroyed it)
    assert_eq!(
        runner.state().objects[&bear_id].zone,
        Zone::Graveyard,
        "Bear should be in graveyard after bolt 2 resolves"
    );

    // Bolt 1 is still on the stack, but its target (bear) is now illegal
    assert_eq!(
        runner.state().stack.len(),
        1,
        "Bolt 1 should still be on stack"
    );

    // Resolve bolt 1 -- it should fizzle (target no longer on battlefield)
    runner.resolve_top();

    // Bolt 1 should have fizzled (no damage dealt, moved to graveyard)
    // P1's life should still be 20 (bolt 1 didn't deal damage to a player)
    assert_eq!(
        runner.state().players[1].life,
        20,
        "P1's life should be unchanged (bolt 1 fizzled)"
    );

    // Stack should be empty
    assert!(
        runner.state().stack.is_empty(),
        "Stack should be empty after both bolts"
    );
}

/// CR 114.1: Spell requires legal targets on cast.
///
/// Attempting to cast a targeted spell with no valid targets should fail.
#[test]
fn no_legal_targets_prevents_casting() {
    // Create a state with a creature-only targeting spell and no creatures.
    // The bolt targets "Any" (always has players), so we build a custom
    // Doom Blade with "Creature" targeting to test this scenario.
    let mut state = engine::types::game_state::GameState::new_two_player(42);
    state.phase = Phase::PreCombatMain;
    state.turn_number = 2;
    state.active_player = P0;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };

    // Create a "Doom Blade" (destroy target creature) in hand
    let card_id = engine::types::identifiers::CardId(state.next_object_id);
    let obj_id = engine::game::zones::create_object(
        &mut state,
        card_id,
        P0,
        "Doom Blade".to_string(),
        Zone::Hand,
    );
    {
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.card_types
            .core_types
            .push(engine::types::card_type::CoreType::Instant);
        obj.abilities
            .push(engine::types::ability::AbilityDefinition {
                kind: engine::types::ability::AbilityKind::Spell,
                effect: Effect::Destroy {
                    target: TargetFilter::Typed(TypedFilter::creature()),
                },
                cost: None,
                sub_ability: None,
                duration: None,
                description: None,
                target_prompt: None,
                sorcery_speed: false,
                condition: None,
            });
    }

    // Try to cast with no creatures on the battlefield
    let result = engine::game::apply(
        &mut state,
        GameAction::CastSpell {
            card_id,
            targets: vec![],
        },
    );

    // Should fail because there are no legal targets (no creatures)
    assert!(
        result.is_err(),
        "Casting a creature-targeting spell with no creatures should fail"
    );
}

/// CR 608.2b: Spell with hexproof target -- hexproof prevents opponent targeting.
///
/// A creature with hexproof cannot be targeted by an opponent's spell.
/// The targeting system should exclude hexproof creatures from the opponent's
/// legal targets.
#[test]
fn hexproof_prevents_opponent_targeting() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P1's creature with hexproof
    let mut hex_builder = scenario.add_creature(P1, "Troll Ascetic", 3, 2);
    hex_builder.hexproof();
    let troll_id = hex_builder.id();

    let bolt_id = scenario.add_bolt_to_hand(P0);

    let mut runner = scenario.build();

    // Cast bolt -- the troll should NOT be a legal target for P0's bolt
    // because it has hexproof and P0 is the opponent.
    let bolt_card_id = runner.state().objects[&bolt_id].card_id;
    let result = runner
        .act(GameAction::CastSpell {
            card_id: bolt_card_id,
            targets: vec![],
        })
        .expect("cast should succeed (there are still legal targets: players)");

    // If target selection is needed, verify the troll is NOT in legal targets
    if let WaitingFor::TargetSelection { legal_targets, .. } = &result.waiting_for {
        assert!(
            !legal_targets.contains(&TargetRef::Object(troll_id)),
            "Hexproof creature should not be in legal targets for opponent"
        );

        // Both players should still be legal targets
        assert!(
            legal_targets.contains(&TargetRef::Player(P0)),
            "P0 should be a legal target"
        );
        assert!(
            legal_targets.contains(&TargetRef::Player(P1)),
            "P1 should be a legal target"
        );
    }

    // Troll should still be on the battlefield (never targeted)
    assert_eq!(
        runner.state().objects[&troll_id].zone,
        Zone::Battlefield,
        "Hexproof creature should remain on battlefield"
    );
}

/// CR 114.6: Shroud prevents targeting by any player.
///
/// A creature with shroud cannot be targeted by any player's spells or abilities,
/// including its controller.
#[test]
fn shroud_prevents_all_targeting() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P0's own creature with shroud
    let mut shroud_builder = scenario.add_creature(P0, "Invisible Stalker", 1, 1);
    shroud_builder.with_keyword(Keyword::Shroud);
    let stalker_id = shroud_builder.id();

    let bolt_id = scenario.add_bolt_to_hand(P0);

    let mut runner = scenario.build();

    // Cast bolt -- the stalker should NOT be a legal target even for P0 (controller)
    let bolt_card_id = runner.state().objects[&bolt_id].card_id;
    let result = runner
        .act(GameAction::CastSpell {
            card_id: bolt_card_id,
            targets: vec![],
        })
        .expect("cast should succeed (players are still valid targets)");

    // If target selection is needed, verify the stalker is NOT in legal targets
    if let WaitingFor::TargetSelection { legal_targets, .. } = &result.waiting_for {
        assert!(
            !legal_targets.contains(&TargetRef::Object(stalker_id)),
            "Shroud creature should not be in legal targets for any player"
        );
    }

    // Stalker should still be on the battlefield
    assert_eq!(
        runner.state().objects[&stalker_id].zone,
        Zone::Battlefield,
        "Shroud creature should remain on battlefield"
    );
}
