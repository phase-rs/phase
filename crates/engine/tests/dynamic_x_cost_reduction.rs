//! Dynamic-{X} activated-ability cost reduction — "{X} less to activate, where X
//! is <count>".
//!
//! Two Standard-legal cards emit this shape:
//!   - Survey Mechan: "... This ability costs {X} less to activate, where X is the
//!     number of differently named lands you control."
//!   - The Dominion Bracelet (granted ability): "... This ability costs {X} less
//!     to activate, where X is this creature's power."
//!
//! Before the parser arm landed, `try_parse_cost_reduction` returned `None` on the
//! `{X}` amount (an honest gap — `parse_mana_symbols` yields `shards: [X]`, which
//! the old generic-only guard rejected). The arm maps the dynamic amount to
//! `CostReduction { amount_per: 1, count: Ref(<qty>) }`; no engine/resolver change
//! is needed because runtime `apply_cost_reduction` already computes
//! `reduce_by = amount_per * count` and resolves `count` from game state.
//!
//! CR 601.2f: cost reductions are folded into the total cost; the generic mana
//! component is floored at {0}. CR 602.2b: an activated ability's activation cost
//! is the analog of a spell's mana cost. CR 107.3c: because X is defined by the
//! ability's own text ("where X is ..."), the controller does not choose it.
//! CR 118.7a: cost reduction affects only the generic component.
//!
//! DISCRIMINATING: the runtime test resolves the parsed `count` through the exact
//! seam `apply_cost_reduction` uses (`resolve_quantity`) against game states with
//! different numbers of differently-named lands, and asserts the resulting paid
//! generic mana DIFFERS (2 lands -> {8}, 7 lands -> {3}, 12 lands -> {0} floor).
//! On a revert (no {X} arm), `find_cost_reduction` returns `None`, the reduction
//! is never applied, the paid generic stays {10} in every case, and the
//! cross-case inequality assertions fail.

use engine::game::quantity::resolve_quantity;
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, CostReduction, ObjectScope, QuantityExpr, QuantityRef, SharedQuality,
};
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const SURVEY_MECHAN: &str = "{10}, Sacrifice this creature: Draw cards equal to the number \
of differently named lands you control. This ability costs {X} less to activate, where X is \
the number of differently named lands you control.";

const DOMINION_BRACELET_GRANT: &str = "{15}, Exile this creature: Search your library for a \
card, put it into your hand, then shuffle. This ability costs {X} less to activate, where X is \
this creature's power.";

/// Walk an ability and its chained `sub_ability` nodes, returning the first
/// `CostReduction` found at any level. Cost reduction is extracted onto the
/// ability owning the reducible cost, which may be a chained sub-ability.
fn find_cost_reduction(def: &AbilityDefinition) -> Option<&CostReduction> {
    let mut cur = Some(def);
    while let Some(d) = cur {
        if let Some(cr) = d.cost_reduction.as_ref() {
            return Some(cr);
        }
        cur = d.sub_ability.as_deref();
    }
    None
}

fn parsed_cost_reduction(oracle: &str, name: &str) -> CostReduction {
    let parsed = parse_oracle_text(oracle, name, &[], &[], &[]);
    parsed
        .abilities
        .iter()
        .find_map(find_cost_reduction)
        .cloned()
        .unwrap_or_else(|| panic!("{name}: no ability captured a dynamic-X cost reduction"))
}

/// Add a land with a distinct name controlled by `controller` on the battlefield.
fn add_named_land(state: &mut GameState, card_id: u64, controller: PlayerId, name: &str) {
    let id = create_object(
        state,
        CardId(card_id),
        controller,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Land);
    obj.controller = controller;
}

/// Mirror of the runtime `apply_cost_reduction` math (CR 601.2f / CR 118.7a):
/// `reduce_by = amount_per * count`, applied to the generic component, floored at
/// {0}. Resolves `count` through the same `resolve_quantity` seam the engine uses.
fn paid_generic_after_reduction(
    state: &GameState,
    reduction: &CostReduction,
    base_generic: u32,
    player: PlayerId,
    source: ObjectId,
) -> u32 {
    let count = resolve_quantity(state, &reduction.count, player, source);
    let reduce_by = (reduction.amount_per as i32 * count).max(0) as u32;
    base_generic.saturating_sub(reduce_by)
}

#[test]
fn survey_mechan_parses_dynamic_distinct_lands_reduction() {
    let reduction = parsed_cost_reduction(SURVEY_MECHAN, "Survey Mechan");
    assert_eq!(reduction.amount_per, 1, "dynamic-X amount_per is 1");
    assert_eq!(reduction.condition, None, "unconditional dynamic reduction");
    match &reduction.count {
        QuantityExpr::Ref {
            qty: QuantityRef::ObjectCountDistinct { qualities, .. },
        } => assert_eq!(qualities.as_slice(), [SharedQuality::Name]),
        other => panic!("expected ObjectCountDistinct[Name], got {other:?}"),
    }
}

#[test]
fn dominion_bracelet_grant_parses_dynamic_power_reduction() {
    let reduction = parsed_cost_reduction(DOMINION_BRACELET_GRANT, "The Dominion Bracelet");
    assert_eq!(reduction.amount_per, 1, "dynamic-X amount_per is 1");
    assert_eq!(reduction.condition, None, "unconditional dynamic reduction");
    assert!(
        matches!(
            reduction.count,
            QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::Source
                }
            }
        ),
        "expected Power(Source), got {:?}",
        reduction.count
    );
}

/// DISCRIMINATING runtime test: the paid generic mana scales with the dynamic
/// count and is floored at {0}. Survey Mechan base activation cost is {10}.
#[test]
fn survey_mechan_paid_generic_scales_with_distinct_lands() {
    let reduction = parsed_cost_reduction(SURVEY_MECHAN, "Survey Mechan");
    let base_generic = 10u32;

    let build_state = |land_names: &[&str]| -> (GameState, ObjectId) {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Survey Mechan".to_string(),
            Zone::Battlefield,
        );
        for (i, name) in land_names.iter().enumerate() {
            add_named_land(&mut state, 100 + i as u64, PlayerId(0), name);
        }
        (state, source)
    };

    // 2 differently named lands -> reduce by {2} -> pay {8}.
    let (state2, src2) = build_state(&["Island", "Forest"]);
    let paid2 = paid_generic_after_reduction(&state2, &reduction, base_generic, PlayerId(0), src2);
    assert_eq!(paid2, 8, "2 distinct lands: {{10}} - {{2}} = {{8}}");

    // 7 differently named lands -> reduce by {7} -> pay {3}.
    let (state7, src7) = build_state(&[
        "Island",
        "Forest",
        "Mountain",
        "Plains",
        "Swamp",
        "Wastes",
        "Cavern of Souls",
    ]);
    let paid7 = paid_generic_after_reduction(&state7, &reduction, base_generic, PlayerId(0), src7);
    assert_eq!(paid7, 3, "7 distinct lands: {{10}} - {{7}} = {{3}}");

    // 12 differently named lands -> reduce by {12} -> floored at {0} (CR 601.2f).
    let (state12, src12) = build_state(&[
        "Island",
        "Forest",
        "Mountain",
        "Plains",
        "Swamp",
        "Wastes",
        "Cavern of Souls",
        "Ancient Tomb",
        "City of Brass",
        "Gaea's Cradle",
        "Karakas",
        "Maze of Ith",
    ]);
    let paid12 =
        paid_generic_after_reduction(&state12, &reduction, base_generic, PlayerId(0), src12);
    assert_eq!(
        paid12, 0,
        "12 distinct lands: {{10}} - {{12}} floored at {{0}}"
    );

    // Cross-case discrimination: the three paid costs are distinct, so a revert
    // (no reduction -> always {10}) cannot satisfy all three assertions.
    assert!(
        paid2 != paid7 && paid7 != paid12 && paid2 != paid12,
        "paid generic must differ across distinct-land counts: {paid2}, {paid7}, {paid12}"
    );
}
