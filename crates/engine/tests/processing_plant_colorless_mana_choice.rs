//! Integration test for Processing Plant — "{T}: Add {W}, {U}, {B}, or {C}."
//!
//! The color-choice mana parser (`parse_mana_color_set`) rejected this choice
//! because it includes colorless `{C}`, which `ManaColor` (WUBRG) cannot
//! represent — the card fell to `Effect::Unimplemented`: no mana, no prompt.
//!
//! The fix augments `ManaProduction::AnyOneColor` with `includes_colorless`
//! (CR 106.1b) and appends `ManaType::Colorless` to the offered options at
//! resolution. The choice prompt/answer layer already speaks `ManaType`, so
//! colorless is a first-class option once it reaches the profile.
//!
//! This drives the real `apply` pipeline: activate the ability, confirm the
//! `ChooseManaColor` prompt offers {W},{U},{B},{C}, choose {C}, and assert one
//! colorless mana in the pool. Reverting the fix removes the prompt (the card is
//! `Unimplemented`), so both assertions flip.

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::{ManaChoice, ManaChoicePrompt, WaitingFor};
use engine::types::mana::ManaType;
use engine::types::phase::Phase;

const PROCESSING_PLANT: &str = "{T}: Add {W}, {U}, {B}, or {C}.";

/// CR 106.1b: the color choice offers colorless {C} alongside W/U/B, and
/// choosing {C} yields one colorless mana.
#[test]
fn processing_plant_offers_colorless_choice_and_produces_it() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_from_oracle(P0, "Processing Plant", 0, 1, PROCESSING_PLANT)
        .id();

    let mut runner = scenario.build();

    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("activating the mana ability must succeed");

    // The prompt must offer all four options INCLUDING colorless {C} — proof the
    // effect parsed to `AnyOneColor { includes_colorless: true }`, not
    // `Unimplemented`.
    match &runner.state().waiting_for {
        WaitingFor::ChooseManaColor {
            choice: ManaChoicePrompt::SingleColor { options },
            ..
        } => {
            for t in [
                ManaType::White,
                ManaType::Blue,
                ManaType::Black,
                ManaType::Colorless,
            ] {
                assert!(
                    options.contains(&t),
                    "the mana choice must offer {t:?}; got {options:?}"
                );
            }
            assert_eq!(options.len(), 4, "exactly W/U/B/C; got {options:?}");
        }
        other => panic!("expected a ChooseManaColor SingleColor prompt, got {other:?}"),
    }

    // Choose colorless {C}.
    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Colorless),
            count: 1,
        })
        .expect("submitting the colorless choice must succeed");

    let pool = &runner.state().players[P0.0 as usize].mana_pool;
    assert_eq!(
        pool.count_color(ManaType::Colorless),
        1,
        "choosing {{C}} must add one colorless mana; pool = {:?}",
        pool.mana,
    );
    assert_eq!(pool.total(), 1, "exactly one mana produced");
}
