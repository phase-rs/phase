//! Bare color-category spell-cost modifiers ("Colorless/Multicolored spells …
//! cost {N} less to cast") must scope the reduction to the color category, not
//! every spell. Herald of Kozilek / Ugin, the Ineffable / Urza's Filter / It That
//! Heralds the End previously parsed with `spell_filter: None`, so the modifier
//! cheapened EVERY spell.
//!
//! CR 105.2: an object's colors; a colorless object has zero colors
//! (`ColorCount { EQ, 0 }`). CR 601.2f: cost reductions apply during cost
//! determination. The bare-word fallback now routes color words through the
//! single `parse_color_property` authority, so a bare "Colorless" resolves to the
//! same `ColorCount { EQ, 0 }` filter the noun-bearing path ("Colorless creature
//! spells") already produced.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::parser::oracle_static::parse_static_line;
use engine::types::ability::{Effect, QuantityExpr, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;

const HERALD: &str = "Colorless spells you cast cost {1} less to cast.";

/// Add a targeting instant (so casting surfaces `TargetSelection`, where the
/// battlefield-modified cost is readable) with an explicit mana cost + derived
/// color.
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

/// Cast the spell and return the mana value of the battlefield-modified cost the
/// engine resolved (read at `TargetSelection`, before payment).
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

/// Build a P0 board with the bare-colorless cost reducer plus one test spell of
/// `cost`, cast it, and return the resolved cost's mana value.
fn resolved_cost_under_reducer(name: &str, cost: ManaCost) -> u32 {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain); // active player = P0
    scenario
        .add_creature(P0, "Colorless Cost Reducer", 2, 4)
        .with_static_definition(parse_static_line(HERALD).expect("HERALD static parses"));
    let spell = add_targeted_spell(&mut scenario, name, cost);
    let mut runner = scenario.build();
    resolved_cost_mv(&mut runner, spell)
}

#[test]
fn bare_colorless_static_parses_to_color_count_filter() {
    // The bare-word subject must resolve to a color-category filter, not None.
    let def = parse_static_line(HERALD).expect("HERALD static parses");
    let StaticMode::ModifyCost { spell_filter, .. } = def.mode else {
        panic!("expected ModifyCost, got {:?}", def.mode);
    };
    let filter = spell_filter.expect("colorless restriction must not be dropped (was `None`)");
    let dbg = format!("{filter:?}");
    assert!(
        dbg.contains("ColorCount") && dbg.contains("EQ") && dbg.contains("0"),
        "bare \"Colorless spells\" must carry ColorCount{{EQ,0}}, got {dbg}",
    );
}

#[test]
fn bare_colorless_reducer_discounts_a_colorless_spell() {
    // A {3}-generic (colorless) spell is reduced by {1} → mana value 2.
    assert_eq!(
        resolved_cost_under_reducer("Test Colorless Spell", ManaCost::generic(3)),
        2,
        "a colorless spell must get the {{1}} discount (ColorCount{{EQ,0}} matches)",
    );
}

#[test]
fn bare_colorless_reducer_does_not_discount_a_colored_spell() {
    // A {2}{R} (red) spell must NOT be reduced → mana value stays 3. This is the
    // revert-failing assertion: before the fix the static parsed with
    // `spell_filter: None` and reduced EVERY spell, dropping this to 2.
    assert_eq!(
        resolved_cost_under_reducer(
            "Test Red Spell",
            ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 2,
            },
        ),
        3,
        "a colored spell must NOT get the colorless-only discount",
    );
}
