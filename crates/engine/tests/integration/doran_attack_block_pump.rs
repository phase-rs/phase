//! Issue #417 — Doran, Besieged by Time.
//!
//! Doran's second ability reads "Whenever a creature you control attacks or
//! blocks, it gets +X/+X until end of turn, where X is the difference between
//! its power and toughness."
//!
//! The parser previously dropped the `where X is …` tail into a verbatim
//! `PtValue::Variable("the difference between its power and toughness")` — the
//! `unwrap_or_else` fallback in `oracle_effect/mod.rs` — because
//! `parse_cda_quantity` had no combinator for the difference phrase. At runtime
//! that `Variable` resolved to 0, so the pump did nothing.
//!
//! The fix teaches `parse_cda_quantity` to emit a typed
//! `QuantityExpr::Difference { Ref(Power{Recipient}), Ref(Toughness{Recipient}) }`,
//! which `apply_where_x_expression` wraps as `PtValue::Quantity`.
//!
//! These tests drive the real combat pipeline through `apply`:
//! declare-attackers/blockers → trigger → stack → resolution → layer system.
//! They assert the *post-layer* `power`/`toughness` of the pumped creature
//! (the layer system writes effective P/T into `obj.power`/`obj.toughness`),
//! never the parsed `Pump` effect.
//!
//! CR 208.1 (a creature's power and toughness), CR 611.2d + CR 608.2h (X is
//! determined once, on trigger resolution, from post-layer values),
//! CR 613.4c (the +X/+X pump is a Layer 7c modification). "The difference
//! between A and B" being unsigned is an Oracle templating convention with no
//! dedicated CR number — the resolver takes `.abs()` of the gap.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::ContinuousModification;
use engine::types::actions::GameAction;
use engine::types::format::FormatConfig;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::PlayerId;

use super::rules::AttackTarget;
use super::support::shared_card_db;

/// Doran's "attacks or blocks" pump line only — the static "assign combat
/// damage equal to toughness" ability is irrelevant to this trigger and is
/// omitted so the test exercises exactly the `where X is` pump path.
const DORAN_PUMP: &str = "Whenever a creature you control attacks or blocks, \
it gets +X/+X until end of turn, where X is the difference between its power \
and toughness.";

/// Drive the runner from a PreCombatMain start through declaring `attacker` as
/// an attacker, then resolve the stack so Doran's trigger applies.
fn declare_attacker(runner: &mut engine::game::scenario::GameRunner, attacker: ObjectId) {
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");
    runner.advance_until_stack_empty();
}

/// CR 611.2d + CR 613.4c: a 0/4 attacks; X = |0 − 4| = 4; the attacker becomes
/// an effective 4/8 after the trigger resolves and the layer system applies the
/// +X/+X modification.
#[test]
fn doran_pumps_attacking_creature_by_pt_difference() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature_from_oracle(P0, "Doran, Besieged by Time", 0, 5, DORAN_PUMP)
        .id();
    let attacker = scenario.add_creature(P0, "Wall of Roots", 0, 4).id();

    let mut runner = scenario.build();
    declare_attacker(&mut runner, attacker);

    let obj = &runner.state().objects[&attacker];
    assert_eq!(
        obj.power,
        Some(4),
        "0/4 attacker should be pumped to power 4 (base 0 + X where X = |0-4|)"
    );
    assert_eq!(
        obj.toughness,
        Some(8),
        "0/4 attacker should be pumped to toughness 8 (base 4 + X where X = |0-4|)"
    );
}

#[test]
fn doran_pumps_each_attacking_creature_by_its_own_pt_difference() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature_from_oracle(P0, "Doran, Besieged by Time", 0, 5, DORAN_PUMP)
        .id();
    let first_attacker = scenario.add_creature(P0, "Wall of Roots", 0, 4).id();
    let second_attacker = scenario.add_creature(P0, "Hill Giant", 3, 1).id();

    let mut runner = scenario.build();
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![
                (first_attacker, AttackTarget::Player(P1)),
                (second_attacker, AttackTarget::Player(P1)),
            ],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");
    runner.advance_until_stack_empty();

    let first = &runner.state().objects[&first_attacker];
    assert_eq!(
        (first.power, first.toughness),
        (Some(4), Some(8)),
        "0/4 attacker should use its own X = |0-4|"
    );
    let second = &runner.state().objects[&second_attacker];
    assert_eq!(
        (second.power, second.toughness),
        (Some(5), Some(3)),
        "3/1 attacker should use its own X = |3-1|"
    );
}

#[test]
fn doran_commander_pumps_each_attacker_against_different_opponents() {
    let db = shared_card_db().expect("integration card fixture loads");
    let mut scenario = GameScenario::new_with_format(FormatConfig::commander(), 3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let doran = scenario.add_real_card(P0, "Doran, Besieged by Time", Zone::Hand, db);
    scenario.with_commander(doran);
    let courser = scenario.add_real_card(P0, "Courser of Kruphix", Zone::Battlefield, db);
    let lions = scenario.add_real_card(P0, "Savannah Lions", Zone::Battlefield, db);
    scenario.with_mana_pool(
        P0,
        [
            ManaType::White,
            ManaType::Black,
            ManaType::Green,
            ManaType::Colorless,
        ]
        .into_iter()
        .map(|kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
        .collect(),
    );
    let mut runner = scenario.build();

    // CR 903.3 + CR 903.8: the commander is designated and cast from the command zone.
    let commander = &runner.state().objects[&doran];
    assert!(commander.is_commander);
    assert_eq!(commander.zone, Zone::Command);
    runner
        .cast(doran)
        .resolve()
        .assert_zone(&[doran], Zone::Battlefield);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));

    // CR 802.3: one declaration can assign attackers to different opponents.
    runner.advance_to_combat();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareAttackers { player: P0, .. }
    ));
    runner
        .declare_attackers(&[
            (courser, AttackTarget::Player(P1)),
            (lions, AttackTarget::Player(PlayerId(2))),
        ])
        .expect("attacking different opponents should succeed");

    // CR 603.2c + CR 508.3a: each declared attacker causes its own trigger.
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    let doran_triggers = runner.state().stack.iter().filter(|entry| {
        entry.source_id == doran && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
    });
    assert_eq!(doran_triggers.count(), 2, "one Doran trigger per attacker");

    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));

    // CR 608.2h + CR 611.2d + CR 613.4c: each trigger reads its attacker's P/T
    // as it resolves and applies that creature's own +X/+X.
    let courser = &runner.state().objects[&courser];
    assert_eq!((courser.power, courser.toughness), (Some(4), Some(6)));
    let lions = &runner.state().objects[&lions];
    assert_eq!((lions.power, lions.toughness), (Some(3), Some(2)));
}

/// CR 509.1: a creature you control that *blocks* triggers the same +X/+X.
/// Doran itself (0/5) blocks an opponent's attacker; X = |0 − 5| = 5 → 5/10.
#[test]
fn doran_pumps_blocking_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P1 is the attacker this turn; switch the active player to P1 so P0 can
    // declare a blocker. Doran sits on P0's battlefield as the blocker.
    let doran = scenario
        .add_creature_from_oracle(P0, "Doran, Besieged by Time", 0, 5, DORAN_PUMP)
        .id();
    let opp_attacker = scenario.add_creature(P1, "Hostile Bear", 2, 2).id();

    let mut runner = scenario.build();
    // Hand the turn to P1 so P1 can declare attackers and P0 can block.
    // `waiting_for` must be set consistently with `active_player` or the
    // combat-step machinery rejects P1's DeclareAttackers.
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(opp_attacker, AttackTarget::Player(P0))],
            bands: vec![],
        })
        .expect("P1 DeclareAttackers should succeed");
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner.pass_both_players();
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { .. }
        ),
        "expected DeclareBlockers, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(doran, opp_attacker)],
        })
        .expect("P0 DeclareBlockers should succeed");
    runner.advance_until_stack_empty();

    let obj = &runner.state().objects[&doran];
    assert_eq!(
        obj.power,
        Some(5),
        "0/5 blocker should be pumped to power 5 (X = |0-5|)"
    );
    assert_eq!(
        obj.toughness,
        Some(10),
        "0/5 blocker should be pumped to toughness 10 (X = |0-5|)"
    );
}

#[test]
fn doran_pumps_each_blocking_creature_by_its_own_pt_difference() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature_from_oracle(P0, "Doran, Besieged by Time", 0, 5, DORAN_PUMP)
        .id();
    let first_blocker = scenario.add_creature(P0, "Wall of Roots", 0, 4).id();
    let second_blocker = scenario.add_creature(P0, "Hill Giant", 3, 1).id();
    let first_attacker = scenario.add_creature(P1, "Hostile Bear", 2, 2).id();
    let second_attacker = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![
                (first_attacker, AttackTarget::Player(P0)),
                (second_attacker, AttackTarget::Player(P0)),
            ],
            bands: vec![],
        })
        .expect("P1 DeclareAttackers should succeed");
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner.pass_both_players();
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { .. }
        ),
        "expected DeclareBlockers, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![
                (first_blocker, first_attacker),
                (second_blocker, second_attacker),
            ],
        })
        .expect("P0 DeclareBlockers should succeed");
    runner.advance_until_stack_empty();

    let first = &runner.state().objects[&first_blocker];
    assert_eq!(
        (first.power, first.toughness),
        (Some(4), Some(8)),
        "0/4 blocker should use its own X = |0-4|"
    );
    let second = &runner.state().objects[&second_blocker];
    assert_eq!(
        (second.power, second.toughness),
        (Some(5), Some(3)),
        "3/1 blocker should use its own X = |3-1|"
    );
}

/// A 3/3 (power == toughness) attacks; X = |3 − 3| = 0; the +0/+0 pump nets
/// no characteristic change. ("The difference between A and B" is an unsigned
/// Oracle templating convention — the resolver takes `.abs()` of the gap.)
#[test]
fn doran_pump_zero_for_square_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature_from_oracle(P0, "Doran, Besieged by Time", 0, 5, DORAN_PUMP)
        .id();
    let attacker = scenario.add_creature(P0, "Gray Ogre", 3, 3).id();

    let mut runner = scenario.build();
    declare_attacker(&mut runner, attacker);

    let obj = &runner.state().objects[&attacker];
    assert_eq!(
        obj.power,
        Some(3),
        "square 3/3 attacker stays power 3 (X = |3-3| = 0)"
    );
    assert_eq!(
        obj.toughness,
        Some(3),
        "square 3/3 attacker stays toughness 3 (X = |3-3| = 0)"
    );
}

/// CR 611.2d + CR 608.2h: X is read once, on trigger resolution, from the
/// *post-layer* P/T. A 1/1 under a +0/+2 anthem is effectively a 1/3 when it
/// attacks → X = |1 − 3| = 2 → effective 3/5 (base 1/1 + anthem 0/2 + pump 2/2).
#[test]
fn doran_pump_uses_current_pt_at_resolution() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature_from_oracle(P0, "Doran, Besieged by Time", 0, 5, DORAN_PUMP)
        .id();
    // A 1/1 attacker carrying its own +0/+2 continuous static (anthem-equivalent
    // self-modification) so its post-layer P/T is 1/3 before combat.
    let mut attacker_builder = scenario.add_creature(P0, "Anthemed Bear", 1, 1);
    attacker_builder
        .with_continuous_static(vec![ContinuousModification::AddToughness { value: 2 }]);
    let attacker = attacker_builder.id();

    let mut runner = scenario.build();
    // Pass priority once so the anthem static recomputes before combat.
    runner.act(GameAction::PassPriority).ok();
    let pre = &runner.state().objects[&attacker];
    assert_eq!(
        (pre.power, pre.toughness),
        (Some(1), Some(3)),
        "anthem self-static must make the attacker an effective 1/3 pre-combat"
    );

    declare_attacker(&mut runner, attacker);

    let obj = &runner.state().objects[&attacker];
    // X = |post-layer power − post-layer toughness| = |1 − 3| = 2.
    // Effective P/T = base 1/1 + anthem 0/2 + pump 2/2 = 3/5.
    assert_eq!(
        obj.power,
        Some(3),
        "X must be computed from post-layer P/T (1/3) → X = 2 → power 1+0+2 = 3"
    );
    assert_eq!(
        obj.toughness,
        Some(5),
        "X must be computed from post-layer P/T (1/3) → X = 2 → toughness 1+2+2 = 5"
    );
}

/// CR 514.2: "until end of turn" effects end during the cleanup step. The +X/+X
/// pump must persist through combat and be gone on the next turn.
#[test]
fn doran_pump_lasts_until_end_of_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature_from_oracle(P0, "Doran, Besieged by Time", 0, 5, DORAN_PUMP)
        .id();
    let attacker = scenario.add_creature(P0, "Wall of Roots", 0, 4).id();

    let mut runner = scenario.build();
    let start_turn = runner.state().turn_number;
    declare_attacker(&mut runner, attacker);

    // Pump is live this turn.
    assert_eq!(
        runner.state().objects[&attacker].power,
        Some(4),
        "pump should be active during the turn it triggered"
    );

    // Advance through the rest of this turn (and into a later turn) by passing
    // priority. Bounded loop guards against a stuck state.
    for _ in 0..200 {
        if runner.state().turn_number > start_turn {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
    assert!(
        runner.state().turn_number > start_turn,
        "the game must advance past the turn the pump was created"
    );

    let obj = &runner.state().objects[&attacker];
    assert_eq!(
        obj.power,
        Some(0),
        "+X/+X must expire at end of turn — power back to base 0"
    );
    assert_eq!(
        obj.toughness,
        Some(4),
        "+X/+X must expire at end of turn — toughness back to base 4"
    );
}
