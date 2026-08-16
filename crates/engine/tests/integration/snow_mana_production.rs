//! Regression: a snow permanent's mana ability must PRODUCE snow mana so that
//! {S} costs (CR 107.4h) become payable.
//!
//! The {S} CONSUME path (`spend_snow_unit`, `ManaUnit::is_snow`) was already
//! complete, but the PRODUCE path never stamped `ManaUnit.supertype =
//! Some(ManaSupertype::Snow)`: mana produced by a Snow-Covered basic was
//! indistinguishable from ordinary mana, so {S} could never be paid. The fix
//! computes snow-ness from the source (CR 205.4g / CR 106.3) and stamps it at
//! the mana-production `ManaUnit` construction site.
//!
//! Each test drives the real production pipeline via `ActivateAbility` through
//! `apply()`; the assertions flip if the produce-site stamp is reverted to
//! `supertype: None`.

use engine::game::can_pay;
use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::card_type::Supertype;
use engine::types::game_state::CastPaymentMode;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;

/// A bare `{S}` cost (one mana from a snow source, CR 107.4h).
fn snow_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Snow],
        generic: 0,
    }
}

/// Mark an existing battlefield permanent as a snow source (CR 205.4g): a
/// printed Snow supertype lives on both the base and the layered type set so a
/// layer recompute preserves it.
fn make_snow_source(runner: &mut engine::game::scenario::GameRunner, id: ObjectId) {
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    if !obj.base_card_types.supertypes.contains(&Supertype::Snow) {
        obj.base_card_types.supertypes.push(Supertype::Snow);
    }
    if !obj.card_types.supertypes.contains(&Supertype::Snow) {
        obj.card_types.supertypes.push(Supertype::Snow);
    }
}

/// Test 1: activating a Snow-Covered basic's `{T}: Add {G}` mana ability
/// produces snow mana. Reverting site 1 to `supertype: None` makes the
/// `is_snow()` assertion fail; the `total() > 0` reach-guard proves mana was
/// actually produced (defeats a vacuous pass).
#[test]
fn snow_source_produces_snow_mana() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land = scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();
    make_snow_source(&mut runner, land);

    runner
        .act(GameAction::ActivateAbility {
            source_id: land,
            ability_index: 0,
        })
        .expect("activating the snow land's mana ability must succeed");

    let pool = &runner.state().players[0].mana_pool;
    assert!(
        pool.total() > 0,
        "reach-guard: the mana ability must have produced mana (found none)",
    );
    assert!(
        pool.mana.iter().any(|u| u.is_snow()),
        "mana produced by a snow source must be snow mana (CR 107.4h); the \
         produce site must stamp ManaSupertype::Snow",
    );
}

/// Test 2 (positive cast pipeline): with snow mana produced by a snow source,
/// a spell whose cost contains `{S}` becomes castable. Reverting site 1 makes
/// the produced mana non-snow, so `{S}` is unpayable and the cast fails —
/// flipping this test.
#[test]
fn snow_mana_pays_snow_cost_in_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land = scenario.add_basic_land(P0, ManaColor::Green);
    // A sorcery costing {S}. Verbatim effect text is irrelevant to the {S}
    // payability under test; it is a no-op spell that only needs to reach the
    // stack once its cost is paid.
    let spell = scenario
        .add_spell_to_hand(P0, "Snow Snap", false)
        .with_mana_cost(snow_cost())
        .id();
    let mut runner = scenario.build();
    make_snow_source(&mut runner, land);

    // Produce one snow mana into the pool.
    runner
        .act(GameAction::ActivateAbility {
            source_id: land,
            ability_index: 0,
        })
        .expect("activating the snow land's mana ability must succeed");

    let spell_card = runner.state().objects[&spell].card_id;
    let stack_before = runner.state().stack.len();
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: spell_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting a {S} spell must succeed once snow mana is floating");
    assert_eq!(
        runner.state().stack.len(),
        stack_before + 1,
        "the {{S}} spell must reach the stack — its snow cost was paid from the \
         snow mana produced by the snow source",
    );
}

/// Test 3 (negative sibling): a NON-snow source of the same color produces
/// ordinary mana. The `is_snow() == false` assertion is paired with a
/// `total() > 0` reach-guard (mana WAS produced) so the negative is not vacuous,
/// and a `{S}` cost is not payable from it.
#[test]
fn nonsnow_source_produces_nonsnow_mana_and_cannot_pay_snow() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // A plain Forest — no Snow supertype.
    let land = scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();

    runner
        .act(GameAction::ActivateAbility {
            source_id: land,
            ability_index: 0,
        })
        .expect("activating the plain land's mana ability must succeed");

    let pool = &runner.state().players[0].mana_pool;
    assert!(
        pool.total() > 0,
        "reach-guard: the mana ability must have produced mana (found none)",
    );
    assert!(
        pool.mana.iter().all(|u| !u.is_snow()),
        "mana produced by a nonsnow source must NOT be snow mana (CR 205.4g)",
    );
    assert!(
        !can_pay(pool, &snow_cost()),
        "a {{S}} cost must not be payable from nonsnow mana (CR 107.4h)",
    );
}
