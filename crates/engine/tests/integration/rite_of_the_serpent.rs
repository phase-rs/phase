//! Rite of the Serpent (BNG) — "Destroy target creature. If that creature had a
//! +1/+1 counter on it, create a 1/1 green Snake creature token."
//!
//! Second in-corpus consumer of the leading past-tense demonstrative condition
//! branch (Dismantle's plan). Exercises ONLY the condition-routing capability —
//! its body is a token creation, not a counter placement, so it never touches
//! the `ChooseOneOf`/shared-count machinery Dismantle needs.
//!
//! Gatherer/MTGJSON ruling (BNG, 2013-01-24): "If the creature had a +1/+1
//! counter on it when it was destroyed, you create the token even if the
//! creature wasn't destroyed [it had indestructible]." — same live-if-survived
//! / LKI-if-gone semantics as Dismantle ruling 2.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::counter::CounterType;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const RITE_ORACLE: &str = "Destroy target creature. If that creature had a +1/+1 counter on it, \
     create a 1/1 green Snake creature token.";

fn snake_token_count(runner: &engine::game::scenario::GameRunner, controller: PlayerId) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|obj| {
            obj.zone == Zone::Battlefield && obj.controller == controller && obj.name == "Snake"
        })
        .count()
}

/// A target with a +1/+1 counter on it: destroyed, and the Snake token is
/// created.
#[test]
fn plus_one_plus_one_counter_creates_the_snake_token() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| {
                engine::types::mana::ManaUnit::new(
                    engine::types::mana::ManaType::Colorless,
                    engine::types::identifiers::ObjectId(0),
                    false,
                    vec![],
                )
            })
            .collect(),
    );
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Rite of the Serpent", false, RITE_ORACLE)
        .id();
    let target = scenario
        .add_creature(P1, "Counter-Laden Creature", 2, 2)
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&target)
        .unwrap()
        .counters
        .insert(CounterType::Plus1Plus1, 1);

    let outcome = runner.cast(spell).target_objects(&[target]).resolve();
    assert_eq!(outcome.zone_of(target), Zone::Graveyard);
    drop(outcome);

    assert_eq!(
        snake_token_count(&runner, P0),
        1,
        "the token must be created when the destroyed creature had a +1/+1 counter"
    );
}

/// A target with a counter of a DIFFERENT kind (not +1/+1): the gate is
/// `Some(Plus1Plus1)`, so no token.
#[test]
fn non_plus_one_counter_does_not_create_the_token() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| {
                engine::types::mana::ManaUnit::new(
                    engine::types::mana::ManaType::Colorless,
                    engine::types::identifiers::ObjectId(0),
                    false,
                    vec![],
                )
            })
            .collect(),
    );
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Rite of the Serpent", false, RITE_ORACLE)
        .id();
    let target = scenario.add_creature(P1, "Oil-Laden Creature", 2, 2).id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&target)
        .unwrap()
        .counters
        .insert(CounterType::Generic("oil".to_string()), 1);

    let outcome = runner.cast(spell).target_objects(&[target]).resolve();
    assert_eq!(outcome.zone_of(target), Zone::Graveyard);
    drop(outcome);

    assert_eq!(
        snake_token_count(&runner, P0),
        0,
        "ruling 3 analogue for a +1/+1-specific gate: a non-matching counter kind must not fire it"
    );
}

/// A target with zero counters: no token.
#[test]
fn zero_counters_does_not_create_the_token() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| {
                engine::types::mana::ManaUnit::new(
                    engine::types::mana::ManaType::Colorless,
                    engine::types::identifiers::ObjectId(0),
                    false,
                    vec![],
                )
            })
            .collect(),
    );
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Rite of the Serpent", false, RITE_ORACLE)
        .id();
    let target = scenario.add_creature(P1, "Bare Creature", 2, 2).id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_objects(&[target]).resolve();
    assert_eq!(outcome.zone_of(target), Zone::Graveyard);
    drop(outcome);

    assert_eq!(snake_token_count(&runner, P0), 0);
}

/// Ruling analogue (indestructible): the target survives `Destroy` — no zone
/// change, no LKI needed — and the token is STILL created from the target's
/// live counters.
#[test]
fn indestructible_target_still_creates_the_token() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| {
                engine::types::mana::ManaUnit::new(
                    engine::types::mana::ManaType::Colorless,
                    engine::types::identifiers::ObjectId(0),
                    false,
                    vec![],
                )
            })
            .collect(),
    );
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Rite of the Serpent", false, RITE_ORACLE)
        .id();
    let target = scenario
        .add_creature(P1, "Indestructible Creature", 2, 2)
        .id();
    let mut runner = scenario.build();
    {
        let obj = runner.state_mut().objects.get_mut(&target).unwrap();
        obj.counters.insert(CounterType::Plus1Plus1, 1);
        obj.base_keywords
            .push(engine::types::keywords::Keyword::Indestructible);
        obj.keywords = obj.base_keywords.clone();
    }

    let outcome = runner.cast(spell).target_objects(&[target]).resolve();
    assert_eq!(
        outcome.zone_of(target),
        Zone::Battlefield,
        "an indestructible target is not destroyed"
    );
    drop(outcome);

    assert_eq!(
        snake_token_count(&runner, P0),
        1,
        "the token still fires from the surviving target's LIVE counter"
    );
}
