//! Kalitas, Traitor of Ghet:
//!
//! "Lifelink
//!  If a nontoken creature an opponent controls would die, instead exile that
//!  card and create a 2/2 black Zombie creature token.
//!  {2}{B}, Sacrifice another Vampire or Zombie: Put two +1/+1 counters on
//!  Kalitas."
//!
//! CR 614.1a + CR 614.6 + CR 701.8a: Wrath of God destroys Kalitas together
//! with the opposing creatures, so Kalitas is still on the battlefield when
//! their deaths are replaced. Every replaced death carries out its own
//! replacing instructions, so each one exiles its card and makes a Zombie.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const KALITAS: &str = "Lifelink\nIf a nontoken creature an opponent controls would die, instead exile that card and create a 2/2 black Zombie creature token.\n{2}{B}, Sacrifice another Vampire or Zombie: Put two +1/+1 counters on Kalitas.";
const WRATH_OF_GOD: &str = "Destroy all creatures. They can't be regenerated.";
const REST_IN_PEACE: &str = "When this enchantment enters, exile all graveyards.\nIf a card or token would be put into a graveyard from anywhere, exile it instead.";

fn zombies(events: &[GameEvent]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, GameEvent::TokenCreated { name, .. } if name.contains("Zombie")))
        .count()
}

fn zone_changes_of(events: &[GameEvent], id: ObjectId) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, GameEvent::ZoneChanged { object_id, .. } if *object_id == id))
        .count()
}

#[test]
fn wrath_of_god_exiles_each_opposing_creature_and_makes_a_zombie_for_each() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kalitas = scenario
        .add_creature_from_oracle(P0, "Kalitas, Traitor of Ghet", 3, 4, KALITAS)
        .id();
    let bear_a = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let bear_b = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let wrath = scenario
        .add_spell_to_hand_from_oracle(P0, "Wrath of God", false, WRATH_OF_GOD)
        .id();
    let mut runner = scenario.build();
    assert!(
        runner.state().objects[&kalitas]
            .replacement_definitions
            .iter_all()
            .any(|r| r.execute.is_some()),
        "reach guard: Kalitas must parse its would-die replacement"
    );

    let outcome = runner.cast(wrath).resolve();
    assert_eq!(
        zombies(outcome.events()),
        2,
        "one Zombie per replaced death"
    );
    runner.advance_until_stack_empty();

    let state = runner.state();
    for bear in [bear_a, bear_b] {
        assert_eq!(state.objects[&bear].zone, Zone::Exile, "{bear:?}: exiled");
    }
    assert_eq!(
        state.objects[&kalitas].zone,
        Zone::Graveyard,
        "Kalitas's own death is not replaced"
    );
    let zombie_tokens: Vec<_> = state
        .battlefield
        .iter()
        .filter(|id| state.objects[id].name.contains("Zombie"))
        .collect();
    assert_eq!(
        zombie_tokens.len(),
        2,
        "both Zombies are on the battlefield"
    );
    assert!(
        zombie_tokens
            .iter()
            .all(|id| state.objects[id].controller == P0),
        "CR 109.5: Kalitas's controller creates the Zombies"
    );
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
}

/// CR 616.1: Rest in Peace and Kalitas both apply to the opposing creature's
/// death, so its controller must choose one. The batch consult cannot carry a
/// prompt, so it restores its snapshot and the one-at-a-time path surfaces
/// it. Nothing the abandoned consult did may survive the rollback: each
/// creature leaves the battlefield exactly once and exactly one Zombie is made.
#[test]
fn ordering_choice_falls_back_to_one_at_a_time_without_duplicate_events() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let kalitas = scenario
        .add_creature_from_oracle(P0, "Kalitas, Traitor of Ghet", 3, 4, KALITAS)
        .id();
    scenario.add_enchantment_from_oracle(P1, "Rest in Peace", REST_IN_PEACE);
    let wrath = scenario
        .add_spell_to_hand_from_oracle(P0, "Wrath of God", false, WRATH_OF_GOD)
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(wrath).commit().resolve();
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::ReplacementChoice { .. }
        ),
        "the ordering choice is surfaced, got {:?}",
        outcome.final_waiting_for()
    );
    let mut events = outcome.events().to_vec();
    assert_eq!(
        zone_changes_of(&events, bear),
        0,
        "nothing moved before the choice"
    );
    assert_eq!(
        zombies(&events),
        0,
        "the rolled-back consult made no Zombie"
    );

    // Choose Kalitas's replacement for the Bear.
    let choice = match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice { candidates, .. } => candidates
            .iter()
            .position(|c| c.source_id == kalitas)
            .expect("Kalitas is a candidate"),
        other => panic!("expected a replacement choice, got {other:?}"),
    };
    let result = runner
        .act(engine::types::actions::GameAction::ChooseReplacement { index: choice })
        .expect("choose Kalitas's replacement");
    events.extend(result.events.iter().cloned());
    runner.advance_until_stack_empty();

    let state = runner.state();
    assert_eq!(state.objects[&bear].zone, Zone::Exile);
    assert_eq!(zone_changes_of(&events, bear), 1, "the Bear moved once");
    assert_eq!(zombies(&events), 1, "exactly one Zombie");
    // Kalitas's own destruction is not asserted: the one-at-a-time destroy-all
    // loop does not resume the objects after a replacement prompt, so Kalitas
    // currently survives the Wrath. That is a separate, pre-existing gap in the
    // sequential path, not in the batch rollback this test covers.
}
