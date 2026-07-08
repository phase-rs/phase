//! Runtime no-regression companion to issue #1375 — the "that CREATURE's mana
//! value" where-X binding must stay resolved against the ability's TARGET.
//!
//! The #1375 fix narrows the demonstrative reroute to the literal "that card's
//! mana value" only. "That creature's mana value" (Living Armor, Feeding
//! Grounds, Surge of Strength, …) is a genuinely targeted reference — the
//! creature is the ability's `target creature` — and must keep binding
//! `ObjectScope::Target`. Flipping it to `Demonstrative` would read an empty
//! effect-context slot and resolve X to 0.
//!
//! Living Armor: "{T}, Sacrifice this artifact: Put X +0/+1 counters on target
//! creature, where X is that creature's mana value." Targeting a mana-value-5
//! creature must place exactly 5 counters — proving X still resolves against
//! the target after the #1375 change.
//!
//! Guard load-bearing: Living Armor's ability is LIVE-PARSED here (it is not in
//! the base fixture), so the `lower.rs` where-X guard governs the "that
//! creature's mana value" scope at test time. The target creature is Baneslayer
//! Angel, a real DB card, so it carries a genuine mana value of 5. This keeps
//! the fixture diff at zero.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 115.10a: an object identified by "target" IS a target.
//!   - CR 202.3: the mana value of an object equals the total mana in its cost.
//!   - CR 122.1a: +0/+1 counters modify power/toughness by independent deltas.

use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::counter::CounterType;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// Living Armor's real activated-ability Oracle text, LIVE-PARSED so the
/// `lower.rs` where-X guard governs the "that creature's mana value" scope at
/// runtime — proving the #1375 change leaves the targeted binding on `Target`.
const LIVING_ARMOR_ORACLE: &str = "{T}, Sacrifice this artifact: Put X +0/+1 counters on target creature, where X is that creature's mana value.";

/// CR 115.10a + CR 202.3 — Living Armor puts X +0/+1 counters on the targeted
/// creature, where X is that (targeted) creature's mana value. A mana-value-5
/// target receives exactly 5 counters. This is unaffected by the #1375 fix,
/// which reroutes only "that card's mana value".
#[test]
fn living_armor_puts_target_creature_mana_value_counters() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // LIVE PARSE Living Armor (a 0/0 Artifact carrier for the activated ability)
    // so the where-X guard is load-bearing. `as_artifact` strips the placeholder
    // Creature type; the ability text is parsed by `from_oracle_text`.
    let armor = scenario
        .add_creature_from_oracle(P0, "Living Armor", 0, 0, LIVING_ARMOR_ORACLE)
        .as_artifact()
        .id();
    // Baneslayer Angel is {3}{W}{W} — mana value 5. A real DB card so it carries
    // a genuine mana value; it is the ability's target creature.
    let target = scenario.add_real_card(P0, "Baneslayer Angel", Zone::Battlefield, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    assert_eq!(
        runner
            .state()
            .objects
            .get(&target)
            .unwrap()
            .mana_cost
            .mana_value(),
        5,
        "precondition: the target creature (Baneslayer Angel) has mana value 5"
    );

    // {T}, Sacrifice this artifact — the cost resolver taps + sacrifices Living
    // Armor itself; the only chosen target is the creature.
    let outcome = runner.activate(armor, 0).target_object(target).resolve();

    // CR 202.3: X = the targeted creature's mana value (5).
    outcome.assert_counters(
        target,
        CounterType::PowerToughness {
            power: 0,
            toughness: 1,
        },
        5,
    );
}
