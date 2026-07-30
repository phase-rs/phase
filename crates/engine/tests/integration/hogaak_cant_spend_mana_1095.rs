//! Regression (issue #1095): Hogaak, Arisen Necropolis — "You can't spend mana
//! to cast this spell." No mana may leave the pool; the entire cost must be met
//! by alternative payments (convoke/delve). The parser records the clause as
//! `CastingRestriction::CantSpendMana`, and the mana-payment eligibility layer
//! (`game::mana_payment::ctx_permits_unit`) makes real (non-convoke) pool units
//! ineligible under that spell context, so both affordability and the spend
//! route the whole cost onto convoke/delve.
//!
//! Built from the real Oracle text so the parser → restriction → payment path is
//! exercised end to end. The printed cost is overridden to a small generic value;
//! the payment restriction itself is cost-agnostic.

use engine::game::scenario::{GameScenario, P0};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const HOGAAK_ORACLE: &str = "You can't spend mana to cast this spell.\n\
Convoke, delve (Each creature you tap while casting this spell pays for {1} or one mana of that creature's color. Each card you exile from your graveyard pays for {1}.)\n\
You may cast this card from your graveyard.\n\
Trample";

fn add_generic_mana(runner: &mut engine::game::scenario::GameRunner, player: usize, n: usize) {
    for _ in 0..n {
        runner.state_mut().players[player]
            .mana_pool
            .add(ManaUnit::new(
                ManaType::Colorless,
                ObjectId(0),
                false,
                vec![],
            ));
    }
}

/// A legal convoke payment resolves Hogaak: two creatures tapped for convoke
/// ({1} each) cover its whole cost, and no mana is recorded as spent to cast it.
#[test]
fn convoke_pays_hogaak_without_spending_mana() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // {2} generic → two convoke taps ({1} each).
    let hogaak = scenario
        .add_creature_to_hand_from_oracle(P0, "Hogaak, Arisen Necropolis", 8, 8, HOGAAK_ORACLE)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let convoker_a = scenario.add_creature(P0, "Bear A", 2, 2).id();
    let convoker_b = scenario.add_creature(P0, "Bear B", 2, 2).id();

    // No pool mana at all — convoke must cover the entire cost.
    let mut runner = scenario.build();

    let outcome = runner
        .cast(hogaak)
        .convoke_with(&[convoker_a, convoker_b])
        .resolve();
    let state = outcome.state();

    assert_eq!(
        state.objects[&hogaak].zone,
        Zone::Battlefield,
        "Hogaak must resolve when convoke covers its whole cost"
    );
    assert!(
        !state.objects[&hogaak].mana_spent_to_cast,
        "no mana may be spent to cast Hogaak — convoke pays it all"
    );
}

/// With only pool mana available (no convoke creatures, no delve fodder), Hogaak
/// cannot be cast: mana can't pay for it and there is no alternative payment.
#[test]
fn pool_mana_alone_cannot_pay_hogaak() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let hogaak = scenario
        .add_creature_to_hand_from_oracle(P0, "Hogaak, Arisen Necropolis", 8, 8, HOGAAK_ORACLE)
        .with_mana_cost(ManaCost::generic(2))
        .id();

    let mut runner = scenario.build();
    add_generic_mana(&mut runner, P0.0 as usize, 5);
    let pool_before = runner.state().players[P0.0 as usize].mana_pool.total();

    // No convoke/delve resources exist, so the cast cannot be paid for.
    let result = runner.cast(hogaak).try_resolve();

    assert!(
        result.is_err() || runner.state().objects[&hogaak].zone != Zone::Battlefield,
        "Hogaak must not be castable from pool mana alone"
    );
    assert_eq!(
        runner.state().objects[&hogaak].zone,
        Zone::Hand,
        "Hogaak stays in hand when only pool mana is available"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        pool_before,
        "no pool mana may be spent on a cast that cannot legally be paid"
    );
}
