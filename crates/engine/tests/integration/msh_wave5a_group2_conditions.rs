//! MSH Wave 5a — Group II: Unrecognized static-condition misparses (real,
//! always-on bugs).
//!
//! Two MSH/MSC statics parsed to `StaticCondition::Unrecognized`, which the
//! layer system treats as ALWAYS-TRUE (`layers.rs` `Unrecognized => true`), so
//! the gated modification applied unconditionally:
//!
//!   - II-A Intrepid Ace: "this creature gets +2/+0 as long as it isn't
//!     attacking or blocking" parsed `Unrecognized{"it isn't attacking or
//!     blocking"}` → permanent +2/+0 even while attacking.
//!   - II-B Captain America, Super-Soldier: "as long as ~ has a shield counter
//!     on him, you and other Heroes you control have hexproof" parsed
//!     `Unrecognized{"~ has a shield counter on him"}` → permanent hexproof even
//!     with no shield counter.
//!
//! Both tests drive the REAL parse → synthesis → layer pipeline via
//! `add_real_card` (the deployed card-data export) + `rehydrate` and read the
//! EFFECTIVE post-layer characteristics. They FAIL on the pre-fix export
//! (always-on) and PASS once the corrected conditions are re-exported.

use super::rules::AttackTarget;
use super::support::shared_card_db;
use engine::game::keywords::has_keyword;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn recompute(runner: &mut GameRunner) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
}

fn power(runner: &GameRunner, id: ObjectId) -> i32 {
    runner.state().objects[&id].power.expect("creature power")
}

/// CR 611.3a: Intrepid Ace's "+2/+0 as long as it isn't attacking or blocking"
/// must gate on the source's combat state, not apply permanently. Not in combat
/// → +2/+0; declared as an attacker (driving the real combat pipeline so
/// `SourceIsAttacking` is set) → buff lapses, back to base power. Discriminating:
/// revert the `tag("it ")` arm in `parse_self_source_subject` and the condition
/// re-parses to `Unrecognized` (always-true), so the attacking assertion drops
/// from base to base+2 and fails.
#[test]
fn intrepid_ace_buff_lapses_while_attacking() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ace = scenario.add_real_card(P0, "Intrepid Ace", Zone::Battlefield, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // Not attacking/blocking → +2/+0 active.
    recompute(&mut runner);
    let buffed = power(&runner, ace);

    // Declare Intrepid Ace as an attacker through the real combat pipeline.
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(ace, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");
    runner.advance_until_stack_empty();
    recompute(&mut runner);
    let attacking = power(&runner, ace);

    assert_eq!(
        buffed - attacking,
        2,
        "the +2/+0 must lapse while attacking: idle power {buffed}, attacking power {attacking}"
    );
}

/// CR 122.1: Captain America's "as long as ~ has a shield counter on him, you
/// and other Heroes you control have hexproof" must gate on the shield counter,
/// not grant hexproof unconditionally. Captain America enters with a shield
/// counter (ETB replacement), so a Hero you control gains Hexproof; removing the
/// counter ends the grant. Discriminating: revert the gendered-pronoun arm in
/// `parse_has_counters_axes` and the condition re-parses to `Unrecognized`
/// (always-true), so the "counter removed" assertion still reports Hexproof and
/// fails.
#[test]
fn captain_america_hexproof_gates_on_shield_counter() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let cap = scenario.add_real_card(P0, "Captain America, Super-Soldier", Zone::Battlefield, db);
    let hero = scenario
        .add_creature(P0, "Allied Hero", 2, 2)
        .with_subtypes(vec!["Human", "Hero"])
        .id();
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // Captain America entered with a shield counter (CR 614.1c: "enters with"
    // replacement; CR 122.6a: controller places the counter).
    let shields = runner.state().objects[&cap]
        .counters
        .get(&CounterType::Shield)
        .copied()
        .unwrap_or(0);
    assert!(
        shields >= 1,
        "Captain America should enter with a shield counter (got {shields})"
    );

    // Gate ON: the other Hero you control has Hexproof.
    recompute(&mut runner);
    assert!(
        has_keyword(&runner.state().objects[&hero], &Keyword::Hexproof),
        "with a shield counter, an allied Hero gains Hexproof"
    );

    // Remove the shield counter → gate OFF.
    runner
        .state_mut()
        .objects
        .get_mut(&cap)
        .unwrap()
        .counters
        .remove(&CounterType::Shield);
    recompute(&mut runner);
    assert!(
        !has_keyword(&runner.state().objects[&hero], &Keyword::Hexproof),
        "removing the shield counter ends the Hexproof grant"
    );
}
