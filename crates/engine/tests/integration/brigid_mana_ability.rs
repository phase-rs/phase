//! Integration test for issue #435 — Brigid, Doun's Mind.
//!
//! Oracle (activated mana ability):
//!   `{T}: Add X {G} or X {W}, where X is the number of other creatures you control.`
//!
//! The parser previously fell to `Effect::Unimplemented` on the
//! count-prefixed disjunctive color choice `"X {G} or X {W}"` — the activated
//! ability produced no mana and surfaced no color-choice prompt.
//!
//! The fix is parser-only: `oracle_effect/mana.rs::parse_repeated_count_color_choice`
//! recognizes the `"<count> {C1} or <count> {C2}"` shape and maps it onto the
//! existing `ManaProduction::AnyOneColor { count, color_options, .. }` variant.
//! The runtime already surfaces `ManaChoicePrompt::SingleColor` for an
//! `AnyOneColor` with more than one option (`mana_abilities.rs`).
//!
//! This test drives the real `apply` pipeline: activate Brigid's `{T}` ability,
//! confirm a `ChooseManaColor` prompt with the `where X is …` count, submit a
//! color choice, and assert the resulting mana pool. No synthetic events.

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::{ManaChoice, ManaChoicePrompt, WaitingFor};
use engine::types::mana::ManaType;
use engine::types::phase::Phase;

const BRIGID_TEXT: &str =
    "{T}: Add X {G} or X {W}, where X is the number of other creatures you control.";

/// CR 106.1: With two other creatures the active player controls, activating
/// Brigid's `{T}` mana ability surfaces a `SingleColor` prompt offering Green
/// and White; choosing Green yields two green mana (X = 2 other creatures).
#[test]
fn brigid_activated_ability_offers_color_choice_and_produces_x_mana() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let brigid_id = scenario
        .add_creature_from_oracle(P0, "Brigid, Doun's Mind", 2, 3, BRIGID_TEXT)
        .id();
    // Two *other* creatures the active player controls — X resolves to 2.
    scenario.add_creature(P0, "Other Creature A", 1, 1);
    scenario.add_creature(P0, "Other Creature B", 1, 1);

    let mut runner = scenario.build();

    // Activate Brigid's {T} mana ability.
    runner
        .act(GameAction::ActivateAbility {
            source_id: brigid_id,
            ability_index: 0,
        })
        .expect("activating Brigid's mana ability must succeed");

    // The engine must surface a single-color choice between Green and White —
    // proof the effect parsed to AnyOneColor, not Unimplemented.
    match &runner.state().waiting_for {
        WaitingFor::ChooseManaColor {
            choice: ManaChoicePrompt::SingleColor { options },
            ..
        } => {
            assert!(
                options.contains(&ManaType::Green) && options.contains(&ManaType::White),
                "color-choice prompt must offer Green and White; got {options:?}"
            );
            assert_eq!(
                options.len(),
                2,
                "Brigid offers exactly two colors; got {options:?}"
            );
        }
        other => panic!("expected ChooseManaColor SingleColor prompt, got {other:?}"),
    }

    // Choose Green.
    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Green),
            count: 1,
        })
        .expect("submitting the Green color choice must succeed");

    // X = 2 other creatures → two green mana in the pool.
    let pool = &runner.state().players[P0.0 as usize].mana_pool;
    assert_eq!(
        pool.count_color(ManaType::Green),
        2,
        "Brigid must produce X (=2 other creatures) green mana; pool = {:?}",
        pool.mana,
    );
    assert_eq!(
        pool.count_color(ManaType::White),
        0,
        "no white mana was chosen",
    );
    assert_eq!(pool.total(), 2, "exactly X mana of the chosen color");
}

/// CR 106.1: With no other creatures, X resolves to 0 — choosing a color
/// produces zero mana (CR 106.5: an ability producing zero mana produces none).
#[test]
fn brigid_with_no_other_creatures_produces_zero_mana() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let brigid_id = scenario
        .add_creature_from_oracle(P0, "Brigid, Doun's Mind", 2, 3, BRIGID_TEXT)
        .id();

    let mut runner = scenario.build();

    runner
        .act(GameAction::ActivateAbility {
            source_id: brigid_id,
            ability_index: 0,
        })
        .expect("activating Brigid's mana ability must succeed");

    // The choice prompt still appears (the production is well-defined; the
    // count is what resolves to zero).
    if let WaitingFor::ChooseManaColor {
        choice: ManaChoicePrompt::SingleColor { .. },
        ..
    } = &runner.state().waiting_for
    {
        runner
            .act(GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(ManaType::White),
                count: 1,
            })
            .expect("submitting the White color choice must succeed");
    }

    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        0,
        "X = 0 other creatures → zero mana produced (CR 106.5)",
    );
}
