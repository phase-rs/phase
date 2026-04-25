//! CR 601.2h / CR 601.2b regression guard for Harrow's "sacrifice a land"
//! additional cost. Two assertions:
//!
//! 1. **Parser-level** — the full Oracle Additional-Cost line parses to
//!    `Required(Sacrifice { Land, 1 })`. This is the static contract between
//!    `parser::oracle_casting::parse_additional_cost_line` and the runtime.
//!
//! 2. **Engine-level (single-authority)** — `AbilityCost::is_payable`
//!    rejects when the controller has no lands and accepts when they do.
//!    Per CLAUDE.md "single authority for ability costs," the legality gate
//!    must live in the engine (`game/cost_payability.rs`), not in
//!    `legal_actions` / AI / frontend pre-filtering.

use engine::game::zones::create_object;
use engine::types::ability::{AbilityCost, AdditionalCost, TargetFilter, TypeFilter, TypedFilter};
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);

fn sacrifice_a_land() -> AbilityCost {
    AbilityCost::Sacrifice {
        target: TargetFilter::Typed(TypedFilter::land()),
        count: 1,
    }
}

/// CR 601.2f: Harrow's full Oracle additional-cost line parses to the
/// `Required(Sacrifice { Land, 1 })` shape. Mirrors the Waterbend regression
/// in `integration_bending.rs::test_parse_waterbend_additional_cost`.
#[test]
fn harrow_additional_cost_parses_required_sacrifice_land_cr_601_2f() {
    use engine::parser::oracle_casting::parse_additional_cost_line;

    let result = parse_additional_cost_line(
        "as an additional cost to cast this spell, sacrifice a land.",
        "As an additional cost to cast this spell, sacrifice a land.",
    );

    let cost = result.expect("Harrow additional-cost line must parse");
    let AdditionalCost::Required(AbilityCost::Sacrifice { target, count }) = cost else {
        panic!("Expected Required(Sacrifice {{ ... }}), got {cost:?}");
    };
    assert_eq!(count, 1, "Harrow sacrifices exactly one land");
    let TargetFilter::Typed(typed) = target else {
        panic!("Expected Typed filter, got {target:?}");
    };
    assert!(
        typed
            .type_filters
            .iter()
            .any(|t| matches!(t, TypeFilter::Land)),
        "Sacrifice filter must include Land, got {:?}",
        typed.type_filters
    );
}

/// CR 601.2h + CR 601.2b: A required additional cost whose choice-of-object
/// is unavailable makes the spell uncastable ("Unpayable costs can't be paid",
/// CR 601.2h, verified via `grep '^601.2h' docs/MagicCompRules.txt`). The
/// legality gate is `AbilityCost::is_payable` in `game/cost_payability.rs:58`
/// — the same predicate `casting_costs::check_additional_cost_or_pay_with_distribute`
/// calls at line 640.
#[test]
fn harrow_sacrifice_land_unpayable_with_no_lands_cr_601_2h() {
    let state = GameState::new_two_player(42);
    let dummy_source = ObjectId(0);

    assert!(
        !sacrifice_a_land().is_payable(&state, P0, dummy_source),
        "Sacrifice-a-land must be unpayable when controller has no lands on the battlefield"
    );
}

/// CR 601.2h + CR 601.2b: Same predicate accepts when at least one matching
/// permanent exists. Together with the negative case above, this proves the
/// engine — not AI/FE — owns the cast-legality decision.
#[test]
fn harrow_sacrifice_land_payable_with_a_forest_cr_601_2h() {
    let mut state = GameState::new_two_player(42);

    let forest = create_object(
        &mut state,
        CardId(100),
        P0,
        "Forest".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&forest).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.base_card_types = obj.card_types.clone();
    }

    assert!(
        sacrifice_a_land().is_payable(&state, P0, ObjectId(0)),
        "Sacrifice-a-land must be payable when controller has at least one land"
    );
}
