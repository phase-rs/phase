//! Ambitious Augmenter — HadCounters-gated death trigger with same-chain
//! Fractal token creation and "that token" counter transfer.

use engine::game::scenario::{CastOutcome, GameScenario, P0};
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const AMBITIOUS_AUGMENTER: &str = "Increment (Whenever you cast a spell, if the amount of mana you spent is greater than this creature's power or toughness, put a +1/+1 counter on this creature.)\nWhen this creature dies, if it had one or more counters on it, create a 0/0 green and blue Fractal creature token, then put this creature's counters on that token.";
const VINDICATE: &str = "Destroy target permanent.";

fn three_generic() -> Vec<ManaUnit> {
    (0..3)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

fn fractal_tokens(outcome: &CastOutcome) -> Vec<ObjectId> {
    outcome
        .state()
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            outcome.state().objects.get(id).is_some_and(|obj| {
                obj.is_token
                    && obj
                        .card_types
                        .subtypes
                        .iter()
                        .any(|subtype| subtype.eq_ignore_ascii_case("Fractal"))
            })
        })
        .collect()
}

fn cast_vindicate_with_counters(counters: &[(CounterType, u32)]) -> (CastOutcome, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, three_generic());
    let augmenter = scenario
        .add_creature_from_oracle(P0, "Ambitious Augmenter", 1, 1, AMBITIOUS_AUGMENTER)
        .id();
    for (counter, count) in counters {
        scenario.with_counter(augmenter, counter.clone(), *count);
    }
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Vindicate", false, VINDICATE)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(destroy).target_object(augmenter).resolve();
    (outcome, augmenter)
}

/// Positive: CR 122.8 moves the same number and kinds of counters from the
/// departed creature's LKI to the token created earlier in the same ability.
#[test]
fn ambitious_augmenter_moves_all_departed_counter_kinds_to_the_fractal() {
    let (outcome, augmenter) = cast_vindicate_with_counters(&[
        (CounterType::Plus1Plus1, 2),
        (CounterType::Generic("oil".to_string()), 1),
    ]);

    assert_eq!(
        outcome.zone_of(augmenter),
        Zone::Graveyard,
        "reach-guard: Vindicate must destroy Ambitious Augmenter"
    );
    let fractals = fractal_tokens(&outcome);
    assert_eq!(
        fractals.len(),
        1,
        "exactly one Fractal token must be created"
    );
    let fractal = fractals[0];
    assert_eq!(
        outcome.counters(fractal, CounterType::Plus1Plus1),
        2,
        "the Fractal gets the source's +1/+1 counters"
    );
    assert_eq!(
        outcome.counters(fractal, CounterType::Generic("oil".to_string())),
        1,
        "the Fractal gets the source's non-+1/+1 counters too"
    );
}

/// Negative (non-vacuous): CR 603.4 suppresses the intervening-if trigger when
/// the dying creature had no counters. The creature still reaches the
/// graveyard, proving the event happened.
#[test]
fn ambitious_augmenter_no_counters_creates_no_fractal() {
    let (outcome, augmenter) = cast_vindicate_with_counters(&[]);

    assert_eq!(
        outcome.zone_of(augmenter),
        Zone::Graveyard,
        "reach-guard: Vindicate must destroy Ambitious Augmenter"
    );
    assert!(
        fractal_tokens(&outcome).is_empty(),
        "a counterless Ambitious Augmenter must not create a Fractal"
    );
}
