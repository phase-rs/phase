//! Phase B (zone-change pipeline) discriminating test for the
//! destroy-redirected-to-battlefield delivery-tail bug-fix.
//!
//! Before Phase B, `destroy::apply_destroy_after_replacement` delivered the
//! inner (post-replacement) `ZoneChange` with a bare
//! `zones::move_to_zone(state, oid, to, events)`. When a `Moved` replacement
//! redirected the destruction *to the battlefield* (CR 614.6), that bare move
//! dropped the entire delivery tail: CR 614.1c "[scope] creatures you control
//! enter with an additional [counter] counter" statics (Kalain / Bard Class /
//! Master Chef class) never applied to the redirected entry, exile-link
//! tracking was skipped, and the post-replacement-continuation never drained.
//!
//! Phase B routes that inner delivery through `zone_pipeline::deliver` (the
//! `ApprovedZoneChange` proof token), so a destruction redirected to the
//! battlefield now gets the same delivery tail as any other battlefield entry.
//!
//! This test drives the real Destroy pipeline (cast "Destroy target creature"
//! -> ProposedEvent::Destroy -> inner ZoneChange propose -> replace_event
//! redirect to the battlefield -> deliver) with an `EntersWithAdditionalCounters`
//! static (CR 614.1c) in play, and asserts the redirected creature picks up the
//! additional +1/+1 counter. It FAILS on the old raw-`move_to_zone` delivery
//! (the redirected creature would have zero counters) and passes with the
//! pipeline tail.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ControllerRef, Effect, FilterProp, ReplacementDefinition,
    StaticDefinition, TargetFilter, TypedFilter,
};
use engine::types::counter::CounterType;
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::statics::StaticMode;
use engine::types::zones::{EtbTapState, Zone};

const DESTROY_TARGET_CREATURE: &str = "Destroy target creature.";

/// CR 614.6: "If this creature would die, return it to the battlefield instead."
/// Modeled as a `Moved` (-> Graveyard) replacement whose execute is a self
/// `ChangeZone` back to the battlefield.
fn return_to_battlefield_instead_of_dying() -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(Zone::Graveyard)
        .valid_card(TargetFilter::SelfRef)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                destination: Zone::Battlefield,
                origin: None,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
        ))
        .description("Return to the battlefield instead of dying".to_string())
}

/// CR 614.1c + CR 122.1: "Other creatures you control enter with an additional
/// +1/+1 counter on them" (Kalain / Bard Class class). The `FilterProp::Another`
/// qualifier excludes the static's own source.
fn other_creatures_enter_with_extra_counter() -> StaticDefinition {
    StaticDefinition::new(StaticMode::EntersWithAdditionalCounters {
        counter_type: CounterType::Plus1Plus1,
        count: 1,
    })
    .affected(TargetFilter::Typed(
        TypedFilter::creature()
            .controller(ControllerRef::You)
            .properties(vec![FilterProp::Another]),
    ))
}

#[test]
fn destroy_redirected_to_battlefield_applies_enters_with_counters_delivery_tail() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P1's creature carries the "would die -> return to the battlefield" redirect.
    let victim = scenario
        .add_creature(P1, "Resilient Bear", 2, 2)
        .with_replacement_definition(return_to_battlefield_instead_of_dying())
        .id();

    // A separate P1 enchantment grants the CR 614.1c additional-counter static.
    // The static must already be functioning when the redirected entry is
    // delivered for the delivery tail to pick it up.
    scenario
        .add_creature(P1, "Counter Lord", 0, 0)
        .as_enchantment()
        .with_static_definition(other_creatures_enter_with_extra_counter());

    // P0 casts "Destroy target creature" at the victim through the real cast
    // pipeline.
    let destroy_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY_TARGET_CREATURE)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(destroy_spell).target_object(victim).resolve();

    let obj = outcome
        .state()
        .objects
        .get(&victim)
        .expect("victim object must still exist");

    // CR 614.6: the destruction was replaced — the victim never reached the
    // graveyard; it re-entered the battlefield.
    assert_eq!(
        obj.zone,
        Zone::Battlefield,
        "the Moved redirect returns the creature to the battlefield instead of the graveyard"
    );
    // CR 614.1c: the discriminating assertion. With the old raw-`move_to_zone`
    // delivery the additional-counter static never fires for the redirected
    // entry (counter count would be 0); the pipeline tail applies it.
    assert_eq!(
        obj.counters.get(&CounterType::Plus1Plus1).copied(),
        Some(1),
        "a destruction redirected to the battlefield must receive the CR 614.1c \
         enters-with-additional-counter via the full delivery tail"
    );
}
