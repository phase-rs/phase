#![allow(unused_imports)]
use super::*;

use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, AdditionalCost, Effect, QuantityExpr,
    TargetFilter, TargetRef,
};
use engine::types::game_state::{CastingVariant, StackEntryKind};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};

/// Helper: advance past TargetSelection if present, return the resulting WaitingFor.
fn handle_target_selection(runner: &mut engine::game::scenario::GameRunner, result: &ActionResult) {
    if matches!(result.waiting_for, WaitingFor::TargetSelection { .. }) {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Player(P1)],
            })
            .expect("target selection should succeed");
    }
}

/// Extract `additional_cost_paid` from the top stack entry (assumes it's a Spell).
fn top_stack_cost_paid(runner: &engine::game::scenario::GameRunner) -> bool {
    let entry = runner
        .state()
        .stack
        .last()
        .expect("stack should not be empty");
    match &entry.kind {
        StackEntryKind::Spell {
            ability: Some(ability),
            ..
        } => ability.context.additional_cost_paid,
        other => panic!("expected Spell on stack, got {:?}", other),
    }
}

/// Cast a spell with an Optional additional cost, choose to pay.
/// Verifies the casting pipeline enters OptionalCostChoice and
/// sets additional_cost_paid = true on the stack entry when paid.
#[test]
fn optional_cost_paid_sets_flag() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::White);

    let spell_id = scenario
        .add_creature_to_hand(P0, "Blight Bolt", 0, 0)
        .as_instant()
        .with_ability(Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 3 },
            target: TargetFilter::Any,
            damage_source: None,
        })
        .with_additional_cost(AdditionalCost::Optional(AbilityCost::Blight { count: 1 }))
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell_id].card_id;

    let result = runner
        .act(GameAction::CastSpell {
            object_id: spell_id,
            card_id,
            targets: vec![],
        })
        .expect("cast should succeed");

    handle_target_selection(&mut runner, &result);

    // Should now be at OptionalCostChoice
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalCostChoice { .. }
        ),
        "expected OptionalCostChoice, got {:?}",
        runner.state().waiting_for,
    );

    // Pay the additional cost
    let result3 = runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("decide optional cost should succeed");

    assert!(
        matches!(result3.waiting_for, WaitingFor::Priority { .. }),
        "expected Priority after paying, got {:?}",
        result3.waiting_for,
    );

    assert!(
        top_stack_cost_paid(&runner),
        "additional_cost_paid should be true when cost is paid"
    );
}

/// Cast a spell with an Optional additional cost, choose to skip.
/// Verifies additional_cost_paid = false on the stack entry.
#[test]
fn optional_cost_skipped_clears_flag() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::White);

    let spell_id = scenario
        .add_creature_to_hand(P0, "Blight Bolt", 0, 0)
        .as_instant()
        .with_ability(Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 3 },
            target: TargetFilter::Any,
            damage_source: None,
        })
        .with_additional_cost(AdditionalCost::Optional(AbilityCost::Blight { count: 1 }))
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell_id].card_id;

    let result = runner
        .act(GameAction::CastSpell {
            object_id: spell_id,
            card_id,
            targets: vec![],
        })
        .expect("cast should succeed");

    handle_target_selection(&mut runner, &result);

    // Skip the additional cost
    let result3 = runner
        .act(GameAction::DecideOptionalCost { pay: false })
        .expect("skip optional cost should succeed");

    assert!(
        matches!(result3.waiting_for, WaitingFor::Priority { .. }),
        "expected Priority after skipping, got {:?}",
        result3.waiting_for,
    );

    assert!(
        !top_stack_cost_paid(&runner),
        "additional_cost_paid should be false when cost is skipped"
    );
}

/// Cast a spell without an additional cost -- should skip OptionalCostChoice entirely.
#[test]
fn no_additional_cost_skips_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::Red);

    let spell_id = scenario.add_bolt_to_hand(P0);

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell_id].card_id;

    let result = runner
        .act(GameAction::CastSpell {
            object_id: spell_id,
            card_id,
            targets: vec![],
        })
        .expect("cast should succeed");

    // Should go to target selection or directly to priority -- never OptionalCostChoice
    assert!(
        !matches!(result.waiting_for, WaitingFor::OptionalCostChoice { .. }),
        "should not enter OptionalCostChoice for spells without additional costs"
    );
}

/// Cancel cast while at OptionalCostChoice returns the spell to hand.
#[test]
fn cancel_cast_at_optional_cost_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::White);

    let spell_id = scenario
        .add_creature_to_hand(P0, "Blight Bolt", 0, 0)
        .as_instant()
        .with_ability(Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 3 },
            target: TargetFilter::Any,
            damage_source: None,
        })
        .with_additional_cost(AdditionalCost::Optional(AbilityCost::Blight { count: 1 }))
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell_id].card_id;

    let result = runner
        .act(GameAction::CastSpell {
            object_id: spell_id,
            card_id,
            targets: vec![],
        })
        .expect("cast should succeed");

    handle_target_selection(&mut runner, &result);

    // Cancel the cast
    let result3 = runner
        .act(GameAction::CancelCast)
        .expect("cancel should succeed");

    assert!(
        matches!(result3.waiting_for, WaitingFor::Priority { .. }),
        "expected Priority after cancel, got {:?}",
        result3.waiting_for,
    );

    assert!(
        runner.state().stack.is_empty(),
        "stack should be empty after cancel"
    );
    assert_eq!(
        runner.state().objects[&spell_id].zone,
        Zone::Hand,
        "spell should return to hand after cancel"
    );
}

// ── Escape casting tests ────────────────────────────────────────────────────

/// Helper: set up a game with an escape creature in the graveyard and N filler
/// graveyard cards. Returns (runner, escape_card_id, escape_obj_id, filler_ids).
fn setup_escape_scenario(
    filler_count: usize,
) -> (
    engine::game::scenario::GameRunner,
    CardId,
    ObjectId,
    Vec<ObjectId>,
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Land for {G} mana
    scenario.add_basic_land(P0, ManaColor::Green);

    // Escape creature: 2/2 with Escape—{G}, Exile two other cards
    let escape_id = scenario
        .add_creature_to_hand(P0, "Escape Bear", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        })
        .with_keyword(Keyword::Escape {
            cost: ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 0,
            },
            exile_count: 2,
        })
        .id();

    let mut runner = scenario.build();
    let escape_card_id = runner.state().objects[&escape_id].card_id;

    // Move escape creature from hand to graveyard
    engine::game::zones::move_to_zone(
        runner.state_mut(),
        escape_id,
        Zone::Graveyard,
        &mut Vec::new(),
    );

    // Add filler cards to graveyard
    let mut filler_ids = Vec::new();
    for i in 0..filler_count {
        let filler_card_id = CardId(runner.state().next_object_id);
        let filler_id = engine::game::zones::create_object(
            runner.state_mut(),
            filler_card_id,
            P0,
            format!("Filler Card {}", i + 1),
            Zone::Graveyard,
        );
        filler_ids.push(filler_id);
    }

    (runner, escape_card_id, escape_id, filler_ids)
}

/// CR 702.138: Escape card in graveyard with enough other cards → appears in castable list.
#[test]
fn escape_card_appears_castable_with_enough_graveyard() {
    let (runner, _card_id, escape_id, _filler) = setup_escape_scenario(2);
    let castable = engine::game::casting::spell_objects_available_to_cast(runner.state(), P0);
    assert!(
        castable.contains(&escape_id),
        "Escape card should be castable when graveyard has enough cards"
    );
}

/// CR 702.138: Escape card in graveyard without enough other cards → NOT castable.
#[test]
fn escape_card_not_castable_without_enough_graveyard() {
    let (runner, _card_id, escape_id, _filler) = setup_escape_scenario(1); // Only 1, need 2
    let castable = engine::game::casting::spell_objects_available_to_cast(runner.state(), P0);
    assert!(
        !castable.contains(&escape_id),
        "Escape card should NOT be castable with insufficient graveyard cards"
    );
}

/// CR 702.138: Full escape casting flow — CastSpell → ExileFromGraveyardForCost → SelectCards → ManaPayment.
#[test]
fn escape_full_casting_flow() {
    let (mut runner, escape_card_id, escape_id, filler) = setup_escape_scenario(3);

    // Cast the escape creature from graveyard
    let result = runner
        .act(GameAction::CastSpell {
            object_id: escape_id,
            card_id: escape_card_id,
            targets: vec![],
        })
        .expect("CastSpell should succeed");

    // Should be prompted to exile cards from graveyard
    assert!(
        matches!(
            result.waiting_for,
            WaitingFor::ExileFromGraveyardForCost { count: 2, .. }
        ),
        "Expected ExileFromGraveyardForCost, got {:?}",
        result.waiting_for
    );

    // Verify the escape card itself is NOT in the eligible list
    if let WaitingFor::ExileFromGraveyardForCost { ref cards, .. } = result.waiting_for {
        assert!(
            !cards.contains(&escape_id),
            "Escape card itself should not be eligible for exile"
        );
    }

    // Select two filler cards to exile
    let result2 = runner
        .act(GameAction::SelectCards {
            cards: vec![filler[0], filler[1]],
        })
        .expect("SelectCards should succeed");

    // Mana auto-taps {G} from the land, so we go straight to Priority (spell on stack)
    assert!(
        matches!(result2.waiting_for, WaitingFor::Priority { .. }),
        "Expected Priority (auto-tapped mana) after exile selection, got {:?}",
        result2.waiting_for
    );

    // Verify exiled cards are in exile zone
    assert_eq!(runner.state().objects[&filler[0]].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&filler[1]].zone, Zone::Exile);

    // Verify the spell is on the stack with Escape casting variant
    assert_eq!(
        runner.state().stack.len(),
        1,
        "Escape spell should be on the stack"
    );
    let stack_entry = &runner.state().stack[0];
    match &stack_entry.kind {
        StackEntryKind::Spell {
            casting_variant, ..
        } => {
            assert_eq!(
                *casting_variant,
                CastingVariant::Escape,
                "Stack entry should have CastingVariant::Escape"
            );
        }
        other => panic!("Expected Spell on stack, got {:?}", other),
    }
}

/// Regression: CastingVariant must survive the ManaPayment detour.
/// When escape cost contains X, pay_and_push_adventure enters ManaPayment.
/// The pending_cast must preserve CastingVariant::Escape.
#[test]
fn escape_variant_preserved_through_mana_payment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Two green lands for {X}{G} where X=1
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_basic_land(P0, ManaColor::Green);

    // Escape creature with X in escape cost: {X}{G}
    let escape_id = scenario
        .add_creature_to_hand(P0, "X Escape", 0, 0)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Green],
            generic: 0,
        })
        .with_keyword(Keyword::Escape {
            cost: ManaCost::Cost {
                shards: vec![ManaCostShard::X, ManaCostShard::Green],
                generic: 0,
            },
            exile_count: 2,
        })
        .id();

    let mut runner = scenario.build();
    let escape_card_id = runner.state().objects[&escape_id].card_id;

    // Move to graveyard
    engine::game::zones::move_to_zone(
        runner.state_mut(),
        escape_id,
        Zone::Graveyard,
        &mut Vec::new(),
    );

    // Add 2 filler graveyard cards
    for i in 0..2 {
        let filler_card_id = CardId(runner.state().next_object_id);
        engine::game::zones::create_object(
            runner.state_mut(),
            filler_card_id,
            P0,
            format!("Filler {}", i),
            Zone::Graveyard,
        );
    }

    // Cast from graveyard
    let result = runner
        .act(GameAction::CastSpell {
            object_id: escape_id,
            card_id: escape_card_id,
            targets: vec![],
        })
        .expect("CastSpell should succeed");

    // Should prompt for exile selection
    assert!(matches!(
        result.waiting_for,
        WaitingFor::ExileFromGraveyardForCost { .. }
    ));

    // Select exile targets
    if let WaitingFor::ExileFromGraveyardForCost { ref cards, .. } = result.waiting_for {
        runner
            .act(GameAction::SelectCards {
                cards: cards[..2].to_vec(),
            })
            .expect("Exile selection should succeed");
    }

    // X cost means ManaPayment should be the current state
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "Expected ManaPayment for X-cost escape, got {:?}",
        runner.state().waiting_for
    );

    // Verify pending_cast has Escape variant
    let pending = runner
        .state()
        .pending_cast
        .as_ref()
        .expect("pending_cast should exist during ManaPayment");
    assert_eq!(
        pending.casting_variant,
        CastingVariant::Escape,
        "CastingVariant::Escape must survive through to ManaPayment pending_cast"
    );
}

/// CR 702.138: CancelCast during exile selection returns to Priority.
#[test]
fn escape_cancel_returns_to_priority() {
    let (mut runner, escape_card_id, escape_id, _filler) = setup_escape_scenario(3);

    runner
        .act(GameAction::CastSpell {
            object_id: escape_id,
            card_id: escape_card_id,
            targets: vec![],
        })
        .expect("CastSpell should succeed");

    let result = runner
        .act(GameAction::CancelCast)
        .expect("CancelCast should succeed");

    assert!(
        matches!(result.waiting_for, WaitingFor::Priority { .. }),
        "Expected Priority after cancel, got {:?}",
        result.waiting_for
    );
}

// --- Zone-scoped cost modification tests ---

/// CR 601.2f: Cost modifications scoped to "from graveyards or from exile"
/// must NOT apply when the spell is cast from hand.
/// Regression test for Aven Interrupter incorrectly taxing hand-cast spells.
#[test]
fn raise_cost_from_exile_does_not_tax_hand_cast() {
    use engine::parser::oracle_static::parse_static_line;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Give P0 exactly 1 red mana — enough for a {R} spell, but not {2}{R}.
    scenario.add_basic_land(P0, ManaColor::Red);

    // Opponent's creature with Aven Interrupter's static:
    // "Spells your opponents cast from graveyards or from exile cost {2} more to cast."
    scenario
        .add_creature(P1, "Aven Interrupter", 2, 2)
        .with_static_definition(
            parse_static_line(
                "Spells your opponents cast from graveyards or from exile cost {2} more to cast.",
            )
            .expect("Aven Interrupter static should parse"),
        );

    // Lightning Bolt in P0's hand: costs {R}
    let spell_id = scenario.add_bolt_to_hand(P0);

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell_id].card_id;

    // Cast from hand — should succeed with just 1 Mountain because the tax
    // only applies to spells cast from graveyards/exile.
    let result = runner.act(GameAction::CastSpell {
        object_id: spell_id,
        card_id,
        targets: vec![],
    });

    assert!(
        result.is_ok(),
        "Spell from hand should NOT be taxed by zone-scoped RaiseCost — got: {:?}",
        result.err(),
    );
}

// --- Graveyard land play permission tests ---

use engine::types::ability::{CardPlayMode, StaticDefinition, TypeFilter};
use engine::types::card_type::CoreType;
use engine::types::statics::StaticMode;

/// CR 604.2 + CR 305.1: A permanent with GraveyardCastPermission { play_mode: Play }
/// allows playing lands from the graveyard.
#[test]
fn play_land_from_graveyard_with_permission() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Add a creature on the battlefield with the graveyard play permission
    let _source_id = scenario
        .add_creature(P0, "Crucible of Worlds", 0, 0)
        .with_static_definition(
            StaticDefinition::new(StaticMode::GraveyardCastPermission {
                once_per_turn: false,
                play_mode: CardPlayMode::Play,
            })
            .affected(TargetFilter::Typed(
                engine::types::ability::TypedFilter::new(TypeFilter::Land),
            )),
        )
        .id();

    let mut runner = scenario.build();

    // Put a Forest in P0's graveyard by creating it there directly
    let forest_id = engine::game::zones::create_object(
        runner.state_mut(),
        engine::types::identifiers::CardId(99),
        P0,
        "Forest".to_string(),
        Zone::Graveyard,
    );
    {
        let obj = runner.state_mut().objects.get_mut(&forest_id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.base_card_types = obj.card_types.clone();
    }

    let card_id = runner.state().objects[&forest_id].card_id;

    // Play the Forest from graveyard
    runner
        .act(GameAction::PlayLand {
            object_id: forest_id,
            card_id,
        })
        .expect("should be able to play land from graveyard");

    // Verify it entered the battlefield
    assert!(
        runner.state().battlefield.contains(&forest_id),
        "Forest should be on the battlefield"
    );
    assert!(
        !runner
            .state()
            .players
            .iter()
            .find(|p| p.id == P0)
            .unwrap()
            .graveyard
            .contains(&forest_id),
        "Forest should no longer be in graveyard"
    );
    // CR 305.2a: Playing from GY counts as a land drop
    assert_eq!(runner.state().lands_played_this_turn, 1);
}

/// CR 305.2a: Playing a land from graveyard counts against the per-turn land limit.
#[test]
fn play_land_from_graveyard_respects_land_drop_limit() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let _source_id = scenario
        .add_creature(P0, "Crucible of Worlds", 0, 0)
        .with_static_definition(
            StaticDefinition::new(StaticMode::GraveyardCastPermission {
                once_per_turn: false,
                play_mode: CardPlayMode::Play,
            })
            .affected(TargetFilter::Typed(
                engine::types::ability::TypedFilter::new(TypeFilter::Land),
            )),
        )
        .id();

    // Also add a land in hand so we can play it first
    let hand_land_id = scenario.add_land_to_hand(P0, "Plains").id();

    let mut runner = scenario.build();

    // Put a Forest in graveyard
    let forest_id = engine::game::zones::create_object(
        runner.state_mut(),
        engine::types::identifiers::CardId(99),
        P0,
        "Forest".to_string(),
        Zone::Graveyard,
    );
    {
        let obj = runner.state_mut().objects.get_mut(&forest_id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.base_card_types = obj.card_types.clone();
    }

    // Play the hand land first (uses the one land drop)
    let hand_card_id = runner.state().objects[&hand_land_id].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: hand_land_id,
            card_id: hand_card_id,
        })
        .expect("should play land from hand");

    // Now try to play from graveyard — should fail (land drop used)
    let gy_card_id = runner.state().objects[&forest_id].card_id;
    let result = runner.act(GameAction::PlayLand {
        object_id: forest_id,
        card_id: gy_card_id,
    });

    assert!(
        result.is_err(),
        "Should not be able to play second land without additional land drops"
    );
}
