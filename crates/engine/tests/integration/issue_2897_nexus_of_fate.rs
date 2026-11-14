//! Regression for issue #2897: Nexus of Fate must shuffle into its owner's
//! library when it would be put into a graveyard from the stack, not land in
//! the graveyard.
//!
//! https://github.com/phase-rs/phase/issues/2897

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{Effect, TargetFilter};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

const NEXUS_OF_FATE: &str = "Take an extra turn after this one.\n\
    If Nexus of Fate would be put into a graveyard from anywhere, reveal Nexus of Fate and \
    shuffle it into its owner's library instead.";

fn floating_mana(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

#[test]
fn nexus_of_fate_parses_shuffle_back_replacement_as_self_scoped() {
    let mut scenario = GameScenario::new();
    let nexus = scenario
        .add_spell_to_hand_from_oracle(P0, "Nexus of Fate", false, NEXUS_OF_FATE)
        .id();
    let runner = scenario.build();
    let obj = &runner.state().objects[&nexus];

    let repl = obj
        .replacement_definitions
        .as_slice()
        .iter()
        .find(|d| d.event == ReplacementEvent::Moved)
        .expect("Nexus of Fate must carry a Moved replacement");
    assert_eq!(repl.destination_zone, Some(Zone::Graveyard));
    assert_eq!(
        repl.valid_card,
        Some(TargetFilter::SelfRef),
        "self shuffle-back must be discoverable while the spell is on the stack"
    );
    let execute = repl.execute.as_ref().expect("replacement execute");
    assert!(matches!(
        *execute.effect,
        Effect::ChangeZone {
            destination: Zone::Library,
            target: TargetFilter::SelfRef,
            ..
        }
    ));
}

#[test]
fn nexus_of_fate_shuffles_into_library_on_resolution() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for i in 0..8 {
        scenario.add_spell_to_library_top(P0, &format!("Filler {i}"), true);
    }
    let nexus = scenario
        .add_spell_to_hand_from_oracle(P0, "Nexus of Fate", false, NEXUS_OF_FATE)
        .id();
    scenario.with_mana_pool(P0, floating_mana(7, ManaType::Blue));

    let mut runner = scenario.build();
    let outcome = runner.cast(nexus).resolve();

    assert_eq!(
        runner.state().objects[&nexus].zone,
        Zone::Library,
        "Nexus of Fate must shuffle into its owner's library instead of going to the graveyard"
    );
    assert!(
        !runner.state().players[0].graveyard.contains(&nexus),
        "Nexus of Fate must not reach the graveyard on resolution"
    );
    assert!(
        runner.state().players[0].library.contains(&nexus),
        "Nexus of Fate must end up in its owner's library"
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "resolution should end at priority, not an unexpected prompt"
    );
}
