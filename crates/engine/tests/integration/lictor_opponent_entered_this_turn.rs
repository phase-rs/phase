//! Lictor (Warhammer 40,000 Commander) — Pheromone Trail:
//!   "When this creature enters, if a creature entered the battlefield under an
//!    opponent's control this turn, create a 3/3 green Tyranid Warrior creature
//!    token with trample."
//!
//! Regression for the dropped opponent-scoped "entered … under an opponent's
//! control this turn" intervening-"if" (CR 603.4). Before the fix the condition
//! parsed to `None`, so the ETB trigger fired UNCONDITIONALLY and Lictor made a
//! token every time it entered — even with no opponent entry that turn.
//!
//! The "under your control" surface of this class was already supported; this
//! adds the opponent-scoped past-tense mirror, carried by
//! `PlayerScope::Opponent { Max }` (the existential "an opponent" reading, per
//! `parse_opponent_had_entered_this_turn`) over the CR 608.2i
//! `BattlefieldEntriesThisTurn` snapshot.

use engine::game::restrictions::record_battlefield_entry;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AggregateFunction, Comparator, PlayerScope, QuantityExpr, QuantityRef, TargetFilter,
    TriggerCondition, TypeFilter,
};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const LICTOR: &str =
    "Flash\nPheromone Trail — When this creature enters, if a creature entered the \
battlefield under an opponent's control this turn, create a 3/3 green Tyranid Warrior creature \
token with trample.";

/// Stamp `id` into the production battlefield-entry ledger for the current turn,
/// exactly as `record_zone_change` does in a real game.
fn record_entry_now(runner: &mut GameRunner, id: ObjectId) {
    let turn = runner.state().turn_number;
    record_battlefield_entry(runner.state_mut(), id);
    runner
        .state_mut()
        .objects
        .get_mut(&id)
        .unwrap()
        .entered_battlefield_turn = Some(turn);
}

/// Count battlefield Tyranid Warrior tokens (Lictor's Pheromone Trail output).
fn tyranid_warrior_count(runner: &GameRunner) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            runner
                .state()
                .objects
                .get(id)
                .is_some_and(|o| o.is_token && o.name == "Tyranid Warrior")
        })
        .count()
}

/// Parse-level shape lock: the intervening-"if" must lower to the opponent-scoped
/// `BattlefieldEntriesThisTurn` comparison, NOT a dropped `None`.
///
/// REVERT-PROBE: remove `parse_entered_this_turn_under_opponent_control` and the
/// condition returns to `None`, panicking here.
#[test]
fn lictor_condition_is_opponent_scoped_entry_tally() {
    let parsed = parse_oracle_text(
        LICTOR,
        "Lictor",
        &[],
        &["Creature".to_string()],
        &["Tyranid".to_string()],
    );
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| t.condition.is_some())
        .expect("Lictor's ETB must carry an intervening-if condition, not a dropped None");
    match trigger.condition.as_ref().unwrap() {
        TriggerCondition::QuantityComparison {
            lhs:
                QuantityExpr::Ref {
                    qty:
                        QuantityRef::BattlefieldEntriesThisTurn {
                            player:
                                PlayerScope::Opponent {
                                    aggregate: AggregateFunction::Max,
                                },
                            filter: TargetFilter::Typed(f),
                        },
                },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        } => {
            assert_eq!(f.controller, None, "controller lives on the PlayerScope");
            assert!(
                f.type_filters.contains(&TypeFilter::Creature),
                "the creature restriction must survive, got {:?}",
                f.type_filters
            );
        }
        other => panic!("expected opponent-scoped BattlefieldEntriesThisTurn GE 1, got {other:?}"),
    }
}

/// Positive: an opponent's creature entered this turn ⇒ Pheromone Trail fires.
#[test]
fn lictor_makes_token_when_opponent_creature_entered() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lictor = scenario
        .add_creature_to_hand_from_oracle(P0, "Lictor", 2, 3, LICTOR)
        .id();
    // An opponent (P1) creature that entered the battlefield this turn.
    let opp_creature = scenario.add_creature(P1, "Opponent Entrant", 2, 2).id();
    let mut runner = scenario.build();
    record_entry_now(&mut runner, opp_creature);

    runner.cast(lictor).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        tyranid_warrior_count(&runner),
        1,
        "CR 603.4: the intervening-if is TRUE (an opponent's creature entered this \
         turn), so Pheromone Trail creates a Tyranid Warrior"
    );
}

/// Negative discriminator: only Lictor itself entered (under P0's control), so no
/// opponent entry exists ⇒ Pheromone Trail must NOT fire.
///
/// REVERT-PROBE: with the condition dropped to `None` the trigger fires
/// unconditionally and this reads 1 token, FAIL. This is the load-bearing
/// assertion — it fails on the unfixed engine.
#[test]
fn lictor_makes_no_token_without_opponent_entry() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lictor = scenario
        .add_creature_to_hand_from_oracle(P0, "Lictor", 2, 3, LICTOR)
        .id();
    // A P0 creature that entered this turn — under YOUR control, not an
    // opponent's — plus Lictor's own entry. Neither satisfies the opponent scope.
    let own_creature = scenario.add_creature(P0, "Own Entrant", 2, 2).id();
    let mut runner = scenario.build();
    record_entry_now(&mut runner, own_creature);

    runner.cast(lictor).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        tyranid_warrior_count(&runner),
        0,
        "CR 603.4: no creature entered under an OPPONENT's control this turn, so \
         the intervening-if is FALSE and no token is created"
    );
}
