//! Issue #1018 — Manifest dread must let the controller choose which of the
//! top two library cards to manifest face-down.

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const MANIFEST_DREAD_ORACLE: &str = "Manifest dread.";

const BREAK_DOWN_THE_DOOR_ORACLE: &str = "Choose one —\n\
• Exile target artifact.\n\
• Exile target enchantment.\n\
• Manifest dread.";

fn top_two_library_ids(
    runner: &engine::game::scenario::GameRunner,
) -> [engine::types::identifiers::ObjectId; 2] {
    let lib = &runner.state().players[0].library;
    assert!(
        lib.len() >= 2,
        "test setup needs at least two library cards"
    );
    [lib[0], lib[1]]
}

#[test]
fn manifest_dread_prompts_for_top_two_library_cards() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P0, "Library Top");
    scenario.add_card_to_library_top(P0, "Second Top");
    let spell = scenario
        .add_spell_to_hand(P0, "Dread Test", false)
        .from_oracle_text(MANIFEST_DREAD_ORACLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    scenario.with_mana_pool(P0, vec![]);

    let mut runner = scenario.build();
    let [top, second] = top_two_library_ids(&runner);

    runner.cast(spell).resolve();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ManifestDreadChoice { .. }
        ),
        "manifest dread must pause for a card choice, got {:?}",
        runner.state().waiting_for
    );

    let WaitingFor::ManifestDreadChoice { player, cards } = runner.state().waiting_for.clone()
    else {
        unreachable!();
    };
    assert_eq!(player, P0);
    assert_eq!(cards, vec![top, second]);

    runner
        .act(GameAction::SelectCards { cards: vec![top] })
        .expect("choose card to manifest");
    runner.advance_until_stack_empty();

    assert!(
        runner.state().objects[&top].face_down,
        "chosen card must enter face-down"
    );
    assert_eq!(
        runner.state().objects[&top].zone,
        Zone::Battlefield,
        "chosen card must be on the battlefield"
    );
    assert_eq!(
        runner.state().objects[&second].zone,
        Zone::Graveyard,
        "unchosen top card must go to the graveyard"
    );
}

#[test]
fn break_down_the_door_manifest_dread_mode_prompts_for_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P0, "Library Top");
    scenario.add_card_to_library_top(P0, "Second Top");
    let spell = scenario
        .add_spell_to_hand(P0, "Break Down the Door", false)
        .from_oracle_text(BREAK_DOWN_THE_DOOR_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Green],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(
                ManaType::Colorless,
                engine::types::identifiers::ObjectId(9_998),
                false,
                vec![],
            ),
            ManaUnit::new(
                ManaType::Colorless,
                engine::types::identifiers::ObjectId(9_999),
                false,
                vec![],
            ),
            ManaUnit::new(
                ManaType::Green,
                engine::types::identifiers::ObjectId(9_997),
                false,
                vec![],
            ),
        ],
    );

    let mut runner = scenario.build();
    let [top, second] = top_two_library_ids(&runner);

    runner.cast(spell).modes(&[2]).resolve();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ManifestDreadChoice { .. }
        ),
        "manifest dread modal mode must pause for card selection, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::SelectCards { cards: vec![top] })
        .expect("choose card to manifest");
    runner.advance_until_stack_empty();

    assert!(runner.state().objects[&top].face_down);
    assert_eq!(runner.state().objects[&second].zone, Zone::Graveyard);
}
