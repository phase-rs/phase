//! Regression coverage for **Iona, Shield of Emeria** (issue #4937).
//!
//! "Your opponents can't cast spells of **the chosen color**." must prohibit
//! only spells whose colors (CR 105.2) include the source's chosen color
//! (CR 105.4) — not every spell. This is the color analog of the "chosen type"
//! (Iona's sibling `IsChosenCardType`) and "chosen name" (`HasChosenName`)
//! cast-lock branches.
//!
//! The bug had two halves, so this is a revert-probe for both: the parser
//! (`parse_cant_cast_type_spells`) dropped the `IsChosenColor` filter, AND the
//! runtime resolver (`cant_cast_filter_matches`) had no arm to evaluate that
//! filter. If either half regresses, the chosen-color spell below becomes
//! castable again and `blue_is_locked` fails; the off-color control
//! (`red_is_free`) guards against the opposite over-broad regression.

use engine::game::casting::can_cast_object_now;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::ChosenAttribute;
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;

const IONA: &str = "Your opponents can't cast spells of the chosen color.";

#[test]
fn iona_locks_only_the_chosen_color() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Iona under P0; its "can't cast spells of the chosen color" static must
    // parse to a `CantBeCast` carrying an `IsChosenColor` filter.
    let iona = scenario
        .add_creature_from_oracle(P0, "Iona, Shield of Emeria", 7, 7, IONA)
        .id();

    // Two {0} instants in the opponent's hand — identical castability except for
    // color, so the only variable is the chosen-color prohibition.
    let blue_spell = scenario
        .add_spell_to_hand_from_oracle(P1, "Blue Spell", true, "Draw a card.")
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let red_spell = scenario
        .add_spell_to_hand_from_oracle(P1, "Red Spell", true, "Draw a card.")
        .with_mana_cost(ManaCost::generic(0))
        .id();

    let mut runner = scenario.build();

    {
        let st = runner.state_mut();
        // CR 105.4: Iona's controller chose blue as it entered.
        st.objects
            .get_mut(&iona)
            .expect("Iona must be on the battlefield")
            .chosen_attributes
            .push(ChosenAttribute::Color(ManaColor::Blue));
        // CR 105.2: fix the spells' colors directly so mana payment is not a
        // confound — both cost {0}; only the color differs.
        st.objects.get_mut(&blue_spell).unwrap().color = vec![ManaColor::Blue];
        st.objects.get_mut(&red_spell).unwrap().color = vec![ManaColor::Red];
    }

    assert!(
        !can_cast_object_now(runner.state(), P1, blue_spell),
        "opponent must NOT be able to cast a spell of the chosen color (blue)"
    );
    assert!(
        can_cast_object_now(runner.state(), P1, red_spell),
        "opponent MUST still be able to cast off-color spells (red) — Iona locks \
         only the chosen color, not every spell"
    );
}
