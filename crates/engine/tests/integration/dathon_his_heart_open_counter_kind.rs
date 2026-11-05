//! Dathon, His Heart Open (TRK):
//!
//! "Whenever you cast an enchantment spell, draw a card. If it's a Saga spell,
//!  Dathon deals 2 damage to each opponent.
//!  {2}: Choose a counter on target enchantment you control. Put another
//!  counter of that kind on it. Activate only as a sorcery."
//!
//! CR 608.2d + CR 122.1: the controller chooses one kind among those already
//! on the target (no choice with one kind, none with zero), then CR 122.6 one
//! more counter of that kind is put on it.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{AbilityDefinition, AbilityKind, Effect};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

const DATHON: &str = "Whenever you cast an enchantment spell, draw a card. If it's a Saga spell, ~ deals 2 damage to each opponent.\n{2}: Choose a counter on target enchantment you control. Put another counter of that kind on it. Activate only as a sorcery.";

fn counters(runner: &GameRunner, id: ObjectId, kind: CounterType) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&kind)
        .copied()
        .unwrap_or(0)
}

fn contains_unimplemented(def: &AbilityDefinition) -> bool {
    matches!(def.effect.as_ref(), Effect::Unimplemented { .. })
        || def
            .sub_ability
            .as_deref()
            .is_some_and(contains_unimplemented)
        || def
            .else_ability
            .as_deref()
            .is_some_and(contains_unimplemented)
}

fn setup(counters: &[(CounterType, u32)]) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dathon = scenario
        .add_creature_from_oracle(P0, "Dathon, His Heart Open", 3, 3, DATHON)
        .id();
    let ench = scenario
        .add_enchantment_from_oracle(P0, "Test Enchantment", "")
        .id();
    for (kind, n) in counters {
        scenario.with_counter(ench, kind.clone(), *n);
    }
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]); 2],
    );
    let runner = scenario.build();
    let obj = &runner.state().objects[&dathon];
    assert!(
        !obj.abilities.iter().any(contains_unimplemented),
        "Dathon's activated ability must parse with no Unimplemented residue: {:?}",
        obj.abilities
    );
    (runner, dathon, ench)
}

fn activated_index(runner: &GameRunner, source: ObjectId) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(|a| matches!(a.kind, AbilityKind::Activated))
        .expect("Dathon's activated ability")
}

#[test]
fn single_counter_kind_gets_one_more_of_that_kind() {
    let (mut runner, dathon, ench) = setup(&[(CounterType::Stun, 1)]);
    let idx = activated_index(&runner, dathon);
    runner.activate(dathon, idx).target_object(ench).resolve();
    assert_eq!(counters(&runner, ench, CounterType::Stun), 2);
}

#[test]
fn two_kinds_prompt_and_only_the_chosen_kind_grows() {
    let (mut runner, dathon, ench) = setup(&[(CounterType::Stun, 1), (CounterType::Shield, 1)]);
    let idx = activated_index(&runner, dathon);
    runner.activate(dathon, idx).target_object(ench).resolve();

    let WaitingFor::NamedChoice { options, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "two counter kinds must prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(options.len(), 2, "one option per kind on the target");
    let choice = options.last().cloned().expect("option");
    runner
        .act(GameAction::ChooseOption { choice })
        .expect("answer the counter-kind choice");
    for _ in 0..10 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pass");
    }
    let stun = counters(&runner, ench, CounterType::Stun);
    let shield = counters(&runner, ench, CounterType::Shield);
    assert_eq!(stun + shield, 3, "exactly one counter added");
    assert!(
        (stun, shield) == (1, 2) || (stun, shield) == (2, 1),
        "one kind has 2, the other stays 1: {stun}/{shield}"
    );
}

#[test]
fn no_counters_adds_none() {
    let (mut runner, dathon, ench) = setup(&[]);
    let idx = activated_index(&runner, dathon);
    runner.activate(dathon, idx).target_object(ench).resolve();
    let obj = &runner.state().objects[&ench];
    assert!(obj.counters.values().all(|n| *n == 0), "nothing to copy");
}
