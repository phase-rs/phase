//! #9659: the for-each put-counter arm publishes `PutCounter` only for a
//! clause headed by the `put <article> <counter type> counter on` instruction.
//!
//! Spell A is Match the Odds, verbatim from MTGJSON AtomicCards: a real
//! put-counter-for-each clause that must keep resolving. Spell B is synthetic:
//! the same counter-on/for-each tail with no `put` verb, which no printed card
//! has. Before the fix the arm read its first word as a counter type and put
//! "foo" counters on the chosen creature.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::Effect;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const MATCH_THE_ODDS: &str = "Create a 1/1 white Ally creature token. Put a +1/+1 counter on it for each creature your opponents control.";
const VERBLESS_COUNTER_CLAUSE: &str =
    "Foo bar counter on target creature for each creature you control.";

#[test]
fn for_each_put_counter_head_runtime() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario.add_creature(P0, "Victim", 2, 2).id();
    for name in [
        "First opponent creature",
        "Second opponent creature",
        "Third opponent creature",
    ] {
        scenario.add_creature(P1, name, 2, 2);
    }
    let match_the_odds = scenario
        .add_spell_to_hand_from_oracle(P0, "Match the Odds", false, MATCH_THE_ODDS)
        .id();
    let verbless = scenario
        .add_spell_to_hand_from_oracle(P0, "Verbless counter clause", true, VERBLESS_COUNTER_CLAUSE)
        .id();
    let mut runner = scenario.build();

    // Reach guard: the real put-counter-for-each clause still resolves through
    // the arm. CR 608.2c + CR 122.1: one +1/+1 counter on the created token for
    // each of the three creatures P1 controls.
    let token = {
        let outcome = runner.cast(match_the_odds).resolve();
        assert_eq!(outcome.zone_of(match_the_odds), Zone::Graveyard);
        let token = outcome
            .find_object(|obj| {
                obj.is_token && obj.controller == P0 && obj.zone == Zone::Battlefield
            })
            .expect("Match the Odds creates an Ally token");
        outcome.assert_counters(token, CounterType::Plus1Plus1, 3);
        token
    };

    // Spell B: the target intent goes unused once the clause no longer
    // publishes a targeted PutCounter.
    let outcome = runner.cast(verbless).target_objects(&[victim]).resolve();
    // Reach guard: spell B was cast and resolved, not refused.
    assert_eq!(outcome.zone_of(verbless), Zone::Graveyard);
    assert!(outcome.state().stack.is_empty());

    // Revert-failing assertion: a verb-less clause is not a put instruction,
    // so no counter of any kind lands on the creature it names.
    assert!(
        outcome.state().objects[&victim].counters.is_empty(),
        "a verb-less clause must not put counters: {:?}, events: {:?}",
        outcome.state().objects[&victim].counters,
        outcome.events()
    );
    assert!(
        !outcome
            .events()
            .iter()
            .any(|event| matches!(event, GameEvent::CounterAdded { .. })),
        "{:?}",
        outcome.events()
    );
    // The token keeps exactly its three +1/+1 counters.
    assert_eq!(outcome.state().objects[&token].counters.len(), 1);
    outcome.assert_counters(token, CounterType::Plus1Plus1, 3);

    // The unsupported clause stays an honest gap rather than a wrong effect.
    let abilities = &outcome.state().objects[&verbless].abilities;
    assert!(
        !abilities.is_empty()
            && abilities
                .iter()
                .all(|def| matches!(def.effect.as_ref(), Effect::Unimplemented { .. })),
        "{abilities:?}"
    );
}
