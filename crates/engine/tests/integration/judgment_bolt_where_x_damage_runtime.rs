//! Judgment Bolt — RUNTIME witness for the compound where-X damage chain fix.
//!
//! Oracle (verbatim, card-data.json): "Judgment Bolt deals 5 damage to target
//! creature and X damage to that creature's controller, where X is the number
//! of Equipment you control."
//!
//! On main the trailing ", where X is …" clause made
//! `try_parse_multi_target_damage_chain_inner` bail, so the 2nd damage clause
//! (X to the creature's controller) was swallowed entirely and the spell dealt
//! only 5 to the creature. The parser now strips the where-X tail so the
//! bare-damage continuation loop chains the 2nd clause; the chain-IR lowering
//! binds X to the dynamic Equipment count.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 120.3a: damage dealt to a player causes that player to lose that much life.
//!   - CR 109.4:  "that creature's controller" is the controller of the target.
//!   - CR 107.3i: every instance of X takes the same value, computed once.
//!
//! Discrimination: the 2nd clause deals X = (Equipment P0 controls) to the
//! target's controller (P1). Two Equipment → P1 loses 2; zero Equipment → P1
//! loses 0. Reverting the where-X strip drops the 2nd clause, so P1 loses 0 in
//! BOTH cases and the `equipment == 2` assertion flips to fail. The extra
//! non-Equipment permanent guards against a "count all permanents you control"
//! mis-bind (which would read one higher).

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const JUDGMENT_BOLT: &str = "Judgment Bolt deals 5 damage to target creature and \
X damage to that creature's controller, where X is the number of Equipment you control.";

/// Cast Judgment Bolt at a P1 creature while P0 controls `equipment` Equipment.
/// Returns `(damage marked on the creature, P1 life delta)`.
fn cast_with_equipment(equipment: usize) -> (u32, i32) {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // Target creature belongs to P1, so "that creature's controller" is P1 (not
    // the caster). Toughness 6 so it survives the 5 damage and its marked
    // damage remains observable.
    let creature = scenario.add_creature(P1, "Stone Sentinel", 4, 6).id();

    // P0's Equipment — the dynamic count "X". Modeled as artifact-creatures that
    // carry the Equipment subtype; the ObjectCount filter matches on subtype.
    for i in 0..equipment {
        scenario
            .add_creature(P0, &format!("Bladed Relic {i}"), 1, 1)
            .as_artifact()
            .with_subtypes(vec!["Equipment"]);
    }
    // A non-Equipment permanent P0 controls: a mis-bind that counted "all
    // permanents you control" instead of Equipment would read one higher.
    scenario.add_vanilla(P0, 2, 2);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Judgment Bolt", true, JUDGMENT_BOLT)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_object(creature).resolve();
    (outcome.damage_marked(creature), outcome.life_delta(P1))
}

#[test]
fn judgment_bolt_deals_five_and_equipment_count_to_controller() {
    let (dmg2, life2) = cast_with_equipment(2);
    assert_eq!(dmg2, 5, "primary clause always deals 5 to the creature");
    assert_eq!(
        life2, -2,
        "2nd clause deals X = 2 Equipment to the creature's controller (P1); \
         0 here means the where-X damage chain was swallowed (fix reverted)"
    );

    let (dmg0, life0) = cast_with_equipment(0);
    assert_eq!(dmg0, 5, "primary clause still deals 5 with zero Equipment");
    assert_eq!(
        life0, 0,
        "X = 0 Equipment → 0 damage to the controller (CR 107.1b, non-negative)"
    );
}
