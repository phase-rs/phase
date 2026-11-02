//! CR 608.2c + CR 701.20a: "deals damage to you equal to the number of cards
//! revealed this way" after a reveal-until that ROUTES its cards (Madcap
//! Experiment: the artifact hit onto the battlefield, the rest on the library
//! bottom) counts every card the reveal looked at — misses included — not only
//! the cards that arrived in the kept zone, even when a replacement sends the
//! hit somewhere else.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, ReplacementDefinition, TargetFilter,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::{EtbTapState, Zone};

const MADCAP_EXPERIMENT: &str = "Reveal cards from the top of your library until you reveal an artifact card. Put that card onto the battlefield and the rest on the bottom of your library in a random order. Madcap Experiment deals damage to you equal to the number of cards revealed this way.";

/// Two nonland misses, then an artifact hit, then an untouched card.
fn stage(redirect_battlefield_entry_to_exile: bool) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Madcap Experiment", false, MADCAP_EXPERIMENT)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    if redirect_battlefield_entry_to_exile {
        scenario
            .add_creature(P0, "Exile Redirect", 1, 1)
            .as_enchantment()
            .with_replacement_definition(
                ReplacementDefinition::new(ReplacementEvent::Moved)
                    .destination_zone(Zone::Battlefield)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::ChangeZone {
                            destination: Zone::Exile,
                            origin: None,
                            target: TargetFilter::SelfRef,
                            owner_library: false,
                            enter_transformed: false,
                            enters_under: None,
                            enter_tapped: EtbTapState::Unspecified,
                            enters_attacking: false,
                            up_to: false,
                            enter_with_counters: vec![],
                            conditional_enter_with_counters: vec![],
                            face_down_profile: None,
                            enters_modified_if: None,
                        },
                    ))
                    .description("Exile Redirect".to_string()),
            );
    }
    scenario.add_card_to_library_top(P0, "Deep Card");
    let hit = scenario
        .add_spell_to_library_top(P0, "Artifact Hit", false)
        .as_artifact()
        .id();
    scenario.add_spell_to_library_top(P0, "Miss B", false);
    scenario.add_spell_to_library_top(P0, "Miss A", false);
    (scenario.build(), spell, hit)
}

#[test]
fn damage_counts_every_revealed_card_including_misses() {
    let (mut runner, spell, hit) = stage(false);

    let outcome = runner.cast(spell).resolve();

    assert_eq!(outcome.state().objects[&hit].zone, Zone::Battlefield);
    outcome.assert_life_delta(P0, -3);
}

#[test]
fn damage_counts_every_revealed_card_when_the_hit_is_redirected() {
    let (mut runner, spell, hit) = stage(true);

    let outcome = runner.cast(spell).resolve();

    assert_eq!(
        outcome.state().objects[&hit].zone,
        Zone::Exile,
        "reach guard: the hit's battlefield entry was redirected"
    );
    outcome.assert_life_delta(P0, -3);
}
