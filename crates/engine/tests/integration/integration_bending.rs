//! Integration tests for the four bending mechanics (Fire, Air, Earth, Water)
//! and their shared infrastructure (meta-triggers, AI candidates, mana payment finalization).

use engine::ai_support::candidate_actions;
use engine::game::scenario::{GameScenario, P0};
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityCost, Effect, EffectScope, PtValue, QuantityExpr, ResolvedAbility, TapStateChange,
    TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::{BendingType, GameEvent};
use engine::types::game_state::{
    CastPaymentMode, CastingVariant, ConvokeMode, GameState, PendingCast, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::mana::{
    ManaColor, ManaCost, ManaCostShard, ManaRestriction, ManaType, ManaUnit,
};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn add_mana(state: &mut GameState, player: PlayerId, color: ManaType, count: usize) {
    let p = state.players.iter_mut().find(|p| p.id == player).unwrap();
    for _ in 0..count {
        p.mana_pool
            .add(ManaUnit::new(color, ObjectId(0), false, Vec::new()));
    }
}

// ---------------------------------------------------------------------------
// Step 1: Earthbend event emission
// ---------------------------------------------------------------------------

#[test]
fn test_earthbending_registers_event_and_turn_tracking() {
    let mut state = GameState::new_two_player(42);
    let land_id = create_object(
        &mut state,
        CardId(1),
        P0,
        "Mountain".to_string(),
        Zone::Battlefield,
    );

    let ability = ResolvedAbility::new(
        Effect::RegisterBending {
            kind: BendingType::Earth,
        },
        vec![],
        land_id,
        P0,
    );

    let mut events = Vec::new();
    engine::game::effects::register_bending::resolve(&mut state, &ability, &mut events).unwrap();

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
fn test_generic_animate_does_not_register_earthbend() {
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
            power: Some(PtValue::Fixed(4)),
            toughness: Some(PtValue::Fixed(4)),
            types: vec!["Creature".to_string()],
            remove_types: vec![],
            target: TargetFilter::None,
            keywords: vec![],
        },
        vec![],
        obj_id,
        P0,
    );

    let mut events = Vec::new();
    engine::game::effects::animate::resolve(&mut state, &ability, &mut events).unwrap();

    // Generic animate should not emit Earthbend or touch bending tracking.
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

    // Set up ManaPayment state with Waterbend mode.
    runner.enter_mana_payment(P0, Some(ConvokeMode::Waterbend));

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
    runner.enter_mana_payment(P0, None);

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
    let mut scenario = GameScenario::default();
    let creature_id = scenario
        .add_creature_to_hand(P0, "Water Warrior", 2, 2)
        .id();
    let mut runner = scenario.build();
    runner.enter_mana_payment(P0, Some(ConvokeMode::Waterbend));

    let result = runner.act(GameAction::TapForConvoke {
        object_id: creature_id,
        mana_type: ManaType::Colorless,
    });
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
            damage_source: None,
            excess: None,
        },
        vec![],
        spell_id,
        P0,
    );

    // Set up the pending cast and ManaPayment state
    runner.state_mut().pending_cast = Some(Box::new(PendingCast::new(
        spell_id,
        CardId(100),
        ability,
        ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Red],
        },
    )));
    // CR 601.2a: Simulate the announcement stack push that the production flow
    // would have performed on entering the cast pipeline.
    runner
        .state_mut()
        .stack
        .push_back(engine::types::game_state::StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: P0,
            kind: engine::types::game_state::StackEntryKind::Spell {
                card_id: CardId(100),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });
    runner.enter_mana_payment(P0, None);

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
            target: TargetFilter::Controller,
        },
        vec![],
        spell_id,
        P0,
    );

    runner.state_mut().pending_cast = Some(Box::new(PendingCast::new(
        spell_id,
        CardId(100),
        ability,
        ManaCost::NoCost,
    )));
    runner.enter_mana_payment(P0, None);

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

    runner.enter_mana_payment(P0, Some(ConvokeMode::Waterbend));

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

    runner.enter_mana_payment(P0, None);

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
    runner.enter_mana_payment(P0, Some(ConvokeMode::Waterbend));

    let actions = candidate_actions(runner.state());

    // CR 702.51a + CR 302.6: Summoning sickness does not restrict tapping for convoke
    assert!(
        actions.iter().any(
            |a| matches!(a.action, GameAction::TapForConvoke { object_id, .. } if object_id == creature_id)
        ),
        "Summoning-sick creature should still be eligible for convoke (CR 702.51a + CR 302.6)"
    );
}

// ---------------------------------------------------------------------------
// Convoke color matching (CR 702.51a)
// ---------------------------------------------------------------------------

#[test]
fn test_convoke_white_creature_pays_white() {
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

    runner.enter_mana_payment(P0, Some(ConvokeMode::Convoke));

    let result = runner
        .act(GameAction::TapForConvoke {
            object_id: creature_id,
            mana_type: ManaType::White,
        })
        .unwrap();

    // Convoke pays without producing mana.
    assert!(
        result
            .events
            .iter()
            .all(|e| !matches!(e, GameEvent::ManaAdded { .. })),
        "Convoke should not produce mana"
    );
    assert!(runner.state().players[0].mana_pool.mana.iter().any(|unit| {
        unit.color == ManaType::White
            && unit.restrictions.contains(&ManaRestriction::ConvokePayment)
    }));

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
fn test_convoke_multicolor_creature_accepts_either_color_payment() {
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

    runner.enter_mana_payment(P0, Some(ConvokeMode::Convoke));

    // Tap for Green — should succeed
    let result = runner
        .act(GameAction::TapForConvoke {
            object_id: creature_id,
            mana_type: ManaType::Green,
        })
        .unwrap();

    assert!(
        result
            .events
            .iter()
            .all(|e| !matches!(e, GameEvent::ManaAdded { .. })),
        "Convoke should not produce mana"
    );
    assert!(runner.state().players[0].mana_pool.mana.iter().any(|unit| {
        unit.color == ManaType::Green
            && unit.restrictions.contains(&ManaRestriction::ConvokePayment)
    }));
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

    runner.enter_mana_payment(P0, Some(ConvokeMode::Convoke));

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

    runner.enter_mana_payment(P0, Some(ConvokeMode::Convoke));

    // Tap for Colorless — always valid for generic mana
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
            .all(|e| !matches!(e, GameEvent::ManaAdded { .. })),
        "Convoke should not produce mana"
    );
    assert!(runner.state().players[0].mana_pool.mana.iter().any(|unit| {
        unit.color == ManaType::Colorless
            && unit.restrictions.contains(&ManaRestriction::ConvokePayment)
    }));
}

#[test]
fn test_convoke_preserves_mode_across_taps() {
    let mut scenario = GameScenario::default();
    scenario.at_phase(Phase::PreCombatMain);
    let c1 = scenario.add_creature(P0, "Helper 1", 1, 1).id();
    let c2 = scenario.add_creature(P0, "Helper 2", 1, 1).id();
    let mut runner = scenario.build();

    runner.enter_mana_payment(P0, Some(ConvokeMode::Convoke));

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

    runner.enter_mana_payment(P0, Some(ConvokeMode::Waterbend));

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

    runner.enter_mana_payment(P0, Some(ConvokeMode::Convoke));

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

// ---------------------------------------------------------------------------
// SearchLibrary → ChangeZone → Shuffle continuation chain (building block)
// ---------------------------------------------------------------------------

/// CR 608.2c: Verify the continuation mechanism works for SearchLibrary chains.
/// After SearchChoice resolves, the pending ChangeZone + Shuffle sub_abilities
/// must complete and the game must return to a valid Priority state.
#[test]
fn test_search_changezone_shuffle_continuation_completes() {
    use engine::game::engine::apply_as_current;
    use engine::game::stack;
    use engine::types::card_type::Supertype;

    let mut state = GameState::new_two_player(42);
    state.phase = Phase::PreCombatMain;
    state.turn_number = 2;
    state.waiting_for = WaitingFor::Priority { player: P0 };
    state.priority_player = P0;

    // Add a basic land card in P0's library (the search target)
    let lib_land_id = create_object(
        &mut state,
        CardId(10),
        P0,
        "Forest".to_string(),
        Zone::Library,
    );
    {
        let obj = state.objects.get_mut(&lib_land_id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.supertypes.push(Supertype::Basic);
        obj.base_card_types = obj.card_types.clone();
    }

    // Add a few more library cards so we can verify shuffle
    for i in 0..4 {
        create_object(
            &mut state,
            CardId(20 + i),
            P0,
            format!("Filler {}", i),
            Zone::Library,
        );
    }

    let lib_size_before = state.players[0].library.len();

    // Source object for the triggered ability
    let source_id = create_object(
        &mut state,
        CardId(1),
        P0,
        "Test Enchantment".to_string(),
        Zone::Battlefield,
    );

    // Build the SearchLibrary → ChangeZone(enter_tapped) → Shuffle chain
    let shuffle_ability = ResolvedAbility::new(
        Effect::Shuffle {
            target: TargetFilter::Controller,
        },
        vec![],
        source_id,
        P0,
    );

    let change_zone_ability = ResolvedAbility {
        effect: Effect::ChangeZone {
            origin: Some(Zone::Library),
            destination: Zone::Battlefield,
            target: TargetFilter::Any,
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: engine::types::zones::EtbTapState::Tapped,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
            enters_modified_if: None,
        },
        sub_ability: Some(Box::new(shuffle_ability)),
        ..ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Battlefield,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: engine::types::zones::EtbTapState::Tapped,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
            vec![],
            source_id,
            P0,
        )
    };

    let search_ability = ResolvedAbility {
        effect: Effect::SearchLibrary {
            filter: TargetFilter::Typed(engine::types::ability::TypedFilter {
                type_filters: vec![engine::types::ability::TypeFilter::Land],
                controller: None,
                properties: vec![engine::types::ability::FilterProp::HasSupertype {
                    value: engine::types::card_type::Supertype::Basic,
                }],
            }),
            count: QuantityExpr::Fixed { value: 1 },
            reveal: false,
            target_player: None,
            selection_constraint: engine::types::ability::SearchSelectionConstraint::None,
            split: None,
            source_zones: vec![engine::types::zones::Zone::Library],
        },
        sub_ability: Some(Box::new(change_zone_ability)),
        ..ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(engine::types::ability::TypedFilter {
                    type_filters: vec![engine::types::ability::TypeFilter::Land],
                    controller: None,
                    properties: vec![engine::types::ability::FilterProp::HasSupertype {
                        value: engine::types::card_type::Supertype::Basic,
                    }],
                }),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
                selection_constraint: engine::types::ability::SearchSelectionConstraint::None,
                split: None,
                source_zones: vec![engine::types::zones::Zone::Library],
            },
            vec![],
            source_id,
            P0,
        )
    };

    // Push as triggered ability on the stack
    let entry_id = ObjectId(state.next_object_id);
    state.next_object_id += 1;
    let entry = engine::types::game_state::StackEntry {
        id: entry_id,
        source_id,
        controller: P0,
        kind: engine::types::game_state::StackEntryKind::TriggeredAbility {
            source_id,
            ability: Box::new(search_ability),
            condition: None,
            trigger_event: None,
            description: None,
            source_name: String::new(),
            subject_match_count: None,
            die_result: None,
        },
    };
    stack::push_to_stack(&mut state, entry, &mut vec![]);

    assert_eq!(state.stack.len(), 1, "trigger should be on the stack");

    // Both players pass priority → resolve_top fires
    let _r1 = apply_as_current(&mut state, GameAction::PassPriority).expect("P0 pass");
    // P0 passed, now P1 passes
    let _r2 = apply_as_current(&mut state, GameAction::PassPriority).expect("P1 pass");

    // After both pass, the trigger resolves: SearchLibrary sets SearchChoice
    assert!(
        matches!(state.waiting_for, WaitingFor::SearchChoice { .. }),
        "Expected SearchChoice after search resolves, got {:?}",
        state.waiting_for
    );
    assert!(
        state.stack.is_empty(),
        "Trigger should be popped from stack during resolve_top"
    );

    // Select the Forest from the search
    let chosen = vec![lib_land_id];
    apply_as_current(&mut state, GameAction::SelectCards { cards: chosen })
        .expect("select card from search");

    // The continuation (ChangeZone + Shuffle) should have completed.
    // The game should be in a valid Priority state (possibly with triggers on stack).
    match &state.waiting_for {
        WaitingFor::Priority { .. } => {
            // Expected: game returned to priority
        }
        WaitingFor::TriggerTargetSelection { .. } => {
            // Also acceptable: a trigger fired and needs targeting
        }
        other => {
            panic!(
                "Game should be in Priority or TriggerTargetSelection after continuation, got: {:?}",
                other
            );
        }
    }

    // The Forest should now be on the battlefield, tapped
    let forest_obj = state
        .objects
        .get(&lib_land_id)
        .expect("Forest should exist");
    assert_eq!(
        forest_obj.zone,
        Zone::Battlefield,
        "Forest should be on the battlefield"
    );
    assert!(forest_obj.tapped, "Forest should enter tapped");

    // Library should have shrunk by 1 (Forest was removed)
    assert_eq!(
        state.players[0].library.len(),
        lib_size_before - 1,
        "Library should shrink by 1 after search"
    );
}

// ---------------------------------------------------------------------------
// Earthbender Ascension ETB + Landfall interaction
// ---------------------------------------------------------------------------

/// Reproduces the stuck-on-stack bug: Earthbender Ascension's ETB trigger
/// resolves (earthbend + search + put + shuffle), but the Landfall trigger
/// fires from the searched land entering and appears stuck on the stack.
#[test]
fn test_earthbender_ascension_etb_completes_with_landfall() {
    use engine::game::engine::apply_as_current;
    use engine::game::stack;
    use engine::types::card_type::Supertype;
    use engine::types::triggers::TriggerMode;
    use engine::types::TriggerDefinition;

    let mut state = GameState::new_two_player(42);
    state.phase = Phase::PreCombatMain;
    state.turn_number = 2;
    state.waiting_for = WaitingFor::Priority { player: P0 };
    state.priority_player = P0;

    // A land on battlefield to earthbend
    let target_land_id = create_object(
        &mut state,
        CardId(2),
        P0,
        "Mountain".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&target_land_id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.supertypes.push(Supertype::Basic);
        obj.base_card_types = obj.card_types.clone();
        obj.entered_battlefield_turn = Some(1);
    }

    // Basic land in library (search target)
    let lib_land_id = create_object(
        &mut state,
        CardId(10),
        P0,
        "Forest".to_string(),
        Zone::Library,
    );
    {
        let obj = state.objects.get_mut(&lib_land_id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.supertypes.push(Supertype::Basic);
        obj.base_card_types = obj.card_types.clone();
    }

    // Filler library cards
    for i in 0..3 {
        create_object(
            &mut state,
            CardId(20 + i),
            P0,
            format!("Filler {}", i),
            Zone::Library,
        );
    }

    // Earthbender Ascension on battlefield with Landfall trigger
    let enchantment_id = create_object(
        &mut state,
        CardId(1),
        P0,
        "Earthbender Ascension".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&enchantment_id).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.base_card_types = obj.card_types.clone();
        obj.entered_battlefield_turn = Some(2);

        // Add the full Landfall trigger matching the card's parsed output:
        // "Whenever a land you control enters, put a quest counter on this enchantment.
        //  When you do, if it has four or more quest counters on it, put a +1/+1 counter
        //  on target creature you control. It gains trample until end of turn."
        use engine::types::ability::{AbilityCondition, AbilityKind, Comparator, QuantityRef};
        let landfall_execute = engine::types::ability::AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutCounter {
                counter_type: CounterType::Generic("quest".to_string()),
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::SelfRef,
            },
        )
        .sub_ability(
            engine::types::ability::AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::PutCounter {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Typed(
                        engine::types::ability::TypedFilter::creature()
                            .controller(engine::types::ability::ControllerRef::You),
                    ),
                },
            )
            // CR 603.4 + CR 608.2c: "if it has four or more quest counters on it"
            .condition(AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::CountersOn {
                        scope: engine::types::ability::ObjectScope::Source,
                        counter_type: Some(CounterType::Generic("quest".to_string())),
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 4 },
            }),
        );

        let landfall_trigger = TriggerDefinition::new(TriggerMode::ChangesZone)
            .destination(Zone::Battlefield)
            .valid_card(TargetFilter::Typed(engine::types::ability::TypedFilter {
                type_filters: vec![engine::types::ability::TypeFilter::Land],
                controller: Some(engine::types::ability::ControllerRef::You),
                properties: vec![],
            }))
            .description(
                "Whenever a land you control enters, put a quest counter on this enchantment. When you do, if it has four or more quest counters on it, put a +1/+1 counter on target creature you control."
                    .to_string(),
            )
            .execute(landfall_execute);
        obj.trigger_definitions.push(landfall_trigger.clone());
        Arc::make_mut(&mut obj.base_trigger_definitions).push(landfall_trigger);
    }

    // Sazh's Chocobo — another Landfall trigger on the board
    let chocobo_id = create_object(
        &mut state,
        CardId(3),
        P0,
        "Sazhs Chocobo".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&chocobo_id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(0);
        obj.toughness = Some(1);
        obj.base_power = Some(0);
        obj.base_toughness = Some(1);
        obj.entered_battlefield_turn = Some(1);

        let chocobo_trigger = TriggerDefinition::new(TriggerMode::ChangesZone)
            .destination(Zone::Battlefield)
            .valid_card(TargetFilter::Typed(engine::types::ability::TypedFilter {
                type_filters: vec![engine::types::ability::TypeFilter::Land],
                controller: Some(engine::types::ability::ControllerRef::You),
                properties: vec![],
            }))
            .description(
                "Whenever a land you control enters, put a +1/+1 counter on this creature."
                    .to_string(),
            )
            .execute(engine::types::ability::AbilityDefinition::new(
                engine::types::ability::AbilityKind::Spell,
                Effect::PutCounter {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::SelfRef,
                },
            ));
        obj.trigger_definitions.push(chocobo_trigger.clone());
        Arc::make_mut(&mut obj.base_trigger_definitions).push(chocobo_trigger);
    }

    // Build the ETB chain: Animate(earthbend) → SearchLibrary → ChangeZone → Shuffle
    let shuffle_ability = ResolvedAbility::new(
        Effect::Shuffle {
            target: TargetFilter::Controller,
        },
        vec![],
        enchantment_id,
        P0,
    );

    let change_zone_ability = ResolvedAbility {
        effect: Effect::ChangeZone {
            origin: Some(Zone::Library),
            destination: Zone::Battlefield,
            target: TargetFilter::Any,
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: engine::types::zones::EtbTapState::Tapped,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
            enters_modified_if: None,
        },
        sub_ability: Some(Box::new(shuffle_ability)),
        ..ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Battlefield,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: engine::types::zones::EtbTapState::Tapped,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
            vec![],
            enchantment_id,
            P0,
        )
    };

    let search_ability = ResolvedAbility {
        effect: Effect::SearchLibrary {
            filter: TargetFilter::Typed(engine::types::ability::TypedFilter {
                type_filters: vec![engine::types::ability::TypeFilter::Land],
                controller: None,
                properties: vec![engine::types::ability::FilterProp::HasSupertype {
                    value: engine::types::card_type::Supertype::Basic,
                }],
            }),
            count: QuantityExpr::Fixed { value: 1 },
            reveal: false,
            target_player: None,
            selection_constraint: engine::types::ability::SearchSelectionConstraint::None,
            split: None,
            source_zones: vec![engine::types::zones::Zone::Library],
        },
        sub_ability: Some(Box::new(change_zone_ability)),
        ..ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(engine::types::ability::TypedFilter {
                    type_filters: vec![engine::types::ability::TypeFilter::Land],
                    controller: None,
                    properties: vec![engine::types::ability::FilterProp::HasSupertype {
                        value: engine::types::card_type::Supertype::Basic,
                    }],
                }),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
                selection_constraint: engine::types::ability::SearchSelectionConstraint::None,
                split: None,
                source_zones: vec![engine::types::zones::Zone::Library],
            },
            vec![],
            enchantment_id,
            P0,
        )
    };

    let animate_ability = ResolvedAbility {
        effect: Effect::Animate {
            power: Some(PtValue::Fixed(2)),
            toughness: Some(PtValue::Fixed(2)),
            types: vec!["Creature".to_string()],
            remove_types: vec![],
            target: TargetFilter::Typed(engine::types::ability::TypedFilter {
                type_filters: vec![engine::types::ability::TypeFilter::Land],
                controller: Some(engine::types::ability::ControllerRef::You),
                properties: vec![],
            }),
            keywords: vec![Keyword::Haste],
        },
        targets: vec![engine::types::ability::TargetRef::Object(target_land_id)],
        sub_ability: Some(Box::new(search_ability)),
        ..ResolvedAbility::new(
            Effect::Animate {
                power: Some(PtValue::Fixed(2)),
                toughness: Some(PtValue::Fixed(2)),
                types: vec!["Creature".to_string()],
                remove_types: vec![],
                target: TargetFilter::Typed(engine::types::ability::TypedFilter {
                    type_filters: vec![engine::types::ability::TypeFilter::Land],
                    controller: Some(engine::types::ability::ControllerRef::You),
                    properties: vec![],
                }),
                keywords: vec![Keyword::Haste],
            },
            vec![engine::types::ability::TargetRef::Object(target_land_id)],
            enchantment_id,
            P0,
        )
    };

    // Push ETB trigger on the stack
    let entry_id = ObjectId(state.next_object_id);
    state.next_object_id += 1;
    let entry = engine::types::game_state::StackEntry {
        id: entry_id,
        source_id: enchantment_id,
        controller: P0,
        kind: engine::types::game_state::StackEntryKind::TriggeredAbility {
            source_id: enchantment_id,
            ability: Box::new(animate_ability),
            condition: None,
            trigger_event: None,
            description: None,
            source_name: String::new(),
            subject_match_count: None,
            die_result: None,
        },
    };
    stack::push_to_stack(&mut state, entry, &mut vec![]);

    // Both players pass → ETB trigger resolves
    apply_as_current(&mut state, GameAction::PassPriority).expect("P0 pass");
    apply_as_current(&mut state, GameAction::PassPriority).expect("P1 pass");

    // Should now be in SearchChoice
    assert!(
        matches!(state.waiting_for, WaitingFor::SearchChoice { .. }),
        "Expected SearchChoice, got {:?}",
        state.waiting_for
    );
    assert!(
        state.stack.is_empty(),
        "ETB trigger should be popped from stack"
    );

    // Select the Forest from search
    let select_result = apply_as_current(
        &mut state,
        GameAction::SelectCards {
            cards: vec![lib_land_id],
        },
    );

    // This is the critical assertion: the apply call should succeed
    assert!(
        select_result.is_ok(),
        "SelectCards should succeed, got error: {:?}",
        select_result.err()
    );

    // CR 603.3b (#531): drain the per-controller ordering prompt with identity
    // before checking the post-resolution waiting state.
    engine::game::triggers::drain_order_triggers_with_identity(&mut state);

    // After continuation + trigger processing, game should reach a valid state
    let is_valid_state = matches!(
        state.waiting_for,
        WaitingFor::Priority { .. } | WaitingFor::TriggerTargetSelection { .. }
    );
    assert!(
        is_valid_state,
        "Game should be in a valid state after ETB completes, got: {:?}",
        state.waiting_for
    );

    // Forest should be on battlefield tapped
    let forest = state
        .objects
        .get(&lib_land_id)
        .expect("Forest should exist");
    assert_eq!(forest.zone, Zone::Battlefield);
    assert!(forest.tapped, "Forest should enter tapped");

    // Resolve any remaining triggers on the stack by passing priority
    // and selecting targets when prompted (the QuantityCheck condition gates
    // the P1P1 effect at resolution time, but targeting still occurs first).
    let mut safety = 0;
    while !matches!(state.waiting_for, WaitingFor::Priority { .. } if state.stack.is_empty())
        && safety < 30
    {
        // CR 603.3b (#531): drain the per-controller ordering prompt with identity.
        if matches!(state.waiting_for, WaitingFor::OrderTriggers { .. }) {
            engine::game::triggers::drain_order_triggers_with_identity(&mut state);
            safety += 1;
            continue;
        }
        match &state.waiting_for {
            WaitingFor::Priority { .. } => {
                apply_as_current(&mut state, GameAction::PassPriority).unwrap_or_else(|e| {
                    panic!("Pass priority failed at iteration {}: {:?}", safety, e)
                });
            }
            WaitingFor::TriggerTargetSelection { target_slots, .. } => {
                // Select the first legal target for the P1P1 counter effect
                let target = target_slots[0].legal_targets[0].clone();
                apply_as_current(
                    &mut state,
                    GameAction::SelectTargets {
                        targets: vec![target],
                    },
                )
                .unwrap_or_else(|e| {
                    panic!("SelectTargets failed at iteration {}: {:?}", safety, e)
                });
            }
            _ => break,
        }
        safety += 1;
    }

    // Eventually the stack should be empty and game in Priority
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "Game should reach Priority after all triggers resolve, got: {:?}",
        state.waiting_for
    );
}

/// Reproduces the user-reported hang: playing a land with Earthbender's Ascension
/// on the battlefield causes the Landfall trigger to fire, but the game gets stuck
/// after both players pass priority. The trigger should resolve (placing a quest
/// counter), the QuantityCheck should fail (< 4 counters), and the game should
/// return to normal priority with an empty stack.
#[test]
fn test_earthbender_landfall_trigger_resolves_without_hang() {
    use engine::game::engine::apply_as_current;
    use engine::types::ability::{AbilityCondition, AbilityKind, Comparator, QuantityRef};
    use engine::types::triggers::TriggerMode;
    use engine::types::TriggerDefinition;

    let mut state = GameState::new_two_player(42);
    state.phase = Phase::PreCombatMain;
    state.turn_number = 3;
    state.active_player = P0;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };

    // A creature for the P1P1 counter target (if condition were met)
    let creature_id = create_object(
        &mut state,
        CardId(5),
        P0,
        "Badgermole Cub".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&creature_id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
        obj.entered_battlefield_turn = Some(1);
    }

    // Earthbender Ascension on battlefield with 0 quest counters
    let enchantment_id = create_object(
        &mut state,
        CardId(1),
        P0,
        "Earthbender Ascension".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&enchantment_id).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.base_card_types = obj.card_types.clone();
        obj.entered_battlefield_turn = Some(2);

        // Landfall trigger: put quest counter, if 4+ quest counters → P1P1 on creature
        let landfall_execute = engine::types::ability::AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutCounter {
                counter_type: CounterType::Generic("quest".to_string()),
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::SelfRef,
            },
        )
        .sub_ability(
            engine::types::ability::AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::PutCounter {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Typed(
                        engine::types::ability::TypedFilter::creature()
                            .controller(engine::types::ability::ControllerRef::You),
                    ),
                },
            )
            .condition(AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::CountersOn {
                        scope: engine::types::ability::ObjectScope::Source,
                        counter_type: Some(CounterType::Generic("quest".to_string())),
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 4 },
            }),
        );

        let landfall_trigger = TriggerDefinition::new(TriggerMode::ChangesZone)
            .destination(Zone::Battlefield)
            .valid_card(TargetFilter::Typed(engine::types::ability::TypedFilter {
                type_filters: vec![engine::types::ability::TypeFilter::Land],
                controller: Some(engine::types::ability::ControllerRef::You),
                properties: vec![],
            }))
            .description(
                "Whenever a land you control enters, put a quest counter on this enchantment."
                    .to_string(),
            )
            .execute(landfall_execute);
        obj.trigger_definitions.push(landfall_trigger.clone());
        Arc::make_mut(&mut obj.base_trigger_definitions).push(landfall_trigger);
    }

    // A land in hand to play
    let land_id = create_object(&mut state, CardId(10), P0, "Forest".to_string(), Zone::Hand);
    {
        let obj = state.objects.get_mut(&land_id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.subtypes.push("Forest".to_string());
        obj.base_card_types = obj.card_types.clone();
    }

    // Step 1: Play the land
    let card_id = state.objects.get(&land_id).unwrap().card_id;
    let result = apply_as_current(
        &mut state,
        GameAction::PlayLand {
            object_id: land_id,
            card_id,
        },
    );
    assert!(
        result.is_ok(),
        "PlayLand should succeed: {:?}",
        result.err()
    );

    // After playing the land, the Landfall trigger should fire.
    // The game should be in Priority (trigger on stack, active player gets priority).
    eprintln!(
        "After PlayLand: waiting_for={:?}, stack_len={}",
        state.waiting_for,
        state.stack.len()
    );
    assert!(
        !state.stack.is_empty(),
        "Landfall trigger should be on the stack, stack_len={}",
        state.stack.len()
    );

    // Step 2: Both players pass priority → trigger resolves
    let mut safety = 0;
    while !state.stack.is_empty() && safety < 20 {
        match &state.waiting_for {
            WaitingFor::Priority { player } => {
                eprintln!(
                    "  Pass priority: player={}, stack_len={}",
                    player.0,
                    state.stack.len()
                );
                let r =
                    apply_as_current(&mut state, GameAction::PassPriority).unwrap_or_else(|e| {
                        panic!("PassPriority failed at iteration {}: {:?}", safety, e)
                    });
                for ev in &r.events {
                    eprintln!("    event: {:?}", ev);
                }
            }
            WaitingFor::TriggerTargetSelection { .. } => {
                panic!(
                    "TriggerTargetSelection should NOT be reached with 0 quest counters! \
                     The QuantityCheck condition defers targeting to resolution time, \
                     and with < 4 counters the sub-ability should be skipped entirely. \
                     Got: {:?}",
                    state.waiting_for
                );
            }
            other => {
                panic!(
                    "Unexpected WaitingFor state during trigger resolution: {:?}",
                    other
                );
            }
        }
        safety += 1;
    }

    // Verify: stack should be empty, game in Priority
    assert!(
        state.stack.is_empty(),
        "Stack should be empty after trigger resolves, stack_len={}",
        state.stack.len()
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "Game should be in Priority after trigger resolves, got: {:?}",
        state.waiting_for
    );

    // Verify: quest counter was placed on the enchantment
    let enchantment = state
        .objects
        .get(&enchantment_id)
        .expect("enchantment exists");
    let quest_count = enchantment
        .counters
        .iter()
        .find_map(|(ct, &count)| {
            if format!("{:?}", ct).to_lowercase().contains("quest") {
                Some(count)
            } else {
                None
            }
        })
        .unwrap_or(0);
    assert_eq!(
        quest_count, 1,
        "Enchantment should have exactly 1 quest counter"
    );

    eprintln!("SUCCESS: Landfall trigger resolved normally with 0→1 quest counters");
}

/// Verify the AI correctly passes priority on Earthbender's Landfall trigger.
/// Regression test for the hang where the AI didn't act after the trigger was placed
/// on the stack.
#[test]
fn test_ai_passes_priority_on_earthbender_landfall() {
    use engine::game::engine::apply_as_current;
    use engine::types::ability::{AbilityCondition, AbilityKind, Comparator, QuantityRef};
    use engine::types::triggers::TriggerMode;
    use engine::types::TriggerDefinition;

    let mut state = GameState::new_two_player(42);
    state.phase = Phase::PreCombatMain;
    state.turn_number = 3;
    state.active_player = P0;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };

    let creature_id = create_object(
        &mut state,
        CardId(5),
        P0,
        "Badgermole Cub".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&creature_id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
        obj.entered_battlefield_turn = Some(1);
    }

    let enchantment_id = create_object(
        &mut state,
        CardId(1),
        P0,
        "Earthbender Ascension".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&enchantment_id).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.base_card_types = obj.card_types.clone();
        obj.entered_battlefield_turn = Some(2);

        let landfall_execute = engine::types::ability::AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutCounter {
                counter_type: CounterType::Generic("quest".to_string()),
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::SelfRef,
            },
        )
        .sub_ability(
            engine::types::ability::AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::PutCounter {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Typed(
                        engine::types::ability::TypedFilter::creature()
                            .controller(engine::types::ability::ControllerRef::You),
                    ),
                },
            )
            .condition(AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::CountersOn {
                        scope: engine::types::ability::ObjectScope::Source,
                        counter_type: Some(CounterType::Generic("quest".to_string())),
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 4 },
            }),
        );

        let landfall_trigger = TriggerDefinition::new(TriggerMode::ChangesZone)
            .destination(Zone::Battlefield)
            .valid_card(TargetFilter::Typed(engine::types::ability::TypedFilter {
                type_filters: vec![engine::types::ability::TypeFilter::Land],
                controller: Some(engine::types::ability::ControllerRef::You),
                properties: vec![],
            }))
            .description(
                "Whenever a land you control enters, put a quest counter on this enchantment."
                    .to_string(),
            )
            .execute(landfall_execute);
        obj.trigger_definitions.push(landfall_trigger.clone());
        Arc::make_mut(&mut obj.base_trigger_definitions).push(landfall_trigger);
    }

    let land_id = create_object(&mut state, CardId(10), P0, "Forest".to_string(), Zone::Hand);
    {
        let obj = state.objects.get_mut(&land_id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.subtypes.push("Forest".to_string());
        obj.base_card_types = obj.card_types.clone();
    }

    // Play the land → Landfall trigger fires
    let card_id = state.objects.get(&land_id).unwrap().card_id;
    apply_as_current(
        &mut state,
        GameAction::PlayLand {
            object_id: land_id,
            card_id,
        },
    )
    .unwrap();

    assert!(
        !state.stack.is_empty(),
        "Landfall trigger should be on stack"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0),
        "Active player should have priority, got {:?}",
        state.waiting_for
    );

    // Simulate human passing priority
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player } if player == PlayerId(1)),
        "AI should have priority after human passes, got {:?}",
        state.waiting_for
    );

    // Now verify AI candidates include PassPriority
    let ctx = candidate_actions(&state);
    eprintln!(
        "AI candidates: {:?}",
        ctx.iter().map(|c| &c.action).collect::<Vec<_>>()
    );
    assert!(
        ctx.iter()
            .any(|c| matches!(c.action, GameAction::PassPriority)),
        "AI must have PassPriority as a candidate"
    );

    // AI passes priority
    let result = apply_as_current(&mut state, GameAction::PassPriority);
    assert!(
        result.is_ok(),
        "AI's chosen action should be valid: {:?}",
        result.err()
    );

    eprintln!(
        "After AI action: waiting_for={:?}, stack_len={}",
        state.waiting_for,
        state.stack.len()
    );

    // After both players pass, stack should be empty (trigger resolved)
    assert!(
        state.stack.is_empty(),
        "Stack should be empty after both pass, stack_len={}",
        state.stack.len()
    );
}

// ---------------------------------------------------------------------------
// Earthbend return + shock-land replacement interaction
// ---------------------------------------------------------------------------
//
// CR 614.7: An Optional replacement whose decline branch would be a no-op on
// the current event must not be presented as a dominated choice. When a shock
// land is returned to the battlefield tapped by an Earthbend delayed trigger,
// the shock land's own "pay 2 life or enter tapped" prompt must be skipped —
// the decline's `Tap SelfRef` would do nothing (the land is tapping anyway),
// and paying 2 life to avoid a tap that isn't happening is strictly worse.

/// Build a shock-land replacement definition matching the parser's output for
/// "As ~ enters, you may pay 2 life. If you don't, it enters tapped."
fn shock_land_replacement() -> engine::types::ability::ReplacementDefinition {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, QuantityExpr, ReplacementDefinition,
        ReplacementMode, TargetFilter,
    };
    use engine::types::replacements::ReplacementEvent;

    let lose_life = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::LoseLife {
            amount: QuantityExpr::Fixed { value: 2 },
            target: None,
        },
    );
    let tap_self = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::SetTapState {
            target: TargetFilter::SelfRef,
            scope: EffectScope::Single,
            state: TapStateChange::Tap,
        },
    );
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .execute(lose_life)
        .mode(ReplacementMode::Optional {
            decline: Some(Box::new(tap_self)),
        })
        .valid_card(TargetFilter::SelfRef)
        .description("As ~ enters, you may pay 2 life. If you don't, it enters tapped.".to_string())
}

fn install_shock_land(state: &mut GameState, card_id: CardId, zone: Zone, name: &str) -> ObjectId {
    let land_id = create_object(state, card_id, P0, name.to_string(), zone);
    let obj = state.objects.get_mut(&land_id).unwrap();
    obj.card_types.core_types.push(CoreType::Land);
    obj.base_card_types = obj.card_types.clone();
    let repl = shock_land_replacement();
    obj.replacement_definitions.push(repl.clone());
    Arc::make_mut(&mut obj.base_replacement_definitions).push(repl);
    land_id
}

/// Drive the Earthbend delayed-trigger resolution for a shock land that died
/// while animated: the ChangeZone effect carries `enter_tapped=true` and
/// `enters_under=Some(ControllerRef::You)` (the fields set by the Earthbending trigger).
/// The shock land's own Optional replacement must NOT prompt the player — the
/// decline branch (`Tap SelfRef`) is a no-op when `enter_tapped` is already
/// `true`, and presenting "pay 2 life or decline" would be a dominated choice.
#[test]
fn earthbend_return_skips_shock_land_pay_life_prompt() {
    use engine::game::replacement::{replace_event, ReplacementResult};
    use engine::types::proposed_event::{EtbTapState, ProposedEvent};

    let mut state = GameState::new_two_player(42);
    state.phase = Phase::PreCombatMain;
    state.active_player = P0;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };

    let land_id = install_shock_land(&mut state, CardId(100), Zone::Graveyard, "Watery Grave");
    let p0_starting_life = state.players.iter().find(|p| p.id == P0).unwrap().life;

    // Simulate the Earthbend delayed trigger resolving: a ChangeZone effect
    // constructs a ProposedEvent::ZoneChange with enter_tapped/controller_override
    // set before entering the replacement pipeline.
    let proposed = ProposedEvent::ZoneChange {
        object_id: land_id,
        from: Zone::Graveyard,
        to: Zone::Battlefield,
        cause: None,
        attach_to: None,
        enter_tapped: EtbTapState::Tapped,
        enter_with_counters: Vec::new(),
        controller_override: Some(P0),
        enter_transformed: false,
        applied: std::collections::HashSet::new(),
        face_down_profile: None,
    };

    let mut events = Vec::new();
    let result = replace_event(&mut state, proposed, &mut events);

    // CR 614.7: With enter_tapped already true, the shock land's Optional
    // replacement's decline branch (Tap SelfRef) is a no-op. The pipeline
    // must NOT surface a NeedsChoice — it proceeds straight to Execute.
    match result {
        ReplacementResult::Execute(ProposedEvent::ZoneChange {
            enter_tapped,
            controller_override,
            to,
            ..
        }) => {
            assert_eq!(to, Zone::Battlefield);
            assert_eq!(
                enter_tapped,
                EtbTapState::Tapped,
                "enter_tapped must remain true after pipeline"
            );
            assert_eq!(
                controller_override,
                Some(P0),
                "controller override must be preserved"
            );
        }
        other => panic!(
            "Earthbend return of shock land must skip the pay-life prompt; got {:?}",
            other
        ),
    }

    // No replacement choice should be pending.
    assert!(
        state.pending_replacement.is_none(),
        "pending_replacement must be cleared — no dominated choice allowed"
    );

    // No life was lost in the pipeline.
    let p0_life_after = state.players.iter().find(|p| p.id == P0).unwrap().life;
    assert_eq!(
        p0_life_after, p0_starting_life,
        "P0's life must be unchanged — no 2-life payment was offered"
    );
}

/// Regression: a plain shock-land ETB from hand (no pre-existing
/// `enter_tapped`) must STILL prompt the player with the pay-2-life choice.
/// This guards against the dominance check becoming too aggressive.
#[test]
fn plain_shock_land_etb_still_prompts_for_life_payment() {
    use engine::game::replacement::{replace_event, ReplacementResult};
    use engine::types::proposed_event::{EtbTapState, ProposedEvent};

    let mut state = GameState::new_two_player(42);
    state.phase = Phase::PreCombatMain;
    state.active_player = P0;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };

    let land_id = install_shock_land(&mut state, CardId(101), Zone::Hand, "Watery Grave");

    let proposed = ProposedEvent::ZoneChange {
        object_id: land_id,
        from: Zone::Hand,
        to: Zone::Battlefield,
        cause: None,
        attach_to: None,
        enter_tapped: EtbTapState::Unspecified,
        enter_with_counters: Vec::new(),
        controller_override: None,
        enter_transformed: false,
        applied: std::collections::HashSet::new(),
        face_down_profile: None,
    };

    let mut events = Vec::new();
    let result = replace_event(&mut state, proposed, &mut events);

    // Normal shock-land behavior: enter_tapped is false → decline's Tap SelfRef
    // is NOT a no-op → the Optional remains applicable → player is prompted.
    match result {
        ReplacementResult::NeedsChoice(player) => {
            assert_eq!(player, P0, "affected player must receive the choice");
        }
        other => panic!(
            "Plain shock-land ETB must prompt the player; got {:?}",
            other
        ),
    }
    assert!(
        state.pending_replacement.is_some(),
        "pending_replacement must be populated for the player's choice"
    );
}

// ---------------------------------------------------------------------------
// Earthbend dies-or-exiled return: GitHub issue #313
// ---------------------------------------------------------------------------
//
// CR 603.7c + CR 614.7: After `earthbend N` resolves, the targeted land becomes
// a 0/0 creature with haste plus a delayed triggered ability:
// "When it dies or is exiled, return it to the battlefield tapped." The user
// reported that an earthbended land taking lethal damage went straight to the
// graveyard without returning. These tests pin down the dies-or-exiled →
// return-tapped runtime contract end-to-end so the regression cannot recur.

/// Build the parser-equivalent `Earthbend N` ability tree (Animate → PutCounter
/// → CreateDelayedTrigger(WhenDiesOrExiled → ChangeZone tapped)). Mirrors
/// `try_parse_earthbend_clause` in `parser/oracle_effect/mod.rs` — keeping the
/// shape identical here means a regression in the parser surface (e.g.,
/// `enter_tapped` flipped to `false`) is also caught by these tests.
fn build_earthbend_ability(
    source_id: ObjectId,
    target_land: ObjectId,
    controller: PlayerId,
    counter_count: i32,
) -> ResolvedAbility {
    use engine::types::ability::TypeFilter;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ControllerRef, DelayedTriggerCondition, TargetRef,
        TypedFilter,
    };

    // Inner delayed-trigger payload — an `AbilityDefinition` per the
    // `Effect::CreateDelayedTrigger.effect: Box<AbilityDefinition>` contract.
    let return_effect_def = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::ChangeZone {
            origin: None,
            destination: Zone::Battlefield,
            target: TargetFilter::TriggeringSource,
            owner_library: false,
            enter_transformed: false,
            enters_under: Some(ControllerRef::You),
            enter_tapped: engine::types::zones::EtbTapState::Tapped,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
            enters_modified_if: None,
        },
    );

    let register_bending = ResolvedAbility::new(
        Effect::RegisterBending {
            kind: BendingType::Earth,
        },
        vec![],
        source_id,
        controller,
    );

    let mut delayed_return = ResolvedAbility::new(
        Effect::CreateDelayedTrigger {
            condition: DelayedTriggerCondition::WhenDiesOrExiled {
                filter: TargetFilter::ParentTarget,
            },
            effect: Box::new(return_effect_def),
            uses_tracked_set: false,
        },
        vec![],
        source_id,
        controller,
    );
    delayed_return.sub_ability = Some(Box::new(register_bending));

    let mut put_counters = ResolvedAbility::new(
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed {
                value: counter_count,
            },
            target: TargetFilter::ParentTarget,
        },
        vec![],
        source_id,
        controller,
    );
    put_counters.sub_ability = Some(Box::new(delayed_return));

    let animate_target = TargetFilter::Typed(TypedFilter {
        type_filters: vec![TypeFilter::Land],
        controller: Some(ControllerRef::You),
        properties: vec![],
    });

    let mut animate = ResolvedAbility::new(
        Effect::Animate {
            power: Some(PtValue::Fixed(0)),
            toughness: Some(PtValue::Fixed(0)),
            types: vec!["Creature".to_string()],
            remove_types: vec![],
            target: animate_target,
            keywords: vec![Keyword::Haste],
        },
        vec![TargetRef::Object(target_land)],
        source_id,
        controller,
    );
    animate.sub_ability = Some(Box::new(put_counters));
    animate
}

/// Resolve an Earthbend ability against a land already on the battlefield by
/// pushing it onto the stack as a sorcery and passing both players' priority
/// so the engine drives the full resolution pipeline (Animate → PutCounter →
/// CreateDelayedTrigger). Returns once the stack is empty.
fn cast_synthetic_earthbend(
    state: &mut GameState,
    earthbend_ability: ResolvedAbility,
    source_id: ObjectId,
    controller: PlayerId,
) {
    use engine::game::engine::apply_as_current;
    use engine::game::stack;
    use engine::types::game_state::{StackEntry, StackEntryKind};

    let entry_id = ObjectId(state.next_object_id);
    state.next_object_id += 1;
    let entry = StackEntry {
        id: entry_id,
        source_id,
        controller,
        kind: StackEntryKind::TriggeredAbility {
            source_id,
            ability: Box::new(earthbend_ability),
            condition: None,
            trigger_event: None,
            description: None,
            source_name: String::new(),
            subject_match_count: None,
            die_result: None,
        },
    };
    stack::push_to_stack(state, entry, &mut vec![]);

    // Both players pass — trigger resolves: Animate + PutCounter + CreateDelayedTrigger.
    apply_as_current(state, GameAction::PassPriority).expect("P0 pass");
    apply_as_current(state, GameAction::PassPriority).expect("P1 pass");
}

/// CR 603.7c + CR 614.7 + Issue #313: An earthbended land that takes lethal
/// damage (SBA-driven death) must return to the battlefield tapped via the
/// delayed "when it dies or is exiled" trigger, not stay in the graveyard.
#[test]
fn earthbended_land_returns_tapped_after_lethal_damage() {
    use engine::game::engine::apply_as_current;
    use engine::types::card_type::Supertype;

    let mut scenario = GameScenario::default();
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();

    // Target land — basic Mountain.
    let land_id = create_object(
        runner.state_mut(),
        CardId(2),
        P0,
        "Mountain".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = runner.state_mut().objects.get_mut(&land_id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.supertypes.push(Supertype::Basic);
        obj.base_card_types = obj.card_types.clone();
        obj.entered_battlefield_turn = Some(1);
    }

    // Source object for Earthbend (the spell/ability that earthbended the land).
    let source_id = create_object(
        runner.state_mut(),
        CardId(1),
        P0,
        "Earthbending Lesson".to_string(),
        Zone::Battlefield,
    );

    let ability = build_earthbend_ability(source_id, land_id, P0, 4);
    cast_synthetic_earthbend(runner.state_mut(), ability, source_id, P0);

    // Sanity: the land is now a 0/0 creature with 4 +1/+1 counters and a delayed
    // trigger registered. (Layers may be dirty — force evaluation by querying.)
    engine::game::layers::evaluate_layers(runner.state_mut());
    let land = &runner.state().objects[&land_id];
    assert!(
        land.card_types.core_types.contains(&CoreType::Creature),
        "Land should be a creature after earthbend"
    );
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "Earthbend should register exactly one delayed trigger (dies-or-exiled return)"
    );

    // Mark lethal damage so SBA destroys the now-creature land (0/0 base + 4 P1P1
    // counters = 4/4; deal 4 damage → toughness 4 with 4 marked = lethal).
    {
        let obj = runner.state_mut().objects.get_mut(&land_id).unwrap();
        obj.damage_marked = 4;
    }

    // Pass priority — engine triggers SBA, which destroys the land. The delayed
    // "when it dies or is exiled" trigger then fires and returns it tapped.
    apply_as_current(runner.state_mut(), GameAction::PassPriority).expect("P0 pass");

    // Drive to stack-empty so the return trigger resolves.
    let mut safety = 0;
    while !runner.state().stack.is_empty() && safety < 30 {
        let _ = apply_as_current(runner.state_mut(), GameAction::PassPriority);
        safety += 1;
    }

    let land_after = runner
        .state()
        .objects
        .get(&land_id)
        .expect("land must still exist as an object");

    // Bug repro: pre-fix this assertion fails — the land sits in the graveyard.
    assert_eq!(
        land_after.zone,
        Zone::Battlefield,
        "Earthbended land must return to the battlefield after dying (issue #313); was in {:?}",
        land_after.zone
    );
    assert!(
        land_after.tapped,
        "Returned land must be tapped (CR 614.1: \"return it to the battlefield tapped\")"
    );

    // The delayed trigger is one-shot — must be cleared after firing.
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "One-shot dies-or-exiled trigger must be removed after firing, got {} remaining",
        runner.state().delayed_triggers.len()
    );
}

/// CR 603.7c: The same delayed trigger must also fire on exile (not just death).
/// An animated land bounced into exile by, e.g., Path to Exile, must come back
/// tapped — the trigger watches `Battlefield → Graveyard | Exile`. Exile is
/// driven via `Effect::Bounce { destination: Exile }` (the canonical exile
/// pathway for permanents-to-exile) so the resulting `ZoneChanged` event reaches
/// the engine's delayed-trigger pipeline.
#[test]
fn earthbended_land_returns_tapped_after_exile() {
    use engine::game::engine::apply_as_current;
    use engine::game::stack;
    use engine::types::ability::TargetRef;
    use engine::types::card_type::Supertype;
    use engine::types::game_state::{StackEntry, StackEntryKind};

    let mut scenario = GameScenario::default();
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();

    let land_id = create_object(
        runner.state_mut(),
        CardId(2),
        P0,
        "Forest".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = runner.state_mut().objects.get_mut(&land_id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.supertypes.push(Supertype::Basic);
        obj.base_card_types = obj.card_types.clone();
        obj.entered_battlefield_turn = Some(1);
    }

    let source_id = create_object(
        runner.state_mut(),
        CardId(1),
        P0,
        "Earthbending Student".to_string(),
        Zone::Battlefield,
    );

    let ability = build_earthbend_ability(source_id, land_id, P0, 2);
    cast_synthetic_earthbend(runner.state_mut(), ability, source_id, P0);

    // Push an exile-the-land triggered ability onto the stack via Effect::Bounce
    // (the engine's primary battlefield→other-zone primitive). Goes through the
    // standard priority-resolution pipeline so the resulting `ZoneChanged
    // { from: Battlefield, to: Exile }` event reaches the delayed-trigger checker.
    let exile_source = create_object(
        runner.state_mut(),
        CardId(99),
        P0,
        "Path to Exile (synthetic)".to_string(),
        Zone::Battlefield,
    );
    let exile_ability = ResolvedAbility::new(
        Effect::ChangeZoneAll {
            origin: Some(Zone::Battlefield),
            destination: Zone::Exile,
            target: TargetFilter::SpecificObject { id: land_id },
            enters_under: None,
            enter_tapped: engine::types::zones::EtbTapState::Unspecified,
            enter_with_counters: vec![],
            face_down_profile: None,
            library_position: None,
            random_order: false,
        },
        vec![TargetRef::Object(land_id)],
        exile_source,
        P0,
    );
    let entry_id = ObjectId(runner.state().next_object_id);
    runner.state_mut().next_object_id += 1;
    let entry = StackEntry {
        id: entry_id,
        source_id: exile_source,
        controller: P0,
        kind: StackEntryKind::TriggeredAbility {
            source_id: exile_source,
            ability: Box::new(exile_ability),
            condition: None,
            trigger_event: None,
            description: None,
            source_name: String::new(),
            subject_match_count: None,
            die_result: None,
        },
    };
    stack::push_to_stack(runner.state_mut(), entry, &mut vec![]);

    let mut safety = 0;
    while !runner.state().stack.is_empty() && safety < 30 {
        let _ = apply_as_current(runner.state_mut(), GameAction::PassPriority);
        safety += 1;
    }

    let land_after = &runner.state().objects[&land_id];
    assert_eq!(
        land_after.zone,
        Zone::Battlefield,
        "Exiled earthbended land must return to the battlefield (CR 603.7c: \"dies or is exiled\"); was in {:?}",
        land_after.zone
    );
    assert!(
        land_after.tapped,
        "Returned land must be tapped after exile-and-return"
    );
}

/// End-to-end card-data test: cast the real parsed `Earthbending Lesson` from
/// hand on a basic Mountain, then send the now-creature land to the graveyard
/// via lethal damage. Verifies the full parser → cast → resolve → SBA →
/// delayed-trigger → return-tapped pipeline against the actual exported AST.
/// This is the closest analog to what the user reported in #313.
#[test]
fn earthbending_lesson_returned_tapped_after_dies_e2e() {
    use engine::game::engine::apply_as_current;
    use engine::game::scenario_db::GameScenarioDbExt;
    use engine::types::card_type::Supertype;

    use crate::support::shared_card_db as load_db;

    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::default();
    scenario.at_phase(Phase::PreCombatMain);
    let lesson_id = scenario.add_real_card(P0, "Earthbending Lesson", Zone::Hand, db);
    let mountain_id = scenario.add_real_card(P0, "Mountain", Zone::Battlefield, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // Pay the cost: {3}{G} — give P0 the mana to cast.
    let dummy = ObjectId(0);
    {
        let pool = &mut runner
            .state_mut()
            .players
            .iter_mut()
            .find(|p| p.id == P0)
            .unwrap()
            .mana_pool;
        for _ in 0..3 {
            pool.add(ManaUnit::new(ManaType::Colorless, dummy, false, vec![]));
        }
        pool.add(ManaUnit::new(ManaType::Green, dummy, false, vec![]));
    }

    // Make sure the Mountain is set up as a basic land (rehydrate may not have).
    {
        let obj = runner.state_mut().objects.get_mut(&mountain_id).unwrap();
        if !obj.card_types.core_types.contains(&CoreType::Land) {
            obj.card_types.core_types.push(CoreType::Land);
        }
        if !obj.card_types.supertypes.contains(&Supertype::Basic) {
            obj.card_types.supertypes.push(Supertype::Basic);
        }
        obj.base_card_types = obj.card_types.clone();
        obj.entered_battlefield_turn = Some(1);
    }

    let card_id = runner.state().objects[&lesson_id].card_id;
    apply_as_current(
        runner.state_mut(),
        GameAction::CastSpell {
            object_id: lesson_id,
            card_id,
            targets: vec![mountain_id],

            payment_mode: CastPaymentMode::Auto,
        },
    )
    .expect("cast Earthbending Lesson");

    // Drive priority until the spell + any chain resolve and stack is empty.
    let mut safety = 0;
    while !runner.state().stack.is_empty() && safety < 30 {
        let _ = apply_as_current(runner.state_mut(), GameAction::PassPriority);
        safety += 1;
    }

    // The Mountain is now a 4/4 creature (0/0 base + 4 +1/+1 counters) with haste.
    engine::game::layers::evaluate_layers(runner.state_mut());
    {
        let m = &runner.state().objects[&mountain_id];
        assert!(
            m.card_types.core_types.contains(&CoreType::Creature),
            "Mountain should be a creature post-Earthbend (got types {:?})",
            m.card_types.core_types
        );
    }
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "Earthbend resolution must register exactly one dies-or-exiled delayed trigger"
    );

    // Apply lethal damage and pass priority — SBA destroys, trigger fires, return.
    runner
        .state_mut()
        .objects
        .get_mut(&mountain_id)
        .unwrap()
        .damage_marked = 4;
    let mut safety = 0;
    while safety < 30 {
        let _ = apply_as_current(runner.state_mut(), GameAction::PassPriority);
        if runner.state().stack.is_empty()
            && runner.state().delayed_triggers.is_empty()
            && runner.state().objects[&mountain_id].zone == Zone::Battlefield
        {
            break;
        }
        safety += 1;
    }

    let m = &runner.state().objects[&mountain_id];
    assert_eq!(
        m.zone,
        Zone::Battlefield,
        "Earthbending Lesson dies-or-exiled return: real card #313 — Mountain must be back on the battlefield, was in {:?}",
        m.zone
    );
    assert!(
        m.tapped,
        "Earthbending Lesson dies-or-exiled return: real card #313 — Mountain must be tapped"
    );
}

/// Building-block test: Earthbend resolution must register the delayed
/// dies-or-exiled trigger on the controller's `state.delayed_triggers` queue
/// with the filter bound to the targeted land's specific ObjectId. This
/// pins the parser → resolver contract that powers issue #313's fix.
#[test]
fn earthbend_registers_dies_or_exiled_delayed_trigger_on_target() {
    use engine::types::ability::{DelayedTriggerCondition, TargetFilter};
    use engine::types::card_type::Supertype;

    let mut scenario = GameScenario::default();
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();

    let land_id = create_object(
        runner.state_mut(),
        CardId(2),
        P0,
        "Plains".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = runner.state_mut().objects.get_mut(&land_id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.supertypes.push(Supertype::Basic);
        obj.base_card_types = obj.card_types.clone();
        obj.entered_battlefield_turn = Some(1);
    }

    let source_id = create_object(
        runner.state_mut(),
        CardId(1),
        P0,
        "Toph Earthbending Master".to_string(),
        Zone::Battlefield,
    );

    let ability = build_earthbend_ability(source_id, land_id, P0, 3);
    cast_synthetic_earthbend(runner.state_mut(), ability, source_id, P0);

    // The delayed trigger must be present, conditioned on `WhenDiesOrExiled` for
    // the specific animated land (parser binds `ParentTarget` → SpecificObject
    // at delayed-trigger creation time per `bind_contextual_filter_to_condition`).
    assert_eq!(runner.state().delayed_triggers.len(), 1);
    let trigger = &runner.state().delayed_triggers[0];
    match &trigger.condition {
        DelayedTriggerCondition::WhenDiesOrExiled { filter } => match filter {
            TargetFilter::SpecificObject { id } => assert_eq!(*id, land_id),
            other => panic!(
                "Expected SpecificObject filter bound to land_id, got {:?}",
                other
            ),
        },
        other => panic!("Expected WhenDiesOrExiled condition, got {:?}", other),
    }
    assert!(
        trigger.one_shot,
        "Dies-or-exiled return is one-shot — must fire once and be removed"
    );
    assert_eq!(trigger.controller, P0);

    // Inner effect must be ChangeZone(destination=Battlefield, enter_tapped=true,
    // enters_under=Some(ControllerRef::You)) — this is the actual return-tapped behavior.
    match &trigger.ability.effect {
        Effect::ChangeZone {
            destination,
            enter_tapped,
            enters_under,
            ..
        } => {
            assert_eq!(*destination, Zone::Battlefield);
            assert!(
                enter_tapped.is_tapped(),
                "Inner ChangeZone must carry enter_tapped=true"
            );
            assert_eq!(
                *enters_under,
                Some(engine::types::ability::ControllerRef::You),
                "Inner ChangeZone must carry enters_under=Some(ControllerRef::You) (returns under earthbender's control)"
            );
        }
        other => panic!("Expected ChangeZone return effect, got {:?}", other),
    }
}
