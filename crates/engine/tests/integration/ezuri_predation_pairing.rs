//! Ezuri's Predation (phase-rs/phase#913, #7840): "For each creature your
//! opponents control, create a 4/4 green Phyrexian Beast creature token. Each
//! of those tokens fights a different one of those creatures."
//!
//! CR 701.7a: the Beasts are created by one instruction, as one batch.
//! CR 701.14a: each Beast then fights exactly one of the counted creatures, and
//! each counted creature is fought by exactly one Beast. Per the card's ruling,
//! Beasts beyond the number of creatures (a token doubler) fight nothing.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const EZURIS_PREDATION: &str = "For each creature your opponents control, create a 4/4 green Phyrexian Beast creature token. Each of those tokens fights a different one of those creatures.";
const DOUBLING_SEASON: &str = "If an effect would create one or more tokens under your control, it creates twice that many of those tokens instead.\nIf an effect would put one or more counters on a permanent you control, it puts twice that many of those counters on that permanent instead.";
const FURNACE_OF_RATH: &str = "If a source would deal damage to a permanent or player, it deals double that damage to that permanent or player instead.";
const LASHKNIFE_BARRIER: &str = "When this enchantment enters, draw a card.\nIf a source would deal damage to a creature you control, it deals that much damage minus 1 to that creature instead.";

const P2: PlayerId = PlayerId(2);

/// The Phyrexian Beast tokens P0 controls on the battlefield, in id order.
fn beasts(state: &GameState) -> Vec<ObjectId> {
    let mut ids: Vec<ObjectId> = state
        .objects
        .values()
        .filter(|obj| {
            obj.is_token
                && obj.zone == Zone::Battlefield
                && obj.controller == P0
                && obj.name == "Phyrexian Beast"
        })
        .map(|obj| obj.id)
        .collect();
    ids.sort_by_key(|id| id.0);
    ids
}

fn damage(state: &GameState, id: ObjectId) -> u32 {
    state.objects[&id].damage_marked
}

fn add_predation(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Ezuri's Predation", false, EZURIS_PREDATION)
        .id()
}

/// Fight damage events (`source` → `target`) among the listed objects.
fn fight_hits(events: &[GameEvent], among: &[ObjectId]) -> Vec<(ObjectId, ObjectId)> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::DamageDealt {
                source_id,
                target: engine::types::ability::TargetRef::Object(target),
                ..
            } if among.contains(source_id) && among.contains(target) => Some((*source_id, *target)),
            _ => None,
        })
        .collect()
}

/// R1 — CR 701.14a: three creatures across two opponents. Each is fought by
/// exactly one Beast, and each Beast is dealt the power of the one creature it
/// fought.
#[test]
fn each_beast_fights_a_different_opponent_creature() {
    let mut scenario = GameScenario::new_n_player(3, 913);
    scenario.at_phase(Phase::PreCombatMain);
    let spell = add_predation(&mut scenario);
    let first = scenario.add_creature(P1, "First", 1, 6).id();
    let second = scenario.add_creature(P1, "Second", 2, 6).id();
    let third = scenario.add_creature(P2, "Third", 3, 6).id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).resolve();
    let state = outcome.state();

    let beasts = beasts(state);
    assert_eq!(beasts.len(), 3, "reach: one Beast per opponent creature");
    for creature in [first, second, third] {
        assert_eq!(
            damage(state, creature),
            4,
            "each opponent creature is fought by exactly one 4/4 Beast"
        );
    }
    let mut beast_damage: Vec<u32> = beasts.iter().map(|&b| damage(state, b)).collect();
    beast_damage.sort_unstable();
    assert_eq!(
        beast_damage,
        vec![1, 2, 3],
        "each Beast is dealt the power of the one creature it fought"
    );
    assert_eq!(outcome.zone_of(spell), Zone::Graveyard);
    outcome.assert_life_delta(P0, 0);
}

/// R2 — one opponent creature: one Beast, one fight.
#[test]
fn a_single_opponent_creature_fights_a_single_beast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = add_predation(&mut scenario);
    let bear = scenario.add_creature(P1, "Bear", 2, 6).id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).resolve();
    let state = outcome.state();

    let beasts = beasts(state);
    assert_eq!(beasts.len(), 1);
    assert_eq!(damage(state, bear), 4);
    assert_eq!(damage(state, beasts[0]), 2);
}

/// R3 — no opponent creature: no Beast, no fight, no prompt.
#[test]
fn no_opponent_creatures_creates_and_fights_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = add_predation(&mut scenario);
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).resolve();

    assert!(beasts(outcome.state()).is_empty());
    assert_eq!(
        outcome.zone_of(spell),
        Zone::Graveyard,
        "reach: the spell resolved"
    );
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}

/// R4 — the caster's own creatures are neither counted nor fought.
#[test]
fn own_creatures_are_not_counted_or_fought() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = add_predation(&mut scenario);
    let mine = scenario.add_creature(P0, "Mine", 3, 6).id();
    let theirs = scenario.add_creature(P1, "Theirs", 1, 6).id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).resolve();
    let state = outcome.state();

    let beasts = beasts(state);
    assert_eq!(beasts.len(), 1, "only the opponent's creature is counted");
    assert_eq!(
        damage(state, theirs),
        4,
        "reach: the opponent creature fought"
    );
    assert_eq!(damage(state, mine), 0);
    assert_eq!(damage(state, beasts[0]), 1);
}

/// R5 — ruling (2015-11-04): with a token doubler, the Beasts beyond the
/// number of creatures don't fight. Each creature is still fought by exactly
/// one Beast.
#[test]
fn doubled_beasts_beyond_the_creature_count_do_not_fight() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = add_predation(&mut scenario);
    scenario.add_enchantment_from_oracle(P0, "Doubling Season", DOUBLING_SEASON);
    let first = scenario.add_creature(P1, "First", 1, 6).id();
    let second = scenario.add_creature(P1, "Second", 2, 6).id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).resolve();
    let state = outcome.state();

    let beasts = beasts(state);
    assert_eq!(beasts.len(), 4, "reach: Doubling Season doubled the batch");
    assert_eq!(damage(state, first), 4, "fought by exactly one Beast");
    assert_eq!(damage(state, second), 4, "fought by exactly one Beast");
    let mut beast_damage: Vec<u32> = beasts.iter().map(|&b| damage(state, b)).collect();
    beast_damage.sort_unstable();
    assert_eq!(
        beast_damage,
        vec![0, 0, 1, 2],
        "two Beasts fought; the extra two fought nothing"
    );
}

/// R7 — CR 608.2c + CR 616.1: a damage replacement choice pauses the loop
/// mid-fight. The remaining iterations resume through the parked loop and
/// still pair each remaining Beast with a different creature.
#[test]
fn paused_loop_resumes_and_keeps_each_pair_distinct() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = add_predation(&mut scenario);
    scenario.add_enchantment_from_oracle(P1, "Furnace of Rath", FURNACE_OF_RATH);
    scenario.add_enchantment_from_oracle(P1, "Lashknife Barrier", LASHKNIFE_BARRIER);
    let creatures = [
        scenario.add_creature(P1, "First", 1, 20).id(),
        scenario.add_creature(P1, "Second", 2, 20).id(),
        scenario.add_creature(P1, "Third", 3, 20).id(),
    ];
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).resolve();
    let mut events: Vec<GameEvent> = outcome.events().to_vec();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "reach: the first fight pauses on the ordering of two damage replacements, got {:?}",
        runner.state().waiting_for
    );
    let beasts = beasts(runner.state());
    assert_eq!(
        beasts.len(),
        3,
        "reach: the batch was created before the fights"
    );
    let mut among: Vec<ObjectId> = beasts.clone();
    among.extend(creatures);
    let fought_at_first_pause = fight_hits(&events, &among).len();
    assert!(
        fought_at_first_pause < 6,
        "reach: fights remain when the loop first pauses"
    );

    let choices = answer_replacement_choices(&mut runner, &mut events);
    assert!(choices >= 1);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));

    let hits = fight_hits(&events, &among);
    for &creature in &creatures {
        let fought_by: Vec<ObjectId> = hits
            .iter()
            .filter(|(source, target)| *target == creature && beasts.contains(source))
            .map(|(source, _)| *source)
            .collect();
        assert_eq!(
            fought_by.len(),
            1,
            "each creature is dealt fight damage by exactly one Beast: {hits:?}"
        );
        assert!(
            hits.contains(&(creature, fought_by[0])),
            "the Beast that damaged a creature is the one that creature damaged back: {hits:?}"
        );
    }
    for &beast in &beasts {
        assert_eq!(
            hits.iter().filter(|(source, _)| *source == beast).count(),
            1,
            "each Beast fights exactly once: {hits:?}"
        );
    }
}

/// Answer every damage-replacement ordering prompt with the first candidate,
/// collecting the events, until the spell finishes resolving.
fn answer_replacement_choices(runner: &mut GameRunner, events: &mut Vec<GameEvent>) -> usize {
    let mut choices = 0;
    while matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ) {
        assert!(choices < 32, "replacement prompts must terminate");
        let result = runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("choosing a replacement must succeed");
        events.extend(result.events);
        choices += 1;
    }
    choices
}
