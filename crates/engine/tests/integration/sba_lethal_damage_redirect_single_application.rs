//! Phase C pre-work (C0b) diagnostic: a lethal-damage death redirected back to
//! the battlefield must re-enter EXACTLY ONCE per SBA fixpoint, applying its
//! CR 614.1c enters-with-additional-counter static exactly once.
//!
//! During Phase B testing, driving the full `check_state_based_actions` fixpoint
//! against a lethal-damage death redirected to the battlefield (under an "enters
//! with an additional +1/+1 counter" static) stacked counters far above 1 — the
//! same entry was re-delivered repeatedly within one SBA fixpoint even though
//! `reset_for_battlefield_entry` clears `damage_marked`.
//!
//! Root cause: the redirected delivery is a Battlefield->Battlefield ZoneChange,
//! which the CR 603.2g no-op guard rejects — `reset_for_battlefield_entry` never
//! runs, lethal `damage_marked` survives, and every fixpoint pass re-derives the
//! same destruction. The fix scrubs the marked damage after the redirect
//! delivers: a "remains on the battlefield instead of dying" effect is
//! regeneration-shaped (CR 701.19a/b — destruction is replaced by "remove all
//! damage marked on it" while the permanent stays the same object), so the
//! creature must NOT be re-destroyed for the already-replaced damage.
//!
//! This drives the real SBA pipeline (mark lethal damage -> repeated
//! `check_state_based_actions` -> inner ZoneChange propose -> replace_event
//! redirect to battlefield -> deliver), and asserts the redirected creature ends
//! with exactly ONE +1/+1 counter.

use engine::game::sba::check_state_based_actions;
use engine::game::scenario::{GameScenario, P1};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ControllerRef, Effect, FilterProp, ReplacementDefinition,
    StaticDefinition, TargetFilter, TypedFilter,
};
use engine::types::counter::CounterType;
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::statics::StaticMode;
use engine::types::zones::{EtbTapState, Zone};

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
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
        ))
        .description("Return to the battlefield instead of dying".to_string())
}

/// CR 614.1c + CR 122.1: "Other creatures you control enter with an additional
/// +1/+1 counter on them" (Kalain / Bard Class class).
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
fn sba_lethal_damage_redirect_to_battlefield_applies_counter_exactly_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P1's creature carries the "would die -> return to the battlefield" redirect
    // and starts with lethal damage marked (toughness 2, 2 damage).
    let victim = scenario
        .add_creature(P1, "Resilient Bear", 2, 2)
        .with_replacement_definition(return_to_battlefield_instead_of_dying())
        .with_damage_marked(2)
        .id();

    // A separate P1 enchantment grants the CR 614.1c additional-counter static.
    scenario
        .add_creature(P1, "Counter Lord", 0, 0)
        .as_enchantment()
        .with_static_definition(other_creatures_enter_with_extra_counter());

    let mut runner = scenario.build();

    let mut events = Vec::new();
    check_state_based_actions(runner.state_mut(), &mut events);

    let obj = runner
        .state()
        .objects
        .get(&victim)
        .expect("victim object must still exist");

    assert_eq!(
        obj.zone,
        Zone::Battlefield,
        "the Moved redirect returns the creature to the battlefield instead of the graveyard"
    );
    // CR 614.5: the discriminating assertion. The one-shot redirect must apply
    // exactly once per SBA fixpoint — re-delivery would stack counters above 1.
    assert_eq!(
        obj.counters.get(&CounterType::Plus1Plus1).copied(),
        Some(1),
        "a lethal-damage death redirected to the battlefield must receive the \
         CR 614.1c additional counter EXACTLY ONCE (re-delivery stacks counters)"
    );
}
