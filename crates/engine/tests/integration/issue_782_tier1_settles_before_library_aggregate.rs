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

use engine::game::casting::ActivationCostGuardSite;
use engine::game::perf_counters;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, AggregateFunction, Comparator, Effect,
    ObjectProperty, QuantityExpr, ReplacementDefinition, ReplacementMode, TargetFilter,
    TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::{EtbTapState, Zone};

/// `Composite { [ Tap, ExileWithAggregate { zone: Library } ] }`.
///
/// Tap is the tier-1 leg and is observable (`GameEvent::PermanentTapped`); the
/// aggregate exile is the tier-2 leg and is interactive, so it must not be
/// surfaced until the tap has settled.
fn tap_then_library_aggregate() -> AbilityCost {
    AbilityCost::Composite {
        costs: vec![AbilityCost::Tap, library_aggregate()],
    }
}

/// `ExileWithAggregate { zone: Library }` over every library card.
fn library_aggregate() -> AbilityCost {
    AbilityCost::ExileWithAggregate {
        // A bare typed filter: every library card is eligible. `SelfRef` (the
        // classifier fixture in `mana_abilities.rs`) would match nothing here —
        // that literal exists to exercise zone-field reads, not to be paid.
        filter: TargetFilter::Typed(TypedFilter::default()),
        function: AggregateFunction::Sum,
        property: ObjectProperty::ManaValue,
        comparator: Comparator::GE,
        value: 0,
        zone: Zone::Library,
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
    install_ability(&mut runner, source, ability);

    let library_before = library_ids(&runner);
    assert!(
        library_before.len() >= 2,
        "reach-guard: the library must be seeded for the aggregate to have candidates; got {library_before:?}"
    );
    assert!(
        !runner.state().objects[&source].tapped,
        "reach-guard: the source starts untapped, so `tapped` is evidence the tap leg was paid"
    );

    perf_counters::reset();
    let result = runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("reach-guard: a tap + library-aggregate composite must be activatable");
    // CR 601.2f: the first-tier settle is a cost-work entry, so it checks the
    // activation's cost lock (a locked carrier proceeds).
    assert!(
        perf_counters::activation_cost_route_snapshot().guard_reaches
            [ActivationCostGuardSite::LibraryAggregateTierOne as usize]
            > 0,
        "the first-tier settle must pass through its cost-lock guard"
    );

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
            assert_eq!(
                library_ids(&runner),
                library_before,
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

/// Negative control for the tiering change: a GRAVEYARD aggregate is public ->
/// public, so CR 400.2 keeps it in CR 601.2h's first tier and the scheduler
/// surfaces it in its established position, ahead of the automatic tap. Without
/// this, a fix that simply deferred every aggregate prompt would look correct
/// above while changing Baron Helmut Zemo's Boast scheduling.
///
/// Discriminating both ways: if the library-only tier split ever widened to the
/// graveyard, the tap would settle first and the prompt-time assertions below
/// fail; if the graveyard leg stopped being paid, the post-answer assertions fail.
#[test]
fn a_graveyard_aggregate_is_not_deferred() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Graveyard Payer", 2, 2).id();
    scenario.with_graveyard(P0, &["Grave One", "Grave Two"]);

    let mut runner = scenario.build();

    let ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
    )
    .cost(AbilityCost::Composite {
        costs: vec![
            AbilityCost::Tap,
            AbilityCost::ExileWithAggregate {
                filter: TargetFilter::Typed(TypedFilter::default()),
                function: AggregateFunction::Sum,
                property: ObjectProperty::ManaValue,
                comparator: Comparator::GE,
                value: 0,
                zone: Zone::Graveyard,
            },
        ],
    });
    install_ability(&mut runner, source, ability);

    let graveyard: Vec<ObjectId> = runner.state().players[0]
        .graveyard
        .iter()
        .copied()
        .collect();
    assert_eq!(
        graveyard.len(),
        2,
        "reach-guard: the graveyard is seeded with the aggregate's candidates"
    );

    let result = runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("a tap + graveyard-aggregate composite must be activatable");
    let kinds = event_kinds(&result.events);

    match &runner.state().waiting_for {
        WaitingFor::PayCost {
            kind: PayCostKind::ExileAggregate { zone, .. },
            choices,
            ..
        } => {
            assert_eq!(*zone, Zone::Graveyard, "the graveyard leg is the prompt");
            assert_eq!(
                choices.len(),
                2,
                "both graveyard cards are offered; got {choices:?}"
            );
        }
        other => panic!(
            "a graveyard aggregate keeps its first-tier prompt; got {other:?}\nevents: {kinds:?}"
        ),
    }
    assert!(
        !result.events.iter().any(|event| matches!(
            event,
            GameEvent::PermanentTapped { object_id, .. } if *object_id == source
        )) && !runner.state().objects[&source].tapped,
        "CR 400.2 + CR 601.2h: a public->public aggregate is not deferred behind the tap, \
         so the prompt opens before the tap is paid; events: {kinds:?}"
    );

    runner
        .act(GameAction::SelectCards {
            cards: graveyard.clone(),
        })
        .expect("answering the graveyard aggregate prompt must be accepted");

    for card in &graveyard {
        assert_eq!(
            runner.state().objects[card].zone,
            Zone::Exile,
            "the selected graveyard card pays the aggregate cost"
        );
    }
    assert!(
        runner.state().objects[&source].tapped,
        "the tap leg is paid after the graveyard selection"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "with every leg paid the ability reaches the stack"
    );
}

/// An OPTIONAL redirect of a move into a player's hand. A self-return cost that
/// meets it pauses on a CR 616.1 replacement choice; declining delivers the
/// source to its owner's hand.
fn optional_hand_redirect() -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(Zone::Hand)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                actor: engine::types::ability::LibraryInstructionActor::Controller,
                library_shuffle: Default::default(),
                destination: Zone::Graveyard,
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
        .mode(ReplacementMode::Optional { decline: None })
}

/// CR 601.2h + CR 602.2b + CR 616.1: when a replacement choice pauses the
/// first-tier payment, the deferred library aggregate leg survives the pause.
/// Resuming surfaces its `ExileAggregate` prompt and pays it before the ability
/// reaches the stack: "Partial payments are not allowed."
///
/// `Composite { [ ReturnToHand(self), ExileWithAggregate { zone: Library } ] }`:
/// the self-return is first tier and pauses on the optional hand redirect.
///
/// Revert-proof: on `d7e8bbfdd` the scheduler stripped the aggregate leg before
/// the tier-1 payment and the pause kept only the unpaid tier-1 suffix, so the
/// replacement answer pushed the ability with no `ExileAggregate` prompt and the
/// library untouched.
#[test]
fn a_replacement_paused_tier_one_still_pays_the_library_aggregate_before_the_stack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Paused Payer", 2, 2).id();
    scenario.with_library_top(P0, &["Lib One", "Lib Two", "Lib Three"]);
    scenario
        .add_creature(P0, "Optional Hand Redirect", 0, 0)
        .as_enchantment()
        .with_replacement_definition(optional_hand_redirect());

    let mut runner = scenario.build();

    let ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
    )
    .cost(AbilityCost::Composite {
        costs: vec![
            AbilityCost::ReturnToHand {
                count: 1,
                filter: Some(TargetFilter::SelfRef),
                from_zone: None,
            },
            library_aggregate(),
        ],
    });
    install_ability(&mut runner, source, ability);

    let library_before = library_ids(&runner);
    assert_eq!(
        library_before.len(),
        3,
        "reach-guard: the library is seeded"
    );
    let life_before = runner.state().players[0].life;

    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("a self-return + library-aggregate composite must be activatable");

    // Reach-guard: the first-tier self-return really paused on the redirect.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "reach-guard: the self-return cost must pause on the optional hand redirect; got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        library_ids(&runner),
        library_before,
        "CR 601.2h: no library card moves while the first tier is unpaid"
    );

    // Decline the redirect: the source settles in its owner's hand.
    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("declining the optional redirect must be accepted");
    assert_eq!(
        runner.state().objects[&source].zone,
        Zone::Hand,
        "reach-guard: declining the redirect pays the self-return into hand"
    );

    let choices = match &runner.state().waiting_for {
        WaitingFor::PayCost {
            kind: PayCostKind::ExileAggregate { zone, .. },
            choices,
            ..
        } => {
            assert_eq!(
                *zone,
                Zone::Library,
                "the deferred library leg is the prompt"
            );
            choices.clone()
        }
        other => panic!(
            "CR 601.2h + CR 602.2b: after the paused first tier resumes, the deferred library \
             aggregate leg must still be surfaced before activation completes; got {other:?} \
             with stack {:?}",
            runner.state().stack
        ),
    };
    assert!(
        runner.state().stack.is_empty(),
        "the ability must not reach the stack before its aggregate leg is paid"
    );
    let paid = library_before[0];
    assert!(
        choices.contains(&paid),
        "reach-guard: the top library card is a candidate; got {choices:?}"
    );

    runner
        .act(GameAction::SelectCards { cards: vec![paid] })
        .expect("answering the library aggregate prompt must be accepted");

    assert_eq!(
        runner.state().objects[&paid].zone,
        Zone::Exile,
        "the selected library card was exiled to pay the aggregate leg"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "with every leg paid the ability reaches the stack exactly once"
    );
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().players[0].life,
        life_before + 1,
        "the ability resolves after its full cost was paid"
    );
}

fn library_ids(runner: &GameRunner) -> Vec<ObjectId> {
    runner.state().players[0].library.iter().copied().collect()
}

/// Both fields: a layer flush rebuilds `abilities` from `base_abilities`, so
/// assigning only the derived field lets the ability vanish before activation
/// and turns an engine result into a harness artifact.
fn install_ability(runner: &mut GameRunner, source: ObjectId, ability: AbilityDefinition) {
    let obj = runner
        .state_mut()
        .objects
        .get_mut(&source)
        .expect("the scenario source exists");
    obj.abilities = Arc::new(vec![ability.clone()]);
    obj.base_abilities = Arc::new(vec![ability]);
}
