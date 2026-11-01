//! CR 601.3b + CR 702.8a: Gandalf the White ("You may cast legendary spells and artifact
//! spells as though they had flash.", verified against MTGJSON) lets its controller cast ANY
//! artifact spell at instant speed, legendary or not. Each leg of the list repeats "spells", so
//! "legendary" qualifies only the left leg.
//!
//! Gandalf is built from its Oracle text, so the permission under test comes from the parser
//! at test time rather than from the precomputed card fixture.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// Gandalf the White's Oracle text (Scryfall), 4/5 Legendary Creature — Avatar Wizard.
const GANDALF_THE_WHITE: &str = "Flash\n\
You may cast legendary spells and artifact spells as though they had flash.\n\
If a legendary permanent or an artifact entering or leaving the battlefield causes a triggered \
ability of a permanent you control to trigger, that ability triggers an additional time.";

/// Gandalf on P0's battlefield in P0's upkeep (no sorcery-speed timing), `card` in hand, and
/// enough colorless and green mana floating to pay it.
fn upkeep_with_gandalf(card: &str) -> (GameRunner, ObjectId) {
    with_gandalf_at(Phase::Upkeep, card)
}

fn with_gandalf_at(phase: Phase, card: &str) -> (GameRunner, ObjectId) {
    let db = load_db().expect("the committed integration card fixture loads");
    let mut scenario = GameScenario::new();
    scenario.at_phase(phase);
    scenario
        .add_creature_from_oracle(P0, "Gandalf the White", 4, 5, GANDALF_THE_WHITE)
        .as_legendary()
        .with_subtypes(vec!["Avatar", "Wizard"]);
    let spell = scenario.add_real_card(P0, card, Zone::Hand, db);
    let mut mana = vec![ManaUnit::new(ManaType::Colorless, ObjectId(9_900), false, vec![]); 3];
    mana.extend(vec![
        ManaUnit::new(
            ManaType::Green,
            ObjectId(9_901),
            false,
            vec![]
        );
        3
    ]);
    scenario.with_mana_pool(P0, mana);
    (scenario.build(), spell)
}

fn cast(runner: &mut GameRunner, spell: ObjectId) -> bool {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: Vec::new(),
            payment_mode: Default::default(),
        })
        .is_ok()
}

#[test]
fn a_nonlegendary_artifact_has_flash_under_gandalf() {
    for card in ["Sol Ring", "Arcane Signet"] {
        let (mut runner, spell) = upkeep_with_gandalf(card);
        assert!(
            cast(&mut runner, spell),
            "{card} is a nonlegendary artifact spell, so Gandalf lets it be cast in the upkeep"
        );
        assert!(
            runner.state().stack.iter().any(|entry| entry.id == spell),
            "{card} is on the stack"
        );
    }
}

#[test]
fn a_nonlegendary_nonartifact_creature_still_needs_sorcery_timing() {
    // Reach guard: the same board in P0's main phase casts the Bears, so the upkeep refusal
    // below is the timing rule and not an unrelated casting failure.
    let (mut runner, spell) = with_gandalf_at(Phase::PreCombatMain, "Grizzly Bears");
    assert!(
        cast(&mut runner, spell),
        "Grizzly Bears can be cast at sorcery speed from this board"
    );

    let (mut runner, spell) = upkeep_with_gandalf("Grizzly Bears");
    assert!(
        !cast(&mut runner, spell),
        "Grizzly Bears is neither legendary nor an artifact, so it can't be cast in the upkeep"
    );
}
