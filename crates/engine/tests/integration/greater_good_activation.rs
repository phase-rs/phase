//! Integration test for issue #340 — Greater Good's `cost_paid_object`
//! snapshot population through the real activation pipeline.
//!
//! Oracle (activated):
//!   "Sacrifice a creature: Draw cards equal to the sacrificed creature's
//!    power, then discard three cards."
//!
//! The draw count is `QuantityExpr::Ref(QuantityRef::Power { scope:
//! ObjectScope::CostPaidObject })`. The existing unit tests in `quantity.rs`
//! construct a `ResolvedAbility` with `cost_paid_object` already populated;
//! they never exercise the population path in `casting_costs.rs`. These tests
//! drive the **real** pipeline:
//!
//!   `GameAction::ActivateAbility` → `WaitingFor::SacrificeForCost`
//!   → `GameAction::SelectCards` → engine snapshots the victim's power into
//!   `cost_paid_object` BEFORE the zone change → resolve → draw N, discard 3.
//!
//! Cases 1-5 drive `apply()` end-to-end. Case 6 is a labelled shape test that
//! documents the singular-slot `chosen.first()` behavior.
//!
//! CR 400.7j + CR 608.2k: the sacrificed object's last-known information is
//! captured before it changes zones, so cost-paid-object property references
//! resolve at ability resolution.
//! CR 701.21: Sacrifice keyword action (verified: docs/MagicCompRules.txt:3443).

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, ObjectScope, QuantityExpr, QuantityRef,
    TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::Zone;

const GREATER_GOOD_TEXT: &str = "Sacrifice a creature: Draw cards equal to the sacrificed \
     creature's power, then discard three cards.";

/// Drive the stack to empty, answering any `WaitingFor::DiscardChoice` prompt
/// (the "discard three cards" sub-ability is an interactive choice resolved by
/// `SelectCards`, not by passing priority). Picks the first `count` legal
/// cards. Bounded loop guards against an unexpected stall.
fn resolve_discards_and_advance(runner: &mut engine::game::scenario::GameRunner) {
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            engine::types::WaitingFor::DiscardChoice { count, cards, .. } => {
                let pick: Vec<_> = cards.into_iter().take(count).collect();
                runner
                    .act(GameAction::SelectCards { cards: pick })
                    .expect("answering the discard choice must succeed");
            }
            engine::types::WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }
}

/// CR 400.7j + CR 608.2k — the headline end-to-end test. Drives the full
/// activation pipeline and PROVES the engine populates the `cost_paid_object`
/// slot from `casting_costs.rs::handle_sacrifice_for_cost`: a 5/5 sacrifice
/// victim yields exactly five drawn cards, then three are discarded.
#[test]
fn greater_good_draws_equal_to_sacrificed_power_then_discards_three() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gg_id = scenario
        .add_creature(P0, "Greater Good", 0, 0)
        .from_oracle_text(GREATER_GOOD_TEXT)
        .as_enchantment()
        .id();
    let beast_id = scenario.add_creature(P0, "Beast", 5, 5).id();
    scenario.with_library_top(P0, &["L1", "L2", "L3", "L4", "L5", "L6"]);
    scenario.with_cards_in_hand(P0, &["H1", "H2", "H3"]);

    let mut runner = scenario.build();

    // `as_enchantment()` must strip the Creature core type — the host is a
    // proper Enchantment, not a creature.
    let gg_types = &runner.state().objects[&gg_id].card_types.core_types;
    assert!(
        !gg_types.contains(&CoreType::Creature),
        "as_enchantment() must strip the Creature core type from the host",
    );
    assert!(
        gg_types.contains(&CoreType::Enchantment),
        "the Greater Good host must be an Enchantment",
    );

    let lib_before = runner.state().players[P0.0 as usize].library.len();

    runner
        .act(GameAction::ActivateAbility {
            source_id: gg_id,
            ability_index: 0,
        })
        .expect("activating Greater Good must succeed");
    assert_eq!(
        runner.waiting_for_kind(),
        "SacrificeForCost",
        "activating a Sacrifice-cost ability must prompt for the sacrifice",
    );

    runner
        .act(GameAction::SelectCards {
            cards: vec![beast_id],
        })
        .expect("selecting the sacrifice victim must succeed");

    resolve_discards_and_advance(&mut runner);

    // The Beast is in the graveyard (sacrificed).
    assert_eq!(
        runner.state().objects[&beast_id].zone,
        Zone::Graveyard,
        "the sacrificed Beast must be in the graveyard",
    );

    // Hand: started with 3, drew 5, discarded 3 → 5.
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        5,
        "hand must be 3 initial + 5 drawn - 3 discarded == 5",
    );

    // Library decreased by exactly 5 (the draw count).
    assert_eq!(
        lib_before - runner.state().players[P0.0 as usize].library.len(),
        5,
        "exactly five cards must be drawn from the library",
    );
}

/// CR 208.3 + CR 400.7j: PROVES the snapshot captures *last-known* power,
/// including continuous modifications — a 2/2 with two +1/+1 counters has
/// effective power 4, so the draw count is 4, not the 2 base power.
/// Tests the `Power { CostPaidObject }` resolution class, not one card.
#[test]
fn greater_good_draw_count_tracks_modified_power() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gg_id = scenario
        .add_creature(P0, "Greater Good", 0, 0)
        .from_oracle_text(GREATER_GOOD_TEXT)
        .as_enchantment()
        .id();
    let victim_id = scenario
        .add_creature(P0, "Counter Beast", 2, 2)
        .with_plus_counters(2)
        .id();
    scenario.with_library_top(P0, &["L1", "L2", "L3", "L4", "L5"]);
    scenario.with_cards_in_hand(P0, &["H1", "H2", "H3"]);

    let mut runner = scenario.build();
    let lib_before = runner.state().players[P0.0 as usize].library.len();

    runner
        .act(GameAction::ActivateAbility {
            source_id: gg_id,
            ability_index: 0,
        })
        .expect("activating Greater Good must succeed");
    runner
        .act(GameAction::SelectCards {
            cards: vec![victim_id],
        })
        .expect("selecting the sacrifice victim must succeed");
    resolve_discards_and_advance(&mut runner);

    assert_eq!(
        lib_before - runner.state().players[P0.0 as usize].library.len(),
        4,
        "snapshot must read effective power (2 base + 2 counters == 4), not base power",
    );
}

/// CR 202.3 + CR 400.7j + CR 608.2k — discard-cost parity. Greater Good has no
/// discard *cost*, so this exercises the *same* `CostPaidObject` snapshot via a
/// discard cost: "Discard a card: Draw cards equal to the discarded card's mana
/// value." The discard-for-cost handler (`casting_costs.rs::handle_discard_for_cost`)
/// calls `set_cost_paid_object_recursive` symmetric with the sacrifice path, so
/// this is a straight positive-assertion test. No real printed card has this
/// shape, so the ability is built from Oracle text and the parse shape is
/// asserted in setup.
#[test]
fn discard_cost_populates_cost_paid_object() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // NB: the host is named "Mind Forge" rather than anything starting with
    // "Discard" — the parser's self-reference normalization rewrites a leading
    // card-name token to `~`, so a name colliding with the cost verb produces
    // a misparse (test-setup artifact, not an engine concern).
    let host_id = scenario
        .add_creature(P0, "Mind Forge", 0, 0)
        .from_oracle_text("Discard a card: Draw cards equal to the discarded card's mana value.")
        .as_enchantment()
        .id();
    // A discardable hand card with a known mana value of 3.
    let discard_id = scenario
        .add_creature_to_hand(P0, "Three Drop", 1, 1)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    scenario.with_library_top(P0, &["L1", "L2", "L3", "L4"]);

    let mut runner = scenario.build();

    // Parse-shape assertion: confirm Oracle parsing produced a Discard cost
    // feeding a Draw whose count references the cost-paid object's mana value.
    {
        let ability = &runner.state().objects[&host_id].abilities[0];
        assert!(
            matches!(ability.cost, Some(AbilityCost::Discard { .. })),
            "the ability must parse to a Discard cost; got {:?}",
            ability.cost,
        );
        assert!(
            matches!(
                ability.effect.as_ref(),
                Effect::Draw {
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectManaValue {
                            scope: ObjectScope::CostPaidObject,
                        },
                    },
                    ..
                },
            ),
            "the draw count must reference ObjectManaValue {{ CostPaidObject }}; got {:?}",
            ability.effect,
        );
    }

    let lib_before = runner.state().players[P0.0 as usize].library.len();

    runner
        .act(GameAction::ActivateAbility {
            source_id: host_id,
            ability_index: 0,
        })
        .expect("activating the discard-cost ability must succeed");
    assert_eq!(
        runner.waiting_for_kind(),
        "DiscardForCost",
        "activating a Discard-cost ability must prompt for the discard",
    );

    runner
        .act(GameAction::SelectCards {
            cards: vec![discard_id],
        })
        .expect("selecting the discarded card must succeed");
    runner.advance_until_stack_empty();

    assert_eq!(
        lib_before - runner.state().players[P0.0 as usize].library.len(),
        3,
        "the discard-cost path must snapshot the discarded card and draw cards \
         equal to its mana value (3)",
    );
}

/// Greater Good ruling (2020-08-07): sacrificing a 0-power creature draws zero
/// cards (the draw count is Greater Good's own Oracle text — `Power`), but the
/// discard still happens — "If you don't have three cards in hand, you discard
/// your hand." A 2-card hand is fully discarded.
#[test]
fn greater_good_zero_power_creature_draws_zero_still_discards() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gg_id = scenario
        .add_creature(P0, "Greater Good", 0, 0)
        .from_oracle_text(GREATER_GOOD_TEXT)
        .as_enchantment()
        .id();
    let victim_id = scenario.add_creature(P0, "Wall", 0, 1).id();
    scenario.with_library_top(P0, &["L1", "L2", "L3"]);
    scenario.with_cards_in_hand(P0, &["H1", "H2"]);

    let mut runner = scenario.build();
    let lib_before = runner.state().players[P0.0 as usize].library.len();

    runner
        .act(GameAction::ActivateAbility {
            source_id: gg_id,
            ability_index: 0,
        })
        .expect("activating Greater Good must succeed");
    runner
        .act(GameAction::SelectCards {
            cards: vec![victim_id],
        })
        .expect("selecting the 0-power victim must succeed");
    resolve_discards_and_advance(&mut runner);

    assert_eq!(
        lib_before - runner.state().players[P0.0 as usize].library.len(),
        0,
        "a 0-power sacrifice victim must draw zero cards",
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        0,
        "with fewer than three cards, the player discards their whole hand",
    );
}

/// CR 400.7j + CR 608.2k — slot lifecycle. PROVES the `cost_paid_object`
/// snapshot is scoped to one resolution: a first activation sacrificing a 5/5
/// draws 5; a second activation sacrificing a 0-power creature draws 0. A
/// leaked stale snapshot would draw 5 again — a fresh slot draws 0.
#[test]
fn cost_paid_object_does_not_leak_between_activations() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gg_id = scenario
        .add_creature(P0, "Greater Good", 0, 0)
        .from_oracle_text(GREATER_GOOD_TEXT)
        .as_enchantment()
        .id();
    let big_id = scenario.add_creature(P0, "Big Beast", 5, 5).id();
    let zero_id = scenario.add_creature(P0, "Zero Beast", 0, 1).id();
    scenario.with_library_top(P0, &["L1", "L2", "L3", "L4", "L5", "L6"]);
    // Hand deep enough to survive two rounds of discard-3.
    scenario.with_cards_in_hand(P0, &["H1", "H2", "H3", "H4", "H5", "H6"]);

    let mut runner = scenario.build();

    // First activation: sacrifice the 5/5 → draws 5.
    let lib_before_1 = runner.state().players[P0.0 as usize].library.len();
    runner
        .act(GameAction::ActivateAbility {
            source_id: gg_id,
            ability_index: 0,
        })
        .expect("first activation must succeed");
    runner
        .act(GameAction::SelectCards {
            cards: vec![big_id],
        })
        .expect("selecting the 5/5 must succeed");
    resolve_discards_and_advance(&mut runner);
    assert_eq!(
        lib_before_1 - runner.state().players[P0.0 as usize].library.len(),
        5,
        "first activation must draw 5 (the 5/5's power)",
    );

    // Second activation: sacrifice the 0-power creature → must draw 0.
    let lib_before_2 = runner.state().players[P0.0 as usize].library.len();
    runner
        .act(GameAction::ActivateAbility {
            source_id: gg_id,
            ability_index: 0,
        })
        .expect("second activation must succeed");
    runner
        .act(GameAction::SelectCards {
            cards: vec![zero_id],
        })
        .expect("selecting the 0-power creature must succeed");
    resolve_discards_and_advance(&mut runner);
    assert_eq!(
        lib_before_2 - runner.state().players[P0.0 as usize].library.len(),
        0,
        "second activation must draw 0 — a leaked stale snapshot would draw 5 again",
    );
}

/// SHAPE TEST (not a pipeline test) — documents two co-located known
/// limitations around multi-permanent sacrifice costs:
///
/// 1. **`cost_paid_object` is a singular slot.**
///    `casting_costs.rs::handle_sacrifice_for_cost` snapshots only
///    `chosen.first()` — for a `count > 1` sacrifice cost, only the first
///    sacrificed object's characteristics are stamped onto the resolving
///    ability. Greater Good's cost is `count: 1`, so this slot is exact for
///    it; the limitation only surfaces for hypothetical multi-sacrifice costs.
///
/// 2. **The engine does not honor `AbilityCost::Sacrifice { count }` > 1.**
///    `casting_costs.rs:1906` builds `WaitingFor::SacrificeForCost` with a
///    hardcoded `count: 1`, discarding the cost's `count` field. A synthetic
///    `count: 2` sacrifice cost therefore still produces a `count: 1` prompt.
///    No printed card currently has a fixed `count > 1` sacrifice *activation*
///    cost feeding a `CostPaidObject` reference, so this is a documented
///    limitation, not a regression introduced by #334's snapshot work.
///
/// This test asserts CURRENT behavior so a future fix to either limitation
/// produces a visible, intentional diff. It is a shape test: it inspects the
/// engine-produced `WaitingFor` rather than driving a multi-sacrifice to a
/// drawn-card result (which is unreachable today).
#[test]
fn multi_sacrifice_cost_count_is_not_honored_shape() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Synthetic ability: a `count: 2` sacrifice cost feeding a draw that
    // references `Power { CostPaidObject }` — the same slot Greater Good uses.
    let synthetic = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Draw {
            count: QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::CostPaidObject,
                },
            },
            target: TargetFilter::Controller,
        },
    )
    .cost(AbilityCost::Sacrifice {
        target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
        count: 2,
    });

    let host_id = scenario
        .add_creature(P0, "Twin Sacrifice", 0, 0)
        .with_ability_definition(synthetic)
        .as_enchantment()
        .id();
    scenario.add_creature(P0, "First Victim", 5, 5);
    scenario.add_creature(P0, "Second Victim", 2, 2);
    scenario.with_library_top(P0, &["L1", "L2", "L3", "L4", "L5", "L6"]);

    let mut runner = scenario.build();

    // SHAPE assertion 1: the synthetic ability carries the count: 2 cost.
    {
        let ability = &runner.state().objects[&host_id].abilities[0];
        assert!(
            matches!(ability.cost, Some(AbilityCost::Sacrifice { count: 2, .. })),
            "the synthetic ability must carry a count: 2 Sacrifice cost",
        );
    }

    runner
        .act(GameAction::ActivateAbility {
            source_id: host_id,
            ability_index: 0,
        })
        .expect("activating the count: 2 sacrifice ability must succeed");

    // SHAPE assertion 2: the engine builds the sacrifice prompt with a
    // hardcoded count: 1, NOT the cost's count: 2. This documents the
    // `casting_costs.rs:1906` limitation — if a future fix honors the cost
    // count, this assertion fails loudly and intentionally.
    match &runner.state().waiting_for {
        engine::types::WaitingFor::SacrificeForCost { count, .. } => {
            assert_eq!(
                *count, 1,
                "documented limitation: SacrificeForCost is built with a \
                 hardcoded count of 1 — the cost's count: 2 is not honored",
            );
        }
        other => panic!("expected SacrificeForCost prompt, got {other:?}"),
    }
}
