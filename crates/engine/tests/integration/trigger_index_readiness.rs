//! CR 603.2: `trigger_index::ensure_ready` keys its lazy rebuild on
//! `TriggerIndex::readiness`, not on bucket emptiness.

use engine::game::combat::AttackTarget;
use engine::game::perf_counters;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::trigger_index;
use engine::types::ability::{Effect, PtValue, QuantityExpr, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{DerivedIndexReadiness, GameState};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const GYSAHL_GREENS: &str = "Create a 2/2 green Bird creature token with \"Whenever a land you control enters, this token gets +1/+0 until end of turn.\"\nFlashback {6}{G} (You may cast this card from your graveyard for its flashback cost. Then exile it.)";
const LAUNCH_THE_FLEET: &str = "Strive \u{2014} This spell costs {1} more to cast for each target beyond the first.\nUntil end of turn, any number of target creatures each gain \"Whenever this creature attacks, create a 1/1 white Soldier creature token that's tapped and attacking.\"";

fn token_batch_effect(name: &str, types: &[&str], keywords: Vec<Keyword>, count: i32) -> Effect {
    Effect::Token {
        name: name.to_string(),
        power: PtValue::Fixed(1),
        toughness: PtValue::Fixed(1),
        types: types.iter().map(|t| (*t).to_string()).collect(),
        colors: vec![],
        keywords,
        tapped: false,
        count: QuantityExpr::Fixed { value: count },
        owner: TargetFilter::Controller,
        attach_to: None,
        enters_attacking: false,
        attack_destination: engine::types::ability::TokenAttackDestination::ControllerChooses,
        supertypes: vec![],
        static_abilities: vec![],
        enter_with_counters: vec![],
    }
}

/// Asserts the board shape these tests are about: the index was fully built,
/// holds no entry, and the battlefield is not empty.
fn assert_built_and_empty(state: &GameState, when: &str) {
    assert_eq!(
        state.trigger_index.readiness,
        DerivedIndexReadiness::Built,
        "{when}: the index must be built"
    );
    assert!(
        state.trigger_index.by_key.is_empty() && state.trigger_index.unclassified.is_empty(),
        "{when}: no permanent on this board carries a trigger source"
    );
    assert!(
        !state.battlefield.is_empty(),
        "{when}: the battlefield must not be empty"
    );
}

/// Resolves a batch of `n` vanilla tokens on a board with no trigger source
/// and returns the number of full trigger-index rebuilds it cost.
fn drive_token_batch(n: i32) -> u64 {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_vanilla(P0, 2, 2);
    let spell = scenario
        .add_spell_to_hand(P0, "Token Batch", false)
        .with_ability(token_batch_effect(
            "Batch Probe",
            &["Creature", "Soldier"],
            vec![],
            n,
        ))
        .id();
    let mut runner = scenario.build();
    let before = runner.state().battlefield.len();

    perf_counters::reset();
    let outcome = runner.cast(spell).resolve();
    let rebuilds = perf_counters::trigger_index_snapshot().full_rebuilds;

    assert_eq!(
        outcome.state().battlefield.len(),
        before + usize::try_from(n).unwrap(),
        "the batch must have entered the battlefield"
    );
    assert_built_and_empty(outcome.state(), "after the batch");
    rebuilds
}

#[test]
fn token_batch_on_trigger_free_board_rebuilds_independently_of_batch_size() {
    let small = drive_token_batch(8);
    let large = drive_token_batch(64);
    assert!(
        small >= 1,
        "the rebuild counter must observe at least the layer-pass rebuild"
    );
    assert_eq!(
        small, large,
        "rebuilds at n=8: {small}, n=64: {large}; a built index must not be rebuilt per event"
    );
}

fn battlefield_ids(state: &GameState) -> Vec<ObjectId> {
    state.battlefield.iter().copied().collect()
}

/// CR 603.6a: a land entering fires the token's own "Whenever a land you
/// control enters" trigger. The token is born after the index was built empty
/// and its entry takes a full layer pass.
#[test]
fn token_trigger_fires_after_index_built_empty() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_vanilla(P0, 2, 2);
    let forest = scenario.add_land_to_hand(P0, "Forest").id();
    let gysahl = scenario
        .add_spell_to_hand_from_oracle(P0, "Gysahl Greens", false, GYSAHL_GREENS)
        .id();
    let mut runner = scenario.build();
    trigger_index::ensure_ready(runner.state_mut());
    assert_built_and_empty(runner.state(), "before the cast");

    let before = battlefield_ids(runner.state());
    let commit = runner.cast(gysahl).commit();
    perf_counters::reset();
    let outcome = commit.resolve();
    let perf = perf_counters::snapshot();
    assert!(
        perf.layers_full_eval >= 1 && perf.layers_incremental == 0,
        "the Bird's entry must take a full layer pass: full_eval={}, incremental={}, escalated={}",
        perf.layers_full_eval,
        perf.layers_incremental,
        perf.layers_escalated
    );

    let birds: Vec<ObjectId> = outcome
        .state()
        .battlefield
        .iter()
        .copied()
        .filter(|id| !before.contains(id))
        .collect();
    assert_eq!(birds.len(), 1, "exactly one Bird token entered");
    let bird = birds[0];
    assert!(outcome.state().objects[&bird].is_token);
    assert_eq!(
        outcome.state().objects[&bird].power,
        Some(2),
        "the Bird is a 2/2 before any land enters"
    );

    let card_id = runner.state().objects[&forest].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: forest,
            card_id,
        })
        .expect("playing the Forest is legal");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&bird].power,
        Some(3),
        "the Bird's landfall trigger must fire for the Forest"
    );
}

/// CR 508.3a: a creature that gained "Whenever this creature attacks" from a
/// continuous effect, with no zone change, triggers when it attacks.
#[test]
fn granted_trigger_fires_after_index_built_empty() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_vanilla(P0, 2, 2);
    let launch = scenario
        .add_spell_to_hand_from_oracle(P0, "Launch the Fleet", false, LAUNCH_THE_FLEET)
        .id();
    let mut runner = scenario.build();
    trigger_index::ensure_ready(runner.state_mut());
    assert_built_and_empty(runner.state(), "before the cast");

    runner.cast(launch).target_objects(&[attacker]).resolve();
    let soldiers = |state: &GameState| -> Vec<ObjectId> {
        state
            .battlefield
            .iter()
            .copied()
            .filter(|id| state.objects[id].name == "Soldier")
            .collect()
    };
    assert!(
        soldiers(runner.state()).is_empty(),
        "no Soldier exists before the attack"
    );

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("declaring the attack is legal");
    runner.advance_until_stack_empty();

    let soldiers = soldiers(runner.state());
    assert_eq!(soldiers.len(), 1, "exactly one attack trigger resolves");
    let soldier = &runner.state().objects[&soldiers[0]];
    assert_eq!(soldier.controller, P0);
    assert!(soldier.tapped, "the Soldier enters tapped");
    assert!(
        runner
            .state()
            .combat
            .as_ref()
            .is_some_and(|combat| combat.attackers.iter().any(|a| a.object_id == soldiers[0])),
        "the Soldier enters attacking"
    );
}

/// CR 702.110a: a token created with exploit triggers on its own entry. The
/// token is born after the index was built empty and its entry takes the
/// incremental layer pass. The fixture is synthetic, not a printed card.
#[test]
fn token_born_keyword_trigger_fires_after_index_built_empty() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario.add_vanilla(P0, 2, 2);
    let spell = scenario
        .add_spell_to_hand(P0, "Exploit Token Maker", false)
        .with_ability(token_batch_effect(
            "Readiness Probe",
            &["Creature"],
            vec![Keyword::Exploit],
            1,
        ))
        .id();
    let mut runner: GameRunner = scenario.build();
    let before = battlefield_ids(runner.state());

    let mut commit = runner
        .cast(spell)
        .target_object(victim)
        .accept_optional()
        .effect_zone(&[victim])
        .commit();
    assert_built_and_empty(commit.state(), "after the cast's own layer pass");

    perf_counters::reset();
    commit
        .act(GameAction::PassPriority)
        .expect("the caster passes priority");
    commit
        .act(GameAction::PassPriority)
        .expect("the opponent passes priority and the sorcery resolves");
    let perf = perf_counters::snapshot();
    assert!(
        perf.layers_incremental >= 1 && perf.layers_full_eval == 0,
        "the token's entry must take the incremental layer pass: incremental={}, full_eval={}, \
         escalated={}, stack={}, waiting_for={:?}",
        perf.layers_incremental,
        perf.layers_full_eval,
        perf.layers_escalated,
        commit.state().stack.len(),
        commit.state().waiting_for
    );

    let new_ids: Vec<ObjectId> = commit
        .state()
        .battlefield
        .iter()
        .copied()
        .filter(|id| !before.contains(id))
        .collect();
    assert_eq!(new_ids.len(), 1, "exactly one token entered");
    let token = &commit.state().objects[&new_ids[0]];
    assert!(token.is_token);
    assert_eq!(token.keywords, vec![Keyword::Exploit]);
    assert!(token.static_definitions.is_empty());
    assert!(token.trigger_definitions.is_empty());

    let outcome = commit.resolve();
    let exploits = outcome
        .events()
        .iter()
        .filter(|event| {
            matches!(event, GameEvent::CreatureExploited { sacrificed, .. } if *sacrificed == victim)
        })
        .count();
    assert_eq!(exploits, 1, "the token's exploit trigger must fire once");
    assert_eq!(outcome.zone_of(victim), Zone::Graveyard);
}
