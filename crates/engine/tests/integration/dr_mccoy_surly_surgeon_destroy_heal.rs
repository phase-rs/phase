//! Dr. McCoy, Surly Surgeon (TRK):
//!
//! "When Dr. McCoy enters, put a +1/+1 counter on another target creature you control.
//!  If another creature you control with a +1/+1 counter on it would be
//!  destroyed, instead remove all +1/+1 counters from it and all damage
//!  already dealt to it is healed."
//!
//! CR 614.1a + CR 701.8a: the destruction of a matching creature is replaced;
//! CR 120.6: marked damage is removed. Unlike regeneration (CR 701.19) the
//! creature is not tapped and not removed from combat.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{AbilityDefinition, Effect};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const MCCOY: &str = "When Dr. McCoy enters, put a +1/+1 counter on another target creature you control.\nIf another creature you control with a +1/+1 counter on it would be destroyed, instead remove all +1/+1 counters from it and all damage already dealt to it is healed.";
const MURDER: &str = "Destroy target creature.";

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

/// McCoy plus a Bear carrying `counters` +1/+1 counters and 1 marked damage;
/// then P0 destroys the Bear. Returns (runner, mccoy, bear).
fn destroy_bear(counters: u32) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mccoy = scenario
        .add_creature_from_oracle(P0, "Dr. McCoy, Surly Surgeon", 2, 2, MCCOY)
        .id();
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    scenario.with_counter(bear, CounterType::Plus1Plus1, counters);
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = scenario.build();
    // Reach guard: the replacement parsed with no Unimplemented residue.
    let obj = &runner.state().objects[&mccoy];
    assert!(
        obj.replacement_definitions
            .iter_all()
            .any(|r| r.execute.is_some()),
        "McCoy must parse a destroy replacement, got abilities {:?}",
        obj.abilities
    );
    assert!(!obj.abilities.iter().any(contains_unimplemented));
    runner
        .state_mut()
        .objects
        .get_mut(&bear)
        .expect("bear")
        .damage_marked = 1;
    runner.cast(murder).target_object(bear).resolve();
    (runner, mccoy, bear)
}

#[test]
fn creature_with_counter_survives_loses_counters_and_is_healed() {
    let (runner, _mccoy, bear) = destroy_bear(2);
    assert!(runner.state().battlefield.contains(&bear), "not destroyed");
    let obj = &runner.state().objects[&bear];
    assert_eq!(
        counters(&runner, bear, CounterType::Plus1Plus1),
        0,
        "counters removed"
    );
    assert_eq!(obj.damage_marked, 0, "damage healed");
    assert!(!obj.tapped, "not regeneration: it is not tapped");
}

#[test]
fn creature_without_counter_is_destroyed_normally() {
    let (runner, _mccoy, bear) = destroy_bear(0);
    assert!(!runner.state().battlefield.contains(&bear));
}

/// "Another": McCoy's own destruction is not replaced, even with a counter.
#[test]
fn mccoy_himself_with_counter_is_destroyed() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mccoy = scenario
        .add_creature_from_oracle(P0, "Dr. McCoy, Surly Surgeon", 2, 2, MCCOY)
        .id();
    scenario.with_counter(mccoy, CounterType::Plus1Plus1, 1);
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = scenario.build();
    runner.cast(murder).target_object(mccoy).resolve();
    assert!(
        !runner.state().battlefield.contains(&mccoy),
        "McCoy only protects another creature"
    );
}

/// CR 704.5g: lethal combat damage destroys the creature as a state-based
/// action, which is a destruction McCoy replaces (CR 614.1a): the Bear
/// survives with its counter removed and its damage healed.
#[test]
fn creature_with_counter_survives_lethal_combat_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Dr. McCoy, Surly Surgeon", 2, 2, MCCOY);
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    scenario.with_counter(bear, CounterType::Plus1Plus1, 1);
    let wurm = scenario.add_creature(P1, "Craw Wurm", 6, 4).id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(bear, AttackTarget::Player(P1))])
        .expect("declare attackers");
    for _ in 0..8 {
        if runner.waiting_for_kind() == "DeclareBlockers" {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority into the declare-blockers step");
    }
    runner
        .declare_blockers(&[(wurm, bear)])
        .expect("declare blockers");
    runner.combat_damage();

    assert!(
        runner.state().battlefield.contains(&bear),
        "lethal damage destruction is replaced"
    );
    assert_eq!(counters(&runner, bear, CounterType::Plus1Plus1), 0);
    assert_eq!(runner.state().objects[&bear].damage_marked, 0, "healed");
    assert!(
        runner.state().battlefield.contains(&wurm),
        "the 6/4 Wurm took only 3"
    );
}
