//! Runtime regression for issue #7817 — Elspeth Resplendent's +1 placed no
//! counter at all: "Put a +1/+1 counter and a counter from among flying, first
//! strike, lifelink, or vigilance on it" lowered to `Effect::Unimplemented`.
//!
//! Both halves already worked apart. The conjoined pair of FIXED kinds is
//! Unexpected Fangs' grammar, and the bare "from among" choice is Aragorn,
//! Company Leader's. Only one fixed counter conjoined with one chosen counter
//! had no reader.
//!
//! Claim-to-test matrix:
//! - the fixed half is unconditional → the +1/+1 counter lands whatever is
//!   chosen;
//! - the chosen half is a real resolution-time choice → the option the player
//!   answers with is the kind that lands, and a different answer lands a
//!   different kind;
//! - one shared target → both counters go on the same creature.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::KeywordKind;
use engine::types::phase::Phase;

/// Verbatim from `client/public/card-data.json`, the +1 line only.
const ELSPETH_PLUS_ONE: &str = "[+1]: Choose up to one target creature. Put a +1/+1 counter and \
     a counter from among flying, first strike, lifelink, or vigilance on it.";

/// Activates the +1 on the single legal creature and answers the counter-kind
/// choice with `chosen`. Returns the recipient's counters afterwards.
fn plus_one_choosing(chosen: &str) -> std::collections::HashMap<CounterType, u32> {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let elspeth = scenario
        .add_planeswalker_from_oracle(P0, "Elspeth Resplendent", "Elspeth", 5, ELSPETH_PLUS_ONE)
        .id();
    let recipient = scenario.add_creature(P0, "Recipient", 2, 2).id();
    let mut runner = scenario.build();

    runner
        .act(GameAction::ActivateAbility {
            source_id: elspeth,
            ability_index: 0,
        })
        .expect("the +1 is activatable");

    // "Choose up to one target creature" is announced at activation: CR 602.2b
    // routes an activated ability through the spell-casting steps 601.2b-i, so
    // CR 601.2c picks the target before the ability reaches the stack.
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(recipient)],
            })
            .expect("the lone creature is a legal target");
    }

    // Precondition, not a reach-guard: an unlowered clause reaches the stack
    // just the same. What discriminates is the `answered` assertion in
    // `answer_counter_choice` — without the lowering no branch choice is ever
    // offered.
    assert!(
        !runner.state().stack.is_empty(),
        "the +1 must be on the stack, waiting_for = {:?}",
        runner.state().waiting_for
    );

    answer_counter_choice(&mut runner, chosen, recipient);

    runner
        .state()
        .objects
        .get(&recipient)
        .expect("the recipient stays on the battlefield")
        .counters
        .iter()
        .filter(|(_, count)| **count > 0)
        .map(|(kind, count)| (kind.clone(), *count))
        .collect()
}

/// Drives resolution, answering the one counter-kind choice on the way. The
/// branch is picked BY ITS PRINTED DESCRIPTION, not by index: an index would
/// still pass if the branches were built in the wrong order.
fn answer_counter_choice(
    runner: &mut engine::game::scenario::GameRunner,
    chosen: &str,
    recipient: ObjectId,
) {
    let mut answered = false;
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ChooseOneOfBranch {
                branch_descriptions,
                ..
            } => {
                let index = branch_descriptions
                    .iter()
                    .position(|description| description.to_lowercase().contains(chosen))
                    .unwrap_or_else(|| {
                        panic!("no branch offers {chosen}; offered: {branch_descriptions:?}")
                    });
                runner
                    .act(GameAction::ChooseBranch { index })
                    .expect("resolving the chosen counter branch must succeed");
                answered = true;
            }
            _ => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner.advance_until_stack_empty();
            }
        }
    }
    assert!(
        answered,
        "the chosen half must ask: a resolution-time choice, not a silent pick. \
         Recipient counters: {:?}",
        runner
            .state()
            .objects
            .get(&recipient)
            .map(|object| object.counters.clone())
    );
}

/// CR 122.1a + CR 122.1b + CR 608.2d: the unconditional +1/+1 counter and the
/// chosen keyword counter both land, on the one shared target.
#[test]
fn elspeth_plus_one_places_the_fixed_counter_and_the_chosen_one() {
    let counters = plus_one_choosing("flying");

    assert_eq!(
        counters.get(&CounterType::Plus1Plus1).copied(),
        Some(1),
        "the +1/+1 half is unconditional, got {counters:?}"
    );
    assert_eq!(
        counters
            .get(&CounterType::Keyword(KeywordKind::Flying))
            .copied(),
        Some(1),
        "the chosen kind must land too, got {counters:?}"
    );
    assert_eq!(counters.len(), 2, "exactly those two, got {counters:?}");
}

/// The chooser's answer is what decides the second kind — pinning that the
/// branches are a real choice and not a fixed first option.
#[test]
fn elspeth_plus_one_lands_the_kind_the_player_answered() {
    let counters = plus_one_choosing("vigilance");

    assert_eq!(
        counters
            .get(&CounterType::Keyword(KeywordKind::Vigilance))
            .copied(),
        Some(1),
        "answering \"vigilance\" must place vigilance, got {counters:?}"
    );
    assert!(
        !counters.contains_key(&CounterType::Keyword(KeywordKind::Flying)),
        "no other printed kind may ride along, got {counters:?}"
    );
    assert_eq!(
        counters.get(&CounterType::Plus1Plus1).copied(),
        Some(1),
        "the fixed half is unchanged by the answer, got {counters:?}"
    );
}

/// The card's printed ruling (2022-04-29) says the chosen counter and the
/// +1/+1 counter "are placed on the target creature at the same time". The
/// observable consequence is the firing granularity of a watcher. Captain
/// Marvel, Apex Avenger triggers on "one or more counters on another
/// creature" — its own wording makes the BATCH the unit, not the kind — and
/// CR 603.2c then gives one trigger per occurrence of that event. So one
/// placement of two kinds must fire it exactly ONCE.
///
/// This is what fails if the fixed half is placed BEFORE the choice: the
/// player decision splits the two placements into separate event batches and
/// the watcher fires twice.
#[test]
fn both_counters_reach_the_watcher_as_one_placement() {
    const CAPTAIN_MARVEL: &str = "Flying, double strike, indestructible\nWhenever you put one or \
                                  more counters on another creature, if it's not a Kree, you may \
                                  put the same number and kind of counters on Captain Marvel.";

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let elspeth = scenario
        .add_planeswalker_from_oracle(P0, "Elspeth Resplendent", "Elspeth", 5, ELSPETH_PLUS_ONE)
        .id();
    let watcher = scenario
        .add_creature_from_oracle(P0, "Captain Marvel, Apex Avenger", 4, 4, CAPTAIN_MARVEL)
        .id();
    let recipient = scenario.add_creature(P0, "Recipient", 2, 2).id();
    let mut runner = scenario.build();

    let mut events: Vec<engine::types::events::GameEvent> = Vec::new();
    let mut push = |result: engine::types::game_state::ActionResult| {
        events.extend(result.events);
    };

    push(
        runner
            .act(GameAction::ActivateAbility {
                source_id: elspeth,
                ability_index: 0,
            })
            .expect("the +1 is activatable"),
    );
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        push(
            runner
                .act(GameAction::SelectTargets {
                    targets: vec![TargetRef::Object(recipient)],
                })
                .expect("the recipient is a legal target"),
        );
    }

    // Drive to the end, answering the counter choice and accepting the
    // watcher's optional reproduction whenever either is asked.
    let mut prompts = 0;
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ChooseOneOfBranch {
                branch_descriptions,
                ..
            } => {
                let index = branch_descriptions
                    .iter()
                    .position(|d| d.to_lowercase().contains("flying"))
                    .expect("a flying branch must exist");
                push(
                    runner
                        .act(GameAction::ChooseBranch { index })
                        .expect("choose flying"),
                );
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                prompts += 1;
                push(
                    runner
                        .act(GameAction::DecideOptionalEffect { accept: true })
                        .expect("accept the watcher's reproduction"),
                );
            }
            _ => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner.advance_until_stack_empty();
            }
        }
    }

    // Reach-guard: the placement really happened.
    let recipient_counters = &runner.state().objects[&recipient].counters;
    assert_eq!(
        recipient_counters
            .get(&CounterType::Keyword(KeywordKind::Flying))
            .copied(),
        Some(1),
        "recipient counters: {recipient_counters:?}"
    );
    assert_eq!(
        recipient_counters.get(&CounterType::Plus1Plus1).copied(),
        Some(1),
        "recipient counters: {recipient_counters:?}"
    );

    // Every reproduction asks first, and every ask runs through `act`, so this
    // count is complete. The event tally below is not: `advance_until_stack_empty`
    // returns `()`, so anything resolving inside it is invisible here.
    assert_eq!(
        prompts,
        1,
        "the watcher must ask exactly once; watcher counters: {:?}",
        runner.state().objects[&watcher].counters
    );

    let firings = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                engine::types::events::GameEvent::EffectResolved { kind, .. }
                    if *kind == engine::types::ability::EffectKind::ReproduceEventCounters
            )
        })
        .count();
    assert_eq!(
        firings,
        1,
        "one placement of two kinds on one creature must fire the watcher once \
         (one trigger per occurrence, CR 603.2c), got {firings}; \
         watcher counters: {:?}",
        runner.state().objects[&watcher].counters
    );
}

/// CR 115.6: "a spell or ability that requires targets may allow zero targets
/// to be chosen" (CR 601.2c only fixes WHEN that count is announced). Declining
/// must then place nothing — in particular not on Elspeth herself, who is not a
/// legal recipient of her own +1.
#[test]
fn elspeth_plus_one_with_no_target_places_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let elspeth = scenario
        .add_planeswalker_from_oracle(P0, "Elspeth Resplendent", "Elspeth", 5, ELSPETH_PLUS_ONE)
        .id();
    let bystander = scenario.add_creature(P0, "Bystander", 2, 2).id();
    let mut runner = scenario.build();

    runner
        .act(GameAction::ActivateAbility {
            source_id: elspeth,
            ability_index: 0,
        })
        .expect("the +1 is activatable");
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::SelectTargets { targets: vec![] })
            .expect("zero targets is legal for \"up to one\"");
    }

    let mut answered = false;
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ChooseOneOfBranch {
                branch_descriptions,
                ..
            } => {
                let index = branch_descriptions
                    .iter()
                    .position(|d| d.to_lowercase().contains("flying"))
                    .expect("a flying branch");
                runner
                    .act(GameAction::ChooseBranch { index })
                    .expect("choose flying");
                answered = true;
            }
            _ => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner.advance_until_stack_empty();
            }
        }
    }

    // Two reach-guards with DIFFERENT reach. The loyalty cost is paid at
    // ACTIVATION (CR 602.2b -> 601.2b-i), so it proves only that the ability was
    // announced. What rules out "never resolved" is `answered`: the branch
    // question is asked while the effect is applied.
    assert_eq!(
        runner.state().objects[&elspeth]
            .counters
            .get(&CounterType::Loyalty)
            .copied(),
        Some(6),
        "the +1 must have been paid: 5 loyalty plus one"
    );
    assert!(
        answered,
        "the counter-kind choice must still be offered with no target chosen"
    );

    let elspeth_counters: Vec<_> = runner.state().objects[&elspeth]
        .counters
        .iter()
        .filter(|(kind, count)| **count > 0 && **kind != CounterType::Loyalty)
        .collect();
    assert!(
        elspeth_counters.is_empty(),
        "no target chosen: Elspeth must receive no counter of her own ability, got {elspeth_counters:?}"
    );
    assert!(
        runner.state().objects[&bystander].counters.is_empty(),
        "the untargeted creature must stay untouched"
    );
}
