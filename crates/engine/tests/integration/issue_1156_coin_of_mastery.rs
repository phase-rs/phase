//! Regression: GitHub issue #1156 — Coin of Mastery ETB counters from artifact mana.
//!
//! Oracle: "Each creature you control enters with an additional +1/+1 counter on
//! it for each mana from an artifact source spent to cast it."
//!
//! Drives the real cast → stack → resolve → ETB replacement pipeline. The
//! discriminating signal is `CastManaSpentMetric::FromSource { Artifact }`
//! resolved against the entering spell's payment-time source snapshots.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 122.6 / 122.6a: counters placed as a permanent enters.
//!   - CR 614.1c: replacement effect modifying how a permanent enters.
//!   - CR 601.2h: mana spent to cast is tracked during payment.

use engine::game::scenario::{CastOutcome, GameScenario, P0};
use engine::game::zones::create_object;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Coin of Mastery's printed Oracle text — byte-identical to `data/card-data.json`.
const COIN_OF_MASTERY: &str = "Each creature you control enters with an additional \
+1/+1 counter on it for each mana from an artifact source spent to cast it.\n\
{T}: Create a Treasure token. (It's an artifact with \"{T}, Sacrifice this token: \
Add one mana of any color.\")";

fn make_treasure(
    state: &mut engine::types::game_state::GameState,
    card_id: u64,
    owner: PlayerId,
) -> ObjectId {
    let id = create_object(
        state,
        CardId(card_id),
        owner,
        "Treasure".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Artifact);
    obj.card_types.subtypes.push("Treasure".to_string());
    obj.base_card_types = obj.card_types.clone();
    id
}

fn add_pool_mana(
    runner: &mut engine::game::scenario::GameRunner,
    player: PlayerId,
    units: &[(ManaType, ObjectId)],
) {
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .unwrap()
        .mana_pool;
    for (mana, source) in units {
        pool.add(ManaUnit::new(*mana, *source, false, vec![]));
    }
}

fn cast_creature_with_tagged_pool(
    setup_treasures: impl FnOnce(&mut engine::types::game_state::GameState) -> Vec<ObjectId>,
) -> CastOutcome {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Coin of Mastery", 0, 0, COIN_OF_MASTERY)
        .as_artifact();
    let creature = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();

    let mut runner = scenario.build();
    let treasure_sources = setup_treasures(runner.state_mut());
    let pool_units: Vec<_> = treasure_sources
        .iter()
        .map(|&source| (ManaType::Colorless, source))
        .collect();
    add_pool_mana(&mut runner, P0, &pool_units);

    runner.cast(creature).resolve()
}

/// Payment-time regression: artifact-source snapshots must be on the spell at
/// stack commit so ETB replacements can read them before battlefield entry.
#[test]
fn coin_of_mastery_records_artifact_snapshots_on_stack_commit() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let coin = scenario
        .add_creature_from_oracle(P0, "Coin of Mastery", 0, 0, COIN_OF_MASTERY)
        .as_artifact()
        .id();
    let creature = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();

    let mut runner = scenario.build();
    let treasure = make_treasure(runner.state_mut(), 9001, P0);
    add_pool_mana(
        &mut runner,
        P0,
        &[
            (ManaType::Colorless, treasure),
            (ManaType::Colorless, treasure),
        ],
    );

    assert!(
        !runner.state().objects[&coin]
            .replacement_definitions
            .is_empty(),
        "Coin of Mastery must register its ETB replacement"
    );

    let commit = runner.cast(creature).commit();
    let spell = commit.state().objects.get(&creature).unwrap();
    assert_eq!(
        spell.zone,
        Zone::Stack,
        "spell must be on the stack after commit"
    );
    assert_eq!(
        spell.mana_spent_source_snapshots.len(),
        2,
        "each pool mana unit must snapshot its artifact source for FromSource queries"
    );
}

/// Two Treasure mana units from one source → two additional +1/+1 counters on ETB.
#[test]
fn coin_of_mastery_etb_counters_from_one_artifact_mana() {
    let outcome = cast_creature_with_tagged_pool(|state| {
        let treasure = make_treasure(state, 9001, P0);
        vec![treasure, treasure]
    });
    let creature = outcome
        .state()
        .objects
        .values()
        .find(|o| o.name == "Grizzly Bears" && o.zone == Zone::Battlefield)
        .expect("creature must enter the battlefield")
        .id;

    outcome.assert_counters(creature, CounterType::Plus1Plus1, 2);
}

/// Two distinct Treasure sources → two additional +1/+1 counters.
#[test]
fn coin_of_mastery_etb_counters_from_two_artifact_sources() {
    let outcome = cast_creature_with_tagged_pool(|state| {
        vec![
            make_treasure(state, 9001, P0),
            make_treasure(state, 9002, P0),
        ]
    });
    let creature = outcome
        .state()
        .objects
        .values()
        .find(|o| o.name == "Grizzly Bears" && o.zone == Zone::Battlefield)
        .expect("creature must enter the battlefield")
        .id;

    outcome.assert_counters(creature, CounterType::Plus1Plus1, 2);
}

/// Non-artifact mana must not contribute to Coin of Mastery's counter count.
#[test]
fn coin_of_mastery_ignores_non_artifact_mana_sources() {
    let outcome = cast_creature_with_tagged_pool(|state| {
        let treasure = make_treasure(state, 9001, P0);
        let forest = create_object(
            state,
            CardId(9002),
            P0,
            "Forest".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&forest).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.subtypes.push("Forest".to_string());
        obj.base_card_types = obj.card_types.clone();
        vec![treasure, forest]
    });
    let creature = outcome
        .state()
        .objects
        .values()
        .find(|o| o.name == "Grizzly Bears" && o.zone == Zone::Battlefield)
        .expect("creature must enter the battlefield")
        .id;

    outcome.assert_counters(creature, CounterType::Plus1Plus1, 1);
}
