//! Unravel — "Counter target spell. If the amount of mana spent to cast that
//! spell was less than its mana value, you draw a card."
//!
//! Parser regression: the intervening-if on the draw rider must lower to a
//! `QuantityCheck` over targeted-spell mana spent vs mana value.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityCondition, CastManaObjectScope, CastManaSpentMetric, Comparator, Effect, ObjectScope,
    QuantityExpr, QuantityRef, StaticDefinition,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::statics::{CostModifyMode, StaticMode};
use engine::types::zones::Zone;

const UNRAVEL_ORACLE: &str = "Counter target spell. If the amount of mana spent to cast that spell was less than its mana value, you draw a card.";

#[test]
fn unravel_parses_counter_and_conditional_draw() {
    let parsed = parse_oracle_text(
        UNRAVEL_ORACLE,
        "Unravel",
        &[],
        &["Instant".to_string()],
        &[],
    );
    let counter = parsed
        .abilities
        .first()
        .expect("Unravel must parse a counter ability");
    assert!(matches!(counter.effect.as_ref(), Effect::Counter { .. }));
    assert!(counter.condition.is_none());

    let draw = counter
        .sub_ability
        .as_ref()
        .expect("Unravel must chain a conditional draw sub-ability");
    assert!(matches!(draw.effect.as_ref(), Effect::Draw { .. }));
    assert!(matches!(
        draw.condition.as_ref(),
        Some(AbilityCondition::QuantityCheck {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::ManaSpentToCast {
                    scope: CastManaObjectScope::AbilityTarget,
                    metric: CastManaSpentMetric::Total,
                },
            },
            comparator: Comparator::LT,
            rhs: QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::Target,
                },
            },
        })
    ));
}

/// Drive Unravel through the real cast→resolve pipeline against a target spell
/// on the stack and report `(was the target countered?, net cards P1 drew)`.
///
/// `reduce_target_cost` puts a `{1}` cost-reduction static permanent under P0
/// so the {3} target spell is paid for with only 2 mana — the engine records
/// `mana_spent_to_cast_amount = 2 < 3 = mana value` on the target object via the
/// real payment path (no hand-set state). Without it the target is paid in full
/// ({3}), so mana spent equals its mana value and the draw rider must not fire.
///
/// CR 701.6a + CR 601.2f/601.2h: countering and the mana-spent-vs-mana-value
/// comparison both read state the cast pipeline produces authentically.
fn counter_with_unravel(reduce_target_cost: bool) -> (bool, i64) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    if reduce_target_cost {
        // CR 601.2f: a static cost reducer lowers the target spell's cost during
        // cost determination, so fewer mana units are actually spent.
        scenario
            .add_creature(P0, "Helmcrafter", 0, 1)
            .with_static_definition(StaticDefinition::new(StaticMode::ModifyCost {
                mode: CostModifyMode::Reduce,
                amount: ManaCost::generic(1),
                spell_filter: None,
                dynamic_count: None,
            }));
    }

    // P0 (active player) casts a {3} creature spell — mana value 3.
    let target = scenario
        .add_creature_to_hand(P0, "Target Ogre", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 3,
        })
        .id();
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Green);
    }

    // P1 holds Unravel ({1}{U}{U} = mana value 3) and a library to draw from.
    let mut unravel = scenario.add_spell_to_hand_from_oracle(P1, "Unravel", true, UNRAVEL_ORACLE);
    unravel.with_mana_cost(ManaCost::Cost {
        generic: 1,
        shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
    });
    let unravel_id = unravel.id();
    for _ in 0..3 {
        scenario.add_basic_land(P1, ManaColor::Blue);
    }
    scenario.with_library_top(P1, &["Forest", "Forest"]);

    let mut runner = scenario.build();
    let p1_library_before = runner.state().players[1].library.len();

    // P0 casts the target spell through the real cast pipeline.
    let target_card = runner.state().objects[&target].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: target,
            card_id: target_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P0 casts the target spell");

    // P0 passes priority; P1 responds with Unravel (auto-targets the only spell).
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority");
    let unravel_card = runner.state().objects[&unravel_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: unravel_id,
            card_id: unravel_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P1 casts Unravel");

    // Resolve the whole stack.
    while !runner.state().stack.is_empty() {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected waiting state while resolving: {other:?}"),
        }
    }

    let countered = runner.state().objects.get(&target).map(|o| o.zone) == Some(Zone::Graveyard);
    let drew = p1_library_before as i64 - runner.state().players[1].library.len() as i64;
    (countered, drew)
}

/// CR 601.2h + CR 608.2c: when the target spell was paid for with less mana
/// than its mana value, Unravel counters it AND its controller draws a card.
#[test]
fn unravel_draws_when_target_paid_below_mana_value() {
    let (countered, drew) = counter_with_unravel(true);
    assert!(countered, "Unravel must counter the target spell");
    assert_eq!(
        drew, 1,
        "mana spent (2) < mana value (3): the draw rider must fire (P1 draws 1)"
    );
}

/// CR 608.2c: when the target spell was paid for at its full mana value, the
/// intervening-if fails — Unravel counters but its controller draws nothing.
#[test]
fn unravel_does_not_draw_when_target_paid_full_mana_value() {
    let (countered, drew) = counter_with_unravel(false);
    assert!(countered, "Unravel must still counter the target spell");
    assert_eq!(
        drew, 0,
        "mana spent (3) == mana value (3): the draw rider must not fire"
    );
}
