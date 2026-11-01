//! Rally the Righteous — "Untap target creature and each other creature that
//! shares a color with it. Those creatures get +2/+0 until end of turn."
//! "Those creatures" is the target plus every other creature the Radiance
//! fan-out reached, so the pump must reach all of them, not just the target.
//!
//! Oracle text is verbatim from `data/card-data.json`.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 207.2c: Radiance is an ability word with no rules meaning.
//!   - CR 608.2c: "those creatures" names the objects the first sentence
//!     affected.
//!   - CR 611.2a: a resolving spell's continuous effect lasts until end of turn.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;

use engine::types::identifiers::ObjectId;

const RALLY: &str = "Radiance — Untap target creature and each other creature that shares a color with it. Those creatures get +2/+0 until end of turn.";

#[test]
fn rally_pumps_target_and_every_color_sharing_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let red_target = scenario.add_creature(P1, "Red Target", 1, 3).id();
    let red_other = scenario.add_creature(P1, "Red Other", 1, 3).id();
    let red_white_other = scenario.add_creature(P0, "Red White Other", 1, 3).id();
    let blue_other = scenario.add_creature(P1, "Blue Other", 1, 3).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Rally the Righteous", true, RALLY)
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Colorless, ObjectId(9_999), false, vec![]); 4],
    );
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        for (id, colors) in [
            (red_target, vec![ManaColor::Red]),
            (red_other, vec![ManaColor::Red]),
            (red_white_other, vec![ManaColor::Red, ManaColor::White]),
            (blue_other, vec![ManaColor::Blue]),
        ] {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.color = colors;
            obj.tapped = true;
        }
    }

    let outcome = runner.cast(spell).target_objects(&[red_target]).resolve();

    for id in [red_target, red_other, red_white_other] {
        assert!(!outcome.is_tapped(id), "{id:?} must be untapped");
        assert_eq!(
            outcome.power_toughness(id),
            (3, 3),
            "{id:?} shares a color with the target and gets +2/+0"
        );
    }
    assert!(outcome.is_tapped(blue_other), "blue creature is untouched");
    assert_eq!(
        outcome.power_toughness(blue_other),
        (1, 3),
        "a creature sharing no color gets no bonus"
    );
}
