//! Q, Stacking the Deck — the ETB half through the production pipeline.
//!
//! Oracle text (Scryfall, verbatim):
//! "When Q enters, shuffle your library, then exile cards from the top of your
//! library until you exile a nonland card. That nonland card gains "Exile a card
//! from your hand: Create a copy of this card. You may cast the copy. Activate
//! only during your turn and only if this card is exiled.""
//!
//! The granted ability itself is not yet supported (the engine does not serve
//! Layer 6 ability grants to cards in exile, and "Create a copy of this card"
//! is unparsed), so these tests pin only the trigger-level readings:
//!   - CR 201.5c: "Q" is the card's shortened name, so "When Q enters" is a
//!     self-reference — the trigger fires for this card even though the test
//!     object carries no "Q" creature type.
//!   - CR 608.2c: "That nonland card" names the card the exile-until loop found,
//!     so the grant lands on that card, never on Q (the trigger source).

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{ContinuousModification, TargetFilter};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const Q_ORACLE: &str = "When Q enters, shuffle your library, then exile cards from the top of your library until you exile a nonland card. That nonland card gains \"Exile a card from your hand: Create a copy of this card. You may cast the copy. Activate only during your turn and only if this card is exiled.\"";

fn pool(colors: &[ManaType]) -> Vec<ManaUnit> {
    colors
        .iter()
        .map(|&color| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect()
}

#[test]
fn q_etb_exiles_until_nonland_and_grants_to_the_found_card_not_q() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let q = scenario
        .add_creature_to_hand_from_oracle(P0, "Q, Stacking the Deck", 2, 2, Q_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red, ManaCostShard::Red],
            generic: 2,
        })
        .as_legendary()
        .id();
    let forest = scenario.add_land_to_library_top(P0, "Forest").id();
    let spell = scenario
        .add_spell_to_library_top(P0, "Probe Growth", false)
        .from_oracle_text("You gain 3 life.")
        .id();
    let mountain = scenario.add_land_to_library_top(P0, "Mountain").id();
    scenario.with_mana_pool(
        P0,
        pool(&[
            ManaType::Red,
            ManaType::Red,
            ManaType::Colorless,
            ManaType::Colorless,
        ]),
    );
    let mut runner = scenario.build();
    let outcome = runner.cast(q).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "Q and its ETB trigger finish resolving, found {:?}",
        outcome.final_waiting_for()
    );
    assert!(outcome.state().stack.is_empty());
    outcome.assert_zone(&[q], Zone::Battlefield);

    // The loop stops on the only nonland card; whichever lands the shuffle put
    // above it are exiled too, and nothing else leaves the library.
    outcome.assert_zone(&[spell], Zone::Exile);
    for land in [forest, mountain] {
        assert!(matches!(outcome.zone_of(land), Zone::Library | Zone::Exile));
    }

    // CR 608.2c: the grant is bound to the found card alone.
    let grants: Vec<_> = outcome
        .state()
        .transient_continuous_effects
        .iter()
        .filter(|effect| {
            effect
                .modifications
                .iter()
                .any(|m| matches!(m, ContinuousModification::GrantAbility { .. }))
        })
        .collect();
    assert_eq!(grants.len(), 1, "{grants:?}");
    assert_eq!(
        grants[0].affected,
        TargetFilter::SpecificObject { id: spell },
        "the grant lands on the exiled nonland card, not on Q"
    );
}
