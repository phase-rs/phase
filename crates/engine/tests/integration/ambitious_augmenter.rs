//! Ambitious Augmenter — HadCounters-gated death trigger with same-chain
//! Fractal token creation and "that token" counter transfer.

use engine::game::scenario::{CastOutcome, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const AMBITIOUS_AUGMENTER: &str = "Increment (Whenever you cast a spell, if the amount of mana you spent is greater than this creature's power or toughness, put a +1/+1 counter on this creature.)\nWhen this creature dies, if it had one or more counters on it, create a 0/0 green and blue Fractal creature token, then put this creature's counters on that token.";
const PARALLEL_LIVES: &str = "If an effect would create one or more tokens under your control, it creates twice that many of those tokens instead.";
const MAKESHIFT_MANNEQUIN: &str = "Return target creature card from your graveyard to the battlefield with a mannequin counter on it. For as long as that creature has a mannequin counter on it, it has \"When this creature becomes the target of a spell or ability, sacrifice it.\"";
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

fn cast_vindicate_with_parallel_lives(counters: &[(CounterType, u32)]) -> (CastOutcome, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, three_generic());
    scenario.add_enchantment_from_oracle(P0, "Parallel Lives", PARALLEL_LIVES);
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

/// Positive: CR 614.1a lets Parallel Lives replace the token-creation count,
/// and CR 122.8 puts the departed creature's counter kinds onto each created
/// token named by "that token."
#[test]
fn ambitious_augmenter_doubled_fractals_each_get_departed_counter_kinds() {
    let (outcome, augmenter) = cast_vindicate_with_parallel_lives(&[
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
        2,
        "Parallel Lives must double Ambitious Augmenter's Fractal token"
    );
    for fractal in fractals {
        assert_eq!(
            outcome.counters(fractal, CounterType::Plus1Plus1),
            2,
            "each Fractal gets the source's +1/+1 counters"
        );
        assert_eq!(
            outcome.counters(fractal, CounterType::Generic("oil".to_string())),
            1,
            "each Fractal gets the source's non-+1/+1 counters too"
        );
    }
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

/// Positive: CR 122.8 + CR 400.7 require the old dies trigger to reproduce
/// the counters on Ambitious Augmenter's departed incarnation, not counters on
/// a new object returned by Makeshift Mannequin before the trigger resolves.
#[test]
fn ambitious_augmenter_reanimated_source_uses_departed_counters_for_fractal() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..7)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    let augmenter = scenario
        .add_creature_from_oracle(P0, "Ambitious Augmenter", 1, 1, AMBITIOUS_AUGMENTER)
        .id();
    scenario.with_counter(augmenter, CounterType::Plus1Plus1, 3);
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Vindicate", false, VINDICATE)
        .id();
    let mannequin = scenario
        .add_spell_to_hand_from_oracle(P0, "Makeshift Mannequin", true, MAKESHIFT_MANNEQUIN)
        .id();

    let mut runner = scenario.build();
    runner.cast(destroy).target_object(augmenter).commit();
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. }
                if runner.state().objects[&augmenter].zone == Zone::Graveyard
                    && runner
                        .state()
                        .stack
                        .iter()
                        .any(|entry| entry.source_id == augmenter) =>
            {
                break;
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority must advance to the Augmenter trigger");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("ordering the Augmenter trigger must succeed");
            }
            other => panic!("unexpected prompt before Augmenter trigger resolved: {other:?}"),
        }
    }
    assert_eq!(
        runner.state().objects[&augmenter].zone,
        Zone::Graveyard,
        "reach-guard: Ambitious Augmenter must die before reanimation"
    );
    assert!(
        runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == augmenter),
        "reach-guard: the original dies trigger must be waiting on the stack"
    );

    let outcome = runner.cast(mannequin).target_object(augmenter).resolve();

    assert_eq!(
        outcome.zone_of(augmenter),
        Zone::Battlefield,
        "reach-guard: Makeshift Mannequin must return Ambitious Augmenter"
    );
    assert_eq!(
        outcome.counters(augmenter, CounterType::Generic("mannequin".to_string())),
        1,
        "reach-guard: the returned new object must have its mannequin counter"
    );
    let fractals = fractal_tokens(&outcome);
    assert_eq!(
        fractals.len(),
        1,
        "the Fractal must survive as a 3/3 after receiving departed counters"
    );
    let fractal = fractals[0];
    assert_eq!(
        outcome.counters(fractal, CounterType::Plus1Plus1),
        3,
        "the Fractal must receive the old incarnation's +1/+1 counters"
    );
    assert_eq!(
        outcome.counters(fractal, CounterType::Generic("mannequin".to_string())),
        0,
        "the Fractal must not receive the new incarnation's mannequin counter"
    );
}
