//! Yuna, Grand Summoner — "Whenever another permanent you control is put into a
//! graveyard from the battlefield, if it had one or more counters on it, you may
//! put that number of +1/+1 counters on target creature."
//!
//! Verbatim Oracle text. Yuna is staged via `add_creature_from_oracle`, so her
//! trigger comes from the real parser, and the departure is caused by casting a
//! destroy spell through the `GameRunner::cast` pipeline.
//!
//! WotC ruling: "Yuna's last ability counts all counters that were on the
//! permanent, not just +1/+1 counters."

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const YUNA_ORACLE: &str = "Grand Summon — {T}: Add one mana of any color. When you next cast a creature spell this turn, that creature enters with two additional +1/+1 counters on it.\nWhenever another permanent you control is put into a graveyard from the battlefield, if it had one or more counters on it, you may put that number of +1/+1 counters on target creature.";

/// Vindicate — verbatim Oracle text. The departure cause: CR 701.8a destroys
/// the permanent, moving it from the battlefield to its owner's graveyard.
const VINDICATE_ORACLE: &str = "Destroy target permanent.";

fn three_generic() -> Vec<ManaUnit> {
    (0..3)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

struct Fixture {
    runner: GameRunner,
    yuna: ObjectId,
    destroy: ObjectId,
    departing: ObjectId,
    recipient: ObjectId,
    bystander: ObjectId,
}

fn fixture(departing_counters: &[(CounterType, u32)]) -> Fixture {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, three_generic());

    let yuna = scenario
        .add_creature_from_oracle(P0, "Yuna, Grand Summoner", 1, 5, YUNA_ORACLE)
        .as_legendary()
        .id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Vindicate", false, VINDICATE_ORACLE)
        .id();
    let departing = scenario.add_creature(P0, "Counter Bearer", 3, 3).id();
    let recipient = scenario.add_creature(P0, "Recipient", 2, 2).id();
    let bystander = scenario.add_creature(P1, "Bystander", 2, 2).id();
    for (kind, count) in departing_counters {
        scenario.with_counter(departing, kind.clone(), *count);
    }

    Fixture {
        runner: scenario.build(),
        yuna,
        destroy,
        departing,
        recipient,
        bystander,
    }
}

/// Positive: CR 603.10a + CR 608.2h + CR 122.2 require the count to be read
/// from the departed permanent's last known information, after its live
/// counters ceased to exist. The departed permanent carried three counters of
/// two kinds, so Yuna's ruling requires three +1/+1 counters on the target.
#[test]
fn yuna_counts_every_counter_kind_on_the_departed_permanent() {
    let Fixture {
        runner,
        yuna,
        destroy,
        departing,
        recipient,
        bystander,
    } = fixture(&[
        (CounterType::Plus1Plus1, 2),
        (CounterType::Generic("oil".to_string()), 1),
    ]);

    let mut runner = runner;
    let outcome = runner
        .cast(destroy)
        .target_object(departing)
        .target_object(recipient)
        .accept_optional()
        .resolve();

    assert_eq!(
        outcome.zone_of(departing),
        Zone::Graveyard,
        "reach-guard: Vindicate must destroy the counter-bearing permanent"
    );
    assert_eq!(
        outcome.counters(recipient, CounterType::Plus1Plus1),
        3,
        "the chosen target must receive one +1/+1 counter per counter of any kind"
    );
    assert_eq!(
        outcome.counters(bystander, CounterType::Plus1Plus1),
        0,
        "the unchosen legal target receives nothing"
    );
    assert_eq!(
        outcome.counters(yuna, CounterType::Plus1Plus1),
        0,
        "Yuna herself was not chosen and receives nothing"
    );
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(outcome.state().stack.is_empty());
}

/// Negative (non-vacuous): CR 603.4 checks the intervening-if when the event
/// occurs. A permanent with no counters must leave without creating a Yuna stack
/// entry, target prompt, or optional prompt. There are multiple legal creature
/// targets, so a wrongly-fired trigger would stop at a visible prompt here.
#[test]
fn yuna_does_not_trigger_when_the_departed_permanent_had_no_counters() {
    let Fixture {
        runner,
        yuna,
        destroy,
        departing,
        recipient,
        bystander,
    } = fixture(&[]);

    let mut runner = runner;
    let mut commit = runner.cast(destroy).target_object(departing).commit();
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(guard < 16, "stack resolution did not terminate");
        assert!(
            !commit
                .state()
                .stack
                .iter()
                .any(|entry| entry.source_id == yuna),
            "a counterless departure must not put Yuna's trigger on the stack"
        );
        match commit.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if commit.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                commit
                    .act(GameAction::PassPriority)
                    .expect("passing priority must succeed");
            }
            other => panic!(
                "a counterless departure must raise no Yuna prompt \
                 (TriggerTargetSelection / OptionalEffectChoice), got {other:?}"
            ),
        }
    }

    assert_eq!(
        commit.state().objects[&departing].zone,
        Zone::Graveyard,
        "reach-guard: Vindicate must destroy the counterless permanent"
    );
    for creature in [yuna, recipient, bystander] {
        assert_eq!(
            commit.state().objects[&creature]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            0,
            "no creature may receive counters from a counterless departure"
        );
    }
}

const DELAYED_COUNTER_ORACLE: &str = "Whenever another permanent you control is put into a graveyard from the battlefield, if it had one or more counters on it, at the beginning of your next end step, put that many +1/+1 counters on target creature.";

/// Positive: CR 603.7 + CR 603.10a + CR 608.2h require the delayed trigger to
/// carry the departed permanent's LKI counter count to the next end step.
#[test]
fn delayed_that_many_uses_departure_counter_count_at_end_step() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    scenario.with_mana_pool(P0, three_generic());
    scenario.add_enchantment_from_oracle(P0, "Delayed Counter Engine", DELAYED_COUNTER_ORACLE);
    let departing = scenario
        .add_creature(P0, "Countered Artifact", 2, 2)
        .as_artifact()
        .id();
    let recipient = scenario.add_creature(P0, "Recipient", 2, 2).id();
    scenario.add_card_to_library_top(P1, "Island");
    scenario.add_card_to_library_top(P1, "Plains");
    scenario.with_counter(departing, CounterType::Plus1Plus1, 2);
    scenario.with_counter(departing, CounterType::Generic("charge".to_string()), 1);
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Vindicate", false, VINDICATE_ORACLE)
        .id();

    let mut runner = scenario.build();
    let destroy_outcome = runner.cast(destroy).target_object(departing).resolve();
    assert_eq!(
        destroy_outcome.zone_of(departing),
        Zone::Graveyard,
        "reach-guard: Vindicate must destroy the counter-bearing permanent"
    );
    assert_eq!(
        destroy_outcome.counters(recipient, CounterType::Plus1Plus1),
        0,
        "reach-guard: counters must not be placed before the delayed trigger fires"
    );

    runner.advance_to_end_step();
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(recipient)),
                    })
                    .expect("delayed trigger target choice must succeed");
                break;
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("ordering the delayed trigger must succeed");
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority must reach the delayed trigger target prompt");
            }
            other => panic!("unexpected prompt before delayed trigger target choice: {other:?}"),
        }
    }
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "delayed trigger target prompt must have been answered"
    );
    runner.advance_until_stack_empty();

    assert_eq!(
        runner
            .state()
            .objects
            .get(&recipient)
            .and_then(|obj| obj.counters.get(&CounterType::Plus1Plus1).copied())
            .unwrap_or(0),
        3,
        "the delayed trigger must use the departed permanent's three counters"
    );
}

/// Positive: CR 122.1 counters of multiple kinds are all counted for Yuna, and
/// the engine must saturate rather than overflow when the sum exceeds i32::MAX.
#[test]
fn yuna_counter_count_saturates_without_overflow() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, three_generic());
    scenario
        .add_creature_from_oracle(P0, "Yuna, Grand Summoner", 1, 5, YUNA_ORACLE)
        .as_legendary();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Vindicate", false, VINDICATE_ORACLE)
        .id();
    let departing = scenario
        .add_artifact_from_oracle(P0, "Counter Relic", "{T}: You gain 1 life.")
        .id();
    let recipient = scenario.add_creature(P0, "Recipient", 2, 2).id();
    scenario.with_counter(
        departing,
        CounterType::Generic("charge".to_string()),
        u32::MAX,
    );
    scenario.with_counter(departing, CounterType::Generic("oil".to_string()), 1);

    let mut runner = scenario.build();
    let outcome = runner
        .cast(destroy)
        .target_object(departing)
        .target_object(recipient)
        .accept_optional()
        .resolve();

    assert_eq!(
        outcome.zone_of(departing),
        Zone::Graveyard,
        "reach-guard: Vindicate must destroy the counter-bearing artifact"
    );
    assert_eq!(
        outcome.counters(recipient, CounterType::Plus1Plus1),
        i32::MAX as u32,
        "the counter total must saturate to i32::MAX instead of overflowing"
    );
}

/// Nikara, Lair Scavenger — verbatim Oracle text. The same "if it had one or
/// more counters on it" gate, on a leaves-the-battlefield trigger (CR 603.10a).
const NIKARA_ORACLE: &str = "Partner with Yannik, Scavenging Sentinel (When this creature enters, target player may put Yannik into their hand from their library, then shuffle.)\nMenace\nWhenever another creature you control leaves the battlefield, if it had one or more counters on it, you draw a card and you lose 1 life.";

fn nikara_departure(counters: &[(CounterType, u32)]) -> (i32, usize, i32, usize, Zone) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, three_generic());
    for name in ["Island", "Plains", "Swamp"] {
        scenario.add_card_to_library_top(P0, name);
    }
    scenario.add_creature_from_oracle(P0, "Nikara, Lair Scavenger", 2, 2, NIKARA_ORACLE);
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Vindicate", false, VINDICATE_ORACLE)
        .id();
    let departing = scenario.add_creature(P0, "Leaving Creature", 2, 2).id();
    for (kind, count) in counters {
        scenario.with_counter(departing, kind.clone(), *count);
    }
    let mut runner = scenario.build();
    let life_before = runner.life(P0);
    let hand_before = runner.state().players[0].hand.len();
    let outcome = runner.cast(destroy).target_object(departing).resolve();
    let zone = outcome.zone_of(departing);
    runner.advance_until_stack_empty();
    (
        life_before,
        hand_before,
        runner.life(P0),
        runner.state().players[0].hand.len(),
        zone,
    )
}

/// CR 603.4 + CR 603.10a + CR 608.2h: Nikara's trigger fires only when the
/// departed creature had a counter, and then draws one card and loses 1 life.
/// The counterless departure is the paired negative; both boards prove the
/// creature actually left (reach guard), and the hand count excludes the cast
/// Vindicate.
#[test]
fn nikara_draws_and_loses_life_only_for_a_countered_departure() {
    let (life_before, hand_before, life_after, hand_after, zone) =
        nikara_departure(&[(CounterType::Generic("oil".to_string()), 1)]);
    assert_eq!(zone, Zone::Graveyard, "reach-guard: the creature left");
    assert_eq!(life_after, life_before - 1, "you lose 1 life");
    assert_eq!(
        hand_after,
        hand_before, // -1 Vindicate cast, +1 card drawn
        "you draw a card (net of the cast Vindicate)"
    );

    let (life_before, hand_before, life_after, hand_after, zone) = nikara_departure(&[]);
    assert_eq!(zone, Zone::Graveyard, "reach-guard: the creature left");
    assert_eq!(life_after, life_before, "no counters: no life loss");
    assert_eq!(hand_after, hand_before - 1, "no counters: no draw");
}
