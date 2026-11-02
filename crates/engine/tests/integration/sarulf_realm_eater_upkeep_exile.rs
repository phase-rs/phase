//! CR 608.2c + CR 608.2i — Sarulf, Realm Eater's upkeep exile keeps its bound
//! numeric threshold: the ordered if-you-do sequence reads back the number
//! the removal instruction left behind.
//!
//! "exile each other nonland permanent with mana value less than or equal to
//! the number of counters removed this way" names a quantity-position anaphor
//! the quantity authority binds (`PreviousEffectAmount`), not an unbound
//! object-membership restriction — so the fail-closed guard must not refuse
//! the sentence, and the production upkeep trigger must remove the counters
//! and exile exactly the permanents at or under the removed count.
//!
//! Oracle text quoted from Scryfall, not from memory.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SARULF: &str = "Whenever a permanent an opponent controls is put into a graveyard from the battlefield, put a +1/+1 counter on Sarulf.\nAt the beginning of your upkeep, if Sarulf has one or more +1/+1 counters on it, you may remove all of them. If you do, exile each other nonland permanent with mana value less than or equal to the number of counters removed this way.";

/// Drain Sarulf's upkeep trigger: pass priority while the stack holds it,
/// then answer the trailing "you may" prompt the trigger resolves into (the
/// trigger object leaves the stack before the choice is offered). Returns
/// whether the optional was offered.
fn drain_sarulf_trigger(runner: &mut GameRunner, accept: bool) -> bool {
    let mut saw_optional = false;
    for _ in 0..16 {
        match &runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => {
                saw_optional = true;
                runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .expect("answering the counter removal must succeed");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("each player passes");
            }
            other => panic!("unexpected prompt while draining Sarulf's trigger: {other:?}"),
        }
    }
    assert!(
        runner.state().stack.is_empty(),
        "the trigger must fully resolve, stack holds {:?}",
        runner.state().stack,
    );
    saw_optional
}

/// F2 — PRODUCTION upkeep/removal/exile control. Sarulf carries two +1/+1
/// counters into its controller's upkeep; accepting the removal exiles each
/// other nonland permanent with MV ≤ 2 (the removed count) while the MV-5
/// creature, Sarulf itself ("other"), and the land survive.
#[test]
fn sarulf_upkeep_removal_exiles_each_other_nonland_at_or_under_removed_count() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let sarulf = scenario
        .add_creature(P0, "Sarulf, Realm Eater", 3, 3)
        .with_mana_cost(ManaCost::generic(3))
        .from_oracle_text(SARULF)
        .id();
    scenario.with_counter(sarulf, CounterType::Plus1Plus1, 2);
    let mite = scenario
        .add_creature(P0, "Mite", 1, 1)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let brute = scenario
        .add_creature(P0, "Brute", 5, 5)
        .with_mana_cost(ManaCost::generic(5))
        .id();
    let bear = scenario
        .add_creature(P1, "Bear", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let land = scenario.add_land_from_oracle(P0, "Forest", "").id();
    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    runner.advance_to_upkeep();
    assert!(
        runner.state().stack.iter().any(|entry| matches!(
            &entry.kind,
            StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == sarulf
        )),
        "reach guard: Sarulf's upkeep trigger fired with counters aboard"
    );
    let saw_optional = drain_sarulf_trigger(&mut runner, true);
    assert!(
        saw_optional,
        "reach guard: the 'you may remove' choice must be offered"
    );

    // Both counters removed (and the count published as the exile threshold).
    assert_eq!(
        runner
            .state()
            .objects
            .get(&sarulf)
            .and_then(|obj| obj.counters.get(&CounterType::Plus1Plus1).copied())
            .unwrap_or(0),
        0,
        "accepting must remove all counters from Sarulf"
    );
    // MV 1 and MV 2 exiled; MV 5, Sarulf itself, and the land survive.
    let zone_of = |runner: &GameRunner, id| runner.state().objects[&id].zone;
    assert_eq!(zone_of(&runner, mite), Zone::Exile, "MV 1 ≤ 2 exiled");
    assert_eq!(zone_of(&runner, bear), Zone::Exile, "MV 2 ≤ 2 exiled");
    assert_eq!(
        zone_of(&runner, brute),
        Zone::Battlefield,
        "MV 5 > 2 survives"
    );
    assert_eq!(
        zone_of(&runner, sarulf),
        Zone::Battlefield,
        "'each OTHER permanent' spares Sarulf"
    );
    assert_eq!(
        zone_of(&runner, land),
        Zone::Battlefield,
        "'nonland' spares the land"
    );
}

/// F2 — DECLINE control. Declining the removal keeps the counters and exiles
/// nothing: the exile is gated on the removal ("If you do"), not a static
/// sweep.
#[test]
fn sarulf_upkeep_decline_keeps_counters_and_exiles_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let sarulf = scenario
        .add_creature(P0, "Sarulf, Realm Eater", 3, 3)
        .with_mana_cost(ManaCost::generic(3))
        .from_oracle_text(SARULF)
        .id();
    scenario.with_counter(sarulf, CounterType::Plus1Plus1, 2);
    let mite = scenario
        .add_creature(P0, "Mite", 1, 1)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    runner.advance_to_upkeep();
    let saw_optional = drain_sarulf_trigger(&mut runner, false);
    assert!(
        saw_optional,
        "reach guard: the 'you may remove' choice must be offered"
    );
    assert_eq!(
        runner
            .state()
            .objects
            .get(&sarulf)
            .and_then(|obj| obj.counters.get(&CounterType::Plus1Plus1).copied())
            .unwrap_or(0),
        2,
        "declining must keep both counters on Sarulf"
    );
    assert_eq!(
        runner.state().objects[&mite].zone,
        Zone::Battlefield,
        "declining the removal must exile nothing"
    );
}
