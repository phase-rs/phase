//! Issue #5798 / PR #5799 review — Painter's Servant multi-zone additive color.
//!
//! Official Oracle (Scryfall): "All cards that aren't on the battlefield, spells,
//! and permanents are the chosen color in addition to their other colors."
//!
//! Revert-probe for two production paths:
//! 1. `ColorChangeMode::Add` on `AddChosenColor` (CR 105.3 retain) — a blue card
//!    painted red must KEEP blue and GAIN red. Reverting the layer arm to
//!    `obj.color = vec![color]` makes the hand assertion fail.
//! 2. `continuous_effect_scan_zones` honoring `InAnyZone` — the hand/stack legs
//!    of the Oxford subject must actually be scanned through
//!    `apply_continuous_effect_filtered` + `matches_target_filter`. Reverting
//!    the leaf scan to `extract_in_zone`-only drops the hand recipient.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0};
use engine::game::zones::create_object;
use engine::types::ability::ChosenAttribute;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastingVariant, StackEntry, StackEntryKind};
use engine::types::identifiers::CardId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const PAINTER_ORACLE: &str = "As this creature enters, choose a color.\n\
All cards that aren't on the battlefield, spells, and permanents are the chosen color in addition to their other colors.";

const SET_CHOSEN_COLOR_ORACLE: &str = "As this creature enters, choose a color.\n\
All creatures are the chosen color.";

#[test]
fn painters_servant_adds_chosen_color_across_oxford_zones() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let painter = scenario
        .add_creature_from_oracle(P0, "Painter's Servant", 1, 3, PAINTER_ORACLE)
        .as_artifact()
        .id();

    // Blue creature permanent — battlefield Oxford leg.
    let blue_permanent = scenario
        .add_creature(P0, "Blue Bear", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Blue],
        })
        .id();

    // Blue card in hand — off-battlefield Oxford leg (`InAnyZone`).
    let blue_hand_card = scenario
        .add_spell_to_hand(P0, "Blue Card", true)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Blue],
        })
        .id();

    let mut runner = scenario.build();

    // Place a blue permanent spell on the stack — stack Oxford leg.
    let stack_spell = {
        let st = runner.state_mut();
        let card_id = CardId(st.next_object_id);
        let id = create_object(st, card_id, P0, "Blue Stack Spell".to_string(), Zone::Stack);
        let obj = st.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.color = vec![ManaColor::Blue];
        obj.base_color = vec![ManaColor::Blue];
        st.stack.push_back(StackEntry {
            id,
            source_id: id,
            controller: P0,
            kind: StackEntryKind::Spell {
                card_id,
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });
        id
    };

    {
        let st = runner.state_mut();
        // CR 105.4: controller chose red as Painter entered.
        st.objects
            .get_mut(&painter)
            .unwrap()
            .chosen_attributes
            .push(ChosenAttribute::Color(ManaColor::Red));
    }

    evaluate_layers(runner.state_mut());

    let assert_retains_blue_and_gains_red = |label: &str, colors: &[ManaColor]| {
        assert!(
            colors.contains(&ManaColor::Blue),
            "{label}: CR 105.3 additive must retain prior blue, got {colors:?}"
        );
        assert!(
            colors.contains(&ManaColor::Red),
            "{label}: must gain the chosen color (red) via its Oxford leg, got {colors:?}"
        );
    };

    assert_retains_blue_and_gains_red(
        "battlefield permanent",
        &runner.state().objects[&blue_permanent].color,
    );
    assert_retains_blue_and_gains_red(
        "hand card (off-battlefield InAnyZone leg)",
        &runner.state().objects[&blue_hand_card].color,
    );
    assert_retains_blue_and_gains_red("stack spell", &runner.state().objects[&stack_spell].color);
}

#[test]
fn bare_chosen_color_replaces_prior_colors() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let source = scenario
        .add_creature_from_oracle(P0, "Color Setter", 1, 1, SET_CHOSEN_COLOR_ORACLE)
        .id();
    let blue_creature = scenario
        .add_creature(P0, "Blue Bear", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Blue],
        })
        .id();
    let mut runner = scenario.build();

    runner
        .state_mut()
        .objects
        .get_mut(&source)
        .unwrap()
        .chosen_attributes
        .push(ChosenAttribute::Color(ManaColor::Red));

    evaluate_layers(runner.state_mut());

    assert_eq!(
        runner.state().objects[&blue_creature].color,
        vec![ManaColor::Red],
        "bare chosen-color statics must retain their CR 105.3 set semantics"
    );
}
