#![allow(unused_imports)]
use super::*;

use engine::types::ability::{DamageAmount, Effect, TargetRef, TargetSpec};
use engine::types::game_state::StackEntryKind;
use engine::types::identifiers::{CardId, ObjectId};

/// CR 704.5g: Creature with lethal damage is destroyed by state-based actions.
///
/// A 2/2 creature that takes 2 damage has lethal damage marked. When SBAs are
/// checked (automatically after each apply()), the creature should be moved
/// to the graveyard.
#[test]
fn lethal_damage_destroys_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // A 2/2 creature on P1's side (target for bolt)
    let bear_id = scenario.add_creature(P1, "Bear", 2, 2).id();

    // Lightning Bolt in P0's hand (3 damage to any target)
    let bolt_id = scenario.add_bolt_to_hand(P0);

    let mut runner = scenario.build();

    // Cast bolt targeting the bear
    let bolt_card_id = runner.state().objects[&bolt_id].card_id;
    let result = runner
        .act(GameAction::CastSpell {
            card_id: bolt_card_id,
            targets: vec![],
        })
        .expect("cast should succeed");

    // Handle target selection if needed (Any = creatures + players)
    if matches!(result.waiting_for, WaitingFor::TargetSelection { .. }) {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(bear_id)],
            })
            .expect("select target");
    }

    // Resolve the bolt
    runner.resolve_top();

    // After bolt resolves and SBAs check: 3 damage >= 2 toughness = lethal
    // The creature should be in the graveyard
    assert_eq!(
        runner.state().objects[&bear_id].zone,
        Zone::Graveyard,
        "Bear with lethal damage should be in graveyard"
    );

    // Verify P1's graveyard contains the bear
    let p1_graveyard = &runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .unwrap()
        .graveyard;
    assert!(
        p1_graveyard.contains(&bear_id),
        "Bear should be in P1's graveyard"
    );
}

/// CR 704.5f: Creature with zero or less toughness is put into graveyard.
///
/// A creature whose toughness is reduced to 0 or less by a pump effect
/// should be moved to the graveyard by SBAs.
#[test]
fn zero_toughness_creature_dies() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Create a creature with 0 toughness -- SBAs should immediately destroy it
    let zero_t_id = scenario.add_vanilla(P0, 1, 0);
    let mut runner = scenario.build();

    // Pass priority to trigger SBA check
    let _ = runner.act(GameAction::PassPriority);

    // Creature with 0 toughness should be in graveyard
    assert_eq!(
        runner.state().objects[&zero_t_id].zone,
        Zone::Graveyard,
        "Creature with 0 toughness should be destroyed by SBAs"
    );
}

/// CR 704.5a: Player with zero or less life loses the game.
///
/// When a player's life is reduced to 0 or below, SBAs should set the game
/// to GameOver state.
#[test]
fn zero_life_player_loses() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P1, 3); // P1 at 3 life

    let bolt_id = scenario.add_bolt_to_hand(P0);

    let mut runner = scenario.build();

    // Cast bolt targeting P1
    let bolt_card_id = runner.state().objects[&bolt_id].card_id;
    let result = runner
        .act(GameAction::CastSpell {
            card_id: bolt_card_id,
            targets: vec![],
        })
        .expect("cast should succeed");

    if matches!(result.waiting_for, WaitingFor::TargetSelection { .. }) {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Player(P1)],
            })
            .expect("select target");
    }

    // Resolve the bolt (3 damage to P1 at 3 life -> 0 life)
    runner.resolve_top();

    // SBAs should detect P1 at 0 life and end the game
    assert_eq!(
        runner.state().players[1].life, 0,
        "P1 should be at 0 life"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::GameOver { winner: Some(p) } if p == P0
        ),
        "Game should be over with P0 winning. Got: {:?}",
        runner.state().waiting_for
    );
}

/// CR 704.5a: Player with negative life also loses.
///
/// Overkill damage should still trigger the loss condition.
#[test]
fn negative_life_player_loses() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P1, 1); // P1 at 1 life

    let bolt_id = scenario.add_bolt_to_hand(P0);

    let mut runner = scenario.build();

    let bolt_card_id = runner.state().objects[&bolt_id].card_id;
    let result = runner
        .act(GameAction::CastSpell {
            card_id: bolt_card_id,
            targets: vec![],
        })
        .expect("cast should succeed");

    if matches!(result.waiting_for, WaitingFor::TargetSelection { .. }) {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Player(P1)],
            })
            .expect("select target");
    }

    runner.resolve_top();

    // P1 at 1 - 3 = -2 life
    assert!(
        runner.state().players[1].life < 0,
        "P1 should have negative life"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::GameOver { .. }),
        "Game should be over when player has negative life"
    );
}

/// CR 704.5g + Deathtouch: Creature with deathtouch damage is destroyed.
///
/// Any amount of damage from a source with deathtouch is considered lethal.
/// A 5/5 creature taking 1 deathtouch damage should be destroyed.
#[test]
fn deathtouch_damage_is_lethal() {
    // Build state directly to test SBA integration with deathtouch damage.
    // The GameScenario builder doesn't expose &mut state after build(),
    // so we construct a GameState with pre-marked deathtouch damage.
    let mut state = engine::types::game_state::GameState::new_two_player(42);
    state.phase = Phase::PreCombatMain;
    state.turn_number = 2;
    state.waiting_for = WaitingFor::Priority { player: P0 };
    state.priority_player = P0;

    let card_id = engine::types::identifiers::CardId(state.next_object_id);
    let id = engine::game::zones::create_object(
        &mut state,
        card_id,
        P1,
        "Giant".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types
            .core_types
            .push(engine::types::card_type::CoreType::Creature);
        obj.power = Some(5);
        obj.toughness = Some(5);
        obj.base_power = Some(5);
        obj.base_toughness = Some(5);
        // 1 damage from a deathtouch source
        obj.damage_marked = 1;
        obj.dealt_deathtouch_damage = true;
    }

    // Run SBAs via apply (pass priority triggers SBA check)
    let result = engine::game::apply(&mut state, GameAction::PassPriority);
    assert!(result.is_ok());

    // The 5/5 with deathtouch damage should be destroyed
    assert_eq!(
        state.objects[&id].zone,
        Zone::Graveyard,
        "5/5 with deathtouch damage should be destroyed by SBAs"
    );
}

/// CR 704.5k: Indestructible prevents destruction from lethal damage.
///
/// An indestructible creature with damage >= toughness should NOT be destroyed.
#[test]
fn indestructible_survives_lethal_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Indestructible 2/2
    let mut indestructible_builder = scenario.add_creature(P1, "Darksteel Colossus", 2, 2);
    indestructible_builder.indestructible();
    let colossus_id = indestructible_builder.id();

    let bolt_id = scenario.add_bolt_to_hand(P0);

    let mut runner = scenario.build();

    // Cast bolt targeting the indestructible creature
    let bolt_card_id = runner.state().objects[&bolt_id].card_id;
    let result = runner
        .act(GameAction::CastSpell {
            card_id: bolt_card_id,
            targets: vec![],
        })
        .expect("cast should succeed");

    if matches!(result.waiting_for, WaitingFor::TargetSelection { .. }) {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(colossus_id)],
            })
            .expect("select target");
    }

    // Resolve the bolt
    runner.resolve_top();

    // The indestructible creature should survive despite lethal damage
    assert_eq!(
        runner.state().objects[&colossus_id].zone,
        Zone::Battlefield,
        "Indestructible creature should survive lethal damage"
    );

    // It should have damage marked on it though
    assert!(
        runner.state().objects[&colossus_id].damage_marked >= 3,
        "Indestructible creature should still have damage marked"
    );
}

/// CR 704.5 (general): SBAs are checked automatically after each action.
///
/// The engine's apply() function runs SBAs after processing each action.
/// Deal damage via spell resolution, and the creature should be automatically
/// destroyed without any explicit SBA call.
#[test]
fn sbas_checked_automatically_after_action() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // 1/1 creature (will die to bolt's 3 damage)
    let elf_id = scenario.add_vanilla(P1, 1, 1);
    let bolt_id = scenario.add_bolt_to_hand(P0);

    let mut runner = scenario.build();

    let bolt_card_id = runner.state().objects[&bolt_id].card_id;
    let result = runner
        .act(GameAction::CastSpell {
            card_id: bolt_card_id,
            targets: vec![],
        })
        .expect("cast should succeed");

    if matches!(result.waiting_for, WaitingFor::TargetSelection { .. }) {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(elf_id)],
            })
            .expect("select target");
    }

    // Resolve
    runner.resolve_top();

    // Without any explicit SBA call, the engine should have already checked
    // SBAs as part of the apply() call that resolved the bolt.
    // The 1/1 with 3 damage should be in the graveyard.
    assert_eq!(
        runner.state().objects[&elf_id].zone,
        Zone::Graveyard,
        "SBAs should automatically destroy creature with lethal damage"
    );

    // Battlefield count for P1 should be 0
    let p1_bf = runner
        .state()
        .battlefield
        .iter()
        .filter(|&&id| {
            runner
                .state()
                .objects
                .get(&id)
                .map(|o| o.owner == P1)
                .unwrap_or(false)
        })
        .count();
    assert_eq!(p1_bf, 0, "P1 should have no creatures on battlefield");
}
