//! Work bounds for real Second Harvest copy deliveries (CR 603.6d + CR 614.12).

use engine::game::perf_counters;
use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const HAMMER_JAMMER: &str = "As this creature enters, roll a six-sided die. This creature enters with a number of +1/+1 counters on it equal to the result.\nWhenever you roll a die, remove all +1/+1 counters from this creature, then put a number of +1/+1 counters on it equal to the result.";
const SECOND_HARVEST: &str =
    "For each token you control, create a token that's a copy of that permanent.";

#[test]
fn growing_second_harvest_batches_share_the_pre_entry_state_allocation() {
    let mut previous_probes = None;
    for (count, pauses_on_tie) in [(2_usize, false), (4, false), (8, false), (8, true)] {
        let mut scenario = GameScenario::new_n_player(2, 7);
        scenario.at_phase(Phase::PreCombatMain);
        if pauses_on_tie {
            scenario.add_creature_from_oracle(P0, "Pixie Guide", 1, 3,
                "Flying\nGrant an Advantage — If you would roll one or more dice, instead roll that many dice plus one and ignore the lowest roll.");
        }
        let originals: Vec<_> = (0..count)
            .map(|_| {
                let id = scenario
                    .add_creature_from_oracle(P0, "Hammer Jammer", 0, 0, HAMMER_JAMMER)
                    .id();
                scenario.with_counter(id, CounterType::Plus1Plus1, 1);
                id
            })
            .collect();
        let harvest = scenario
            .add_spell_to_hand_from_oracle(P0, "Second Harvest", true, SECOND_HARVEST)
            .with_mana_cost(ManaCost::generic(0))
            .id();
        let mut runner = scenario.build();
        // Fixture: these resident Jammers are token copies, with the printed
        // copiable abilities. All measured creation goes through Second Harvest.
        for id in &originals {
            runner.state_mut().objects.get_mut(id).unwrap().is_token = true;
        }
        let commit = runner.cast(harvest).commit();
        let mut events = commit.events().to_vec();
        drop(commit);
        perf_counters::reset_copy_entry_work_counters();
        let mut pauses = 0;
        let mut previous_board_builds = 0;
        for _ in 0..(16 * count * count) {
            match &runner.state().waiting_for {
                WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
                WaitingFor::Priority { .. } => {
                    events.extend(runner.act(GameAction::PassPriority).unwrap().events);
                }
                WaitingFor::DieKeepChoice {
                    ignorable_indices,
                    ignore_count,
                    ..
                } => {
                    pauses += 1;
                    let ignore_indices = ignorable_indices
                        .iter()
                        .take(*ignore_count)
                        .copied()
                        .collect();
                    events.extend(
                        runner
                            .act(GameAction::SelectDieRolls { ignore_indices })
                            .unwrap()
                            .events,
                    );
                }
                WaitingFor::OrderTriggers { .. } => {
                    engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
                }
                other => panic!("unexpected copy-delivery prompt: {other:?}"),
            }
            let builds = perf_counters::copy_entry_work_snapshot().full_state_board_builds;
            assert!(
                builds - previous_board_builds <= 2,
                "{count} copies, tied-roll fixture {pauses_on_tie}, pauses {pauses}: per-action builds {} (total {builds}); a resumed member and remaining owner share their allocations", builds - previous_board_builds
            );
            previous_board_builds = builds;
        }
        let work = perf_counters::copy_entry_work_snapshot();
        let state = runner.state();
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        assert!(
            state.stack.is_empty(),
            "every resident roll trigger resolves"
        );
        let rolls = events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    GameEvent::DieRolled {
                        result: Some(_),
                        ..
                    }
                )
            })
            .count();
        assert_eq!(rolls, count, "one actual roll per new Jammer");
        assert_eq!(
            state
                .battlefield
                .iter()
                .filter(|id| state.objects[id].name == "Hammer Jammer")
                .count(),
            2 * count,
            "every original and copy survives"
        );
        for id in originals {
            assert_eq!(state.objects[&id].zone, Zone::Battlefield);
            let resets = events.iter().filter(|event| matches!(event,
                GameEvent::CounterRemoved { object_id, counter_type: CounterType::Plus1Plus1, .. }
                    if *object_id == id
            )).count();
            assert_eq!(
                resets, count,
                "the resident's printed trigger resets counters after each roll"
            );
            assert!(state.objects[&id].counters[&CounterType::Plus1Plus1] > 0);
        }
        if pauses_on_tie {
            assert!(
                pauses > 0,
                "reach: a tied as-enters roll paused this copy owner"
            );
            assert!(work.full_state_board_builds <= 2 * (pauses + 1), "{work:?}");
        } else {
            assert_eq!(work.full_state_board_builds, 1, "{count} copies: {work:?}");
        }
        assert!(
            work.membership_probes > 0,
            "the shared membership authority ran"
        );
        assert!(
            work.membership_probes <= 32 * (count * count) as u64,
            "{work:?}"
        );
        if !pauses_on_tie {
            if let Some(previous) = previous_probes {
                assert!(
                    work.membership_probes <= 6 * previous,
                    "doubling copies is not cubic: {work:?}"
                );
            }
            previous_probes = Some(work.membership_probes);
        }
    }
}
