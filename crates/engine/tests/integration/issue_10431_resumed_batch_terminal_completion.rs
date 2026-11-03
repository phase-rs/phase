//! Issue #10431 — a resumed batch member must be classified before its owner drains.
//!
//! `drain_pending_batch_deliveries` requires every resumed member's exact
//! `terminal_completion` (CR 616.1 + CR 603.10a: the look-back trigger record is
//! the settlement authority). A resumed delivery that parks AGAIN in its own tail
//! used to return before the resume seam captured that classification, so the
//! batch owner later drained with `terminal_completion: None` and panicked at
//! `zone_pipeline.rs` ("resumed batch delivery records its exact terminal
//! completion"). The fix records the classification before either terminal.
//!
//! Rows:
//!  1. `resumed_batch_member_settles_after_moved_then_counter_ordering` — a
//!     reveal-until kept creature enters through a CR 616.1 controller-ordering
//!     choice and then a CR 616.1 +1/+1-counter ordering choice; the batch must
//!     settle exactly once with the entrant classified `Moved`.
//!  2. `resumed_batch_member_without_tail_park_settles_synchronously` — control:
//!     the Done path is unchanged (no counter prompt, entrant gets 3 counters).
//!
//! A planned entering-attacker tail-park row was dropped after measuring that a
//! reveal-until entry with `enters_attacking` reaches `Priority` after the
//! controller-ordering answer (no `EntryAttackTargetChoice`), so it could not
//! discriminate the seam; the counter tail park above covers the class.

use engine::game::effects::reveal_until;
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ControllerRef, Effect, FilterProp, QuantityExpr,
    ReplacementDefinition, ResolvedAbility, StaticDefinition, TargetFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::{WaitingFor, ZoneMoveCompletion};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

const HARDENED_SCALES: &str =
    "If one or more +1/+1 counters would be put on a creature you control, \
     that many plus one +1/+1 counters are put on it instead.";
const BRANCHING_EVOLUTION: &str = "If one or more +1/+1 counters would be put on a creature you \
     control, twice that many +1/+1 counters are put on that creature instead.";

/// CR 614.12a: "enters under [controller]" as a `Moved` replacement candidate.
fn enters_under_replacement(controller: ControllerRef, label: &str) -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .valid_card(TargetFilter::SelfRef)
        .destination_zone(Zone::Battlefield)
        .enters_under(controller)
        .execute(AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp))
        .description(label.to_string())
}

/// CR 614.1c: "Other creatures you control enter with an additional +1/+1 counter."
fn other_creatures_enter_with_two_counters() -> StaticDefinition {
    StaticDefinition::new(StaticMode::EntersWithAdditionalCounters {
        counter_type: CounterType::Plus1Plus1,
        count: 2,
    })
    .affected(TargetFilter::Typed(
        TypedFilter::creature()
            .controller(ControllerRef::You)
            .properties(vec![FilterProp::Another]),
    ))
}

/// Put a creature card on top of P0's library with the two controller-ordering
/// replacements attached, and two non-creature cards beneath it.
fn stage_library(scenario: &mut GameScenario) -> ObjectId {
    scenario.add_card_to_library_top(P0, "Below A");
    scenario.add_card_to_library_top(P0, "Below B");
    scenario.add_card_to_library_top(P0, "Revealed Beast")
}

/// Apply the library card's core type and the two controller-ordering
/// replacements once the runner owns the state.
fn configure_kept_creature(
    state: &mut engine::types::game_state::GameState,
    kept: ObjectId,
    two_controller_choices: bool,
) {
    let obj = state.objects.get_mut(&kept).unwrap();
    obj.card_types.core_types = vec![CoreType::Creature];
    obj.base_card_types = obj.card_types.clone();
    obj.power = Some(2);
    obj.toughness = Some(2);
    obj.base_power = Some(2);
    obj.base_toughness = Some(2);
    if two_controller_choices {
        obj.replacement_definitions.push(enters_under_replacement(
            ControllerRef::You,
            "Enters under you",
        ));
        obj.replacement_definitions.push(enters_under_replacement(
            ControllerRef::Opponent,
            "Enters under an opponent",
        ));
    }
}

fn reveal_until_ability(source: ObjectId, enters_attacking: bool) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::RevealUntil {
            player: TargetFilter::Controller,
            filter: TargetFilter::Typed(TypedFilter::creature()),
            count: QuantityExpr::Fixed { value: 1 },
            matched_disposition: Default::default(),
            kept_destination: Zone::Battlefield,
            rest_destination: Zone::Library,
            rest_order: Default::default(),
            enter_tapped: Default::default(),
            enters_attacking,
            kept_optional_to: None,
            enters_under: None,
            kept_destination_if: None,
        },
        vec![],
        source,
        P0,
    )
}

/// Read the parked batch's paused member classification through the same path
/// the resume capture uses (the batch may sit below a PostReplacement child).
fn parked_terminal_completion(
    runner: &mut engine::game::scenario::GameRunner,
) -> Option<Option<ZoneMoveCompletion>> {
    runner
        .state_mut()
        .resolution_stack
        .active_batch_delivery_or_post_replacement_child_mut()
        .and_then(|frame| frame.paused_current.as_ref())
        .map(|paused| paused.terminal_completion)
}

fn describe_waiting(state: &engine::types::game_state::GameState) -> String {
    match &state.waiting_for {
        WaitingFor::ReplacementChoice {
            candidate_count,
            candidates,
            ..
        } => format!(
            "ReplacementChoice({candidate_count}: {})",
            candidates
                .iter()
                .map(|candidate| candidate.description.clone())
                .collect::<Vec<_>>()
                .join(" | ")
        ),
        other => format!("{other:?}"),
    }
}

#[test]
fn resumed_batch_member_settles_after_moved_then_counter_ordering() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // CR 614.1c + CR 616.1: a static 2-counter grant plus two non-commuting
    // counter replacements (additive Hardened Scales vs multiplicative Branching
    // Evolution) force the counter-ordering choice.
    scenario
        .add_creature(P0, "Counter Lord", 1, 1)
        .with_static_definition(other_creatures_enter_with_two_counters());
    scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES);
    scenario.add_enchantment_from_oracle(P0, "Branching Evolution", BRANCHING_EVOLUTION);

    let kept = stage_library(&mut scenario);
    let mut runner = scenario.build();
    configure_kept_creature(runner.state_mut(), kept, true);
    let source = kept;

    let ability = reveal_until_ability(source, false);
    let mut events = Vec::new();
    reveal_until::resolve(runner.state_mut(), &ability, &mut events).unwrap();

    // Pause 1: the two `enters_under` candidates (CR 616.1b).
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice {
                candidate_count: 2,
                ..
            }
        ),
        "pause 1 must be the two-way controller ordering, got {}",
        describe_waiting(runner.state())
    );
    assert!(
        runner.state().active_batch_delivery().is_some(),
        "the kept-card batch must be parked across pause 1"
    );
    assert_eq!(
        parked_terminal_completion(&mut runner),
        Some(None),
        "nothing is classified before the resumed delivery runs"
    );

    // Answer pause 1 with the `Opponent` candidate so `You` is applied last and
    // the entrant is controlled by P0 (the only order where the counter
    // replacements apply at all).
    let opponent_index = match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice { candidates, .. } => candidates
            .iter()
            .position(|candidate| candidate.description.contains("opponent"))
            .expect("the Opponent candidate is offered"),
        other => panic!("expected the controller ordering, got {other:?}"),
    };
    runner
        .act(GameAction::ChooseReplacement {
            index: opponent_index,
        })
        .expect("answer the controller ordering");

    // Pause 2: the counter-ordering choice. The resumed segment has already been
    // classified `Moved` (this is the fix), even though the delivery parked again.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice {
                candidate_count: 2,
                ..
            }
        ),
        "pause 2 must be the two-way counter ordering, got {}",
        describe_waiting(runner.state())
    );
    // Answer the counter ordering with Hardened Scales first: (2 + 1) * 2 = 6.
    let scales_index = match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice { candidates, .. } => candidates
            .iter()
            .position(|candidate| candidate.description.contains("plus one"))
            .unwrap_or(0),
        other => panic!("expected the counter ordering, got {other:?}"),
    };
    runner
        .act(GameAction::ChooseReplacement {
            index: scales_index,
        })
        .expect("answer the counter ordering");

    // The tail continuation drains at the next priority window; the batch
    // settles exactly once (pre-fix this drain panics at `zone_pipeline.rs`).
    for _ in 0..8 {
        if runner.state().active_batch_delivery().is_none() {
            break;
        }
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "the drained batch must return to priority, got {}",
            describe_waiting(runner.state())
        );
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority to drain the batch tail");
    }
    assert!(
        runner.state().active_batch_delivery().is_none(),
        "the batch must settle exactly once, got {}",
        describe_waiting(runner.state())
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "settled back to priority, got {}",
        describe_waiting(runner.state())
    );
    let entrant = runner
        .state()
        .objects
        .get(&kept)
        .expect("the kept creature is live");
    assert_eq!(entrant.zone, Zone::Battlefield, "the kept creature entered");
    assert_eq!(entrant.controller, P0, "the entrant is controlled by P0");
    assert_eq!(
        entrant
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        6,
        "the non-commuting counter replacements apply in the answered order"
    );
}

#[test]
fn resumed_batch_member_without_tail_park_settles_synchronously() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Counter Lord", 1, 1)
        .with_static_definition(other_creatures_enter_with_two_counters());
    // Only one counter replacement: additive alone is a single candidate, so the
    // delivery returns Done and the pre-existing capture path classifies it.
    scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES);

    let kept = stage_library(&mut scenario);
    let mut runner = scenario.build();
    configure_kept_creature(runner.state_mut(), kept, true);
    let source = kept;

    let ability = reveal_until_ability(source, false);
    let mut events = Vec::new();
    reveal_until::resolve(runner.state_mut(), &ability, &mut events).unwrap();

    let opponent_index = match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice { candidates, .. } => candidates
            .iter()
            .position(|candidate| candidate.description.contains("opponent"))
            .expect("the Opponent candidate is offered"),
        other => panic!("expected the controller ordering, got {other:?}"),
    };
    runner
        .act(GameAction::ChooseReplacement {
            index: opponent_index,
        })
        .expect("answer the controller ordering");

    assert!(
        runner.state().active_batch_delivery().is_none(),
        "no tail park: the batch settles synchronously"
    );
    let entrant = &runner.state().objects[&kept];
    assert_eq!(entrant.zone, Zone::Battlefield);
    assert_eq!(entrant.controller, P0);
    assert_eq!(
        entrant
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        3,
        "2 from the static plus 1 from Hardened Scales"
    );
}
