//! Issue #6405: Aang, Master of Elements — "Spells you cast cost {W}{U}{B}{R}{G}
//! less to cast. (This can reduce generic costs.)" was not reducing generic
//! mana at all. The reduction is stored as five colored shards with a generic
//! component of 0, and `apply_shard_reduction` used to silently drop any
//! reduction unit that had no matching colored pip left in the spell's cost,
//! instead of letting it spill over to generic mana.
//!
//! CR 118.7b: a colored reduction unit with no matching component in the cost
//! converts to reducing generic mana — this is also why a reduction never
//! touches a mismatched color's pip. CR 118.7c: a colored reduction that
//! exceeds the cost's component of that color reduces the color to nothing,
//! then spills the excess to generic.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::parser::oracle_static::parse_static_line;
use engine::types::ability::{Effect, QuantityExpr, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;

const AANG_STATIC: &str = "Spells you cast cost {W}{U}{B}{R}{G} less to cast.";

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

/// Cast the spell and return the mana value of the battlefield-modified cost
/// the engine resolved (read at `TargetSelection`, before payment).
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

/// Build a P0 board with Aang's cost reducer plus one test spell of `cost`,
/// cast it, and return the resolved cost's mana value.
fn resolved_cost_under_aang(name: &str, cost: ManaCost) -> u32 {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain); // active player = P0
    scenario
        .add_creature(P0, "Aang, Master of Elements", 6, 6)
        .with_static_definition(parse_static_line(AANG_STATIC).expect("Aang static parses"));
    let spell = add_targeted_spell(&mut scenario, name, cost);
    let mut runner = scenario.build();
    resolved_cost_mv(&mut runner, spell)
}

#[test]
fn all_generic_spell_is_reduced_by_the_full_five() {
    // CR 118.7b: none of the five colors are present, so all five reduction
    // units convert to generic — {5} generic drops to {0}.
    assert_eq!(
        resolved_cost_under_aang("Test Generic Spell", ManaCost::generic(5)),
        0,
        "a spell with no colored pips must still get the full 5-mana discount",
    );
}

#[test]
fn partially_generic_spell_spills_excess_beyond_matching_color() {
    // CR 118.7c: the lone red pip cancels one reduction unit; the other four
    // (white/blue/black/green) have nothing to match, so they spill to
    // generic — {2}{R} (mana value 3) drops to {0}, not {2}.
    assert_eq!(
        resolved_cost_under_aang(
            "Test Red Spell",
            ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 2,
            },
        ),
        0,
        "unmatched colored reduction units must spill over to generic mana",
    );
}

#[test]
fn one_of_each_color_plus_generic_reduces_only_the_matched_pips() {
    // Every one of the five reduction units matches an exact colored pip, so
    // none spill over — the {1} generic component is untouched.
    assert_eq!(
        resolved_cost_under_aang(
            "Test Rainbow Spell",
            ManaCost::Cost {
                shards: vec![
                    ManaCostShard::White,
                    ManaCostShard::Blue,
                    ManaCostShard::Black,
                    ManaCostShard::Red,
                    ManaCostShard::Green,
                ],
                generic: 1,
            },
        ),
        1,
        "a fully-matched colored reduction must not touch the remaining generic mana",
    );
}

#[test]
fn cheap_generic_spell_floors_at_zero_rather_than_underflowing() {
    // Five spillover-eligible reduction units against a {1} generic spell must
    // floor the total at {0}, not underflow past zero.
    assert_eq!(
        resolved_cost_under_aang("Test Cheap Spell", ManaCost::generic(1)),
        0,
        "reduction spillover must floor generic at 0, never go negative",
    );
}
