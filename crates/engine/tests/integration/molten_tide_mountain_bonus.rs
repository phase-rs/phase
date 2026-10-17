//! Molten Tide (FRA Instant {R}): "Until end of turn, whenever you tap a
//! Mountain for mana, add an additional {R}."
//!
//! Runtime proof that the "you tap … for mana" delayed-trigger actor only
//! fires when the trigger source's controller taps their own Mountain, pays
//! the bonus into the tapper's (= controller's) pool, fires repeatedly, and
//! expires at end of turn.

use engine::game::mana_abilities::activate_mana_ability;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::{check_delayed_triggers, process_triggers};
use engine::game::turns::execute_cleanup;
use engine::types::events::{GameEvent, ManaTapState};
use engine::types::game_state::ManaAbilityResume;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const MOLTEN_TIDE_ORACLE: &str =
    "Until end of turn, whenever you tap a Mountain for mana, add an additional {R}.";

fn red(runner: &GameRunner, p: PlayerId) -> usize {
    runner.state().players[p.0 as usize]
        .mana_pool
        .count_color(ManaType::Red)
}

fn clear_pools(runner: &mut GameRunner) {
    for p in [P0, P1] {
        runner.state_mut().players[p.0 as usize].mana_pool.clear();
    }
}

/// Deposit one base {R} into `p`'s pool, simulating the land's own production
/// (CR 106.12a) ahead of a synthetic `TappedForMana` boundary input. The
/// delayed trigger under test only ever adds the BONUS, so each boundary row
/// asserts base + 0-or-1 and the base deposit keeps a no-fire row honest: a
/// pool that stays at exactly 1 proves the matcher declined, not that the
/// fixture never produced.
fn deposit_base_red(runner: &mut GameRunner, p: PlayerId, source: ObjectId) {
    let _ = runner
        .state_mut()
        .add_mana_to_pool(p, ManaUnit::new(ManaType::Red, source, false, vec![]));
}

#[test]
fn molten_tide_bonus_only_for_controller_taps_own_mountain_then_expires() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tide = scenario
        .add_spell_to_hand_from_oracle(P0, "Molten Tide", true, MOLTEN_TIDE_ORACLE)
        .id();
    let mtn0 = scenario.add_basic_land(P0, ManaColor::Red);
    let mtn1 = scenario.add_basic_land(P0, ManaColor::Red);
    let mtn2 = scenario.add_basic_land(P0, ManaColor::Red);
    let mtn_opp = scenario.add_basic_land(P1, ManaColor::Red);
    // Fund P0's {R} to cast Molten Tide through the real pipeline (registers
    // the delayed trigger authentically).
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Red, ObjectId(9999), false, vec![])],
    );
    let mut runner = scenario.build();
    runner.cast(tide).resolve();

    // Positive reach-guard FIRST: the instant registered exactly one
    // until-end-of-turn delayed trigger on resolution.
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "Molten Tide must register its delayed trigger on resolution"
    );
    clear_pools(&mut runner);

    // (1) P0 taps its own Mountain for mana. The activation resolves the
    // TapsForMana bonus inline (tap_state FromTapTriggersResolved), so the
    // land's base {R} plus the trigger's additional {R} land in P0's pool
    // (CR 106.4: the subjectless body deposits into the controller's pool;
    // CR 106.12a: the tap event). `process_triggers` over the same batch is
    // a no-op second pass (resolved marker), mirroring the
    // issue_6979_land_mana_amplification driving — it must NOT add again.
    let ability = runner.state().objects[&mtn0].abilities[0].clone();
    let mut events = Vec::new();
    activate_mana_ability(
        runner.state_mut(),
        mtn0,
        P0,
        0,
        &ability,
        &mut events,
        ManaAbilityResume::Priority,
        None,
    )
    .expect("P0 taps its own Mountain for mana");
    process_triggers(runner.state_mut(), &events);
    assert_eq!(
        red(&runner, P0),
        2,
        "P0 taps its own Mountain: base {{R}} + additional {{R}} = 2 red"
    );
    assert_eq!(red(&runner, P1), 0, "P1 gets nothing when P0 taps");

    // (2) Multi-fire: P0 taps a second Mountain the same turn for another +1.
    let ability = runner.state().objects[&mtn1].abilities[0].clone();
    let mut events = Vec::new();
    activate_mana_ability(
        runner.state_mut(),
        mtn1,
        P0,
        0,
        &ability,
        &mut events,
        ManaAbilityResume::Priority,
        None,
    )
    .expect("P0 taps a second Mountain for mana");
    process_triggers(runner.state_mut(), &events);
    assert_eq!(
        red(&runner, P0),
        4,
        "second Mountain the same turn fires again: 2 + base {{R}} + additional {{R}} = 4 red"
    );

    // (3) SAME-PATH POSITIVE via the delayed-matcher API: an otherwise
    // identical P0-taps-P0's-Mountain event through `check_delayed_triggers`
    // (the Any-scope delayed matcher) must yield base + bonus. This is the
    // reach guard for rows (4)-(5): it proves synthetic `TappedForMana`
    // input reaches Molten Tide's delayed matcher at all, so a hostile row
    // that stays at base is a true matcher decline, not a delivery bypass
    // (e.g. the trigger never registered, or the event never scanned).
    clear_pools(&mut runner);
    deposit_base_red(&mut runner, P0, mtn0);
    check_delayed_triggers(
        runner.state_mut(),
        &[GameEvent::TappedForMana {
            player_id: P0,
            source_id: mtn0,
            produced: vec![ManaType::Red],
            tap_state: ManaTapState::FromTap,
        }],
    );
    assert_eq!(
        red(&runner, P0),
        2,
        "same-path positive: P0 taps P0's Mountain via check_delayed_triggers = base {{R}} + bonus {{R}}"
    );
    assert_eq!(red(&runner, P1), 0, "P1 gets nothing when P0 taps");

    // (4) HOSTILE F1 (matcher boundary): P1 taps a Mountain CONTROLLED BY
    // P0. The tapper is not the trigger source's controller
    // (valid_target = Controller), so the matcher must decline and NEITHER
    // pool gains beyond the simulated base. Boundary label: CR 602.2 lets
    // only a land's controller activate its ability, so no legal activation
    // produces this event pair — it probes the matcher filter directly,
    // while CR 106.12a fixes the event shape a real tap would emit.
    // Revert check: deleting the `valid_target = Controller` gate flips the
    // P0 assert below from 0 to 1 (bonus misfires to the controller, which
    // holds no base here).
    clear_pools(&mut runner);
    deposit_base_red(&mut runner, P1, mtn0);
    check_delayed_triggers(
        runner.state_mut(),
        &[GameEvent::TappedForMana {
            player_id: P1,
            source_id: mtn0,
            produced: vec![ManaType::Red],
            tap_state: ManaTapState::FromTap,
        }],
    );
    assert_eq!(
        red(&runner, P0),
        0,
        "no bonus in P0's pool when P1 taps P0's Mountain"
    );
    assert_eq!(
        red(&runner, P1),
        1,
        "P1 keeps exactly the simulated base — no bonus misfires and none misroutes"
    );

    // (5) HOSTILE F2 (matcher boundary): P0 taps an OPPONENT-controlled
    // Mountain. The source is not You-controlled (valid_card controller
    // gate), so the matcher must decline. Same CR 602.2 / CR 106.12a
    // boundary label as F1.
    // Revert check: deleting `add_controller(filter, You)` flips the P0
    // assert below from 1 to 2.
    clear_pools(&mut runner);
    deposit_base_red(&mut runner, P0, mtn_opp);
    check_delayed_triggers(
        runner.state_mut(),
        &[GameEvent::TappedForMana {
            player_id: P0,
            source_id: mtn_opp,
            produced: vec![ManaType::Red],
            tap_state: ManaTapState::FromTap,
        }],
    );
    assert_eq!(
        red(&runner, P0),
        1,
        "no bonus when P0 taps an opponent-controlled Mountain — only the simulated base remains"
    );
    assert_eq!(red(&runner, P1), 0, "P1 gains nothing either");

    // (6) Expiry: end-of-turn cleanup purges the multi-fire delayed trigger
    // (CR 603.7b); a later Mountain tap yields only the base {R}.
    let mut cleanup_events = Vec::new();
    execute_cleanup(runner.state_mut(), &mut cleanup_events);
    assert_eq!(
        runner.state().delayed_triggers.len(),
        0,
        "the until-end-of-turn delayed trigger must be purged at cleanup"
    );
    clear_pools(&mut runner);
    let ability = runner.state().objects[&mtn2].abilities[0].clone();
    let mut events = Vec::new();
    activate_mana_ability(
        runner.state_mut(),
        mtn2,
        P0,
        0,
        &ability,
        &mut events,
        ManaAbilityResume::Priority,
        None,
    )
    .expect("P0 taps a Mountain after cleanup");
    process_triggers(runner.state_mut(), &events);
    assert_eq!(
        red(&runner, P0),
        1,
        "after cleanup: only the base {{R}}, no bonus — the delayed trigger expired"
    );
}
