//! Integration tests for Kaito, Bane of Nightmares.
//!
//! Exercises all five building blocks together through the full engine pipeline:
//! - Ninjutsu activation (CR 702.49): hand -> battlefield tapped & attacking
//! - Emblem creation: +1 loyalty ability creates command zone emblem
//! - Planeswalker-to-creature animation: compound DuringYourTurn + HasCounters condition
//! - Surveil + dynamic draw: for each opponent who lost life this turn
//! - Tap + stun counters: -2 loyalty ability

use engine::game::combat::{AttackTarget, AttackerInfo, CombatState};
use engine::game::effects::counters::remove_counter_with_replacement;
use engine::game::layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ContinuousModification, ControllerRef, Effect,
    PlayerFilter, QuantityExpr, QuantityRef, StaticDefinition, TargetFilter, TargetRef,
    TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;
use std::sync::Arc;

// Verbatim Scryfall Oracle text for Kaito, Bane of Nightmares.
const KAITO_ORACLE: &str = r#"Ninjutsu {1}{U}{B} ({1}{U}{B}, Return an unblocked attacker you control to hand: Put this card onto the battlefield from your hand tapped and attacking.)
During your turn, as long as Kaito has one or more loyalty counters on him, he's a 3/4 Ninja creature and has hexproof.
+1: You get an emblem with "Ninjas you control get +1/+1."
0: Surveil 2. Then draw a card for each opponent who lost life this turn.
−2: Tap target creature. Put two stun counters on it."#;

// Verbatim MTGJSON Oracle text for Futurist Operative.
const FUTURIST_ORACLE: &str = "As long as this creature is tapped, it's a Human Citizen with base power and toughness 1/1 and can't be blocked.\n{2}{U}: Untap this creature.";

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

/// Emblem static for "Ninjas you control get +1/+1".
fn ninja_pump_static() -> StaticDefinition {
    StaticDefinition {
        mode: StaticMode::Continuous,
        affected: Some(TargetFilter::Typed(
            TypedFilter::creature()
                .subtype("Ninja".to_string())
                .controller(ControllerRef::You),
        )),
        modifications: vec![
            ContinuousModification::AddPower { value: 1 },
            ContinuousModification::AddToughness { value: 1 },
        ],
        condition: None,
        per_player_condition: None,
        affected_zone: None,
        effect_zone: None,
        active_zones: vec![],
        characteristic_defining: false,
        description: None,
        attack_defended: None,
        source_controller: None,
        source_object: None,
        bypass_beneficiary: None,
        protection_does_not_remove: None,
        room_door: None,
        granting_object: None,
    }
}

/// Kaito's compound animation static: DuringYourTurn + HasCounters(loyalty, 1).
fn kaito_animation_static() -> StaticDefinition {
    let parsed = parse_oracle_text(
        KAITO_ORACLE,
        "Kaito, Bane of Nightmares",
        &[],
        &["Planeswalker".to_string()],
        &["Kaito".to_string()],
    );
    assert_eq!(
        parsed.statics.len(),
        1,
        "Kaito must parse its animation static"
    );
    let animation = parsed.statics.into_iter().next().unwrap();
    assert_eq!(animation.affected, Some(TargetFilter::SelfRef));
    animation
}

/// Set up a runner with Kaito on the battlefield at the given phase.
/// Returns (runner, kaito_object_id).
fn setup_kaito_on_battlefield(phase: Phase) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(phase);

    // Create a placeholder creature, then modify it into Kaito
    let kaito_id = scenario
        .add_creature(P0, "Kaito, Bane of Nightmares", 0, 0)
        .id();

    let mut runner = scenario.build();

    // Transform into Kaito planeswalker via state_mut
    {
        let state = runner.state_mut();
        let obj = state.objects.get_mut(&kaito_id).unwrap();

        // Set as Planeswalker type
        obj.card_types.core_types.clear();
        obj.card_types.core_types.push(CoreType::Planeswalker);
        obj.card_types.subtypes.push("Kaito".to_string());
        obj.base_card_types = obj.card_types.clone();

        // Set loyalty
        obj.loyalty = Some(4);
        obj.counters.insert(CounterType::Loyalty, 4);
        obj.power = None;
        obj.toughness = None;
        obj.base_power = None;
        obj.base_toughness = None;

        // Add Ninjutsu keyword
        let ninjutsu_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Black],
            generic: 1,
        };
        obj.keywords.push(Keyword::Ninjutsu(ninjutsu_cost.clone()));
        obj.base_keywords.push(Keyword::Ninjutsu(ninjutsu_cost));

        // Add compound animation static
        let animation = kaito_animation_static();
        obj.static_definitions.push(animation.clone());
        Arc::make_mut(&mut obj.base_static_definitions).push(animation);

        // +1 loyalty: CreateEmblem
        let emblem_ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::CreateEmblem {
                statics: vec![ninja_pump_static()],
                triggers: Vec::new(),
            },
        )
        .cost(AbilityCost::Loyalty { amount: 1 });
        Arc::make_mut(&mut obj.abilities).push(emblem_ability.clone());
        Arc::make_mut(&mut obj.base_abilities).push(emblem_ability);

        // 0 loyalty: Surveil 2, then draw for each opponent who lost life
        let draw_sub = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::PlayerCount {
                        filter: PlayerFilter::OpponentLostLife,
                    },
                },
                target: TargetFilter::Controller,
            },
        );
        let surveil_ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Surveil {
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Controller,
            },
        )
        .cost(AbilityCost::Loyalty { amount: 0 })
        .sub_ability(draw_sub);
        Arc::make_mut(&mut obj.abilities).push(surveil_ability.clone());
        Arc::make_mut(&mut obj.base_abilities).push(surveil_ability);

        obj.mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Black],
            generic: 2,
        };

        state.layers_dirty.mark_full();
    }

    (runner, kaito_id)
}

// ---------------------------------------------------------------------------
// Test: Ninjutsu activation
// ---------------------------------------------------------------------------

#[test]
fn kaito_ninjutsu_activation() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareBlockers);

    // P0 has an unblocked 1/1 attacker
    let attacker_id = scenario
        .add_creature(P0, "Ninja of the Deep Hours", 1, 1)
        .id();

    // Add Kaito to P0's hand
    let kaito_id = scenario
        .add_creature_to_hand(P0, "Kaito, Bane of Nightmares", 0, 0)
        .id();

    let mut runner = scenario.build();

    // Set Kaito as planeswalker with Ninjutsu
    {
        let state = runner.state_mut();
        let obj = state.objects.get_mut(&kaito_id).unwrap();
        obj.card_types.core_types.clear();
        obj.card_types.core_types.push(CoreType::Planeswalker);
        obj.base_card_types = obj.card_types.clone();
        obj.loyalty = Some(4);
        obj.power = None;
        obj.toughness = None;
        obj.base_power = None;
        obj.base_toughness = None;

        let ninjutsu_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Black],
            generic: 1,
        };
        obj.keywords.push(Keyword::Ninjutsu(ninjutsu_cost.clone()));
        obj.base_keywords.push(Keyword::Ninjutsu(ninjutsu_cost));
    }

    // Set up combat state with the attacker as unblocked
    {
        let state = runner.state_mut();
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker_id, P1)],
            ..Default::default()
        });
        state.waiting_for = WaitingFor::Priority { player: P0 };
        state.priority_player = P0;
    }

    // Add mana to pay for Ninjutsu cost
    add_mana(&mut runner, P0, ManaType::Blue, 1);
    add_mana(&mut runner, P0, ManaType::Black, 1);
    add_mana(&mut runner, P0, ManaType::Colorless, 1);

    // Activate Ninjutsu
    let result = runner.act(GameAction::ActivateNinjutsu {
        ninjutsu_object_id: kaito_id,
        creature_to_return: attacker_id,
    });
    assert!(result.is_ok(), "Ninjutsu activation should succeed");

    let state = runner.state();

    // Attacker should be back in hand
    assert_eq!(
        state.objects[&attacker_id].zone,
        Zone::Hand,
        "Returned attacker should be in hand"
    );

    // Kaito should be on battlefield and tapped
    let kaito = &state.objects[&kaito_id];
    assert_eq!(
        kaito.zone,
        Zone::Battlefield,
        "Kaito should be on battlefield"
    );
    assert!(kaito.tapped, "Kaito should enter tapped");

    // Kaito should be in the attackers list
    let combat = state
        .combat
        .as_ref()
        .expect("Combat should still be active");
    assert!(
        combat.attackers.iter().any(|a| a.object_id == kaito_id),
        "Kaito should be in the attackers list"
    );

    // The original attacker should NOT be in attackers list
    assert!(
        !combat.attackers.iter().any(|a| a.object_id == attacker_id),
        "Returned attacker should not be in combat"
    );
}

// ---------------------------------------------------------------------------
// Test: Emblem creation
// ---------------------------------------------------------------------------

#[test]
fn kaito_emblem_creation() {
    let (mut runner, kaito_id) = setup_kaito_on_battlefield(Phase::PreCombatMain);

    // Add a Ninja creature for P0
    let ninja_id = {
        let state = runner.state_mut();
        let card_id = CardId(state.next_object_id);
        let id = zones::create_object(
            state,
            card_id,
            P0,
            "Ninja Token".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.card_types.subtypes.push("Ninja".to_string());
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
        obj.entered_battlefield_turn = Some(state.turn_number.saturating_sub(1));
        state.layers_dirty.mark_full();
        id
    };

    // Add a non-Ninja creature for P0
    let non_ninja_id = {
        let state = runner.state_mut();
        let card_id = CardId(state.next_object_id);
        let id = zones::create_object(state, card_id, P0, "Goblin".to_string(), Zone::Battlefield);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(1);
        obj.toughness = Some(1);
        obj.base_power = Some(1);
        obj.base_toughness = Some(1);
        obj.entered_battlefield_turn = Some(state.turn_number.saturating_sub(1));
        state.layers_dirty.mark_full();
        id
    };

    // Activate +1 loyalty ability (CreateEmblem) and drive it to resolution.
    let outcome = runner.activate(kaito_id, 0).resolve();

    let state = outcome.state();

    // An emblem should exist in command zone
    assert!(
        !state.command_zone.is_empty(),
        "Command zone should contain the emblem"
    );
    let emblem_id = state.command_zone[0];
    let emblem = &state.objects[&emblem_id];
    assert!(emblem.is_emblem, "Object should be an emblem");
    assert_eq!(
        emblem.zone,
        Zone::Command,
        "Emblem should be in command zone"
    );

    // Kaito's loyalty should have increased by 1 (from 4 to 5)
    let kaito = &state.objects[&kaito_id];
    assert_eq!(
        kaito.loyalty,
        Some(5),
        "Kaito loyalty should be 5 after +1 ability"
    );

    // After layer evaluation, Ninja should get +1/+1 from emblem
    let ninja = &state.objects[&ninja_id];
    assert_eq!(
        ninja.power,
        Some(3),
        "Ninja should have 3 power (2 base + 1 emblem)"
    );
    assert_eq!(
        ninja.toughness,
        Some(3),
        "Ninja should have 3 toughness (2 base + 1 emblem)"
    );

    // Non-Ninja should NOT get the bonus
    let goblin = &state.objects[&non_ninja_id];
    assert_eq!(goblin.power, Some(1), "Non-Ninja should keep base power");
    assert_eq!(
        goblin.toughness,
        Some(1),
        "Non-Ninja should keep base toughness"
    );
}

// ---------------------------------------------------------------------------
// Test: Planeswalker-to-creature animation
// ---------------------------------------------------------------------------

// CR 205.1a + CR 613.1d: setting only subtypes preserves existing card types.
#[test]
fn futurist_subtype_animation_retains_artifact_and_uses_own_tapped_condition() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tapped_id = scenario
        .add_creature_from_oracle(P0, "Futurist Operative", 3, 4, FUTURIST_ORACLE)
        .with_subtypes(vec!["Human", "Ninja"])
        .as_artifact_creature()
        .id();
    let untapped_id = scenario
        .add_creature_from_oracle(P0, "Futurist Operative", 3, 4, FUTURIST_ORACLE)
        .with_subtypes(vec!["Human", "Ninja"])
        .as_artifact_creature()
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&tapped_id)
        .unwrap()
        .tapped = true;
    runner.state_mut().layers_dirty.mark_full();
    for id in [tapped_id, untapped_id] {
        assert_eq!(
            runner.state().objects[&id].base_card_types.core_types,
            vec![CoreType::Creature, CoreType::Artifact]
        );
    }
    runner
        .act(GameAction::PassPriority)
        .expect("evaluate the parsed subtype-only animation");
    let state = runner.state();
    let tapped = &state.objects[&tapped_id];
    // CR 613.4b: the active base-P/T change proves the layer path was reached.
    assert_eq!((tapped.power, tapped.toughness), (Some(1), Some(1)));
    assert_eq!(
        tapped.card_types.core_types,
        vec![CoreType::Creature, CoreType::Artifact]
    );
    assert!(tapped.card_types.subtypes.contains(&"Human".to_string()));
    assert!(tapped.card_types.subtypes.contains(&"Citizen".to_string()));
    // CR 611.3a: the other source's tapped state cannot activate this source.
    let untapped = &state.objects[&untapped_id];
    assert_eq!((untapped.power, untapped.toughness), (Some(3), Some(4)));
    assert_eq!(
        untapped.card_types.core_types,
        vec![CoreType::Creature, CoreType::Artifact]
    );
    assert_eq!(untapped.card_types.subtypes, vec!["Human", "Ninja"]);
    assert!(state.stack.is_empty());
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));

    runner
        .state_mut()
        .objects
        .get_mut(&tapped_id)
        .unwrap()
        .tapped = false;
    runner.state_mut().layers_dirty.mark_full();
    runner
        .act(GameAction::PassPriority)
        .expect("re-evaluate after the source becomes untapped");
    let state = runner.state();
    for id in [tapped_id, untapped_id] {
        let object = &state.objects[&id];
        assert_eq!((object.power, object.toughness), (Some(3), Some(4)));
        assert_eq!(
            object.card_types.core_types,
            vec![CoreType::Creature, CoreType::Artifact]
        );
        assert_eq!(object.card_types.subtypes, vec!["Human", "Ninja"]);
    }
    assert!(state.stack.is_empty());
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
}

#[test]
fn kaito_animation_during_your_turn_with_loyalty() {
    let (mut runner, kaito_id) = setup_kaito_on_battlefield(Phase::PreCombatMain);

    // Force layer evaluation
    let _ = runner.act(GameAction::PassPriority);

    let state = runner.state();
    let kaito = &state.objects[&kaito_id];

    // During your turn with loyalty counters: should be a 3/4 Ninja creature with hexproof
    assert_eq!(
        kaito.power,
        Some(3),
        "Kaito should be 3 power during your turn"
    );
    assert_eq!(
        kaito.toughness,
        Some(4),
        "Kaito should be 4 toughness during your turn"
    );
    assert!(
        kaito.card_types.core_types.contains(&CoreType::Creature),
        "Kaito should be a creature during your turn"
    );
    assert!(
        kaito.card_types.subtypes.contains(&"Ninja".to_string()),
        "Kaito should be a Ninja during your turn"
    );
    assert!(
        kaito.keywords.contains(&Keyword::Hexproof),
        "Kaito should have hexproof during your turn"
    );
}

// CR 205.1a + CR 120.3e: Kaito becomes only a creature, retaining loyalty
// counters while a blocker's combat damage is marked on him.
#[test]
fn kaito_blocked_as_creature_takes_damage_without_losing_loyalty() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kaito_id = scenario
        .add_planeswalker_from_oracle(P0, "Kaito, Bane of Nightmares", "Kaito", 4, KAITO_ORACLE)
        .as_legendary()
        .id();
    let blocker_id = scenario.add_creature(P1, "Blocker", 2, 2).id();
    let mut runner = scenario.build();
    runner
        .act(GameAction::PassPriority)
        .expect("evaluate Kaito's parsed static");
    let kaito = &runner.state().objects[&kaito_id];
    assert_eq!(kaito.card_types.core_types, vec![CoreType::Creature]);
    assert_eq!(kaito.card_types.subtypes, vec!["Ninja".to_string()]);
    // CR 205.4b: changing card types preserves supertypes.
    assert!(kaito.card_types.supertypes.contains(&Supertype::Legendary));
    assert_eq!((kaito.power, kaito.toughness), (Some(3), Some(4)));
    assert!(kaito.keywords.contains(&Keyword::Hexproof));
    assert_eq!(kaito.loyalty, Some(4));
    assert_eq!(kaito.counters.get(&CounterType::Loyalty), Some(&4));

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(kaito_id, AttackTarget::Player(P1))])
        .expect("animated Kaito can attack");
    // CR 509.1: reach the defending player's actual blocker declaration prompt.
    for _ in 0..32 {
        match &runner.state().waiting_for {
            WaitingFor::DeclareBlockers { .. } => break,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority until blockers are declared");
            }
            other => panic!("expected priority or declare blockers, got {other:?}"),
        }
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { player: P1, .. }
    ));
    runner
        .declare_blockers(&[(blocker_id, kaito_id)])
        .expect("the 2/2 can block Kaito");
    let outcome = runner.combat_damage();
    assert!(
        outcome.events().iter().any(|event| matches!(
            event,
            GameEvent::DamageDealt { source_id, target: TargetRef::Object(target), amount: 2, .. }
                if *source_id == blocker_id && *target == kaito_id
        )),
        "the blocker must actually deal two damage to Kaito"
    );
    let state = outcome.state();
    let kaito = &state.objects[&kaito_id];
    assert_eq!(kaito.zone, Zone::Battlefield);
    assert_eq!(kaito.damage_marked, 2);
    assert_eq!(kaito.loyalty, Some(4));
    assert_eq!(kaito.counters.get(&CounterType::Loyalty), Some(&4));
    assert_eq!(state.objects[&blocker_id].zone, Zone::Graveyard);
    assert!(state.stack.is_empty());
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(matches!(
        state.phase,
        Phase::EndCombat | Phase::PostCombatMain
    ));
}

#[test]
fn kaito_not_animated_on_opponents_turn() {
    let (mut runner, kaito_id) = setup_kaito_on_battlefield(Phase::PreCombatMain);
    runner
        .act(GameAction::PassPriority)
        .expect("apply Kaito's animation on his controller's turn");
    assert_eq!(
        runner.state().objects[&kaito_id].card_types.core_types,
        vec![CoreType::Creature]
    );

    // Switch to opponent's turn
    runner.state_mut().active_player = P1;
    runner.state_mut().layers_dirty.mark_full();
    runner
        .act(GameAction::PassPriority)
        .expect("re-evaluate Kaito on the opponent's turn");

    let state = runner.state();
    let kaito = &state.objects[&kaito_id];

    // Not your turn: should NOT be a creature
    assert!(
        !kaito.card_types.core_types.contains(&CoreType::Creature),
        "Kaito should NOT be a creature on opponent's turn"
    );
    // Should still be a planeswalker
    assert!(
        kaito
            .card_types
            .core_types
            .contains(&CoreType::Planeswalker),
        "Kaito should still be a planeswalker"
    );
    assert_eq!(kaito.card_types.subtypes, vec!["Kaito".to_string()]);
    assert_eq!(kaito.loyalty, Some(4));
    assert!(!kaito.keywords.contains(&Keyword::Hexproof));
}

#[test]
fn kaito_layer_animation_stops_without_loyalty_counters() {
    let (mut runner, kaito_id) = setup_kaito_on_battlefield(Phase::PreCombatMain);
    runner
        .act(GameAction::PassPriority)
        .expect("apply Kaito's animation with loyalty counters");
    assert_eq!(
        runner.state().objects[&kaito_id].card_types.core_types,
        vec![CoreType::Creature]
    );

    // CR 611.3a: the static's counter condition is live. This focused layer-only
    // check stops before state-based actions would remove a zero-loyalty walker.
    let mut events = Vec::new();
    remove_counter_with_replacement(
        runner.state_mut(),
        kaito_id,
        CounterType::Loyalty,
        4,
        &mut events,
    );
    assert!(events.iter().any(|event| matches!(
        event,
        GameEvent::CounterRemoved {
            object_id,
            counter_type: CounterType::Loyalty,
            count: 4,
        } if *object_id == kaito_id
    )));
    layers::evaluate_layers(runner.state_mut());
    let kaito = &runner.state().objects[&kaito_id];
    assert_eq!(kaito.card_types.core_types, vec![CoreType::Planeswalker]);
    assert_eq!(kaito.card_types.subtypes, vec!["Kaito".to_string()]);
    assert_eq!(kaito.loyalty, Some(0));
    assert_eq!(kaito.counters.get(&CounterType::Loyalty), Some(&0));
}

// ---------------------------------------------------------------------------
// Test: Surveil + draw for each opponent who lost life
// ---------------------------------------------------------------------------

#[test]
fn kaito_surveil_and_draw() {
    let (mut runner, kaito_id) = setup_kaito_on_battlefield(Phase::PostCombatMain);

    // Add cards to P0's library for draw/surveil. `create_object` already adds
    // the object to the owner's library zone — a manual `push_back` here would
    // double-add each card, malforming the library with duplicate ObjectIds.
    for i in 0..5u32 {
        let state = runner.state_mut();
        let card_id = CardId(state.next_object_id);
        zones::create_object(
            state,
            card_id,
            P0,
            format!("Library Card {}", i),
            Zone::Library,
        );
    }

    // Mark opponent (P1) as having lost life this turn
    runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P1)
        .unwrap()
        .life_lost_this_turn = 3;

    let hand_before = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .unwrap()
        .hand
        .len();

    // Activate 0 loyalty ability (Surveil 2, then draw for each opponent who lost life).
    // Resolve by effect shape instead of hardcoded index; this test runner object
    // can carry pre-existing abilities from card data.
    let surveil_ability_index = {
        let state = runner.state();
        let kaito = state.objects.get(&kaito_id).unwrap();
        kaito
            .abilities
            .iter()
            .position(|ability| matches!(*ability.effect, Effect::Surveil { .. }))
            .expect("Kaito should have a surveil loyalty ability")
    };

    let result = runner.act(GameAction::ActivateAbility {
        source_id: kaito_id,
        ability_index: surveil_ability_index,
    });
    assert!(
        result.is_ok(),
        "Surveil ability activation should succeed: {:?}",
        result.err()
    );

    // Resolve the ability fully
    for _ in 0..30 {
        match &runner.state().waiting_for {
            WaitingFor::SurveilChoice { cards, .. } => {
                // CR 701.25a: the payload is the keep-on-top set — pass every
                // looked-at card to keep them all on top (none milled).
                let keep = cards.clone();
                let _ = runner.act(GameAction::SelectCards { cards: keep });
            }
            WaitingFor::Priority { .. } => {
                let _ = runner.act(GameAction::PassPriority);
            }
            _ => break,
        }
    }

    let state = runner.state();

    // In 2-player game, 1 opponent lost life -> draw 1 card
    let hand_after = state
        .players
        .iter()
        .find(|p| p.id == P0)
        .unwrap()
        .hand
        .len();

    // Should have drawn at least 1 card (for the opponent who lost life)
    assert!(
        hand_after > hand_before,
        "Should have drawn cards: before={}, after={}",
        hand_before,
        hand_after
    );
}
