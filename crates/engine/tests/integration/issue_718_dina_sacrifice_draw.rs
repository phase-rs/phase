//! Regression for issue #718 — Dina, Essence Brewer.
//!
//! Oracle:
//!   Whenever you sacrifice a creature, draw a card. This ability triggers only
//!   once each turn.
//!   {2}, {T}, Sacrifice another creature: You gain X life and put X +1/+1
//!   counters on target creature you control, where X is the sacrificed
//!   creature's power.
//!
//! The Discord report targets the first ability (draw on sacrifice). The
//! activated ability's sacrifice-before-target ordering is covered separately in
//! `greater_good_activation::sacrifice_cost_defers_target_selection_until_cost_paid_object_exists`.
//!
//! CR 603.10a: sacrifice triggers look back in time at the sacrificed object's
//! last-known information.
//! CR 603.2h: "only once each turn" is modeled by `TriggerConstraint::OncePerTurn`.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const DINA_ORACLE: &str = "Whenever you sacrifice a creature, draw a card. This ability triggers only once each turn.\n\
{2}, {T}, Sacrifice another creature: You gain X life and put X +1/+1 counters on target creature you control, where X is the sacrificed creature's power.";

fn floating_colorless(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

fn mark_token(runner: &mut GameRunner, id: ObjectId) {
    runner
        .state_mut()
        .objects
        .get_mut(&id)
        .expect("object exists")
        .is_token = true;
}

/// CR 603.10a: sacrificing another creature to Dina's activated ability must
/// also fire her draw trigger (deferred from the cost-payment boundary).
#[test]
fn dina_draws_when_you_sacrifice_a_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Draw Card"]);
    scenario.with_mana_pool(P0, floating_colorless(2));

    let dina = scenario
        .add_creature_from_oracle(P0, "Dina, Essence Brewer", 2, 3, DINA_ORACLE)
        .id();
    let victim = scenario.add_creature(P0, "Sacrifice Fodder", 3, 3).id();
    let counter_target = scenario.add_creature(P0, "Counter Recipient", 1, 1).id();
    // Keep a second legal counter target so auto-select does not skip the prompt
    // after the victim leaves play.
    scenario.add_creature(P0, "Alternate Recipient", 2, 2);

    let mut runner = scenario.build();
    assert_eq!(
        runner.state().objects[&dina].abilities.len(),
        1,
        "Dina should expose exactly one activated ability (triggers are separate)"
    );

    let outcome = runner
        .activate(dina, 0)
        .pay_with(&[victim])
        .target_object(counter_target)
        .resolve();

    outcome.assert_hand_drawn(P0, 1);
    outcome.assert_life_delta(P0, 3);
    assert_eq!(outcome.zone_of(victim), Zone::Graveyard);
    assert_eq!(
        outcome
            .state()
            .objects
            .get(&counter_target)
            .and_then(|o| o.counters.get(&CounterType::Plus1Plus1).copied())
            .unwrap_or(0),
        3,
        "counters must equal the sacrificed creature's power"
    );
}

/// CR 603.2h: the draw trigger fires at most once per turn even across multiple
/// sacrifices.
#[test]
fn dina_draw_trigger_fires_only_once_per_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Draw A", "Draw B", "Draw C"]);
    scenario.with_mana_pool(P0, floating_colorless(4));

    let dina = scenario
        .add_creature_from_oracle(P0, "Dina, Essence Brewer", 2, 3, DINA_ORACLE)
        .id();
    let victim_a = scenario.add_creature(P0, "Fodder A", 3, 3).id();
    let victim_b = scenario.add_creature(P0, "Fodder B", 2, 2).id();
    let counter_target = scenario.add_creature(P0, "Counter Recipient", 1, 1).id();
    scenario.add_creature(P0, "Alternate Recipient", 4, 4);

    let mut runner = scenario.build();

    let first = runner
        .activate(dina, 0)
        .pay_with(&[victim_a])
        .target_object(counter_target)
        .resolve();
    first.assert_hand_drawn(P0, 1);
    first.assert_life_delta(P0, 3);

    runner
        .state_mut()
        .objects
        .get_mut(&dina)
        .expect("Dina")
        .tapped = false;

    let second = runner
        .activate(dina, 0)
        .pay_with(&[victim_b])
        .target_object(counter_target)
        .resolve();
    second.assert_hand_drawn(P0, 0);
    second.assert_life_delta(P0, 2);

    let hand_after_first = first.state().players[0].hand.len();
    assert_eq!(
        second.state().players[0].hand.len(),
        hand_after_first,
        "the second sacrifice must not draw again this turn"
    );
}

/// CR 603.10a + CR 111.7: token creatures cease to exist after sacrifice; the
/// draw trigger must still match via LKI.
#[test]
fn dina_draws_when_you_sacrifice_a_token_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Token Draw"]);
    scenario.with_mana_pool(P0, floating_colorless(2));

    let dina = scenario
        .add_creature_from_oracle(P0, "Dina, Essence Brewer", 2, 3, DINA_ORACLE)
        .id();
    let token = scenario.add_creature(P0, "Token Fodder", 2, 2).id();
    let counter_target = scenario.add_creature(P0, "Counter Recipient", 1, 1).id();
    scenario.add_creature(P0, "Alternate Recipient", 3, 3);

    let mut runner = scenario.build();
    mark_token(&mut runner, token);
    let outcome = runner
        .activate(dina, 0)
        .pay_with(&[token])
        .target_object(counter_target)
        .resolve();

    outcome.assert_hand_drawn(P0, 1);
    outcome.assert_life_delta(P0, 2);
    assert!(
        !outcome.state().objects.contains_key(&token),
        "sacrificed tokens cease to exist (CR 111.7)"
    );
}
