//! Regression for #3280 (partial): owner-axis mandatory "all cards" name-hate must
//! exile every same-name copy from the parent target's owner's graveyard, hand,
//! and library, then shuffle that player's library.
//!
//! Surgical Extraction uses "any number of cards" and remains unsupported until
//! SearchChoice is implemented — this test locks in shuffle propagation only.
//!
//! https://github.com/phase-rs/phase/issues/3280

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const OWNER_AXIS_ALL_CARDS_NAME_HATE_ORACLE: &str = "Choose target card in a graveyard. Search its owner's graveyard, hand, and library for all cards with the same name as that card and exile them. Then that player shuffles.";

fn floating_mana(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

#[test]
fn owner_axis_name_hate_exiles_all_same_name_cards_and_shuffles_owner_library() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let target_bolt = scenario
        .add_spell_to_graveyard(P1, "Lightning Bolt", true)
        .id();
    let other_bolt_gy = scenario
        .add_spell_to_graveyard(P1, "Lightning Bolt", true)
        .id();
    let bolt_hand = scenario.add_spell_to_hand(P1, "Lightning Bolt", true).id();
    let filler_lib = scenario
        .add_spell_to_library_top(P1, "Counterspell", true)
        .id();
    let bolt_lib = scenario
        .add_spell_to_library_top(P1, "Lightning Bolt", true)
        .id();
    let counterspell_gy = scenario
        .add_spell_to_graveyard(P1, "Counterspell", true)
        .id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Owner-Axis Name Hate",
            true,
            OWNER_AXIS_ALL_CARDS_NAME_HATE_ORACLE,
        )
        .id();
    scenario.with_mana_pool(P0, floating_mana(1, ManaType::Black));

    let mut runner = scenario.build();
    let lib_before = runner.state().players[1].library.len();
    let outcome = runner.cast(spell).target_objects(&[target_bolt]).resolve();

    for &id in &[target_bolt, other_bolt_gy, bolt_hand, bolt_lib] {
        outcome.assert_zone(&[id], Zone::Exile);
    }
    outcome.assert_zone(&[counterspell_gy], Zone::Graveyard);
    outcome.assert_zone(&[filler_lib], Zone::Library);
    assert_eq!(
        runner.state().players[1].graveyard,
        vec![counterspell_gy].into(),
        "only non-matching graveyard cards may remain after same-name exile"
    );
    assert_eq!(
        runner.state().players[1].library.len(),
        lib_before - 1,
        "matching library cards must be exiled before shuffle; non-matching cards remain"
    );
}
