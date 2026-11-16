//! CR 118.9 + CR 601.2b: Demon of Fate's Design — "Once during each of your
//! turns, you may cast an enchantment spell by paying life equal to its mana
//! value rather than paying its mana cost." Runtime proof that:
//!  1. The once-per-turn alternative cost is offered when casting an enchantment.
//!  2. Accepting the grant pays life equal to the spell's mana value.
//!  3. The once-per-turn slot is consumed after acceptance.
//!  4. Non-enchantment spells are NOT offered the grant.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{AbilityCost, AdditionalCost, QuantityExpr, QuantityRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const DEMON_ORACLE: &str = concat!(
    "Once during each of your turns, you may cast an ",
    "enchantment spell by paying life equal to its mana value rather than paying ",
    "its mana cost.",
);

/// CR 118.9 + CR 601.2b: Casting an enchantment with Demon on the battlefield
/// surfaces an `OptionalCostChoice` offering `PayLife { SelfManaValue }` vs the
/// printed mana cost.
#[test]
fn demon_of_fates_design_offers_alt_cost_for_enchantment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Demon of Fate's Design on the battlefield — only the alt-cost static
    // matters; hosted on a creature shell via the full parser.
    let _demon = scenario
        .add_creature_from_oracle(P0, "Demon of Fate's Design", 6, 6, DEMON_ORACLE)
        .id();

    // Enchantment spell in hand: MV = 3 ({2}{W}).
    let ench_id = scenario
        .add_creature_to_hand(P0, "Test Enchantment", 0, 0)
        .as_enchantment()
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 2,
        })
        .id();

    let mut runner = scenario.build();
    let life_before = runner.life(P0);
    let card_id = runner.state().objects[&ench_id].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: ench_id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the enchantment should succeed");

    // The engine must surface an OptionalCostChoice with the PayLife alternative.
    match &runner.state().waiting_for {
        WaitingFor::OptionalCostChoice { cost, .. } => match cost {
            AdditionalCost::Choice(alt, _printed) => {
                assert_eq!(
                    *alt,
                    AbilityCost::PayLife {
                        amount: QuantityExpr::Ref {
                            qty: QuantityRef::SelfManaValue,
                        },
                    },
                    "alternative cost must be PayLife with SelfManaValue"
                );
            }
            other => panic!("expected AdditionalCost::Choice, got {other:?}"),
        },
        other => panic!("expected OptionalCostChoice for the Demon grant, got {other:?}"),
    }

    // Life must not have been deducted yet (choice not yet accepted).
    assert_eq!(
        life_before,
        runner.life(P0),
        "life must not change before accepting the alternative cost"
    );
}

/// CR 118.9 + CR 601.2b: Accepting the PayLife alternative cost deducts life
/// equal to the spell's mana value and moves the spell to the stack.
#[test]
fn demon_of_fates_design_accept_alt_cost_pays_life() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let demon_id = scenario
        .add_creature_from_oracle(P0, "Demon of Fate's Design", 6, 6, DEMON_ORACLE)
        .id();

    // MV = 4 ({3}{B}).
    let ench_id = scenario
        .add_creature_to_hand(P0, "Test Enchantment MV4", 0, 0)
        .as_enchantment()
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 3,
        })
        .id();

    let mut runner = scenario.build();
    let life_before = runner.life(P0);
    let card_id = runner.state().objects[&ench_id].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: ench_id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast should start");

    // Accept the alternative cost.
    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("accepting the alternative cost should succeed");

    // Spell must be on the stack.
    assert_eq!(
        runner.state().objects[&ench_id].zone,
        Zone::Stack,
        "enchantment should be on the stack after paying the alternative cost"
    );

    // Life must have decreased by the spell's mana value (4).
    assert_eq!(
        runner.life(P0),
        life_before - 4,
        "life should decrease by the spell's mana value (4)"
    );

    // Once-per-turn slot must be consumed.
    assert!(
        runner
            .state()
            .alt_cost_grant_permissions_used
            .contains(&demon_id),
        "accepting the alternative cost must consume the Demon's per-turn slot"
    );
}

/// CR 118.9: Non-enchantment spells must NOT be offered the Demon's grant.
#[test]
fn demon_of_fates_design_does_not_offer_for_non_enchantment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let _demon = scenario
        .add_creature_from_oracle(P0, "Demon of Fate's Design", 6, 6, DEMON_ORACLE)
        .id();

    // A creature spell (NOT an enchantment): MV = 2.
    let creature_id = scenario
        .add_creature_to_hand(P0, "Test Creature", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        })
        .id();

    // Fund the player's mana pool so the creature can actually be cast.
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&creature_id].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: creature_id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast should start");

    // Must NOT be offered an OptionalCostChoice for the Demon's grant.
    assert!(
        !matches!(
            &runner.state().waiting_for,
            WaitingFor::OptionalCostChoice { .. }
        ),
        "non-enchantment spell must not be offered the Demon's alternative cost, got {:?}",
        runner.state().waiting_for
    );
}
