//! Runtime cast-pipeline tests for Runaway Steam-Kin's intervening-if
//! "if this creature has fewer than three +1/+1 counters on it" (CR 603.4 +
//! CR 107.1 + CR 122.1).
//!
//! Built via the `/card-test` recipe: `GameScenario` +
//! `GameRunner::cast(..).resolve()` on verbatim Oracle text. The 2→3 positive
//! is the PutCounter reach-guard for the 3-stays-3 negative. Both halves share
//! a no-draw red instant (`"You gain 1 life."`) so CR 704.5b cannot SBA-end the
//! game on `GameScenario::new()`'s empty library before PutCounter runs.
//!
//! Revert discriminator: drop the `parse_strict_n_counters` arm → `condition`
//! is `None` → the 3-counter Steam-Kin gains a fourth.

use engine::game::scenario::{CastOutcome, GameScenario, P0};
use engine::types::ability::TriggerCondition;
use engine::types::counter::{CounterMatch, CounterType};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

/// Verbatim Oracle (Scryfall). The mana ability is out of scope and is not
/// activated; it is kept so the card is not a paraphrase of the trigger line.
const STEAM_KIN: &str = "Whenever you cast a red spell, if this creature has fewer than three +1/+1 counters on it, put a +1/+1 counter on this creature.\nRemove three +1/+1 counters from this creature: Add {R}{R}{R}.";

/// Cast a no-draw red instant at Steam-Kin with `n` +1/+1 counters.
///
/// CR 704.5b: `GameScenario::new()` libraries are empty, so a `"Draw a card."`
/// stimulus SBA-ends the game (`WaitingFor::GameOver`) and PutCounter never
/// runs. `"You gain 1 life."` keeps `WaitingFor::Priority` so the counter
/// gate is observable.
fn run_with_plus1(n: u32) -> (CastOutcome, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kin = scenario
        .add_creature_from_oracle(P0, "Runaway Steam-Kin", 1, 1, STEAM_KIN)
        .with_plus_counters(n)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Red Instant", true, "You gain 1 life.")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])],
    );
    let mut runner = scenario.build();

    // SHAPE reach-guard: if the combinator is reverted this is empty and both
    // runtime tests are honestly red.
    let has_gate = runner.state().objects[&kin]
        .trigger_definitions
        .iter_unchecked()
        .any(|entry| {
            matches!(
                entry.definition.condition,
                Some(TriggerCondition::HasCounters {
                    counters: CounterMatch::OfType(CounterType::Plus1Plus1),
                    minimum: 0,
                    maximum: Some(2),
                })
            )
        });
    assert!(
        has_gate,
        "Steam-Kin must parse intervening-if HasCounters {{ Plus1Plus1, 0, Some(2) }}"
    );

    let outcome = runner.cast(spell).resolve();
    (outcome, kin)
}

fn plus1_count(outcome: &CastOutcome, kin: ObjectId) -> u32 {
    outcome.state().objects[&kin]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn assert_stimulus_resolved(outcome: &CastOutcome) {
    // CR 704.5b: prove the red spell resolved and empty-library draw did not
    // end the game, so PutCounter was actually on the pipeline.
    outcome.assert_life_delta(P0, 1);
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "expected Priority after resolve, got {:?}",
        outcome.final_waiting_for()
    );
}

/// CR 603.4 + CR 122.1 + CR 107.1: below the LT-3 band, PutCounter applies.
/// `n=0` proves `unwrap_or(0)` on a missing counter map; `n=2` is the
/// positive reach-guard for the 3-stays-3 negative.
#[test]
fn runaway_steam_kin_puts_counter_when_below_three() {
    let (from_zero, kin_zero) = run_with_plus1(0);
    assert_stimulus_resolved(&from_zero);
    assert_eq!(plus1_count(&from_zero, kin_zero), 1, "0 counters → 1");

    let (from_two, kin_two) = run_with_plus1(2);
    assert_stimulus_resolved(&from_two);
    assert_eq!(plus1_count(&from_two, kin_two), 3, "2 counters → 3");
}

/// CR 603.4 + CR 107.1: at three +1/+1 counters the intervening-if fails, so
/// Steam-Kin does not gain a fourth. Discriminates LT from LE ("three or fewer"
/// would put a fourth). The 2→3 sibling in
/// `runaway_steam_kin_puts_counter_when_below_three` is the PutCounter
/// reach-guard — without it, "stays at 3" could pass because PutCounter never
/// ran.
#[test]
fn runaway_steam_kin_does_not_put_at_three() {
    let (at_three, kin) = run_with_plus1(3);
    assert_stimulus_resolved(&at_three);
    assert_eq!(plus1_count(&at_three, kin), 3, "3 counters stays 3");
}
