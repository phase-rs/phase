//! CR 611.2c + CR 701.40a + CR 708.2: Whisperwood Elemental's sacrifice
//! ability gives "When this creature dies, manifest the top card of your
//! library." to the face-up nontoken creatures its controller controls as it
//! resolves. A face-down creature and a token are outside that set, so their
//! deaths manifest nothing; a face-up nontoken creature's death manifests the
//! top card of the library.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

// Printed Oracle text, as in client/public/card-data.json.
const WHISPERWOOD_ELEMENTAL: &str = "At the beginning of your end step, manifest the top card of your library. (Put it onto the battlefield face down as a 2/2 creature. Turn it face up any time for its mana cost if it's a creature card.)\nSacrifice this creature: Until end of turn, face-up nontoken creatures you control gain \"When this creature dies, manifest the top card of your library.\"";
const SOUL_SUMMONS: &str = "Manifest the top card of your library. (Put it onto the battlefield face down as a 2/2 creature. Turn it face up any time for its mana cost if it's a creature card.)";
const RAISE_THE_ALARM: &str = "Create two 1/1 white Soldier creature tokens.";
const MURDER: &str = "Destroy target creature.";

fn library(runner: &GameRunner) -> Vec<ObjectId> {
    runner.state().players[0].library.iter().copied().collect()
}

fn face_down_on_battlefield(runner: &GameRunner) -> Vec<ObjectId> {
    runner
        .state()
        .battlefield
        .iter()
        .copied()
        .filter(|id| runner.state().objects[id].face_down)
        .collect()
}

/// Cast a zero-cost Murder at `victim` and resolve it and every trigger it
/// causes.
fn murder(runner: &mut GameRunner, spell: ObjectId, victim: ObjectId) {
    runner.cast(spell).target_object(victim).commit();
    runner.advance_until_stack_empty();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "{:?}",
        runner.state().waiting_for
    );
    // CR 704.5d: a token that left the battlefield ceases to exist, so the
    // victim is checked as gone from the battlefield.
    assert!(!runner.state().battlefield.contains(&victim));
}

#[test]
fn whisperwood_grant_manifests_only_for_face_up_nontoken_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["First card", "Second card", "Third card"]);
    let whisperwood = scenario
        .add_creature_from_oracle(P0, "Whisperwood Elemental", 4, 4, WHISPERWOOD_ELEMENTAL)
        .with_subtypes(vec!["Elemental"])
        .id();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let summons = scenario
        .add_spell_to_hand_from_oracle(P0, "Soul Summons", false, SOUL_SUMMONS)
        .with_mana_cost(ManaCost::zero())
        .id();
    let alarm = scenario
        .add_spell_to_hand_from_oracle(P0, "Raise the Alarm", true, RAISE_THE_ALARM)
        .with_mana_cost(ManaCost::zero())
        .id();
    let murders: Vec<ObjectId> = (0..3)
        .map(|_| {
            scenario
                .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
                .with_mana_cost(ManaCost::zero())
                .id()
        })
        .collect();
    let mut runner = scenario.build();
    let [first, second, third] = library(&runner)[..] else {
        panic!("three library cards")
    };

    // A real face-down creature (CR 701.40a) and two real tokens (CR 111.1).
    runner.cast(summons).resolve();
    let face_down = face_down_on_battlefield(&runner);
    assert_eq!(
        face_down,
        vec![first],
        "Soul Summons manifests the top card"
    );
    runner.cast(alarm).resolve();
    let tokens: Vec<ObjectId> = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .filter(|id| runner.state().objects[id].is_token)
        .collect();
    assert_eq!(tokens.len(), 2);

    // Sacrifice Whisperwood Elemental: the set of affected creatures is fixed
    // as the effect begins (CR 611.2c).
    runner.activate(whisperwood, 0).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&whisperwood].zone, Zone::Graveyard);
    assert_eq!(
        library(&runner),
        vec![second, third],
        "nothing manifested yet"
    );

    // CR 708.2: the face-down creature had no "face-up" status to qualify.
    murder(&mut runner, murders[0], first);
    assert_eq!(library(&runner), vec![second, third]);
    assert!(face_down_on_battlefield(&runner).is_empty());

    // CR 111.1: a token is excluded by "nontoken".
    murder(&mut runner, murders[1], tokens[0]);
    assert_eq!(library(&runner), vec![second, third]);
    assert!(face_down_on_battlefield(&runner).is_empty());

    // CR 700.4 + CR 603.6c + CR 701.40a: the face-up nontoken creature dies,
    // and its granted ability manifests the top card of P0's library.
    murder(&mut runner, murders[2], bears);
    assert_eq!(library(&runner), vec![third], "one card manifested");
    assert_eq!(face_down_on_battlefield(&runner), vec![second]);
    let manifested = &runner.state().objects[&second];
    assert_eq!(manifested.controller, P0);
    assert_eq!(manifested.zone, Zone::Battlefield);
}
