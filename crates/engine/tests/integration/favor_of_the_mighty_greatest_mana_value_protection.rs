//! Backlog root cause 1, at the STATIC-ABILITY boundary.
//!
//! Favor of the Mighty: "Each creature with the greatest mana value has
//! protection from each color."
//!
//! The bare postnominal superlative (no trailing `among <set>` clause) was
//! dropped by the target/filter grammar, so this whole line failed the static
//! parser and lowered to `Effect::Unimplemented { name: "static_structure" }`.
//! Recognizing the superlative takes the card to zero `Unimplemented` — which is
//! exactly why it needs a runtime test rather than an AST assertion: a card that
//! newly reports as *supported* must be demonstrably correct at the boundary that
//! actually consumes it.
//!
//! That boundary is NOT target legality (this ability targets nothing). It is
//! CR 613 layer evaluation of `StaticDefinition.affected`, which re-derives the
//! affected set on every layer pass. So the discriminating property is not just
//! "the highest-mana-value creature has protection" but that the protection
//! MOVES when a bigger creature arrives — the continuous re-check.
//!
//! CR 109.2: with no zone clause and no "card", the ranked population is
//! battlefield creatures. CR 611.3a: a static ability's continuous effect isn't
//! locked in — it is reapplied as the game state changes.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, ProtectionTarget};
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;

/// Favor of the Mighty, verbatim (the whole card is this one line).
const FAVOR_OF_THE_MIGHTY: &str =
    "Each creature with the greatest mana value has protection from each color.";

/// True iff `id` carries layer-baked protection from white after a fresh layer
/// pass. Protection from *each* color grants one `Protection(Color)` keyword per
/// color; white is a representative probe.
fn has_protection_from_white(runner: &mut GameRunner, id: ObjectId) -> bool {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    runner.state().objects[&id]
        .keywords
        .contains(&Keyword::Protection(ProtectionTarget::Color(
            ManaColor::White,
        )))
}

/// CR 109.2 + CR 613.1f + CR 611.3a: protection lands on the greatest-mana-value
/// creature, not on the others — and it MOVES when a larger creature enters.
///
/// Reverting the parser change makes the whole line `Unimplemented`, so no
/// creature gets protection at all and the first assertion fails.
#[test]
fn favor_of_the_mighty_protects_only_the_greatest_mana_value_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Favor of the Mighty", 0, 0, FAVOR_OF_THE_MIGHTY);

    // FOOT-GUN: `add_creature` does not set `mana_cost`, so every fixture
    // permanent needs an explicit one or they all tie at MV 0 and the ranking is
    // vacuous.
    let small = scenario
        .add_creature(P0, "Small Bear", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let big = scenario
        .add_creature(P1, "Big Bear", 4, 4)
        .with_mana_cost(ManaCost::generic(5))
        .id();

    let mut runner = scenario.build();

    // The population is global (no "you control" in the noun phrase), so the
    // opponent's creature can be the one protected — that is the correct reading
    // of "Each creature with the greatest mana value".
    assert!(
        has_protection_from_white(&mut runner, big),
        "the MV 5 creature is the population maximum and must gain protection; \
         no protection at all means the line is still Unimplemented"
    );
    assert!(
        !has_protection_from_white(&mut runner, small),
        "the MV 2 creature is not the maximum and must NOT gain protection"
    );

    // CR 611.3a: the affected set is re-derived, not locked in. A newly-entering
    // larger creature takes the protection over, and the previous holder loses it.
    let bigger = {
        let card_id = engine::types::identifiers::CardId(runner.state().next_object_id);
        let id = engine::game::zones::create_object(
            runner.state_mut(),
            card_id,
            P0,
            "Bigger Bear".to_string(),
            engine::types::zones::Zone::Battlefield,
        );
        let obj = runner.state_mut().objects.get_mut(&id).unwrap();
        obj.card_types
            .core_types
            .push(engine::types::card_type::CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(6);
        obj.toughness = Some(6);
        obj.base_power = Some(6);
        obj.base_toughness = Some(6);
        obj.mana_cost = ManaCost::generic(8);
        obj.base_mana_cost = obj.mana_cost.clone();
        obj.summoning_sick = false;
        id
    };

    assert!(
        has_protection_from_white(&mut runner, bigger),
        "the new MV 8 creature is now the population maximum and must gain protection"
    );
    assert!(
        !has_protection_from_white(&mut runner, big),
        "CR 611.3a: the former maximum must LOSE protection once a larger creature \
         enters — a snapshotted affected set would wrongly keep it"
    );
}
