//! CR 614.1a + CR 122.6: counter-placement replacement effects — printed
//! statics scoped by actor / recipient / counter type, a player-recipient
//! clause, and until-end-of-turn replacements created by a resolving trigger or
//! activation (CR 611.2a), all through the production cast / activate /
//! trigger pipeline.
//!
//! Oracle text verbatim from `client/public/card-data.json`.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{Effect, RestrictionExpiry};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;

const GEORDI: &str = "Lt. Cmdr. Geordi La Forge";
const GEORDI_ORACLE: &str = "If you would put one or more charge counters on an artifact you control, put twice that many charge counters on that artifact instead.";
const BRAD: &str = "Brad Boimler, Eager Ensign";
const BRAD_ORACLE: &str = "Lifelink\nWhenever Brad Boimler becomes tapped, until end of turn, if one or more counters would be put on a permanent you control, that many plus one of each of those kinds of counters are put on that permanent instead.";
const PRAIRIE_DOG_ORACLE: &str = "Lifelink\nAt the beginning of your end step, if you haven't cast a spell from your hand this turn, put a +1/+1 counter on this creature.\n{4}{W}: Until end of turn, if you would put one or more +1/+1 counters on a creature you control, put that many plus one +1/+1 counters on it instead.";
const WINDING_CONSTRICTOR: &str = "Winding Constrictor";
const WINDING_CONSTRICTOR_ORACLE: &str = "If one or more counters would be put on an artifact or creature you control, that many plus one of each of those kinds of counters are put on that permanent instead.\nIf you would get one or more counters, you get that many plus one of each of those kinds of counters instead.";
const SURGE_NODE_ORACLE: &str = "This artifact enters with six charge counters on it.\n{1}, {T}, Remove a charge counter from this artifact: Put a charge counter on target artifact.";
const BATTLEGROWTH_ORACLE: &str = "Put a +1/+1 counter on target creature.";
const DJERUS_RENUNCIATION_ORACLE: &str = "Tap up to two target creatures.";
const GLIMMER_OF_GENIUS_ORACLE: &str =
    "Scry 2. Draw two cards. You get {E}{E} (two energy counters).";

fn charge() -> CounterType {
    CounterType::Generic("charge".to_string())
}

fn colorless(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

fn counters(runner: &GameRunner, id: ObjectId, ct: &CounterType) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(ct)
        .copied()
        .unwrap_or(0)
}

/// A Surge Node with charge counters to spend; its activation is the
/// production "you put a charge counter on target artifact" event.
fn add_surge_node(scenario: &mut GameScenario) -> ObjectId {
    let node = scenario
        .add_artifact_from_oracle(P0, "Surge Node", SURGE_NODE_ORACLE)
        .id();
    scenario.with_counter(node, charge(), 6);
    node
}

fn activate_surge_node(runner: &mut GameRunner, node: ObjectId, target: ObjectId) {
    runner.activate(node, 0).target_object(target).resolve();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "Surge Node's activation must resolve to priority, got {:?}",
        runner.state().waiting_for
    );
}

/// CR 614.1a + CR 122.6: Geordi doubles charge counters YOU put on an artifact
/// YOU control — and only charge counters, and only on your artifacts.
#[test]
fn geordi_doubles_only_charge_counters_you_put_on_your_artifacts() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, GEORDI, 3, 2, GEORDI_ORACLE);
    let node = add_surge_node(&mut scenario);
    let second_node = add_surge_node(&mut scenario);
    let my_artifact = scenario.add_artifact_from_oracle(P0, "Mind Stone", "").id();
    let their_artifact = scenario.add_artifact_from_oracle(P1, "Mind Stone", "").id();
    let my_artifact_creature = scenario
        .add_creature(P0, "Ornithopter", 0, 2)
        .as_artifact_creature()
        .id();
    let growth = scenario
        .add_spell_to_hand_from_oracle(P0, "Battlegrowth", true, BATTLEGROWTH_ORACLE)
        .id();
    scenario.with_mana_pool(P0, colorless(2));
    let mut runner = scenario.build();

    // Reach guard + positive: one charge counter on my artifact becomes two.
    activate_surge_node(&mut runner, node, my_artifact);
    assert_eq!(counters(&runner, my_artifact, &charge()), 2);

    // An opponent's artifact is outside "an artifact you control".
    activate_surge_node(&mut runner, second_node, their_artifact);
    assert_eq!(counters(&runner, their_artifact, &charge()), 1);

    // A +1/+1 counter on my artifact is not a charge counter.
    runner
        .cast(growth)
        .target_object(my_artifact_creature)
        .resolve();
    assert_eq!(
        counters(&runner, my_artifact_creature, &CounterType::Plus1Plus1),
        1
    );
}

/// CR 616.1 + CR 616.1f: two counter replacements on one event — the affected
/// object's controller orders them, and the chosen order is what applies
/// (Winding Constrictor then Geordi: (1 + 1) * 2 = 4; Geordi then Winding
/// Constrictor: 1 * 2 + 1 = 3).
#[test]
fn geordi_and_winding_constrictor_apply_in_the_chosen_order() {
    for (constrictor_first, expected) in [(true, 4), (false, 3)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let geordi = scenario
            .add_creature_from_oracle(P0, GEORDI, 3, 2, GEORDI_ORACLE)
            .id();
        let constrictor = scenario
            .add_creature_from_oracle(P0, WINDING_CONSTRICTOR, 2, 3, WINDING_CONSTRICTOR_ORACLE)
            .id();
        let node = add_surge_node(&mut scenario);
        let artifact = scenario.add_artifact_from_oracle(P0, "Mind Stone", "").id();
        scenario.with_mana_pool(P0, colorless(1));
        let mut runner = scenario.build();

        runner.activate(node, 0).target_object(artifact).resolve();
        let WaitingFor::ReplacementChoice {
            player, candidates, ..
        } = runner.state().waiting_for.clone()
        else {
            panic!(
                "two applicable counter replacements must prompt (CR 616.1), got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(player, P0, "the affected artifact's controller chooses");
        assert_eq!(candidates.len(), 2);
        let first = if constrictor_first {
            constrictor
        } else {
            geordi
        };
        let index = candidates
            .iter()
            .position(|c| c.source_id == first)
            .expect("both sources are offered");
        runner
            .act(GameAction::ChooseReplacement { index })
            .expect("choose replacement");
        runner.advance_to_priority_window();
        assert_eq!(
            counters(&runner, artifact, &charge()),
            expected,
            "constrictor_first={constrictor_first}"
        );
    }
}

/// CR 122.1 + CR 614.1a: Winding Constrictor's player clause adds one to the
/// counters you get (energy), and its permanent clause still applies.
#[test]
fn winding_constrictor_adds_one_to_counters_you_get() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for i in 0..4 {
        scenario.add_spell_to_library_top(P0, &format!("Filler {i}"), true);
    }
    scenario.add_creature_from_oracle(P0, WINDING_CONSTRICTOR, 2, 3, WINDING_CONSTRICTOR_ORACLE);
    let glimmer = scenario
        .add_spell_to_hand_from_oracle(P0, "Glimmer of Genius", true, GLIMMER_OF_GENIUS_ORACLE)
        .id();
    let mut runner = scenario.build();

    let before = runner.state().players[0].energy;
    runner.cast(glimmer).resolve();
    assert_eq!(
        runner.state().players[0].energy - before,
        3,
        "{{E}}{{E}} plus one"
    );
}

/// CR 611.2a + CR 614.1a: Brad's tap trigger creates an until-end-of-turn
/// replacement that adds one of each kind to counters put on permanents you
/// control (any kind, any permanent type), not on opponents' permanents, and
/// ends at end of turn — even though it outlives nothing on Brad himself.
#[test]
fn brad_tap_trigger_installs_until_end_of_turn_plus_one_counter_replacement() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let brad = scenario
        .add_creature_from_oracle(P0, BRAD, 2, 2, BRAD_ORACLE)
        .id();
    let mine = scenario.add_vanilla(P0, 2, 2);
    let theirs = scenario.add_vanilla(P1, 2, 2);
    let node = add_surge_node(&mut scenario);
    let artifact = scenario.add_artifact_from_oracle(P0, "Mind Stone", "").id();
    let tap = scenario
        .add_spell_to_hand_from_oracle(P0, "Djeru's Renunciation", true, DJERUS_RENUNCIATION_ORACLE)
        .id();
    let grow_mine = scenario
        .add_spell_to_hand_from_oracle(P0, "Battlegrowth", true, BATTLEGROWTH_ORACLE)
        .id();
    let grow_theirs = scenario
        .add_spell_to_hand_from_oracle(P0, "Battlegrowth", true, BATTLEGROWTH_ORACLE)
        .id();
    scenario.with_mana_pool(P0, colorless(1));
    let mut runner = scenario.build();

    // Before Brad is tapped, counters are unmodified.
    assert!(
        !runner
            .state()
            .pending_damage_replacements
            .iter()
            .any(|r| r.event == ReplacementEvent::AddCounter),
        "no counter replacement before the trigger"
    );

    runner.cast(tap).target_objects(&[brad]).resolve();
    runner.advance_to_priority_window();
    assert!(runner.state().objects[&brad].tapped, "Brad became tapped");
    assert!(runner.state().stack.is_empty(), "the tap trigger resolved");
    let installed: Vec<_> = runner
        .state()
        .pending_damage_replacements
        .iter()
        .filter(|r| r.event == ReplacementEvent::AddCounter)
        .collect();
    assert_eq!(
        installed.len(),
        1,
        "Brad's trigger installs one replacement"
    );
    assert_eq!(installed[0].expiry, Some(RestrictionExpiry::EndOfTurn));

    runner.cast(grow_mine).target_object(mine).resolve();
    assert_eq!(counters(&runner, mine, &CounterType::Plus1Plus1), 2);

    runner.cast(grow_theirs).target_object(theirs).resolve();
    assert_eq!(
        counters(&runner, theirs, &CounterType::Plus1Plus1),
        1,
        "an opponent's permanent is not a permanent you control"
    );

    activate_surge_node(&mut runner, node, artifact);
    assert_eq!(
        counters(&runner, artifact, &charge()),
        2,
        "any kind of counter, on any permanent you control"
    );

    // CR 514.2: the effect ends in the cleanup step.
    runner.advance_to_combat();
    runner.declare_attackers(&[]).expect("declare no attackers");
    runner.advance_to_phase(Phase::Upkeep);
    assert_ne!(
        runner.state().active_player,
        P0,
        "the turn has passed: phase={:?} waiting={:?}",
        runner.state().phase,
        runner.state().waiting_for
    );
    assert!(
        !runner
            .state()
            .pending_damage_replacements
            .iter()
            .any(|r| r.event == ReplacementEvent::AddCounter),
        "Brad's replacement expired at end of turn"
    );
}

/// CR 611.2a + CR 614.1a: Prairie Dog's activation creates an until-end-of-turn
/// "that many plus one" replacement for +1/+1 counters you put on creatures you
/// control — not for other counter kinds.
#[test]
fn prairie_dog_activation_adds_one_to_plus_one_counters_until_end_of_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dog = scenario
        .add_creature_from_oracle(P0, "Prairie Dog", 2, 2, PRAIRIE_DOG_ORACLE)
        .id();
    let node = add_surge_node(&mut scenario);
    let artifact = scenario.add_artifact_from_oracle(P0, "Mind Stone", "").id();
    let grow = scenario
        .add_spell_to_hand_from_oracle(P0, "Battlegrowth", true, BATTLEGROWTH_ORACLE)
        .id();
    let mut pool = colorless(5);
    pool.push(ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]));
    scenario.with_mana_pool(P0, pool);
    let mut runner = scenario.build();

    let activated = runner.state().objects[&dog]
        .abilities
        .iter()
        .position(|a| a.cost.is_some())
        .expect("Prairie Dog has an activated ability");
    assert!(
        matches!(
            *runner.state().objects[&dog].abilities[activated].effect,
            Effect::AddTargetReplacement { .. }
        ),
        "reach guard: the activation parses as a replacement install"
    );
    runner.activate(dog, activated).resolve();
    assert!(runner.state().stack.is_empty());

    runner.cast(grow).target_object(dog).resolve();
    assert_eq!(counters(&runner, dog, &CounterType::Plus1Plus1), 2);

    activate_surge_node(&mut runner, node, artifact);
    assert_eq!(
        counters(&runner, artifact, &charge()),
        1,
        "only +1/+1 counters are modified"
    );
}
