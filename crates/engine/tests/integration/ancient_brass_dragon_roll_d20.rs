//! Reproduction for issue #1602, Deliverable 2 — Ancient Brass Dragon's
//! reflexive graveyard reanimation gated by an aggregate mana-value cap.
//!
//! Oracle (Ancient Brass Dragon):
//! > Flying
//! > Whenever this creature deals combat damage to a player, roll a d20. When
//! > you do, put any number of target creature cards with total mana value X or
//! > less from graveyards onto the battlefield under your control, where X is
//! > the result.
//!
//! The bug: the reflexive `ChangeZone` dropped the graveyard origin, the
//! "total mana value X or less" cap, and the "any number of target" multi-target
//! spec — so it tried to put battlefield creatures, ignored the cap, and left X
//! unbound (resolving to 0). Deliverable 2 routes the cap through a typed
//! `TargetSelectionConstraint::TotalManaValue` bound to the die result and
//! enforces it during target legality.
//!
//! This test drives real combat (unblocked 6/5 flyer → 6 combat damage), reads
//! the carried d20 result, and asserts:
//!   (a) an over-cap selection (a single card whose MV exceeds the roll) is
//!       REJECTED;
//!   (b) the legal targets all satisfy the cap and live in a graveyard, and
//!       selecting them moves them onto the battlefield under the Brass
//!       controller (FROM the graveyard, not the battlefield);
//!   (c) selecting zero targets is a clean no-op (unlimited targeting permits 0).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::rules::run_combat;

const ANCIENT_BRASS_DRAGON_ORACLE: &str = "Flying\nWhenever this creature deals combat damage \
to a player, roll a d20. When you do, put any number of target creature cards with total mana \
value X or less from graveyards onto the battlefield under your control, where X is the result.";

/// Seed combat to the roll, returning the runner, the carried d20 result, and
/// the graveyard creature ids (with their mana values) for both players.
fn drive_to_reflexive_selection() -> (GameRunner, usize, Vec<(ObjectId, u32)>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dragon = scenario
        .add_creature_from_oracle(
            P0,
            "Ancient Brass Dragon",
            6,
            5,
            ANCIENT_BRASS_DRAGON_ORACLE,
        )
        .id();

    // A spread of mana values across both players' graveyards. With a d20 result
    // in 1..=20 at least the MV-1 card is always under the cap, and the MV-20
    // card is over the cap for any non-natural-20 roll.
    let mut grave: Vec<(ObjectId, u32)> = Vec::new();
    for (i, mv) in [1u32, 2, 3, 5, 8].into_iter().enumerate() {
        let owner = if i % 2 == 0 { P0 } else { P1 };
        let mut b = scenario.add_creature_to_graveyard(owner, &format!("GraveBeast {mv}"), 2, 2);
        b.with_mana_cost(ManaCost::generic(mv));
        grave.push((b.id(), mv));
    }
    // A deliberately huge MV so it is over-cap for any roll < 20.
    let mut big = scenario.add_creature_to_graveyard(P1, "Colossus", 9, 9);
    big.with_mana_cost(ManaCost::generic(20));
    grave.push((big.id(), 20));

    let mut runner = scenario.build();
    run_combat(&mut runner, vec![dragon], vec![]);

    // Pass priority until the reflexive trigger pauses for target selection,
    // collecting events so we can read the d20 result.
    let mut all_events: Vec<GameEvent> = Vec::new();
    for _ in 0..30 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => match runner.act(GameAction::PassPriority) {
                Ok(result) => all_events.extend(result.events),
                Err(_) => break,
            },
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => break,
            _ => break,
        }
    }

    let rolled = all_events
        .iter()
        .find_map(|e| match e {
            GameEvent::DieRolled {
                result, sides: 20, ..
            } => Some(*result as usize),
            _ => None,
        })
        .expect("Ancient Brass Dragon should roll a d20 on combat damage");
    assert!(
        (1..=20).contains(&rolled),
        "d20 result out of range: {rolled}"
    );

    (runner, rolled, grave)
}

#[test]
fn ancient_brass_dragon_over_cap_selection_rejected() {
    let (mut runner, rolled, grave) = drive_to_reflexive_selection();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "expected reflexive target selection, got {:?}",
        runner.state().waiting_for
    );

    // Pick a single graveyard card whose mana value exceeds the rolled cap.
    if let Some((over_id, _)) = grave.iter().copied().find(|(_, mv)| *mv as usize > rolled) {
        let result = runner.act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(over_id)],
        });
        assert!(
            result.is_err(),
            "selecting an over-cap card (cap = d20 {rolled}) must be rejected"
        );
    }
}

#[test]
fn ancient_brass_dragon_under_cap_reanimates_from_graveyard() {
    let (mut runner, rolled, grave) = drive_to_reflexive_selection();

    // CR 202.3: the cap is on the COMBINED mana value of the chosen set, so
    // greedily pick cards (ascending MV) while the running total stays at/under
    // the rolled cap. The MV-1 card always fits, so the set is never empty.
    let mut sorted = grave.clone();
    sorted.sort_by_key(|(_, mv)| *mv);
    let mut running = 0usize;
    let mut under: Vec<ObjectId> = Vec::new();
    for (id, mv) in sorted {
        if running + mv as usize <= rolled {
            running += mv as usize;
            under.push(id);
        }
    }
    assert!(
        !under.is_empty(),
        "the MV-1 card is always under cap (roll = {rolled})"
    );

    // Confirm they really start in a graveyard (not the battlefield).
    for id in &under {
        assert_eq!(
            runner.state().objects.get(id).map(|o| o.zone),
            Some(Zone::Graveyard),
            "reanimation target must start in a graveyard"
        );
    }

    runner
        .act(GameAction::SelectTargets {
            targets: under.iter().copied().map(TargetRef::Object).collect(),
        })
        .expect("at/under-cap selection must be accepted");

    // Resolve the reflexive ability — pass priority only until every selected
    // card has been reanimated onto the battlefield (the stack empties this
    // turn). Stop there so the assertion observes the reanimation result rather
    // than later-turn game state.
    let all_on_battlefield = |runner: &GameRunner| {
        under
            .iter()
            .all(|id| runner.state().objects.get(id).map(|o| o.zone) == Some(Zone::Battlefield))
    };
    for _ in 0..12 {
        if all_on_battlefield(&runner) {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }

    // CR 110.2a: each selected card is now on the battlefield under P0's control.
    for id in &under {
        let obj = runner
            .state()
            .objects
            .get(id)
            .expect("reanimated object should still exist");
        assert_eq!(obj.zone, Zone::Battlefield, "card must move to battlefield");
        assert_eq!(
            obj.controller, P0,
            "reanimated card enters under the Brass controller (P0)"
        );
    }
}

#[test]
fn ancient_brass_dragon_zero_targets_is_clean_no_op() {
    let (mut runner, _rolled, grave) = drive_to_reflexive_selection();

    let battlefield_before = runner.state().battlefield.len();

    // "any number of" permits choosing zero targets — a clean no-op.
    runner
        .act(GameAction::SelectTargets { targets: vec![] })
        .expect("selecting zero targets must be a clean no-op");

    for _ in 0..30 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }

    // No graveyard card moved (still in a graveyard) and the battlefield count
    // did not grow from reanimation.
    for (id, _) in &grave {
        assert_eq!(
            runner.state().objects.get(id).map(|o| o.zone),
            Some(Zone::Graveyard),
            "no card should move when zero targets are chosen"
        );
    }
    assert_eq!(
        runner.state().battlefield.len(),
        battlefield_before,
        "battlefield must be unchanged after a zero-target reanimation"
    );
}
