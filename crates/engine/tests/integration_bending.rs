//! Integration tests for the four bending mechanics (Fire, Air, Earth, Water)
//! and their shared infrastructure (meta-triggers, AI candidates, mana payment finalization).

use engine::ai_support::candidate_actions;
use engine::game::scenario::{GameScenario, P0};
use engine::game::zones::create_object;
use engine::types::ability::{AbilityCost, Effect, QuantityExpr, ResolvedAbility, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::{BendingType, GameEvent};
use engine::types::game_state::{CastingVariant, ConvokeMode, GameState, PendingCast, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn add_mana(state: &mut GameState, player: PlayerId, color: ManaType, count: usize) {
    let p = state.players.iter_mut().find(|p| p.id == player).unwrap();
    for _ in 0..count {
        p.mana_pool.add(ManaUnit {
            color,
            source_id: ObjectId(0),
            snow: false,
            restrictions: Vec::new(),
            expiry: None,
        });
    }
}

// ---------------------------------------------------------------------------
// Step 1: Earthbend event emission
// ---------------------------------------------------------------------------

#[test]
fn test_earthbending_animate_and_event() {
    let mut state = GameState::new_two_player(42);
    let land_id = create_object(
        &mut state,
        CardId(1),
        P0,
        "Mountain".to_string(),
        Zone::Battlefield,
    );

    let ability = ResolvedAbility::new(
        Effect::Animate {
            power: Some(3),
            toughness: Some(3),
            types: vec!["Creature".to_string()],
            target: TargetFilter::None,
            keywords: vec![Keyword::Haste],
            is_earthbend: true,
        },
        vec![],
        land_id,
        P0,
    );

    let mut events = Vec::new();
    engine::game::effects::animate::resolve(&mut state, &ability, &mut events).unwrap();

    // Verify the land became a 3/3 creature with haste
    let obj = &state.objects[&land_id];
    assert_eq!(obj.power, Some(3));
    assert_eq!(obj.toughness, Some(3));
    assert!(obj.card_types.core_types.contains(&CoreType::Creature));
    assert!(obj.keywords.contains(&Keyword::Haste));

    // Verify Earthbend event emitted
    assert!(
        events.iter().any(|e| matches!(
            e,
            GameEvent::Earthbend {
                source_id,
                controller,
            } if *source_id == land_id && *controller == P0
        )),
        "Expected Earthbend event, got: {events:?}"
    );

    // Verify BendingType::Earth tracked on player
    let player = state.players.iter().find(|p| p.id == P0).unwrap();
    assert!(player.bending_types_this_turn.contains(&BendingType::Earth));
}

#[test]
fn test_earthbending_non_earthbend_animate_no_event() {
    let mut state = GameState::new_two_player(42);
    let obj_id = create_object(
        &mut state,
        CardId(1),
        P0,
        "Enchantment".to_string(),
        Zone::Battlefield,
    );

    let ability = ResolvedAbility::new(
        Effect::Animate {
            power: Some(4),
            toughness: Some(4),
            types: vec!["Creature".to_string()],
            target: TargetFilter::None,
            keywords: vec![],
            is_earthbend: false,
        },
        vec![],
        obj_id,
        P0,
    );

    let mut events = Vec::new();
    engine::game::effects::animate::resolve(&mut state, &ability, &mut events).unwrap();

    // No Earthbend event for non-earthbend animations
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, GameEvent::Earthbend { .. })),
        "Non-earthbend animate should not emit Earthbend event"
    );
    let player = state.players.iter().find(|p| p.id == P0).unwrap();
    assert!(!player.bending_types_this_turn.contains(&BendingType::Earth));
}

// ---------------------------------------------------------------------------
// Step 2: Waterbend event emission + zone check
// ---------------------------------------------------------------------------

#[test]
fn test_waterbending_tap_to_pay() {
    let mut scenario = GameScenario::default();
    scenario.at_phase(Phase::PreCombatMain);

    let creature_id = scenario.add_creature(P0, "Water Tribe Warrior", 2, 2).id();

    let mut runner = scenario.build();

    // Set up ManaPayment state with Waterbend mode
    runner.state_mut().waiting_for = WaitingFor::ManaPayment {
        player: P0,
        convoke_mode: Some(ConvokeMode::Waterbend),
    };

    let result = runner
        .act(GameAction::TapForConvoke {
            object_id: creature_id,
            mana_type: ManaType::Colorless,
        })
        .unwrap();

    // Verify creature was tapped
    assert!(runner.state().objects[&creature_id].tapped);

    // Verify Waterbend event emitted
    assert!(
        result.events.iter().any(|e| matches!(
            e,
            GameEvent::Waterbend {
                source_id,
                controller,
            } if *source_id == creature_id && *controller == P0
        )),
        "Expected Waterbend event"
    );

    // Verify BendingType::Water tracked on player
    let player = runner.state().players.iter().find(|p| p.id == P0).unwrap();
    assert!(player.bending_types_this_turn.contains(&BendingType::Water));
}

#[test]
fn test_waterbending_rejected_when_not_eligible() {
    let mut scenario = GameScenario::default();
    scenario.at_phase(Phase::PreCombatMain);
    let creature_id = scenario.add_creature(P0, "Water Tribe Warrior", 2, 2).id();
    let mut runner = scenario.build();

    // convoke_mode: None should reject TapForConvoke
    runner.state_mut().waiting_for = WaitingFor::ManaPayment {
        player: P0,
        convoke_mode: None,
    };

    let result = runner.act(GameAction::TapForConvoke {
        object_id: creature_id,
        mana_type: ManaType::Colorless,
    });
    assert!(
        result.is_err(),
        "TapForConvoke should fail when convoke not eligible"
    );
}

#[test]
fn test_waterbending_zone_check() {
    let mut state = GameState::new_two_player(42);
    // Create creature in hand (not battlefield)
    let creature_id = create_object(
        &mut state,
        CardId(1),
        P0,
        "Water Warrior".to_string(),
        Zone::Hand,
    );
    {
        let obj = state.objects.get_mut(&creature_id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
    }

    state.waiting_for = WaitingFor::ManaPayment {
        player: P0,
        convoke_mode: Some(ConvokeMode::Waterbend),
    };

    let result = engine::game::engine::apply(
        &mut state,
        GameAction::TapForConvoke {
            object_id: creature_id,
            mana_type: ManaType::Colorless,
        },
    );
    assert!(
        result.is_err(),
        "TapForConvoke on creature not on battlefield should fail"
    );
}

// ---------------------------------------------------------------------------
// Step 3: ManaPayment finalization via PassPriority
// ---------------------------------------------------------------------------

#[test]
fn test_mana_payment_finalization() {
    let mut scenario = GameScenario::default();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::Red);

    let mut runner = scenario.build();

    // Create a spell in hand
    let spell_id = create_object(
        runner.state_mut(),
        CardId(100),
        P0,
        "Fire Bolt".to_string(),
        Zone::Hand,
    );
    {
        let obj = runner.state_mut().objects.get_mut(&spell_id).unwrap();
        obj.card_types.core_types.push(CoreType::Instant);
    }

    // Add mana to the pool
    add_mana(runner.state_mut(), P0, ManaType::Red, 2);

    let ability = ResolvedAbility::new(
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 3 },
            target: TargetFilter::Any,
        },
        vec![],
        spell_id,
        P0,
    );

    // Set up the pending cast and ManaPayment state
    runner.state_mut().pending_cast = Some(Box::new(PendingCast {
        object_id: spell_id,
        card_id: CardId(100),
        ability,
        cost: ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Red],
        },
        activation_cost: None,
        activation_ability_index: None,
        target_constraints: vec![],
        casting_variant: CastingVariant::Normal,
    }));
    runner.state_mut().waiting_for = WaitingFor::ManaPayment {
        player: P0,
        convoke_mode: None,
    };

    // Finalize payment with PassPriority
    let result = runner.act(GameAction::PassPriority).unwrap();

    // Spell should now be on the stack
    assert!(
        result
            .events
            .iter()
            .any(|e| matches!(e, GameEvent::SpellCast { controller, .. } if *controller == P0)),
        "Expected SpellCast event after finalization"
    );

    // pending_cast should be consumed
    assert!(runner.state().pending_cast.is_none());
}

#[test]
fn test_mana_payment_cancel_clears_pending_cast() {
    let mut scenario = GameScenario::default();
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();

    let spell_id = create_object(
        runner.state_mut(),
        CardId(100),
        P0,
        "Spell".to_string(),
        Zone::Hand,
    );

    let ability = ResolvedAbility::new(
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
        },
        vec![],
        spell_id,
        P0,
    );

    runner.state_mut().pending_cast = Some(Box::new(PendingCast {
        object_id: spell_id,
        card_id: CardId(100),
        ability,
        cost: ManaCost::NoCost,
        activation_cost: None,
        activation_ability_index: None,
        target_constraints: vec![],
        casting_variant: CastingVariant::Normal,
    }));
    runner.state_mut().waiting_for = WaitingFor::ManaPayment {
        player: P0,
        convoke_mode: None,
    };

    runner.act(GameAction::CancelCast).unwrap();
    assert!(runner.state().pending_cast.is_none());
}

// ---------------------------------------------------------------------------
// Step 5: AI candidate generation
// ---------------------------------------------------------------------------

#[test]
fn test_ai_waterbend_candidates() {
    let mut scenario = GameScenario::default();
    scenario.at_phase(Phase::PreCombatMain);
    let creature_id = scenario.add_creature(P0, "Convoke Helper", 1, 1).id();
    let mut runner = scenario.build();

    runner.state_mut().waiting_for = WaitingFor::ManaPayment {
        player: P0,
        convoke_mode: Some(ConvokeMode::Waterbend),
    };

    let actions = candidate_actions(runner.state());

    // Should include TapForConvoke with Colorless for the creature
    assert!(
        actions.iter().any(
            |a| matches!(a.action, GameAction::TapForConvoke { object_id, mana_type }
                if object_id == creature_id && mana_type == ManaType::Colorless)
        ),
        "Should include TapForConvoke candidate for untapped creature"
    );
    // Should include PassPriority
    assert!(
        actions
            .iter()
            .any(|a| matches!(a.action, GameAction::PassPriority)),
        "Should include PassPriority candidate"
    );
    // Should include CancelCast
    assert!(
        actions
            .iter()
            .any(|a| matches!(a.action, GameAction::CancelCast)),
        "Should include CancelCast candidate"
    );
}

#[test]
fn test_ai_no_convoke_candidates_when_not_eligible() {
    let mut scenario = GameScenario::default();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P0, "Ignored Creature", 1, 1);
    let mut runner = scenario.build();

    runner.state_mut().waiting_for = WaitingFor::ManaPayment {
        player: P0,
        convoke_mode: None,
    };

    let actions = candidate_actions(runner.state());

    assert!(
        !actions
            .iter()
            .any(|a| matches!(a.action, GameAction::TapForConvoke { .. })),
        "Should NOT include TapForConvoke when convoke not eligible"
    );
    assert!(
        actions
            .iter()
            .any(|a| matches!(a.action, GameAction::PassPriority)),
        "Should include PassPriority even without convoke"
    );
}

#[test]
fn test_ai_convoke_ignores_summoning_sickness() {
    let mut scenario = GameScenario::default();
    scenario.at_phase(Phase::PreCombatMain);

    // Create creature that just entered (has summoning sickness)
    let creature_id = scenario
        .add_creature(P0, "Fresh Creature", 1, 1)
        .with_summoning_sickness()
        .id();

    let mut runner = scenario.build();
    runner.state_mut().waiting_for = WaitingFor::ManaPayment {
        player: P0,
        convoke_mode: Some(ConvokeMode::Waterbend),
    };

    let actions = candidate_actions(runner.state());

    // CR 702.6b: Summoning sickness does not restrict tapping for convoke
    assert!(
        actions.iter().any(
            |a| matches!(a.action, GameAction::TapForConvoke { object_id, .. } if object_id == creature_id)
        ),
        "Summoning-sick creature should still be eligible for convoke (CR 702.6b)"
    );
}

// ---------------------------------------------------------------------------
// Convoke color matching (CR 702.6a)
// ---------------------------------------------------------------------------

#[test]
fn test_convoke_white_creature_produces_white() {
    let mut scenario = GameScenario::default();
    scenario.at_phase(Phase::PreCombatMain);
    let creature_id = scenario.add_creature(P0, "White Knight", 2, 2).id();
    let mut runner = scenario.build();

    // Give creature white color
    runner
        .state_mut()
        .objects
        .get_mut(&creature_id)
        .unwrap()
        .color
        .push(ManaColor::White);

    runner.state_mut().waiting_for = WaitingFor::ManaPayment {
        player: P0,
        convoke_mode: Some(ConvokeMode::Convoke),
    };

    let result = runner
        .act(GameAction::TapForConvoke {
            object_id: creature_id,
            mana_type: ManaType::White,
        })
        .unwrap();

    // Should produce white mana
    assert!(
        result.events.iter().any(|e| matches!(
            e,
            GameEvent::ManaAdded { mana_type, .. } if *mana_type == ManaType::White
        )),
        "Expected White mana from convoke with white creature"
    );

    // Should NOT emit Waterbend event
    assert!(
        !result
            .events
            .iter()
            .any(|e| matches!(e, GameEvent::Waterbend { .. })),
        "Convoke should NOT emit Waterbend event"
    );
}

#[test]
fn test_convoke_multicolor_creature_accepts_either_color() {
    let mut scenario = GameScenario::default();
    scenario.at_phase(Phase::PreCombatMain);
    let creature_id = scenario.add_creature(P0, "Simic Hybrid", 2, 2).id();
    let mut runner = scenario.build();

    // Give creature white and green color
    {
        let obj = runner.state_mut().objects.get_mut(&creature_id).unwrap();
        obj.color.push(ManaColor::White);
        obj.color.push(ManaColor::Green);
    }

    runner.state_mut().waiting_for = WaitingFor::ManaPayment {
        player: P0,
        convoke_mode: Some(ConvokeMode::Convoke),
    };

    // Tap for Green — should succeed
    let result = runner
        .act(GameAction::TapForConvoke {
            object_id: creature_id,
            mana_type: ManaType::Green,
        })
        .unwrap();

    assert!(
        result.events.iter().any(|e| matches!(
            e,
            GameEvent::ManaAdded { mana_type, .. } if *mana_type == ManaType::Green
        )),
        "Expected Green mana from convoke with W/G creature"
    );
}

#[test]
fn test_convoke_wrong_color_rejected() {
    let mut scenario = GameScenario::default();
    scenario.at_phase(Phase::PreCombatMain);
    let creature_id = scenario.add_creature(P0, "Red Goblin", 1, 1).id();
    let mut runner = scenario.build();

    // Give creature red color only
    runner
        .state_mut()
        .objects
        .get_mut(&creature_id)
        .unwrap()
        .color
        .push(ManaColor::Red);

    runner.state_mut().waiting_for = WaitingFor::ManaPayment {
        player: P0,
        convoke_mode: Some(ConvokeMode::Convoke),
    };

    // Attempt to tap for White — creature is Red, should fail
    let result = runner.act(GameAction::TapForConvoke {
        object_id: creature_id,
        mana_type: ManaType::White,
    });
    assert!(
        result.is_err(),
        "Convoke should reject tapping Red creature for White mana"
    );
}

#[test]
fn test_convoke_colorless_always_valid() {
    let mut scenario = GameScenario::default();
    scenario.at_phase(Phase::PreCombatMain);
    // Colorless artifact creature (no colors)
    let creature_id = scenario.add_creature(P0, "Myr Token", 1, 1).id();
    let mut runner = scenario.build();

    runner.state_mut().waiting_for = WaitingFor::ManaPayment {
        player: P0,
        convoke_mode: Some(ConvokeMode::Convoke),
    };

    // Tap for Colorless — always valid for generic mana
    let result = runner
        .act(GameAction::TapForConvoke {
            object_id: creature_id,
            mana_type: ManaType::Colorless,
        })
        .unwrap();

    assert!(
        result.events.iter().any(|e| matches!(
            e,
            GameEvent::ManaAdded { mana_type, .. } if *mana_type == ManaType::Colorless
        )),
        "Colorless creature should produce colorless mana for generic"
    );
}

#[test]
fn test_convoke_preserves_mode_across_taps() {
    let mut scenario = GameScenario::default();
    scenario.at_phase(Phase::PreCombatMain);
    let c1 = scenario.add_creature(P0, "Helper 1", 1, 1).id();
    let c2 = scenario.add_creature(P0, "Helper 2", 1, 1).id();
    let mut runner = scenario.build();

    runner.state_mut().waiting_for = WaitingFor::ManaPayment {
        player: P0,
        convoke_mode: Some(ConvokeMode::Convoke),
    };

    // First tap
    runner
        .act(GameAction::TapForConvoke {
            object_id: c1,
            mana_type: ManaType::Colorless,
        })
        .unwrap();

    // State should still be ManaPayment with Convoke
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ManaPayment {
                convoke_mode: Some(ConvokeMode::Convoke),
                ..
            }
        ),
        "convoke_mode should be preserved after tap"
    );

    // Second tap
    runner
        .act(GameAction::TapForConvoke {
            object_id: c2,
            mana_type: ManaType::Colorless,
        })
        .unwrap();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ManaPayment {
                convoke_mode: Some(ConvokeMode::Convoke),
                ..
            }
        ),
        "convoke_mode should be preserved after second tap"
    );
}

#[test]
fn test_waterbend_tap_does_emit_waterbend_event() {
    let mut scenario = GameScenario::default();
    scenario.at_phase(Phase::PreCombatMain);
    let creature_id = scenario.add_creature(P0, "Water Helper", 1, 1).id();
    let mut runner = scenario.build();

    runner.state_mut().waiting_for = WaitingFor::ManaPayment {
        player: P0,
        convoke_mode: Some(ConvokeMode::Waterbend),
    };

    let result = runner
        .act(GameAction::TapForConvoke {
            object_id: creature_id,
            mana_type: ManaType::Colorless,
        })
        .unwrap();

    assert!(
        result
            .events
            .iter()
            .any(|e| matches!(e, GameEvent::Waterbend { .. })),
        "Waterbend mode SHOULD emit Waterbend event"
    );
}

#[test]
fn test_ai_convoke_generates_per_color_candidates() {
    let mut scenario = GameScenario::default();
    scenario.at_phase(Phase::PreCombatMain);
    let creature_id = scenario.add_creature(P0, "Gold Creature", 2, 2).id();
    let mut runner = scenario.build();

    // W/G creature
    {
        let obj = runner.state_mut().objects.get_mut(&creature_id).unwrap();
        obj.color.push(ManaColor::White);
        obj.color.push(ManaColor::Green);
    }

    runner.state_mut().waiting_for = WaitingFor::ManaPayment {
        player: P0,
        convoke_mode: Some(ConvokeMode::Convoke),
    };

    let actions = candidate_actions(runner.state());

    // Should have Colorless + White + Green candidates
    let convoke_actions: Vec<_> = actions
        .iter()
        .filter(|a| {
            matches!(
                a.action,
                GameAction::TapForConvoke { object_id, .. } if object_id == creature_id
            )
        })
        .collect();

    assert!(
        convoke_actions.len() >= 3,
        "Expected at least 3 TapForConvoke candidates (Colorless + W + G), got {}",
        convoke_actions.len()
    );
}

// ---------------------------------------------------------------------------
// Waterbend cost parsing
// ---------------------------------------------------------------------------

#[test]
fn test_parse_waterbend_single_cost() {
    use engine::parser::oracle_cost::parse_single_cost;

    let cost = parse_single_cost("waterbend {3}");
    assert!(
        matches!(
            cost,
            AbilityCost::Waterbend {
                cost: ManaCost::Cost { generic: 3, .. }
            }
        ),
        "Expected Waterbend {{ cost: generic 3 }}, got {cost:?}"
    );

    let cost5 = parse_single_cost("waterbend {5}");
    assert!(
        matches!(
            cost5,
            AbilityCost::Waterbend {
                cost: ManaCost::Cost { generic: 5, .. }
            }
        ),
        "Expected Waterbend {{ cost: generic 5 }}, got {cost5:?}"
    );
}

#[test]
fn test_parse_waterbend_additional_cost() {
    use engine::parser::oracle_casting::parse_additional_cost_line;
    use engine::types::ability::AdditionalCost;

    let result = parse_additional_cost_line(
        "as an additional cost to cast this spell, waterbend {5}.",
        "As an additional cost to cast this spell, waterbend {5}.",
    );
    assert!(
        matches!(
            result,
            Some(AdditionalCost::Required(AbilityCost::Waterbend {
                cost: ManaCost::Cost { generic: 5, .. }
            }))
        ),
        "Expected Required(Waterbend {{ 5 }}), got {result:?}"
    );
}

#[test]
fn test_parse_composite_tap_waterbend() {
    use engine::parser::oracle_cost::parse_oracle_cost;

    let cost = parse_oracle_cost("{T}, waterbend {3}");
    assert!(
        matches!(cost, AbilityCost::Composite { ref costs } if costs.len() == 2),
        "Expected Composite with 2 costs, got {cost:?}"
    );
    if let AbilityCost::Composite { costs } = cost {
        assert!(matches!(costs[0], AbilityCost::Tap));
        assert!(matches!(
            costs[1],
            AbilityCost::Waterbend {
                cost: ManaCost::Cost { generic: 3, .. }
            }
        ));
    }
}

// ---------------------------------------------------------------------------
// Elemental bend meta-trigger (all four bending types)
// ---------------------------------------------------------------------------

#[test]
fn test_elemental_bend_all_four_types_tracked() {
    let mut state = GameState::new_two_player(42);
    let player = state.players.iter_mut().find(|p| p.id == P0).unwrap();

    player.bending_types_this_turn.insert(BendingType::Fire);
    player.bending_types_this_turn.insert(BendingType::Air);
    player.bending_types_this_turn.insert(BendingType::Earth);
    player.bending_types_this_turn.insert(BendingType::Water);

    assert_eq!(player.bending_types_this_turn.len(), 4);
}
