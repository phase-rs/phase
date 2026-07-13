#![allow(unused_imports)]
use super::*;
use std::sync::Arc;

use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, AdditionalCost, Effect, QuantityExpr,
    TargetFilter, TargetRef,
};
use engine::types::game_state::{CastOfferKind, CastingVariant, StackEntryKind};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::{EscapeCost, Keyword};
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
    // Blight requires a creature target; add one to the battlefield.
    let blight_target_id = scenario.add_creature(P0, "Blight Target", 2, 2).id();

    let spell_id = scenario
        .add_creature_to_hand(P0, "Blight Bolt", 0, 0)
        .as_instant()
        .with_ability(Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 3 },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        })
        .with_additional_cost(AdditionalCost::Optional {
            cost: AbilityCost::Blight { count: 1 },
            repeatability: engine::types::ability::AdditionalCostRepeatability::Once,
        })
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell_id].card_id;

    let result = runner
        .act(GameAction::CastSpell {
            object_id: spell_id,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
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

    // Pay the additional cost — this opens BlightChoice.
    let result_opt = runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("decide optional cost should succeed");
    assert!(
        matches!(result_opt.waiting_for, WaitingFor::BlightChoice { .. }),
        "expected BlightChoice after paying, got {:?}",
        result_opt.waiting_for,
    );

    // Select the creature to blight.
    let result3 = runner
        .act(GameAction::SelectCards {
            cards: vec![blight_target_id],
        })
        .expect("blight selection should succeed");

    assert!(
        matches!(result3.waiting_for, WaitingFor::Priority { .. }),
        "expected Priority after blight, got {:?}",
        result3.waiting_for,
    );

    assert!(
        top_stack_cost_paid(&runner),
        "additional_cost_paid should be true when cost is paid"
    );

    // Verify the -1/-1 counter landed on the chosen creature.
    use engine::types::counter::CounterType;
    assert_eq!(
        runner.state().objects[&blight_target_id]
            .counters
            .get(&CounterType::Minus1Minus1)
            .copied()
            .unwrap_or(0),
        1,
        "blight should place a -1/-1 counter on the chosen creature"
    );
}

/// Cast a spell with an Optional additional cost, choose to skip.
/// Verifies additional_cost_paid = false on the stack entry.
#[test]
fn optional_cost_skipped_clears_flag() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::White);
    // CR 601.2b: A creature must exist on the battlefield for blight to be
    // payable; otherwise the OptionalCostChoice prompt is correctly skipped
    // and there is no decision to make.
    scenario.add_creature(P0, "Blight Target", 2, 2);

    let spell_id = scenario
        .add_creature_to_hand(P0, "Blight Bolt", 0, 0)
        .as_instant()
        .with_ability(Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 3 },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        })
        .with_additional_cost(AdditionalCost::Optional {
            cost: AbilityCost::Blight { count: 1 },
            repeatability: engine::types::ability::AdditionalCostRepeatability::Once,
        })
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell_id].card_id;

    let result = runner
        .act(GameAction::CastSpell {
            object_id: spell_id,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
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

/// CR 702.166a + CR 601.2f/601.2g: Bargain cost-ordering proof. A spell with a
/// self-spell `ReduceCost {2}` static gated on `StaticCondition::AdditionalCostPaid`
/// plus an optional additional cost. When the optional cost is PAID, the cost
/// modifiers are re-run (recompute_pending_cast_cost) and the {4} cost drops to
/// {2}, so only 2 of 4 lands are tapped. When SKIPPED, the full {4} is charged.
#[test]
fn bargain_additional_cost_paid_reduces_self_spell_cost() {
    use engine::types::ability::{StaticCondition, StaticDefinition};
    use engine::types::statics::{CostModifyMode, StaticMode};

    fn build_scenario() -> (engine::game::scenario::GameRunner, ObjectId, Vec<ObjectId>) {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        // Four lands — exactly the base {4} cost.
        let lands: Vec<ObjectId> = (0..4)
            .map(|_| scenario.add_basic_land(P0, ManaColor::Green))
            .collect();

        let reduce_static = StaticDefinition::new(StaticMode::ModifyCost {
            mode: CostModifyMode::Reduce,
            amount: ManaCost::generic(2),
            spell_filter: None,
            dynamic_count: None,
        })
        .affected(TargetFilter::SelfRef)
        .condition(StaticCondition::AdditionalCostPaid)
        .active_zones(vec![Zone::Hand, Zone::Stack]);

        let spell_id = scenario
            .add_creature_to_hand(P0, "Bargain Beast", 3, 3)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![],
                generic: 4,
            })
            .with_additional_cost(AdditionalCost::Optional {
                cost: AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                },
                repeatability: engine::types::ability::AdditionalCostRepeatability::Once,
            })
            .with_static_definition(reduce_static)
            .id();

        (scenario.build(), spell_id, lands)
    }

    fn tapped_count(runner: &engine::game::scenario::GameRunner, lands: &[ObjectId]) -> usize {
        lands
            .iter()
            .filter(|id| runner.state().objects[id].tapped)
            .count()
    }

    // --- Paid: cost recomputed to {2}, only 2 lands tapped. ---
    {
        let (mut runner, spell_id, lands) = build_scenario();
        let card_id = runner.state().objects[&spell_id].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: spell_id,
                card_id,
                targets: vec![],

                payment_mode: CastPaymentMode::Auto,
            })
            .expect("cast should succeed at base cost");
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::OptionalCostChoice { .. }
            ),
            "expected OptionalCostChoice, got {:?}",
            runner.state().waiting_for,
        );
        runner
            .act(GameAction::DecideOptionalCost { pay: true })
            .expect("paying the optional cost should succeed");
        assert_eq!(
            tapped_count(&runner, &lands),
            2,
            "Bargain paid → {{4}} reduced to {{2}} → only 2 lands tapped"
        );
    }

    // --- Skipped: full {4} charged, all 4 lands tapped. ---
    {
        let (mut runner, spell_id, lands) = build_scenario();
        let card_id = runner.state().objects[&spell_id].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: spell_id,
                card_id,
                targets: vec![],

                payment_mode: CastPaymentMode::Auto,
            })
            .expect("cast should succeed at base cost");
        runner
            .act(GameAction::DecideOptionalCost { pay: false })
            .expect("skipping the optional cost should succeed");
        assert_eq!(
            tapped_count(&runner, &lands),
            4,
            "Bargain skipped → full {{4}} charged → all 4 lands tapped"
        );
    }
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

            payment_mode: CastPaymentMode::Auto,
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
    // CR 601.2b: A creature must exist for blight to be payable, so the
    // OptionalCostChoice prompt is offered (not auto-skipped).
    scenario.add_creature(P0, "Blight Target", 2, 2);

    let spell_id = scenario
        .add_creature_to_hand(P0, "Blight Bolt", 0, 0)
        .as_instant()
        .with_ability(Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 3 },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        })
        .with_additional_cost(AdditionalCost::Optional {
            cost: AbilityCost::Blight { count: 1 },
            repeatability: engine::types::ability::AdditionalCostRepeatability::Once,
        })
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell_id].card_id;

    let result = runner
        .act(GameAction::CastSpell {
            object_id: spell_id,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
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
        .with_keyword(Keyword::Escape(EscapeCost::NonMana(
            AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Mana {
                        cost: ManaCost::Cost {
                            shards: vec![ManaCostShard::Green],
                            generic: 0,
                        },
                    },
                    AbilityCost::Exile {
                        count: 2,
                        zone: Some(Zone::Graveyard),
                        filter: None,
                    },
                ],
            },
        )))
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

/// CR 702.138: Full escape casting flow — CastSpell → ExileForCost (Graveyard) → SelectCards → ManaPayment.
#[test]
fn escape_full_casting_flow() {
    let (mut runner, escape_card_id, escape_id, filler) = setup_escape_scenario(3);

    // Cast the escape creature from graveyard
    let result = runner
        .act(GameAction::CastSpell {
            object_id: escape_id,
            card_id: escape_card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("CastSpell should succeed");

    // Should be prompted to exile cards from graveyard
    assert!(
        matches!(
            result.waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::ExileFromZone {
                    zone: ExileCostSourceZone::Graveyard,
                },
                count: 2,
                ..
            }
        ),
        "Expected PayCost ExileFromZone (Graveyard), got {:?}",
        result.waiting_for
    );

    // Verify the escape card itself is NOT in the eligible list
    if let WaitingFor::PayCost {
        kind:
            PayCostKind::ExileFromZone {
                zone: ExileCostSourceZone::Graveyard,
            },
        choices: ref cards,
        ..
    } = result.waiting_for
    {
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

/// CR 702.138a + CR 601.2h + CR 701.13 (WHO cluster #9 — Lunar Hatchling): a
/// multi-clause escape additional cost ("Exile a land you control, Exile five
/// other cards from your graveyard") must pay BOTH exile clauses, one at a time,
/// before the spell reaches the stack. The Composite peels the battlefield
/// land-exile clause first (`ExilePermanent`), then on resume the graveyard clause
/// (`ExileFromZone{Graveyard}`). Both selections must complete before the cast.
#[test]
fn escape_multi_clause_exiles_land_then_graveyard_cards() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // A green land that taps for the escape mana cost.
    scenario.add_basic_land(P0, ManaColor::Green);
    // A SECOND land that is the "exile a land you control" cost fodder.
    let cost_land = scenario.add_basic_land(P0, ManaColor::Green);

    // Land-you-control filter for the battlefield exile clause.
    let land_you_control = TargetFilter::Typed(TypedFilter {
        type_filters: vec![TypeFilter::Land],
        controller: Some(engine::types::ability::ControllerRef::You),
        properties: vec![],
    });

    // Lunar-Hatchling-shaped escape: {G}, exile a land you control, exile 2
    // other graveyard cards (count reduced from 5 for a compact fixture).
    let escape_id = scenario
        .add_creature_to_hand(P0, "Multi Escape", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        })
        .with_keyword(Keyword::Escape(EscapeCost::NonMana(
            AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Mana {
                        cost: ManaCost::Cost {
                            shards: vec![ManaCostShard::Green],
                            generic: 0,
                        },
                    },
                    AbilityCost::Exile {
                        count: 1,
                        zone: None,
                        filter: Some(land_you_control),
                    },
                    AbilityCost::Exile {
                        count: 2,
                        zone: Some(Zone::Graveyard),
                        filter: None,
                    },
                ],
            },
        )))
        .id();

    let mut runner = scenario.build();
    let escape_card_id = runner.state().objects[&escape_id].card_id;

    // Move the escape creature to the graveyard.
    engine::game::zones::move_to_zone(
        runner.state_mut(),
        escape_id,
        Zone::Graveyard,
        &mut Vec::new(),
    );

    // Two OTHER graveyard cards to pay the graveyard exile clause.
    let mut filler = Vec::new();
    for i in 0..2 {
        let card_id = CardId(runner.state().next_object_id);
        let id = engine::game::zones::create_object(
            runner.state_mut(),
            card_id,
            P0,
            format!("GY Filler {i}"),
            Zone::Graveyard,
        );
        filler.push(id);
    }

    // Cast from the graveyard.
    let result = runner
        .act(GameAction::CastSpell {
            object_id: escape_id,
            card_id: escape_card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("CastSpell should succeed");

    // FIRST: the battlefield land-exile clause (ExilePermanent), mandatory count 1.
    let WaitingFor::PayCost {
        kind: PayCostKind::ExilePermanent { .. },
        choices: ref land_choices,
        count,
        min_count,
        ..
    } = result.waiting_for
    else {
        panic!(
            "Expected PayCost ExilePermanent (land clause) first, got {:?}",
            result.waiting_for
        );
    };
    assert_eq!(count, 1, "land-exile clause is count 1");
    assert_eq!(min_count, 1, "land-exile clause is mandatory");
    assert!(
        land_choices.contains(&cost_land),
        "the controlled land must be an eligible choice"
    );
    assert!(
        !land_choices.contains(&escape_id),
        "the spell being cast must not be exilable as its own cost"
    );

    // Select the land to exile.
    let result2 = runner
        .act(GameAction::SelectCards {
            cards: vec![cost_land],
        })
        .expect("land exile selection should succeed");

    // SECOND: the graveyard-exile clause (ExileFromZone{Graveyard}), count 2.
    assert!(
        matches!(
            result2.waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::ExileFromZone {
                    zone: ExileCostSourceZone::Graveyard,
                },
                count: 2,
                ..
            }
        ),
        "Expected PayCost ExileFromZone(Graveyard) after land exile, got {:?}",
        result2.waiting_for
    );

    // Select the two graveyard cards.
    let result3 = runner
        .act(GameAction::SelectCards {
            cards: vec![filler[0], filler[1]],
        })
        .expect("graveyard exile selection should succeed");

    // Mana auto-taps {G}; the spell goes to the stack.
    assert!(
        matches!(result3.waiting_for, WaitingFor::Priority { .. }),
        "Expected Priority after both exile clauses, got {:?}",
        result3.waiting_for
    );
    assert_eq!(runner.state().objects[&cost_land].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&filler[0]].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&filler[1]].zone, Zone::Exile);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "escape spell should be on the stack"
    );
}

/// CR 702.138a: a multi-clause escape is NOT castable when the player controls
/// no land to pay the "Exile a land you control" clause, even with enough
/// graveyard cards for the graveyard clause.
#[test]
fn escape_multi_clause_not_castable_without_land() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // No lands at all on the battlefield — the "Exile a land you control" clause
    // is unpayable. The escape mana sub-cost is free (NoCost) so the only
    // affordability failure is the land-exile clause itself.

    let land_you_control = TargetFilter::Typed(TypedFilter {
        type_filters: vec![TypeFilter::Land],
        controller: Some(engine::types::ability::ControllerRef::You),
        properties: vec![],
    });

    let escape_id = scenario
        .add_creature_to_hand(P0, "Multi Escape NoLand", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        })
        .with_keyword(Keyword::Escape(EscapeCost::NonMana(
            AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Mana {
                        cost: ManaCost::NoCost,
                    },
                    AbilityCost::Exile {
                        count: 1,
                        zone: None,
                        filter: Some(land_you_control),
                    },
                    AbilityCost::Exile {
                        count: 2,
                        zone: Some(Zone::Graveyard),
                        filter: None,
                    },
                ],
            },
        )))
        .id();

    let mut runner = scenario.build();
    engine::game::zones::move_to_zone(
        runner.state_mut(),
        escape_id,
        Zone::Graveyard,
        &mut Vec::new(),
    );
    // Two OTHER graveyard cards (graveyard clause is satisfiable; only the land clause fails).
    for i in 0..2 {
        let card_id = CardId(runner.state().next_object_id);
        engine::game::zones::create_object(
            runner.state_mut(),
            card_id,
            P0,
            format!("GY Filler {i}"),
            Zone::Graveyard,
        );
    }

    // The player controls no land to exile for the land-exile clause, so the
    // escape additional cost is not payable (CR 601.2h: all costs must be
    // payable). The affordability gate must reject it from the castable set.
    let castable = engine::game::casting::spell_objects_available_to_cast(runner.state(), P0);
    assert!(
        !castable.contains(&escape_id),
        "escape must NOT be castable when the player controls no land to exile"
    );
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
        .with_keyword(Keyword::Escape(EscapeCost::NonMana(
            AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Mana {
                        cost: ManaCost::Cost {
                            shards: vec![ManaCostShard::X, ManaCostShard::Green],
                            generic: 0,
                        },
                    },
                    AbilityCost::Exile {
                        count: 2,
                        zone: Some(Zone::Graveyard),
                        filter: None,
                    },
                ],
            },
        )))
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

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("CastSpell should succeed");

    // Should prompt for exile selection
    assert!(matches!(
        result.waiting_for,
        WaitingFor::PayCost {
            kind: PayCostKind::ExileFromZone {
                zone: ExileCostSourceZone::Graveyard,
            },
            ..
        }
    ));

    // Select exile targets
    if let WaitingFor::PayCost {
        kind:
            PayCostKind::ExileFromZone {
                zone: ExileCostSourceZone::Graveyard,
            },
        choices: ref cards,
        ..
    } = result.waiting_for
    {
        runner
            .act(GameAction::SelectCards {
                cards: cards[..2].to_vec(),
            })
            .expect("Exile selection should succeed");
    }

    // CR 107.1b + CR 601.2f: X costs divert to ChooseXValue before mana payment.
    // The escape casting variant must be preserved through that diversion so the
    // subsequent ManaPayment step knows it is still an escape cast.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ChooseXValue { .. }),
        "Expected ChooseXValue for X-cost escape after exile selection, got {:?}",
        runner.state().waiting_for
    );

    let pending_after_exile = runner
        .state()
        .pending_cast
        .as_ref()
        .expect("pending_cast should exist during ChooseXValue");
    assert_eq!(
        pending_after_exile.casting_variant,
        CastingVariant::Escape,
        "CastingVariant::Escape must survive into ChooseXValue"
    );

    runner
        .act(GameAction::ChooseX { value: 1 })
        .expect("ChooseX should auto-pay and land the spell on the stack");

    // With auto-pay, the concretized `{1}{B}{B}` cost (no hybrid/Phyrexian) is
    // classified as Unambiguous and `ManaPayment` is skipped entirely. The
    // CastingVariant::Escape must still survive all the way into the stack entry.
    let state = runner.state();
    assert_eq!(state.stack.len(), 1, "spell on stack after auto-pay");
    match &state.stack[0].kind {
        engine::types::game_state::StackEntryKind::Spell {
            casting_variant, ..
        } => assert_eq!(
            *casting_variant,
            CastingVariant::Escape,
            "CastingVariant::Escape must survive auto-finalization onto the stack"
        ),
        other => panic!("expected StackEntryKind::Spell, got {other:?}"),
    }
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

            payment_mode: CastPaymentMode::Auto,
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

// ── Exile-from-hand alternative-cost tests (Force of Will family) ───────────

/// Helper: set up an instant in P0's hand with `AdditionalCost::Required(Exile
/// {1, Hand, blue card filter})`. Mirrors the runtime shape of pitch alternatives
/// (Force of Will, Force of Negation, Misdirection, Unmask, Mindbreak Trap, …)
/// — the spell being cast must NOT appear in the eligible list, and only blue
/// cards in hand are eligible.
fn setup_pitch_scenario() -> (
    engine::game::scenario::GameRunner,
    CardId,
    ObjectId,
    ObjectId, // eligible blue filler in hand
    ObjectId, // ineligible non-blue filler in hand
) {
    use engine::types::ability::{FilterProp, TypeFilter, TypedFilter};

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let pitch_filter = TargetFilter::Typed(TypedFilter {
        type_filters: vec![TypeFilter::Card],
        controller: Some(engine::types::ability::ControllerRef::You),
        properties: vec![FilterProp::HasColor {
            color: ManaColor::Blue,
        }],
    });

    // The pitch spell itself: zero mana cost, Required Exile-from-hand cost.
    // Build as a "creature" placeholder then morph to instant — same trick used
    // by `optional_cost_skipped_clears_flag`.
    let spell_id = scenario
        .add_creature_to_hand(P0, "Pitch Counter", 0, 0)
        .as_instant()
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 0,
        })
        .with_ability(Effect::Counter {
            target: TargetFilter::Any,
            source_rider: None,
            countered_spell_zone: None,
        })
        .with_additional_cost(AdditionalCost::Required(AbilityCost::Exile {
            count: 1,
            zone: Some(engine::types::zones::Zone::Hand),
            filter: Some(pitch_filter),
        }))
        .id();

    // Eligible blue filler card in hand.
    let blue_id = scenario
        .add_creature_to_hand(P0, "Blue Filler", 1, 1)
        .as_instant()
        .id();
    // Ineligible red filler card in hand.
    let red_id = scenario
        .add_creature_to_hand(P0, "Red Filler", 1, 1)
        .as_instant()
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell_id].card_id;

    // Tag colors on the runtime objects: pitch spell itself is blue (so the
    // self-exclusion guard inside `find_eligible_exile_for_cost_targets` is
    // exercised end-to-end), `blue_id` is blue (eligible), `red_id` is red.
    {
        let s = runner.state_mut();
        for &id in &[spell_id, blue_id] {
            let obj = s.objects.get_mut(&id).unwrap();
            obj.color.push(ManaColor::Blue);
            obj.base_color.push(ManaColor::Blue);
        }
        let red = s.objects.get_mut(&red_id).unwrap();
        red.color.push(ManaColor::Red);
        red.base_color.push(ManaColor::Red);
    }

    (runner, card_id, spell_id, blue_id, red_id)
}

/// CR 118.9a + CR 601.2b + CR 601.2h: Full pitch flow — `CastSpell` →
/// `ExileForCost` (Hand) → `SelectCards` → spell on stack with the chosen card
/// exiled. Mirrors the escape-cost integration test (`escape_full_casting_flow`)
/// for the hand-source variant.
#[test]
fn pitch_full_casting_flow() {
    let (mut runner, card_id, spell_id, blue_id, red_id) = setup_pitch_scenario();

    let result = runner
        .act(GameAction::CastSpell {
            object_id: spell_id,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("CastSpell should succeed");

    // Counter target a player to advance past TargetSelection.
    let result = if matches!(result.waiting_for, WaitingFor::TargetSelection { .. }) {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Player(P1)],
            })
            .expect("target selection should succeed")
    } else {
        result
    };

    let eligible = match &result.waiting_for {
        WaitingFor::PayCost {
            kind:
                PayCostKind::ExileFromZone {
                    zone: ExileCostSourceZone::Hand,
                },
            choices: cards,
            count,
            player,
            ..
        } => {
            assert_eq!(*player, P0);
            assert_eq!(*count, 1);
            cards.clone()
        }
        other => panic!("expected PayCost ExileFromZone (Hand), got {other:?}"),
    };
    assert!(
        !eligible.contains(&spell_id),
        "the spell being cast must never appear in its own eligible list"
    );
    assert!(
        eligible.contains(&blue_id),
        "blue card in hand must be eligible: {eligible:?}"
    );
    assert!(
        !eligible.contains(&red_id),
        "non-blue card in hand must be filtered out: {eligible:?}"
    );

    let result2 = runner
        .act(GameAction::SelectCards {
            cards: vec![blue_id],
        })
        .expect("SelectCards for pitch cost should succeed");

    // Zero mana cost + cost paid → spell lands on stack and we return to Priority.
    assert!(
        matches!(result2.waiting_for, WaitingFor::Priority { .. }),
        "expected Priority after pitch payment, got {:?}",
        result2.waiting_for
    );

    // Blue card was exiled, spell is on the stack.
    assert_eq!(
        runner.state().objects[&blue_id].zone,
        engine::types::zones::Zone::Exile,
        "pitched card must be in exile"
    );
    assert_eq!(
        runner.state().objects[&red_id].zone,
        engine::types::zones::Zone::Hand,
        "non-pitched card must remain in hand"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "pitch spell should be on the stack"
    );
}

/// CR 601.2i: `CancelCast` from `ExileForCost` (Hand) rolls the cast back —
/// no cards exiled, spell back in hand, priority restored.
#[test]
fn pitch_cancel_returns_to_priority() {
    let (mut runner, card_id, spell_id, _blue_id, _red_id) = setup_pitch_scenario();

    let cast_result = runner
        .act(GameAction::CastSpell {
            object_id: spell_id,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("CastSpell should succeed");

    // Advance past TargetSelection so we cancel from ExileForCost (Hand).
    if matches!(cast_result.waiting_for, WaitingFor::TargetSelection { .. }) {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Player(P1)],
            })
            .expect("target selection should succeed");
    }

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::ExileFromZone {
                    zone: ExileCostSourceZone::Hand,
                },
                ..
            }
        ),
        "expected PayCost ExileFromZone (Hand) before cancel, got {:?}",
        runner.state().waiting_for
    );

    let result = runner
        .act(GameAction::CancelCast)
        .expect("CancelCast should succeed");

    assert!(
        matches!(result.waiting_for, WaitingFor::Priority { .. }),
        "expected Priority after cancel, got {:?}",
        result.waiting_for
    );

    // No cards exiled. Spell back in (or never left) the caster's hand.
    let hand = &runner.state().players[P0.0 as usize].hand;
    assert!(
        hand.iter().any(|&id| id == spell_id),
        "spell must be back in hand after CancelCast"
    );
    assert!(
        runner
            .state()
            .objects
            .values()
            .filter(|o| o.zone == engine::types::zones::Zone::Exile)
            .count()
            == 0,
        "no cards may be exiled when CancelCast unwinds the pitch cost"
    );
    assert!(
        runner.state().stack.is_empty(),
        "stack must be empty after CancelCast"
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

        payment_mode: CastPaymentMode::Auto,
    });

    assert!(
        result.is_ok(),
        "Spell from hand should NOT be taxed by zone-scoped RaiseCost — got: {:?}",
        result.err(),
    );
}

// --- Graveyard land play permission tests ---

use engine::types::ability::{CardPlayMode, StaticDefinition, TypeFilter, TypedFilter};
use engine::types::card_type::CoreType;
use engine::types::statics::{CastFreeOrigin, CastFrequency, StaticMode};

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
                frequency: CastFrequency::Unlimited,
                play_mode: CardPlayMode::Play,
                graveyard_destination_replacement: None,
                extra_cost: None,
                enters_with_counter: None,
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
                frequency: CastFrequency::Unlimited,
                play_mode: CardPlayMode::Play,
                graveyard_destination_replacement: None,
                extra_cost: None,
                enters_with_counter: None,
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

// --- Muldrotha-class once-per-turn-per-permanent-type tests (CR 110.4) ---

/// CR 110.4 + CR 305.1 + CR 601.2a: Muldrotha grants `OncePerTurnPerPermanentType`
/// graveyard play permission for lands. Playing a land from graveyard
/// consumes the `(source, Land)` slot, blocking a second land play from the
/// same source even when an additional land drop is available.
#[test]
fn muldrotha_per_permanent_type_blocks_second_land_from_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Source has Muldrotha-class permission (per-permanent-type per turn).
    let _source_id = scenario
        .add_creature(P0, "Muldrotha, the Gravetide", 0, 0)
        .with_static_definition(
            StaticDefinition::new(StaticMode::GraveyardCastPermission {
                frequency: CastFrequency::OncePerTurnPerPermanentType,
                play_mode: CardPlayMode::Play,
                graveyard_destination_replacement: None,
                extra_cost: None,
                enters_with_counter: None,
            })
            .affected(TargetFilter::Typed(
                engine::types::ability::TypedFilter::new(TypeFilter::Permanent),
            )),
        )
        .id();

    // Grant an additional land drop so the per-turn land cap doesn't mask
    // the per-permanent-type-slot enforcement.
    let _explore = scenario
        .add_creature(P0, "Exploration", 0, 0)
        .with_static_definition(
            StaticDefinition::new(StaticMode::AdditionalLandDrop { count: 1 })
                .affected(TargetFilter::Player),
        )
        .id();

    let mut runner = scenario.build();

    // Two lands in P0's graveyard.
    let forest_id = engine::game::zones::create_object(
        runner.state_mut(),
        engine::types::identifiers::CardId(101),
        P0,
        "Forest".to_string(),
        Zone::Graveyard,
    );
    let swamp_id = engine::game::zones::create_object(
        runner.state_mut(),
        engine::types::identifiers::CardId(102),
        P0,
        "Swamp".to_string(),
        Zone::Graveyard,
    );
    for id in [forest_id, swamp_id] {
        let obj = runner.state_mut().objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.base_card_types = obj.card_types.clone();
    }

    let forest_card_id = runner.state().objects[&forest_id].card_id;
    let swamp_card_id = runner.state().objects[&swamp_id].card_id;

    // First land play succeeds (consumes the (source, Land) slot).
    runner
        .act(GameAction::PlayLand {
            object_id: forest_id,
            card_id: forest_card_id,
        })
        .expect("first land play from graveyard should succeed");
    assert!(runner.state().battlefield.contains(&forest_id));

    // Second land play from same source must fail — Land slot consumed.
    let result = runner.act(GameAction::PlayLand {
        object_id: swamp_id,
        card_id: swamp_card_id,
    });
    assert!(
        result.is_err(),
        "second land play from same Muldrotha source must be blocked by per-permanent-type slot"
    );
}

/// CR 110.4: Each permanent-type slot is independent — playing a land does
/// not consume the creature/artifact/etc. slot. After the per-turn slot is
/// cleared (turn cycle), the source can play another land.
#[test]
fn muldrotha_per_permanent_type_resets_at_turn_start() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let source_id = scenario
        .add_creature(P0, "Muldrotha, the Gravetide", 0, 0)
        .with_static_definition(
            StaticDefinition::new(StaticMode::GraveyardCastPermission {
                frequency: CastFrequency::OncePerTurnPerPermanentType,
                play_mode: CardPlayMode::Play,
                graveyard_destination_replacement: None,
                extra_cost: None,
                enters_with_counter: None,
            })
            .affected(TargetFilter::Typed(
                engine::types::ability::TypedFilter::new(TypeFilter::Permanent),
            )),
        )
        .id();

    let mut runner = scenario.build();

    // Pre-populate the per-type used-set as if the Land slot was already
    // consumed this turn.
    runner
        .state_mut()
        .graveyard_cast_permissions_used_per_type
        .insert((source_id, CoreType::Land));
    assert!(runner
        .state()
        .graveyard_cast_permissions_used_per_type
        .contains(&(source_id, CoreType::Land)));

    // Trigger turn cleanup directly (mirror the start_next_turn path used by
    // production turns.rs).
    engine::game::turns::start_next_turn(runner.state_mut(), &mut Vec::new());

    // CR 500.1 + CR 514: per-turn slot trackers reset for the incoming turn
    // (analogous to other once-per-turn counters cleared in `start_next_turn`).
    assert!(
        runner
            .state()
            .graveyard_cast_permissions_used_per_type
            .is_empty(),
        "per-permanent-type used-set must clear on turn start"
    );
}

// ── CR 601.2b: Cost-payability pre-gate ─────────────────────────────────────

/// CR 601.2b: An optional additional cost that requires a choice of object
/// skips the OptionalCostChoice prompt entirely when no legal object exists.
/// The spell proceeds as if the player declined to pay.
#[test]
fn optional_blight_with_no_creatures_skips_prompt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::White);
    // Deliberately no creatures on the battlefield.

    let spell_id = scenario
        .add_creature_to_hand(P0, "Blight Bolt", 0, 0)
        .as_instant()
        .with_ability(Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 3 },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        })
        .with_additional_cost(AdditionalCost::Optional {
            cost: AbilityCost::Blight { count: 1 },
            repeatability: engine::types::ability::AdditionalCostRepeatability::Once,
        })
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell_id].card_id;

    let result = runner
        .act(GameAction::CastSpell {
            object_id: spell_id,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast should succeed");

    handle_target_selection(&mut runner, &result);

    // CR 601.2b: Prompt is bypassed when the optional cost is unpayable.
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalCostChoice { .. }
        ),
        "optional cost prompt must be skipped when unpayable, got {:?}",
        runner.state().waiting_for,
    );

    // additional_cost_paid remains false since the cost was not paid.
    assert!(
        !top_stack_cost_paid(&runner),
        "additional_cost_paid must be false when optional cost is auto-skipped"
    );
}

/// CR 601.2b: A required additional cost that requires a choice of object
/// makes the spell uncastable when no legal object exists.
#[test]
fn required_blight_with_no_creatures_rejects_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::White);
    // No creatures on the battlefield.

    let spell_id = scenario
        .add_creature_to_hand(P0, "Required Blight Bolt", 0, 0)
        .as_instant()
        .with_ability(Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 3 },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        })
        .with_additional_cost(AdditionalCost::Required(AbilityCost::Blight { count: 1 }))
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell_id].card_id;

    let first = runner.act(GameAction::CastSpell {
        object_id: spell_id,
        card_id,
        targets: vec![],

        payment_mode: CastPaymentMode::Auto,
    });

    // CastSpell may enter TargetSelection first. The gate fires once the
    // required cost is about to be paid — either at CastSpell time if no
    // targets are required, or at SelectTargets time.
    let final_result = match first {
        Err(_) => first,
        Ok(res) if matches!(res.waiting_for, WaitingFor::TargetSelection { .. }) => {
            runner.act(GameAction::SelectTargets {
                targets: vec![TargetRef::Player(P1)],
            })
        }
        other => other,
    };

    assert!(
        final_result.is_err(),
        "cast must fail when required additional cost is unpayable, got {:?}",
        final_result
    );
}

/// CR 601.2b: When an `AdditionalCost::Choice(A, B)` has an unpayable
/// preferred cost A, the fallback B is applied automatically with no prompt.
#[test]
fn choice_cost_falls_through_when_preferred_unpayable() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::White);
    // No creatures — blight half is unpayable — but life is available.

    let spell_id = scenario
        .add_creature_to_hand(P0, "Choice Bolt", 0, 0)
        .as_instant()
        .with_ability(Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 3 },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        })
        .with_additional_cost(AdditionalCost::Choice(
            AbilityCost::Blight { count: 1 },
            AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 2 },
            },
        ))
        .id();

    let mut runner = scenario.build();
    let life_before = runner.state().players[0].life;
    let card_id = runner.state().objects[&spell_id].card_id;

    let result = runner
        .act(GameAction::CastSpell {
            object_id: spell_id,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast should succeed");

    handle_target_selection(&mut runner, &result);

    // CR 601.2b: No prompt; fallback was applied automatically.
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalCostChoice { .. }
        ),
        "no prompt expected when preferred cost is unpayable and fallback applies, got {:?}",
        runner.state().waiting_for,
    );
    assert_eq!(
        runner.state().players[0].life,
        life_before - 2,
        "fallback life cost should have been paid"
    );
}

// --- CastFromHandFree { OncePerTurn } tests (Zaffai and the Tempests) ---

/// CR 601.2b + CR 118.9a: Zaffai's once-per-turn permission emits a
/// `CastSpellForFree` candidate for a matching hand spell. Casting via it
/// consumes the source's slot and finalizes the spell on the stack with
/// `CastingVariant::HandPermission`.
#[test]
fn zaffai_once_per_turn_hand_free_casts_with_no_mana() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Zaffai-equivalent permission: "once during each of your turns, you may cast
    // an instant or sorcery spell from your hand without paying its mana cost".
    let source_id = scenario
        .add_creature(P0, "Zaffai, Thunder Conductor", 0, 0)
        .with_static_definition(
            StaticDefinition::new(StaticMode::CastFromHandFree {
                frequency: CastFrequency::OncePerTurn,
                origin: CastFreeOrigin::Hand,
            })
            .affected(TargetFilter::Typed(TypedFilter::new(TypeFilter::Instant))),
        )
        .id();
    let bolt_id = scenario.add_bolt_to_hand(P0);

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&bolt_id].card_id;
    let mana_before = runner.state().players[0].mana_pool.clone();

    // Legal-actions must surface a `CastSpellForFree` candidate for (bolt, Zaffai).
    let actions = engine::ai_support::legal_actions(runner.state());
    let found = actions.iter().any(|a| {
        matches!(
            a,
            GameAction::CastSpellForFree {
                object_id,
                source_id: src,
                ..
            } if *object_id == bolt_id && *src == source_id
        )
    });
    assert!(
        found,
        "CastSpellForFree should appear in legal_actions for a matching hand spell"
    );

    let result = runner
        .act(GameAction::CastSpellForFree {
            object_id: bolt_id,
            card_id,
            source_id,

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("CastSpellForFree should succeed");

    // Bolt requires target selection (Any) — resolve it and finalize.
    if let WaitingFor::TargetSelection { .. } = &result.waiting_for {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Player(P1)],
            })
            .expect("target selection should succeed");
    }

    // Bolt should now be on the stack.
    assert_eq!(runner.state().stack.len(), 1, "bolt should be on the stack");
    // CastingVariant::HandPermission must be recorded on the stack entry.
    let entry = runner.state().stack.last().unwrap();
    match &entry.kind {
        StackEntryKind::Spell {
            casting_variant, ..
        } => {
            assert!(
                matches!(
                    casting_variant,
                    CastingVariant::HandPermission { source, frequency }
                        if *source == source_id && *frequency == CastFrequency::OncePerTurn
                ),
                "stack entry variant = {casting_variant:?}",
            );
        }
        other => panic!("expected Spell on stack, got {other:?}"),
    }
    // CR 118.9a: No mana was paid.
    assert_eq!(
        runner.state().players[0].mana_pool,
        mana_before,
        "no mana should have been paid"
    );
    // CR 601.2b: Source's once-per-turn slot is consumed.
    assert!(
        runner
            .state()
            .hand_cast_free_permissions_used
            .contains(&source_id),
        "source should be recorded as used"
    );
}

/// CR 601.2b + CR 400.7: After the once-per-turn slot is consumed, no further
/// `CastSpellForFree` candidate is emitted this turn.
#[test]
fn zaffai_second_cast_is_suppressed_same_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let source_id = scenario
        .add_creature(P0, "Zaffai, Thunder Conductor", 0, 0)
        .with_static_definition(
            StaticDefinition::new(StaticMode::CastFromHandFree {
                frequency: CastFrequency::OncePerTurn,
                origin: CastFreeOrigin::Hand,
            })
            .affected(TargetFilter::Typed(TypedFilter::new(TypeFilter::Instant))),
        )
        .id();
    let _bolt_id = scenario.add_bolt_to_hand(P0);

    let mut runner = scenario.build();
    // Mark the source as already used this turn.
    runner
        .state_mut()
        .hand_cast_free_permissions_used
        .insert(source_id);

    let actions = engine::ai_support::legal_actions(runner.state());
    let found = actions
        .iter()
        .any(|a| matches!(a, GameAction::CastSpellForFree { .. }));
    assert!(
        !found,
        "consumed once-per-turn slot must suppress further CastSpellForFree candidates"
    );
}

/// CR 601.2b + CR 118.9a: When multiple once-per-turn sources admit the same
/// hand spell, the selected `CastSpellForFree` action must validate that named
/// source directly rather than re-deriving the first matching source.
#[test]
fn cast_spell_for_free_uses_the_named_permission_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let first_source = scenario
        .add_creature(P0, "First Zaffai Stand-In", 0, 0)
        .with_static_definition(
            StaticDefinition::new(StaticMode::CastFromHandFree {
                frequency: CastFrequency::OncePerTurn,
                origin: CastFreeOrigin::Hand,
            })
            .affected(TargetFilter::Typed(TypedFilter::new(TypeFilter::Instant))),
        )
        .id();
    let second_source = scenario
        .add_creature(P0, "Second Zaffai Stand-In", 0, 0)
        .with_static_definition(
            StaticDefinition::new(StaticMode::CastFromHandFree {
                frequency: CastFrequency::OncePerTurn,
                origin: CastFreeOrigin::Hand,
            })
            .affected(TargetFilter::Typed(TypedFilter::new(TypeFilter::Instant))),
        )
        .id();
    let bolt_id = scenario.add_bolt_to_hand(P0);

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&bolt_id].card_id;
    let actions = engine::ai_support::legal_actions(runner.state());
    assert!(
        actions.iter().any(|a| matches!(
            a,
            GameAction::CastSpellForFree {
                object_id,
                source_id,
                ..
            } if *object_id == bolt_id && *source_id == first_source
        )),
        "first source should be advertised"
    );
    assert!(
        actions.iter().any(|a| matches!(
            a,
            GameAction::CastSpellForFree {
                object_id,
                source_id,
                ..
            } if *object_id == bolt_id && *source_id == second_source
        )),
        "second source should be advertised"
    );

    let result = runner
        .act(GameAction::CastSpellForFree {
            object_id: bolt_id,
            card_id,
            source_id: second_source,

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("selected second source should authorize the free cast");
    handle_target_selection(&mut runner, &result);

    assert!(
        runner
            .state()
            .hand_cast_free_permissions_used
            .contains(&second_source),
        "selected source should be the consumed source"
    );
    assert!(
        !runner
            .state()
            .hand_cast_free_permissions_used
            .contains(&first_source),
        "earlier matching source must not be consumed"
    );
}

fn add_expensive_dragon_commander(scenario: &mut GameScenario) -> ObjectId {
    let commander_id = scenario
        .add_creature_to_hand(P0, "Niv-Mizzet, Dragon Commander", 5, 5)
        .with_subtypes(vec!["Dragon"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![
                ManaCostShard::Blue,
                ManaCostShard::Blue,
                ManaCostShard::Red,
                ManaCostShard::Red,
            ],
            generic: 2,
        })
        .id();
    scenario.with_commander(commander_id);
    commander_id
}

/// CR 114.4 + CR 601.2b + CR 118.9a (issue #1355): Tamiyo, Field Researcher's
/// emblem functions from the command zone and waives mana for hand spells.
#[test]
fn tamiyo_emblem_allows_free_cast_from_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source_id = scenario
        .add_creature(P0, "Tamiyo, Field Researcher", 0, 0)
        .id();
    let bolt_id = scenario.add_bolt_to_hand(P0);

    let mut runner = scenario.build();
    let emblem_static = engine::parser::oracle_static::parse_static_line(
        "You may cast spells from your hand without paying their mana costs.",
    )
    .expect("Tamiyo emblem static should parse");
    let ability = engine::types::ability::ResolvedAbility::new(
        Effect::CreateEmblem {
            statics: vec![emblem_static],
            triggers: Vec::new(),
        },
        vec![],
        source_id,
        P0,
    );
    let mut events = Vec::<GameEvent>::new();
    engine::game::effects::create_emblem::resolve(runner.state_mut(), &ability, &mut events)
        .expect("Tamiyo emblem should be created");
    let emblem_id = *runner
        .state()
        .command_zone
        .last()
        .expect("CreateEmblem should put an emblem in the command zone");
    assert!(runner.state().objects[&emblem_id].is_emblem);

    let card_id = runner.state().objects[&bolt_id].card_id;
    let mana_before = runner.state().players[0].mana_pool.clone();

    let result = runner
        .act(GameAction::CastSpell {
            object_id: bolt_id,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Tamiyo emblem should allow casting a hand spell");
    handle_target_selection(&mut runner, &result);

    assert_eq!(
        runner.state().stack.len(),
        1,
        "bolt should be on the stack after a free cast"
    );
    assert_eq!(
        runner.state().players[0].mana_pool,
        mana_before,
        "no mana should have been paid under Tamiyo emblem"
    );
}

/// CR 601.2a + CR 118.9a + CR 903.8: A hand-qualified free-cast static
/// (Omniscience class) does not replace the mana cost for a commander cast from
/// the command zone.
#[test]
fn hand_only_free_cast_source_does_not_apply_to_command_zone_commander() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Omniscience Stand-In", 0, 0)
        .with_static_definition(
            StaticDefinition::new(StaticMode::CastFromHandFree {
                frequency: CastFrequency::Unlimited,
                origin: CastFreeOrigin::Hand,
            })
            .affected(TargetFilter::Any),
        );
    let commander_id = add_expensive_dragon_commander(&mut scenario);

    let mut runner = scenario.build();
    runner.state_mut().format_config.command_zone = true;
    let card_id = runner.state().objects[&commander_id].card_id;

    assert!(
        runner
            .act(GameAction::CastSpell {
                object_id: commander_id,
                card_id,
                targets: vec![],

                payment_mode: CastPaymentMode::Auto,
            })
            .is_err(),
        "hand-only free-cast source must not waive a command-zone commander's mana cost"
    );
}

/// CR 601.2a + CR 118.9a + CR 903.8: An unqualified free-cast static
/// (Dracogenesis class) applies to a Dragon commander that is already castable
/// from the command zone. A hand-only source that appears earlier on the
/// battlefield must not mask the later command-zone-capable source.
#[test]
fn unqualified_free_cast_source_applies_to_dragon_commander_after_hand_only_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature(P0, "Omniscience Stand-In", 0, 0)
        .with_static_definition(
            StaticDefinition::new(StaticMode::CastFromHandFree {
                frequency: CastFrequency::Unlimited,
                origin: CastFreeOrigin::Hand,
            })
            .affected(TargetFilter::Any),
        );
    scenario
        .add_creature(P0, "Dracogenesis Stand-In", 0, 0)
        .with_static_definition(
            StaticDefinition::new(StaticMode::CastFromHandFree {
                frequency: CastFrequency::Unlimited,
                origin: CastFreeOrigin::DefaultCastPermission,
            })
            .affected(TargetFilter::Typed(TypedFilter::new(TypeFilter::Subtype(
                "Dragon".to_string(),
            )))),
        );

    let commander_id = add_expensive_dragon_commander(&mut scenario);

    let mut runner = scenario.build();
    runner.state_mut().format_config.command_zone = true;
    let card_id = runner.state().objects[&commander_id].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: commander_id,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Dragon commander should cast without mana through Dracogenesis-class source");

    assert_eq!(
        runner.state().stack.len(),
        1,
        "Dragon commander should be on the stack after the free cast"
    );
}

// --- Miracle tests (CR 702.94a + CR 603.11) ---

/// CR 702.94a: A card with `Keyword::Miracle(cost)` drawn as the first card of
/// the turn surfaces `WaitingFor::MiracleReveal` once priority is entered.
#[test]
fn miracle_first_draw_surfaces_reveal_prompt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Give P0 {W} available to pay the miracle cost.
    scenario.add_basic_land(P0, ManaColor::White);

    // Put a miracle spell in P0's library as the top card, with an effect that
    // has no targets (DrawCards N) so resolution doesn't need target selection.
    let miracle_obj = scenario
        .add_spell_to_library_top(P0, "TestMiracleDraw", false)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 5,
        })
        .with_keyword(Keyword::Miracle(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        }))
        .with_ability(Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        })
        .id();

    let mut runner = scenario.build();

    // Tap a mana source so {W} is in pool.
    runner.state_mut().players[0]
        .mana_pool
        .add(engine::types::mana::ManaUnit::new(
            engine::types::mana::ManaType::White,
            ObjectId(0),
            false,
            Vec::new(),
        ));

    // Drive a draw via a direct effect on the pipeline:
    // the simplest path is to synthesize a Draw effect resolution.
    let mut events = Vec::new();
    let draw_ability = engine::types::ability::ResolvedAbility::new(
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
        Vec::new(),
        miracle_obj,
        P0,
    );
    engine::game::effects::draw::resolve(runner.state_mut(), &draw_ability, &mut events)
        .expect("draw should succeed");

    // Miracle offer should be queued.
    assert_eq!(
        runner.state().pending_miracle_offers.len(),
        1,
        "miracle offer should be queued after first draw"
    );
    let offer = &runner.state().pending_miracle_offers[0];
    assert_eq!(offer.player, P0);
    assert_eq!(offer.object_id, miracle_obj);
}

/// CR 702.94a: Declining a miracle reveal via `DecideOptionalEffect { accept: false }`
/// consumes the offer and returns control to normal priority.
#[test]
fn miracle_decline_returns_to_priority() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let miracle_obj = scenario
        .add_spell_to_hand(P0, "TestMiracle", false)
        .with_keyword(Keyword::Miracle(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        }))
        .id();

    let mut runner = scenario.build();

    // Seed the pending offer directly and set the reveal waiting state.
    runner
        .state_mut()
        .pending_miracle_offers
        .push(engine::types::game_state::MiracleOffer {
            player: P0,
            object_id: miracle_obj,
            cost: ManaCost::Cost {
                shards: vec![ManaCostShard::White],
                generic: 0,
            },
        });
    // Surface the reveal prompt by forcing the state directly — simulating
    // what `flush_pending_miracle_offer` would do.
    runner.state_mut().waiting_for = WaitingFor::MiracleReveal {
        player: P0,
        object_id: miracle_obj,
        cost: ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        },
    };
    // Pop the queue to reflect that the prompt consumed it.
    runner.state_mut().pending_miracle_offers.clear();

    let result = runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("decline should succeed");

    // After decline we should be back at Priority, and no further offers.
    assert!(
        matches!(result.waiting_for, WaitingFor::Priority { .. }),
        "decline should return to Priority, got {:?}",
        result.waiting_for,
    );
    assert!(
        runner.state().pending_miracle_offers.is_empty(),
        "queue should be empty after decline"
    );
    // Card remains in hand — it was not cast.
    assert_eq!(
        runner.state().objects.get(&miracle_obj).map(|o| o.zone),
        Some(Zone::Hand),
    );
}

/// CR 702.94a + CR 118.9a: Accepting the reveal pushes a triggered ability
/// on the stack. When that trigger resolves, the player casts the spell for
/// the miracle cost via `CastingVariant::Miracle`, bypassing timing restrictions
/// (CR 608.2g). The printed cost is ignored.
#[test]
fn miracle_accept_casts_for_miracle_cost() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::White);

    let miracle_obj = scenario
        .add_spell_to_hand(P0, "TestMiracle", false)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 99,
        })
        .with_keyword(Keyword::Miracle(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        }))
        .with_ability(Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        })
        .id();

    let mut runner = scenario.build();
    // Tap the land for {W}.
    runner.state_mut().players[0]
        .mana_pool
        .add(engine::types::mana::ManaUnit::new(
            engine::types::mana::ManaType::White,
            ObjectId(0),
            false,
            Vec::new(),
        ));

    let card_id = runner.state().objects[&miracle_obj].card_id;

    // Phase 1: Surface the reveal prompt directly.
    runner.state_mut().waiting_for = WaitingFor::MiracleReveal {
        player: P0,
        object_id: miracle_obj,
        cost: ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        },
    };

    // Accept the reveal — this pushes a triggered ability onto the stack.
    runner
        .act(GameAction::CastSpellAsMiracle {
            object_id: miracle_obj,
            card_id,

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Reveal should succeed");

    // The miracle trigger should be on the stack.
    assert_eq!(
        runner.state().stack.len(),
        1,
        "miracle trigger should be on the stack"
    );
    assert!(
        matches!(
            &runner.state().stack.last().unwrap().kind,
            StackEntryKind::TriggeredAbility { .. }
        ),
        "stack entry should be a TriggeredAbility"
    );

    // Phase 2: Both players pass priority — trigger resolves, presenting the cast offer.
    runner
        .act(GameAction::PassPriority)
        .expect("P0 pass priority");
    runner
        .act(GameAction::PassPriority)
        .expect("P1 pass priority");

    // Should now be MiracleCastOffer.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::CastOffer {
                kind: CastOfferKind::Miracle { .. },
                ..
            }
        ),
        "should be MiracleCastOffer, got {:?}",
        runner.state().waiting_for
    );

    // Phase 3: Accept the cast — the spell goes on the stack with CastingVariant::Miracle.
    runner
        .act(GameAction::CastSpellAsMiracle {
            object_id: miracle_obj,
            card_id,

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Miracle cast should succeed");

    // Stack should have the miracle-cast spell with CastingVariant::Miracle.
    assert_eq!(
        runner.state().stack.len(),
        1,
        "miracle spell should be on the stack"
    );
    let entry = runner.state().stack.last().unwrap();
    match &entry.kind {
        StackEntryKind::Spell {
            casting_variant, ..
        } => {
            assert_eq!(
                *casting_variant,
                CastingVariant::Miracle,
                "stack entry should record CastingVariant::Miracle"
            );
        }
        other => panic!("expected Spell on stack, got {other:?}"),
    }
    // The {W} was paid — pool should be empty.
    assert!(
        runner.state().players[0].mana_pool.mana.is_empty(),
        "miracle cost of {{W}} should have consumed the white mana"
    );
}

/// CR 702.94a + CR 608.2g: A sorcery with Miracle can be cast during the
/// draw step because the cast happens during trigger resolution, bypassing
/// timing restrictions.
#[test]
fn miracle_sorcery_casts_during_draw_step() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Draw);
    scenario.add_basic_land(P0, ManaColor::White);

    let miracle_obj = scenario
        .add_spell_to_hand(P0, "DrawStepMiracle", false)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 99,
        })
        .with_keyword(Keyword::Miracle(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        }))
        .with_ability(Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        })
        .id();

    let mut runner = scenario.build();
    runner.state_mut().players[0]
        .mana_pool
        .add(engine::types::mana::ManaUnit::new(
            engine::types::mana::ManaType::White,
            ObjectId(0),
            false,
            Vec::new(),
        ));

    let card_id = runner.state().objects[&miracle_obj].card_id;

    // Reveal prompt during draw step.
    runner.state_mut().waiting_for = WaitingFor::MiracleReveal {
        player: P0,
        object_id: miracle_obj,
        cost: ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        },
    };

    // Reveal → trigger on stack.
    runner
        .act(GameAction::CastSpellAsMiracle {
            object_id: miracle_obj,
            card_id,

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Reveal should succeed during draw step");

    // Resolve trigger.
    runner.act(GameAction::PassPriority).expect("P0 pass");
    runner.act(GameAction::PassPriority).expect("P1 pass");

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::CastOffer {
                kind: CastOfferKind::Miracle { .. },
                ..
            }
        ),
        "should be MiracleCastOffer during draw step"
    );

    // Cast the sorcery during draw step — should succeed (CR 608.2g bypass).
    runner
        .act(GameAction::CastSpellAsMiracle {
            object_id: miracle_obj,
            card_id,

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Sorcery miracle cast should succeed during draw step (CR 608.2g)");

    // Spell on the stack.
    assert!(
        matches!(
            &runner.state().stack.last().unwrap().kind,
            StackEntryKind::Spell {
                casting_variant: CastingVariant::Miracle,
                ..
            }
        ),
        "sorcery should be on the stack via Miracle variant"
    );
}

/// CR 118.9: Rooftop Storm — "You may pay {0} rather than pay the mana cost for
/// Zombie creature spells you cast." End-to-end: parse the Oracle text onto a
/// battlefield permanent, then casting a Zombie creature offers the alternative
/// {0} cost (CR 118.9 grant), accepting reaches the stack with the alternative
/// paid, while a non-Zombie creature is NOT offered the grant.
#[test]
fn rooftop_storm_grants_alternative_zero_cost_to_zombie_spells() {
    use engine::types::statics::StaticMode;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Rooftop Storm on the battlefield, abilities from Oracle text (full parser
    // path → CastWithAlternativeCost static).
    let storm_id = scenario
        .add_creature(P0, "Rooftop Storm", 0, 0)
        .from_oracle_text(
            "You may pay {0} rather than pay the mana cost for Zombie creature spells you cast.",
        )
        .id();

    // A Zombie creature in hand with a nonzero printed mana cost (so {0} is a
    // meaningful alternative).
    let zombie_id = scenario
        .add_creature_to_hand(P0, "Test Zombie", 2, 2)
        .with_subtypes(vec!["Zombie"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 6,
        })
        .id();

    // A non-Zombie creature in hand — must NOT receive the grant.
    let elf_id = scenario
        .add_creature_to_hand(P0, "Test Elf", 1, 1)
        .with_subtypes(vec!["Elf"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 2,
        })
        .id();

    let mut runner = scenario.build();

    // Regression: the line must parse to a CastWithAlternativeCost static, NOT
    // a free-floating Effect::PayCost ability (the prior misparse).
    {
        use engine::types::ability::Effect;
        let storm = &runner.state().objects[&storm_id];
        assert!(
            storm
                .static_definitions
                .iter_unchecked()
                .any(|d| matches!(d.mode, StaticMode::CastWithAlternativeCost { .. })),
            "Rooftop Storm must carry a CastWithAlternativeCost static"
        );
        assert!(
            !storm
                .abilities
                .iter()
                .any(|a| matches!(*a.effect, Effect::PayCost { .. })),
            "Rooftop Storm must NOT have a free-floating PayCost ability (prior misparse)"
        );
    }

    // --- Zombie: grant offered, accepting reaches the stack. ---
    let zombie_card = runner.state().objects[&zombie_id].card_id;
    let result = runner
        .act(GameAction::CastSpell {
            object_id: zombie_id,
            card_id: zombie_card,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting a Zombie should succeed");
    handle_target_selection(&mut runner, &result);

    match &runner.state().waiting_for {
        WaitingFor::OptionalCostChoice { cost, .. } => match cost {
            AdditionalCost::Choice(alt, printed) => {
                assert_eq!(
                    *alt,
                    AbilityCost::Mana {
                        cost: ManaCost::zero()
                    },
                    "alternative cost must be {{0}} (Rooftop Storm)"
                );
                assert_eq!(
                    *printed,
                    AbilityCost::Mana {
                        cost: ManaCost::Cost {
                            shards: vec![],
                            generic: 6,
                        }
                    },
                    "printed fallback must be the Zombie's {{6}} mana cost"
                );
            }
            other => panic!("expected AdditionalCost::Choice(alt, printed), got {other:?}"),
        },
        other => panic!("expected OptionalCostChoice for the grant, got {other:?}"),
    }

    // Accept the alternative cost → Zombie reaches the stack with {0} paid.
    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("accepting the alternative cost should succeed");
    assert_eq!(
        runner.state().objects[&zombie_id].zone,
        Zone::Stack,
        "Zombie should be on the stack after paying the alternative cost"
    );

    // --- Non-Zombie: grant NOT offered. ---
    // Sanity: the static is present so the negative is meaningful.
    assert!(
        runner.state().objects.values().any(|o| matches!(
            o.static_definitions.first().map(|d| &d.mode),
            Some(StaticMode::CastWithAlternativeCost { .. })
        )),
        "Rooftop Storm must carry a CastWithAlternativeCost static"
    );

    let elf_card = runner.state().objects[&elf_id].card_id;
    let elf_result = runner.act(GameAction::CastSpell {
        object_id: elf_id,
        card_id: elf_card,
        targets: vec![],

        payment_mode: CastPaymentMode::Auto,
    });
    // The Elf has a {2} cost and no mana available, so the cast may fail at
    // payment — but it must NEVER enter the OptionalCostChoice grant prompt.
    if let Ok(elf_result) = elf_result {
        handle_target_selection(&mut runner, &elf_result);
        assert!(
            !matches!(
                runner.state().waiting_for,
                WaitingFor::OptionalCostChoice { .. }
            ),
            "non-Zombie spell must not be offered the Rooftop Storm grant, got {:?}",
            runner.state().waiting_for,
        );
    }
}
