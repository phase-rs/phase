//! Issue #8432: Morophon, the Boundless was reducing the FULL cost of a
//! chosen-type spell instead of only its colored mana.
//!
//! Morophon reads "Spells of the chosen type you cast cost {W}{U}{B}{R}{G} less
//! to cast. **This effect reduces only the amount of colored mana you pay.**"
//! That second sentence is a card-level override of CR 118.7b/c/d: normally a
//! reduction unit whose color is absent from (or in excess of) the cost spills
//! over and reduces generic mana instead (this is what Aang, Master of Elements
//! relies on — see `issue_6405_aang_multicolor_cost_reduction`). Morophon
//! suppresses that spillover, so an unmatched unit is simply lost.
//!
//! Morophon's own ruling is the worked example: a chosen-type spell costing
//! {4}{R}{W}{W} costs {4}{W} afterwards — the {4} is untouched and only one of
//! the two {W} pips is cancelled.
//!
//! The Discord report: with Morophon naming Sliver, The First Sliver /
//! Sliver Queen ({W}{U}{B}{R}{G}) correctly went to {0}, but Manaweft Sliver /
//! Muscle Sliver ({1}{G}) also went to {0} when it should still cost {1}.
//!
//! These tests drive the reduction ARITHMETIC with the filter already satisfied
//! (Morophon's chosen-creature-type filter has its own coverage in
//! `morophon_chosen_type_1653`); the Oracle-text → `CostReductionReach` mapping
//! for the whole printed class is covered by the parser tests.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::parser::oracle_static::parse_static_line;
use engine::types::ability::{Effect, QuantityExpr, StaticDefinition, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::statics::{CostModifyMode, CostReductionReach, StaticMode};

fn wubrg() -> ManaCost {
    ManaCost::Cost {
        shards: vec![
            ManaCostShard::White,
            ManaCostShard::Blue,
            ManaCostShard::Black,
            ManaCostShard::Red,
            ManaCostShard::Green,
        ],
        generic: 0,
    }
}

/// Morophon's reducer with its chosen-creature-type filter already satisfied.
fn morophon_reducer() -> StaticDefinition {
    StaticDefinition::new(StaticMode::ModifyCost {
        mode: CostModifyMode::Reduce,
        amount: wubrg(),
        spell_filter: None,
        dynamic_count: None,
        reach: CostReductionReach::ColoredManaOnly,
    })
}

/// The same reducer WITHOUT the colored-only rider — Aang, Master of Elements
/// ("(This can reduce generic costs.)"), the CR 118.7b default.
fn spillover_reducer() -> StaticDefinition {
    StaticDefinition::new(StaticMode::ModifyCost {
        mode: CostModifyMode::Reduce,
        amount: wubrg(),
        spell_filter: None,
        dynamic_count: None,
        reach: CostReductionReach::SpillsToGeneric,
    })
}

fn add_targeted_spell(scenario: &mut GameScenario, name: &str, cost: ManaCost) -> ObjectId {
    let mut b = scenario.add_spell_to_hand(P0, name, true);
    b.with_mana_cost(cost);
    b.with_ability(Effect::DealDamage {
        amount: QuantityExpr::Fixed { value: 2 },
        target: TargetFilter::Any,
        damage_source: None,
        excess: None,
    });
    b.id()
}

/// Cast the spell and return the mana value of the cost the engine locked in
/// (read at `TargetSelection`, before payment).
fn resolved_cost_mv(runner: &mut GameRunner, spell_id: ObjectId) -> u32 {
    let card_id = runner.state().objects[&spell_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell_id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the test spell should begin");
    match &runner.state().waiting_for {
        WaitingFor::TargetSelection { pending_cast, .. } => pending_cast.cost.mana_value(),
        other => panic!("expected TargetSelection after casting, got {other:?}"),
    }
}

fn resolved_cost_under(reducer: StaticDefinition, name: &str, cost: ManaCost) -> u32 {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain); // active player = P0
    scenario
        .add_creature(P0, "Morophon, the Boundless", 6, 6)
        .with_static_definition(reducer);
    let spell = add_targeted_spell(&mut scenario, name, cost);
    let mut runner = scenario.build();
    resolved_cost_mv(&mut runner, spell)
}

/// The Discord report, both halves in one assertion pair so the test cannot go
/// green with the fix reverted. Morophon names Sliver:
///   * Sliver Queen / The First Sliver {W}{U}{B}{R}{G} — every unit matches a
///     pip, spell is free. This half was already correct and is pinned here so
///     the fix cannot over-correct into "colored-only means no discount".
///   * Muscle Sliver / Manaweft Sliver {1}{G} — the {G} unit cancels the green
///     pip and the {W}{U}{B}{R} units have nothing to match. Under the rider
///     they are LOST, so the {1} survives. This is the half that was wrong.
#[test]
fn reported_sliver_costs_reduce_only_their_colored_mana() {
    assert_eq!(
        resolved_cost_under(
            morophon_reducer(),
            "Muscle Sliver",
            ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 1,
            },
        ),
        1,
        "{{1}}{{G}} must cost {{1}} under Morophon — the four unmatched colored \
         units may not be spent on generic mana",
    );
    assert_eq!(
        resolved_cost_under(morophon_reducer(), "Sliver Queen", wubrg()),
        0,
        "a {{W}}{{U}}{{B}}{{R}}{{G}} spell must still be free under Morophon",
    );
}

/// Morophon's printed ruling, verbatim: {4}{R}{W}{W} becomes {4}{W}.
/// Exercises CR 118.7c's "excess" half — the second {W} pip has no second {W}
/// reduction unit to cancel it, and the unmatched {U}/{B}/{G} units must not
/// eat into the {4}.
#[test]
fn morophon_ruling_example_four_r_w_w_becomes_four_w() {
    assert_eq!(
        resolved_cost_under(
            morophon_reducer(),
            "Test Ruling Spell",
            ManaCost::Cost {
                shards: vec![
                    ManaCostShard::Red,
                    ManaCostShard::White,
                    ManaCostShard::White,
                ],
                generic: 4,
            },
        ),
        5,
        "Morophon's ruling: {{4}}{{R}}{{W}}{{W}} costs {{4}}{{W}} (mana value 5)",
    );
}

/// An all-generic spell has nothing for any unit to match, so the colored-only
/// rider means NO discount at all. Paired with the CR 118.7b control on the same
/// cost, which must still go free — that pairing is what makes this test red on
/// revert, since reverting collapses the two arms onto the same answer.
#[test]
fn all_generic_spell_gets_no_discount_but_the_default_reach_still_does() {
    assert_eq!(
        resolved_cost_under(
            morophon_reducer(),
            "Test Generic Spell",
            ManaCost::generic(5)
        ),
        5,
        "a colorless-costed spell gets nothing from a colored-only reduction",
    );
    assert_eq!(
        resolved_cost_under(
            spillover_reducer(),
            "Test Generic Spell",
            ManaCost::generic(5)
        ),
        0,
        "CR 118.7b spillover must still apply to a reduction with no colored-only rider",
    );
}

/// A colored-only reduction that also carries an explicit generic component
/// ({1}{W}) may not spend that component either: CR 118.7a already bars it from
/// touching colored mana, and the printed rider bars it from touching generic
/// mana, so only the white pip is cancelled. No printed card pairs the rider
/// with a generic component today; this pins the axis so a future one is right.
/// The CR 118.7b control arm shows the same amount consuming the generic.
#[test]
fn colored_only_reduction_does_not_spend_its_generic_component() {
    let one_white = ManaCost::Cost {
        shards: vec![ManaCostShard::White],
        generic: 1,
    };
    let reducer = |reach| {
        StaticDefinition::new(StaticMode::ModifyCost {
            mode: CostModifyMode::Reduce,
            amount: one_white.clone(),
            spell_filter: None,
            dynamic_count: None,
            reach,
        })
    };

    // {2}{W} reduced by a colored-only {1}{W}: the {W} pip goes, the {2} stays.
    assert_eq!(
        resolved_cost_under(
            reducer(CostReductionReach::ColoredManaOnly),
            "Test Mixed Reduction",
            ManaCost::Cost {
                shards: vec![ManaCostShard::White],
                generic: 2,
            },
        ),
        2,
        "the rider confines a {{1}}{{W}} reduction to the white pip",
    );
    // CR 118.7a control: without the rider the {1} also lands.
    assert_eq!(
        resolved_cost_under(
            reducer(CostReductionReach::SpillsToGeneric),
            "Test Mixed Reduction",
            ManaCost::Cost {
                shards: vec![ManaCostShard::White],
                generic: 2,
            },
        ),
        1,
        "CR 118.7a: with no rider the {{1}} reduces generic mana as well",
    );
}

// --- End-to-end: printed Oracle text → parser → casting pipeline -------------
//
// The tests above drive the arithmetic with the reducer's filter already
// satisfied. These close the loop on the printed cards whose spell filter a
// scenario can satisfy directly, so the Oracle sentence, the parser's
// `CostReductionReach`, and the casting-pipeline cost lock are all exercised by
// one assertion. Both cards state the expected result in their own reminder
// text, so the numbers here are the cards' own worked examples.

/// Build `line`'s static, put a creature spell of `subtype` costing `cost` in
/// hand alongside `lands` basic lands of `land_color`, cast it through the
/// normal pipeline, and return how many lands the engine had to tap.
fn lands_tapped_casting_under(
    line: &str,
    subtype: &str,
    cost: ManaCost,
    land_color: ManaColor,
    lands: usize,
) -> usize {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land_ids: Vec<ObjectId> = (0..lands)
        .map(|_| scenario.add_basic_land(P0, land_color))
        .collect();
    scenario
        .add_creature(P0, "Cost Reducer", 2, 2)
        .with_static_definition(
            parse_static_line(line).unwrap_or_else(|| panic!("line must parse: {line}")),
        );
    let spell_id = scenario
        .add_creature_to_hand(P0, "Test Spell", 2, 2)
        .with_mana_cost(cost)
        .with_subtypes(vec![subtype])
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(spell_id).resolve();
    land_ids.iter().filter(|&&id| outcome.is_tapped(id)).count()
}

/// Edgewalker's own reminder text: "if you cast a Cleric spell with mana cost
/// {1}{W}, it costs {1} to cast." The {W} unit cancels the white pip and the
/// {B} unit — having no black pip to cancel — is lost rather than eating the
/// {1}. One Plains taps; before the fix, none did.
///
/// The second arm is this test's reach guard, in the same function so it cannot
/// drift: the SAME sentence minus the rider must still spill over and make the
/// spell free. That proves the Cleric filter really matched and the reduction
/// really ran, so the first arm's result cannot come from the spell quietly
/// failing to match and going unreduced by some other route.
#[test]
fn edgewalker_reminder_text_example_end_to_end() {
    let cleric_cost = ManaCost::Cost {
        shards: vec![ManaCostShard::White],
        generic: 1,
    };
    assert_eq!(
        lands_tapped_casting_under(
            "Cleric spells you cast cost {W}{B} less to cast. This effect reduces only the amount of colored mana you pay.",
            "Cleric",
            cleric_cost.clone(),
            ManaColor::White,
            3,
        ),
        1,
        "Edgewalker: a {{1}}{{W}} Cleric spell must still cost {{1}}",
    );
    assert_eq!(
        lands_tapped_casting_under(
            "Cleric spells you cast cost {W}{B} less to cast.",
            "Cleric",
            cleric_cost,
            ManaColor::White,
            3,
        ),
        0,
        "reach guard — CR 118.7b: with no rider the unmatched {{B}} consumes the \
         {{1}} and the same spell is free, proving the filter matched",
    );
}

/// Ragemonger's own reminder text: "if you cast a Minotaur spell with mana cost
/// {2}{R}, it costs {2} to cast." Two Mountains tap; before the fix, none did —
/// the unmatched {B} plus the surplus would have consumed the whole {2}.
///
/// The land count is its own reach guard: 3 tapped would mean the Minotaur
/// filter never matched and the cost went unreduced, 0 would mean the rider was
/// ignored, and only 2 means the reduction ran AND stayed on colored mana.
#[test]
fn ragemonger_reminder_text_example_end_to_end() {
    assert_eq!(
        lands_tapped_casting_under(
            "Minotaur spells you cast cost {B}{R} less to cast. This effect reduces only the amount of colored mana you pay.",
            "Minotaur",
            ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 2,
            },
            ManaColor::Red,
            4,
        ),
        2,
        "Ragemonger: a {{2}}{{R}} Minotaur spell must still cost {{2}}",
    );
}
