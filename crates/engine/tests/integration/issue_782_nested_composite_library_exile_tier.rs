//! Issue #782 / PR #8911 review round 6 — CR 601.2h tiering must see a library
//! exile nested inside a child `Composite`, not only at the top level.
//!
//! CR 601.2h: "First, they pay all costs that don't involve random elements or
//! moving objects from the library to a public zone, in any order. Then they pay
//! all remaining costs in any order." CR 602.2b binds 601.2b-i to activated
//! abilities, so an activation cost pays its library-to-public-zone leg LAST.
//! CR 400.2 fixes "public": graveyard, battlefield, stack, exile, ante, command —
//! library and hand are hidden, so only library->public moves are deferred.
//!
//! The partition in `costs::pay_ability_cost_inner`'s `Composite` arm classifies
//! each component with `is_library_to_public_zone_cost`, which matches only an
//! IMMEDIATE `Exile { zone: Library }` / `ExileWithAggregate { zone: Library }`.
//! A child `Composite` that *contains* such a leg is therefore classified tier 1
//! and paid in stored order — before a sibling tap/sacrifice that CR 601.2h says
//! must be paid first.
//!
//! The nesting is reachable: `AbilityCost::Composite` nests itself, and a
//! constructed ability (an `AbilityDefinition` built in code, as engine callers
//! and the sibling test in `issue_782_thought_lash_exile_top_activation_cost.rs`
//! do) is not required to be flat. `OneOf` and `PerCounter` are deliberately NOT
//! covered here: both are rejected before this arm — `OneOf` at
//! `costs.rs` ("OneOf cost is only valid as an unless-cost") and `PerCounter`
//! ("PerCounter cost must be expanded against game state before reaching
//! pay_ability_cost") — so `Composite` is the only reachable nesting.
//!
//! DISCRIMINATING OBSERVATION. The tap leg is the instrument: it can only be paid
//! while the source is untapped, and paying it is observable. Ordering the nested
//! library-exile leg FIRST in the stored vector means a tier-blind partition
//! moves the library card before the tap is paid. Asserting "the card left the
//! library" alone would NOT discriminate — a correct engine also exiles it, just
//! later. So the assertion is on the ORDER: at the moment the library card
//! leaves, the source must already be tapped.

use std::sync::Arc;

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// `Composite { [ Composite { [ Exile{Library} ] }, Tap ] }` — the library leg is
/// NESTED and stored FIRST, which is exactly the shape the top-level-only
/// predicate misclassifies as tier 1.
fn nested_library_exile_then_tap() -> AbilityCost {
    AbilityCost::Composite {
        costs: vec![
            AbilityCost::Composite {
                costs: vec![AbilityCost::Exile {
                    count: 1,
                    zone: Some(Zone::Library),
                    filter: None,
                }],
            },
            AbilityCost::Tap,
        ],
    }
}

fn source_with_nested_cost() -> (GameRunner, ObjectId, ObjectId, usize) {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Paid Card", "Filler One", "Filler Two"]);
    let source = scenario.add_creature(P0, "Tiered Payer", 2, 2).id();

    let mut runner = scenario.build();

    let ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
    )
    .cost(nested_library_exile_then_tap());
    {
        let obj = runner
            .state_mut()
            .objects
            .get_mut(&source)
            .expect("the scenario source object exists");
        // Both fields: a layer flush rebuilds `abilities` from `base_abilities`,
        // so setting only the derived field lets the ability vanish before
        // activation and turns an engine result into a harness artifact.
        obj.abilities = Arc::new(vec![ability.clone()]);
        obj.base_abilities = Arc::new(vec![ability]);
    }

    let top = runner.state().players[0]
        .library
        .iter()
        .copied()
        .next()
        .expect("reach-guard: library seeded");

    // Reach-guard: the constructed ability survived to activation time carrying
    // the nested library-exile cost, so nothing below can pass vacuously.
    let index = runner.state().objects[&source]
        .abilities
        .iter()
        .position(|ability| {
            matches!(
                &ability.cost,
                Some(AbilityCost::Composite { costs })
                    if costs.iter().any(|c| matches!(c, AbilityCost::Composite { .. }))
            )
        })
        .unwrap_or_else(|| {
            panic!(
                "the nested-composite activation cost must survive to activation; got {:?}",
                runner.state().objects[&source]
                    .abilities
                    .iter()
                    .map(|a| &a.cost)
                    .collect::<Vec<_>>()
            )
        });

    (runner, source, top, index)
}

/// CR 601.2h + CR 602.2b: the tap leg (tier 1) must be paid before the nested
/// library-exile leg (tier 2), even though the library leg is stored first.
///
/// Revert-proof: with the top-level-only `is_library_to_public_zone_cost`, the
/// nested child sorts into tier 1 and is paid in stored order, so the card leaves
/// the library while the source is still untapped and the order assertion fails.
#[test]
fn a_nested_library_exile_leg_is_paid_after_the_first_tier_tap() {
    let (mut runner, source, top, index) = source_with_nested_cost();

    assert!(
        !runner.state().objects[&source].tapped,
        "reach-guard: the source starts untapped, so 'tapped' is evidence the tap leg was paid"
    );
    assert_eq!(
        runner.state().objects[&top].zone,
        Zone::Library,
        "reach-guard: the paid card starts in the library"
    );

    let result = runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: index,
        })
        .expect("a nested composite activation cost must be payable");

    // ORDER, not end state. Both legs are deterministic, so by the time `act`
    // returns the source is tapped AND the card is exiled no matter which leg
    // was paid first — an end-state assertion is satisfied by a tier-blind
    // engine just as well as a correct one, and proves nothing. The event
    // stream is where the ordering is observable: the `Tap` arm routes through
    // `tap_permanent_for_cost(state, source_id, events)` and the library-exile
    // arm through `zone_pipeline::move_object(.., events)`, both pushing into
    // this same vector in payment order.
    let tap_at = result.events.iter().position(|event| {
        matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == source)
    });
    let exile_at = result.events.iter().position(|event| {
        matches!(
            event,
            GameEvent::ZoneChanged { object_id, from: Some(Zone::Library), to: Zone::Exile, .. }
                if *object_id == top
        )
    });

    // Reach-guards first: a missing event would make the comparison below pass
    // or fail for the wrong reason, so neither index may be absent.
    let tap_at = tap_at.unwrap_or_else(|| {
        panic!(
            "reach-guard: the tier-1 tap leg must emit PermanentTapped; events: {:?}",
            result.events
        )
    });
    let exile_at = exile_at.unwrap_or_else(|| {
        panic!(
            "reach-guard: the library-exile leg must emit a Library->Exile ZoneChanged; \
             events: {:?}",
            result.events
        )
    });

    assert!(
        tap_at < exile_at,
        "CR 601.2h + CR 602.2b: the tier-1 tap leg must be paid BEFORE the NESTED \
         library->public exile leg. Paid at tap={tap_at}, exile={exile_at} — the \
         nested child Composite was classified tier 1 because the partition's \
         predicate only matches an immediate Exile/ExileWithAggregate."
    );

    assert_eq!(
        runner.state().objects[&top].zone,
        Zone::Exile,
        "the library leg exiles the top card once it is paid"
    );
    assert!(
        runner.state().objects[&source].tapped,
        "the tap leg leaves the source tapped"
    );
}

/// Negative control for the tiering change: a composite with NO library leg must
/// keep paying in stored order. Without this, a partition that reordered every
/// composite would look correct to the test above.
#[test]
fn a_composite_without_a_library_leg_keeps_its_stored_order() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Untouched One", "Untouched Two"]);
    let source = scenario.add_creature(P0, "Flat Payer", 2, 2).id();

    let mut runner = scenario.build();

    let ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
    )
    .cost(AbilityCost::Composite {
        costs: vec![AbilityCost::Tap],
    });
    {
        let obj = runner
            .state_mut()
            .objects
            .get_mut(&source)
            .expect("the scenario source object exists");
        obj.abilities = Arc::new(vec![ability.clone()]);
        obj.base_abilities = Arc::new(vec![ability]);
    }

    let library_before: Vec<ObjectId> = runner.state().players[0].library.iter().copied().collect();

    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("a tap-only composite activation cost must be payable");

    assert!(
        runner.state().objects[&source].tapped,
        "reach-guard: the tap leg was paid"
    );
    let library_after: Vec<ObjectId> = runner.state().players[0].library.iter().copied().collect();
    assert_eq!(
        library_before, library_after,
        "a composite with no library-to-public leg must not disturb the library"
    );
}
