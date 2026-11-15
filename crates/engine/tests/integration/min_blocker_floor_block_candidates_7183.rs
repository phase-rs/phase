//! Issue #7183 — the AI's `DeclareBlockers` candidate enumerator must be able to
//! express a legal gang block against a minimum-blocker floor.
//!
//! CR 509.1b lets an attacker carry a floor on how many creatures must block it.
//! Two things create one: the Menace keyword (CR 702.111b, floor 2) and a
//! `CantBeBlockedExceptBy { MinBlockers { min } }` static parsed from "can't be
//! blocked except by N or more creatures" (Pathrazer of Ulamog, floor 3).
//!
//! `blocker_actions` used to seed only the empty declaration plus every *single*
//! `(blocker, attacker)` pair. Against a floor above 1 every one of those pairs is
//! an illegal declaration, so `complete_blocker_proposals` rewrote each to the same
//! tax-free witness and dedup collapsed the whole candidate set to a single entry —
//! "don't block". The defending player's entire legal-action list therefore said
//! blocking was impossible even with a board full of untapped creatures, and the
//! engine would happily accept a hand-built gang block the enumerator never
//! offered. This test drives the real `legal_actions_full` entry point.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::StaticDefinition;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::statics::{BlockExceptionKind, StaticMode};

use super::rules::AttackTarget;

/// One 6/6 attacker carrying `floor` (as a keyword and/or a `MinBlockers` static)
/// attacks P1, who controls eight untapped 2/2s. Returns the enumerated
/// `DeclareBlockers` actions and the attacker's id.
fn declare_blockers_candidates(
    menace: bool,
    min_blockers: Option<u32>,
) -> (Vec<Vec<(ObjectId, ObjectId)>>, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let attacker = scenario.add_creature(P0, "Attacker", 6, 6).id();
    for i in 0..8 {
        scenario.add_creature(P1, &format!("Blocker {i}"), 2, 2);
    }

    let mut runner = scenario.build();
    {
        let obj = runner.state_mut().objects.get_mut(&attacker).unwrap();
        if menace {
            obj.keywords.push(Keyword::Menace);
        }
        if let Some(min) = min_blockers {
            obj.static_definitions
                .push(StaticDefinition::new(StaticMode::CantBeBlockedExceptBy {
                    kind: BlockExceptionKind::MinBlockers { min },
                }));
        }
    }

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("declare attackers");
    runner.pass_both_players();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { .. }
        ),
        "expected the defending player's declare-blockers prompt, got {}",
        runner.state().waiting_for.variant_name()
    );

    let declarations = engine::ai_support::legal_actions_full(runner.state())
        .0
        .into_iter()
        .filter_map(|action| match action {
            GameAction::DeclareBlockers { assignments } => Some(assignments),
            _ => None,
        })
        .collect();
    (declarations, attacker)
}

/// CR 702.111b: a menace attacker's floor of 2 must be reachable — at least one
/// enumerated declaration puts exactly two blockers on it.
#[test]
fn menace_attacker_enumerates_two_blocker_gang_declarations() {
    let (declarations, attacker) = declare_blockers_candidates(true, None);

    assert!(
        declarations.iter().any(|assignments| assignments
            .iter()
            .filter(|&&(_, a)| a == attacker)
            .count()
            == 2),
        "the AI must be offered a legal two-blocker gang block against menace; \
         got {declarations:?}"
    );
}

/// CR 509.1b: an arbitrary `MinBlockers` floor (Pathrazer of Ulamog's three) is the
/// same class and must be reachable too. This is the arm that regressed — menace is
/// a keyword the AI special-cased, a `MinBlockers` static is not.
#[test]
fn min_blockers_three_attacker_enumerates_three_blocker_gang_declarations() {
    let (declarations, attacker) = declare_blockers_candidates(false, Some(3));

    assert!(
        declarations.iter().any(|assignments| assignments
            .iter()
            .filter(|&&(_, a)| a == attacker)
            .count()
            == 3),
        "the AI must be offered a legal three-blocker gang block against a \
         'can't be blocked except by three or more creatures' attacker; got {declarations:?}"
    );
}

/// No enumerated declaration may sit below the floor — a short set is illegal, and
/// offering one only burns a candidate slot on an action the completion authority
/// rewrites away.
#[test]
fn no_enumerated_declaration_falls_below_the_floor() {
    for (menace, min, floor) in [(true, None, 2), (false, Some(3), 3), (true, Some(4), 4)] {
        let (declarations, attacker) = declare_blockers_candidates(menace, min);
        for assignments in &declarations {
            let on_attacker = assignments.iter().filter(|&&(_, a)| a == attacker).count();
            assert!(
                on_attacker == 0 || on_attacker >= floor,
                "floor {floor}: enumerated declaration {assignments:?} puts {on_attacker} \
                 blocker(s) on the attacker — below the CR 509.1b floor"
            );
        }
    }
}

/// Guard against over-correction: an attacker with no floor keeps its ordinary
/// single-blocker candidates, which are the overwhelmingly common case.
#[test]
fn unrestricted_attacker_keeps_single_blocker_declarations() {
    let (declarations, attacker) = declare_blockers_candidates(false, None);

    assert!(
        declarations.iter().any(|assignments| assignments
            .iter()
            .filter(|&&(_, a)| a == attacker)
            .count()
            == 1),
        "an attacker with no minimum-blocker floor must still offer single blocks; \
         got {declarations:?}"
    );
}
