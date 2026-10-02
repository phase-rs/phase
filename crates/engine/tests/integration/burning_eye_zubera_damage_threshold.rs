//! Runtime tests for Burning-Eye Zubera's intervening-if
//! "if 4 or more damage was dealt to it this turn" (CR 603.4 + CR 107.1 +
//! CR 120.1).
//!
//! Revert discriminator: drop the quantity-first combinator arm → trigger
//! `condition` is `None` → a 3-damage lethal hit still deals 3 to P1.

use engine::game::scenario::{CastOutcome, GameScenario, P0, P1};
use engine::types::ability::{Comparator, QuantityExpr, QuantityRef, TriggerCondition};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ZUBERA: &str =
    "When this creature dies, if 4 or more damage was dealt to it this turn, this creature deals 3 damage to any target.";

fn deal_n_to_zubera(n: i32) -> CastOutcome {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let zubera = scenario
        .add_creature_from_oracle(P0, "Burning-Eye Zubera", 3, 3, ZUBERA)
        .id();
    let oracle = format!("Burn deals {n} damage to any target.");
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Burn", true, &oracle)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])],
    );
    let mut runner = scenario.build();

    let has_gate = runner.state().objects[&zubera]
        .trigger_definitions
        .iter_unchecked()
        .any(|entry| {
            matches!(
                entry.definition.condition,
                Some(TriggerCondition::QuantityComparison {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::DamageDealtThisTurn { .. },
                    },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 4 },
                })
            )
        });
    assert!(
        has_gate,
        "Zubera must parse intervening-if DamageDealtThisTurn GE 4"
    );

    // Spell targets the Zubera. When the gate is true the dies trigger also
    // needs an any-target (P1). When it is false that extra target is not asked.
    let outcome = if n >= 4 {
        runner
            .cast(spell)
            .target_object(zubera)
            .target_player(P1)
            .resolve()
    } else {
        runner.cast(spell).target_object(zubera).resolve()
    };
    assert_eq!(
        outcome.state().objects[&zubera].zone,
        Zone::Graveyard,
        "reach-guard: {n} damage must kill the 3/3"
    );
    outcome
}

#[test]
fn burning_eye_zubera_deals_three_when_four_damage_this_turn() {
    // CR 603.4: 4 damage marked on a 3/3 is lethal and satisfies the
    // intervening-if, so the dies trigger deals 3 to P1.
    let outcome = deal_n_to_zubera(4);
    assert!(
        matches!(
            outcome.state().waiting_for,
            WaitingFor::Priority { .. } | WaitingFor::GameOver { .. }
        ),
        "expected Priority/GameOver, got {:?}",
        outcome.state().waiting_for
    );
    outcome.assert_life_delta(P1, -3);
}

#[test]
fn burning_eye_zubera_does_not_deal_when_three_damage_this_turn() {
    // 3 damage is lethal for a 3/3 but fails GE 4. Reach-guard: P1 is not
    // the spell's target, so life_delta 0 proves the trigger did not fire.
    let outcome = deal_n_to_zubera(3);
    assert!(
        matches!(outcome.state().waiting_for, WaitingFor::Priority { .. }),
        "expected Priority, got {:?}",
        outcome.state().waiting_for
    );
    outcome.assert_life_delta(P1, 0);
}
