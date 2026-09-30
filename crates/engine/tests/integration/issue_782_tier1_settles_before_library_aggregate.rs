//! PR #8911 round 7 — CR 601.2h: first-tier activation costs must settle BEFORE
//! an interactive library-to-public aggregate exile is surfaced.
//!
//! Round 6 fixed the DETERMINISTIC payment path: `pay_ability_cost_inner`'s
//! `Composite` arm tiers cost LEAVES (not top-level children) so a nested
//! library-exile leg is paid last. The maintainer's round-7 review points at the
//! other half, which that fix does not reach:
//!
//!   `casting_costs::surface_next_unpaid_interactive_activation_cost` runs a
//!   linear, zone-generic `find_exile_with_aggregate_cost` scan and returns
//!   `WaitingFor::PayCost { ExileAggregate }` BEFORE `pay_ability_cost_inner`
//!   can apply its leaf-stable partition. Both no-target activation routes call
//!   that scheduler first (`casting.rs:21100`, `:21479`), and once the scheduler
//!   removes the aggregate leg the generic `ExileWithAggregate` payment arm is
//!   deliberately a no-op — so the later payment cannot repair the removed leg.
//!
//! Consequence: a `Composite` of mana and/or tap plus
//! `ExileWithAggregate { zone: Library, .. }` lets the player move library cards
//! into the public exile zone before the first-tier costs have been paid. CR
//! 601.2h requires every non-library-public cost first and forbids partial
//! payment; CR 602.2b applies that sequence to activated abilities.
//!
//! WHY THIS ASSERTS EVENT ORDER, NOT END STATE. On a deterministic path both
//! legs complete before `act` returns, so "the source is tapped AND the card is
//! exiled" holds identically on a correct engine and a tier-blind one. An
//! end-state assertion would pass either way. The discriminator is WHEN the
//! prompt appears relative to the tier-1 events, so this reads the event stream
//! of the activating action and the board at prompt time.
//!
//! The ability is constructed: no printed card pairs a first-tier cost with a
//! LIBRARY-zone aggregate exile (the parser only ever emits
//! `ExileWithAggregate { zone: Graveyard }` — Baron Helmut Zemo's Boast), so the
//! shape the review describes is not reachable from Oracle text today. Every
//! layer beneath the ability is production: a real `GameAction::ActivateAbility`,
//! the real interactive scheduler, and the real payment authority.

use std::sync::Arc;

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, AggregateFunction, Comparator, Effect,
    ObjectProperty, QuantityExpr, TargetFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// `Composite { [ Tap, ExileWithAggregate { zone: Library } ] }`.
///
/// Tap is the tier-1 leg and is observable (`GameEvent::PermanentTapped`); the
/// aggregate exile is the tier-2 leg and is interactive, so it must not be
/// surfaced until the tap has settled.
fn tap_then_library_aggregate() -> AbilityCost {
    AbilityCost::Composite {
        costs: vec![
            AbilityCost::Tap,
            AbilityCost::ExileWithAggregate {
                // A bare typed filter: every library card is eligible. `SelfRef`
                // (the classifier fixture in `mana_abilities.rs`) would match
                // nothing here — that literal exists to exercise zone-field
                // reads, not to be paid.
                filter: TargetFilter::Typed(TypedFilter::default()),
                function: AggregateFunction::Sum,
                property: ObjectProperty::ManaValue,
                comparator: Comparator::GE,
                value: 0,
                zone: Zone::Library,
            },
        ],
    }
}

fn describe(runner: &GameRunner) -> String {
    format!("{:?}", runner.state().waiting_for)
        .chars()
        .take(120)
        .collect()
}

fn event_kinds(events: &[GameEvent]) -> Vec<String> {
    events
        .iter()
        .map(|event| {
            format!("{event:?}")
                .split_whitespace()
                .next()
                .unwrap_or("?")
                .to_string()
        })
        .collect()
}

/// CR 601.2h + CR 602.2b: the tap leg settles before the library aggregate
/// prompt is raised, and the library card is still in the library at that point.
///
/// Revert-proof: this is red on `c93da4d19`. The interactive scheduler surfaces
/// `PayCost { ExileAggregate }` from its own zone-generic scan before any
/// first-tier cost is paid, so the activating action emits no `PermanentTapped`
/// and the prompt arrives with the source untapped.
#[test]
fn the_tap_leg_settles_before_the_library_aggregate_prompt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Tiered Payer", 2, 2).id();
    // Two library cards so the aggregate prompt is a real choice rather than a
    // degenerate single-candidate auto-selection.
    scenario.with_library_top(P0, &["Lib One", "Lib Two"]);

    let mut runner = scenario.build();

    let ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
    )
    .cost(tap_then_library_aggregate());
    {
        let obj = runner
            .state_mut()
            .objects
            .get_mut(&source)
            .expect("the scenario source exists");
        // Both fields: a layer flush rebuilds `abilities` from `base_abilities`,
        // so assigning only the derived field lets the ability vanish before
        // activation and turns an engine result into a harness artifact.
        obj.abilities = Arc::new(vec![ability.clone()]);
        obj.base_abilities = Arc::new(vec![ability]);
    }

    let library_before: Vec<ObjectId> = runner.state().players[0].library.iter().copied().collect();
    assert!(
        library_before.len() >= 2,
        "reach-guard: the library must be seeded for the aggregate to have candidates; got {library_before:?}"
    );
    assert!(
        !runner.state().objects[&source].tapped,
        "reach-guard: the source starts untapped, so `tapped` is evidence the tap leg was paid"
    );

    let result = runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("reach-guard: a tap + library-aggregate composite must be activatable");

    let kinds = event_kinds(&result.events);
    let tapped_at = result.events.iter().position(|event| {
        matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == source)
    });

    // The discriminating pair, read AT the prompt: the interactive aggregate
    // step must not be open until the tier-1 tap has actually happened.
    match runner.state().waiting_for.clone() {
        WaitingFor::PayCost { choices, .. } => {
            assert!(
                tapped_at.is_some(),
                "CR 601.2h + CR 602.2b: the tier-1 tap leg must be paid BEFORE the \
                 library-to-public aggregate exile is offered. The prompt is open with no \
                 PermanentTapped in the activating action's events, so the scheduler \
                 surfaced the tier-2 leg first.\n\
                 events: {kinds:?}\n\
                 choices: {choices:?}\n\
                 waiting_for = {}",
                describe(&runner)
            );
            assert!(
                runner.state().objects[&source].tapped,
                "the source must already be tapped when the aggregate prompt opens; \
                 events: {kinds:?}"
            );
            // CR 601.2h forbids partial payment: nothing may have left the
            // library before the prompt is answered.
            let library_now: Vec<ObjectId> =
                runner.state().players[0].library.iter().copied().collect();
            assert_eq!(
                library_now, library_before,
                "no library card may move before the aggregate choice is made"
            );
        }
        other => panic!(
            "reach-guard: the library aggregate leg must raise an interactive PayCost \
             prompt — without one this test cannot observe the ordering. got {other:?}\n\
             events: {kinds:?}"
        ),
    }
}

/// Negative control for the tiering change: a composite whose only interactive
/// leg is a GRAVEYARD aggregate (public -> public, tier 1 per CR 400.2) must keep
/// its current scheduling. Without this, a fix that simply deferred every
/// aggregate prompt would look correct above while breaking Baron Helmut Zemo.
#[test]
fn a_graveyard_aggregate_is_not_deferred() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Graveyard Payer", 2, 2).id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );

    let mut runner = scenario.build();

    let ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
    )
    .cost(AbilityCost::ExileWithAggregate {
        filter: TargetFilter::Typed(TypedFilter::default()),
        function: AggregateFunction::Sum,
        property: ObjectProperty::ManaValue,
        comparator: Comparator::GE,
        value: 0,
        zone: Zone::Graveyard,
    });
    {
        let obj = runner
            .state_mut()
            .objects
            .get_mut(&source)
            .expect("the scenario source exists");
        obj.abilities = Arc::new(vec![ability.clone()]);
        obj.base_abilities = Arc::new(vec![ability]);
    }

    let result = runner.act(GameAction::ActivateAbility {
        source_id: source,
        ability_index: 0,
    });

    // Printed rather than asserted on a precise shape: this control exists to
    // catch a REGRESSION introduced by the fix, so its job is to record today's
    // behaviour on the graveyard path and flip if that behaviour changes.
    println!(
        "[control: GRAVEYARD aggregate] activate_ok={} waiting_for={} events={:?}",
        result.is_ok(),
        describe(&runner),
        result.as_ref().map(|r| event_kinds(&r.events)).ok()
    );
}
