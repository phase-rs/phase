//! Regression tests for GitHub issue #7384 — an orphaned `Proliferate` frame
//! poisons the resolution stack for the rest of the game.
//!
//! `WaitingFor::ProliferateChoice`'s handler used to call `apply_proliferate`
//! BEFORE taking the proliferate frame off the resolution stack, and to return
//! early when that call paused. `apply_proliferate` pauses whenever a
//! counter-placement replacement needs a CR 616.1 ordering choice (two
//! simultaneously-applicable `AddCounter` replacements — Hardened Scales plus
//! Doubling Season is the common pairing). The counter-additions drain that
//! resumes after the choice never popped the proliferate frame and never
//! resumed the proliferate, so:
//!
//!   * the `Proliferate` direct-choice frame stayed on the stack forever, and
//!     `ResolutionStack::validate` failed every LATER frame transition against
//!     it — the reported panic was a tutor (`SearchLibrary` + trailing
//!     `Shuffle`) parking its tail through `prepend_to_pending_continuation`,
//!     reporting `PromptMismatch { frame: Proliferate, waiting_for:
//!     "SearchChoice" }`;
//!   * every proliferate action after the first was silently skipped, so
//!     "proliferate twice" (Tekuthal, Inquiry Dominus) performed once
//!     (CR 701.34a).
//!
//! The frame is now taken before any counter is applied, and the remaining
//! actions ride `PendingCounterPostAction::ContinueProliferateActions` on the
//! counter-additions completion.

use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, QuantityExpr, QuantityModification,
    ReplacementDefinition, ReplacementPlayerScope, ResolvedAbility, SearchSelectionConstraint,
    TargetFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::WaitingFor;
use engine::types::replacements::ReplacementEvent;
use engine::types::resolution::ResolutionFrame;

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::EffectKind;
use engine::types::identifiers::ObjectId;

/// CR 614.1a: a counter-placement replacement in the Hardened Scales / Doubling
/// Season class. Two of these are simultaneously applicable to the same
/// `AddCounter`, which is what makes the engine raise a CR 616.1 ordering
/// prompt mid-proliferate.
fn counter_modifier(modification: QuantityModification) -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::AddCounter)
        .valid_card(TargetFilter::Typed(TypedFilter::creature()))
        .quantity_modification(modification)
}

/// CR 701.34a + CR 614.1a: Tekuthal, Inquiry Dominus — "If you would
/// proliferate, proliferate twice instead."
fn proliferate_doubler() -> ReplacementDefinition {
    let mut execute = AbilityDefinition::new(AbilityKind::Spell, Effect::Proliferate);
    execute.repeat_for = Some(QuantityExpr::Fixed { value: 2 });
    let mut replacement =
        ReplacementDefinition::new(ReplacementEvent::Proliferate).execute(execute);
    replacement.valid_player = Some(ReplacementPlayerScope::You);
    replacement
}

fn proliferate_frames(state: &engine::types::game_state::GameState) -> usize {
    state
        .resolution_stack
        .iter()
        .filter(|frame| matches!(frame, ResolutionFrame::Proliferate(_)))
        .count()
}

fn count_events(events: &[GameEvent], action: PlayerActionKind) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::PlayerPerformedAction { action: got, .. } if *got == action
            )
        })
        .count()
}

/// The discriminating row. A doubled proliferate whose counter placement pauses
/// on a CR 616.1 ordering choice must (a) leave no `Proliferate` frame stranded
/// on the resolution stack and (b) still perform BOTH actions.
///
/// Pre-fix this stranded a `Proliferate` frame after the very first replacement
/// choice and performed one action instead of two.
#[test]
fn issue_7384_proliferate_paused_by_counter_replacement_keeps_the_stack_clean() {
    let mut scenario = GameScenario::new();
    let tekuthal = scenario
        .add_creature(P0, "Tekuthal, Inquiry Dominus", 3, 3)
        .with_replacement_definition(proliferate_doubler())
        .id();
    scenario
        .add_creature(P0, "Doubling Season", 0, 0)
        .as_enchantment()
        .with_replacement_definition(counter_modifier(QuantityModification::DOUBLE));
    scenario
        .add_creature(P0, "Hardened Scales", 0, 0)
        .as_enchantment()
        .with_replacement_definition(counter_modifier(QuantityModification::Plus { value: 1 }));
    let grown = scenario
        .add_creature(P0, "Counter Carrier", 1, 1)
        .with_plus_counters(1)
        .id();

    let mut runner = scenario.build();
    let starting_counters = runner.state().objects[&grown].counters[&CounterType::Plus1Plus1];

    // CR 701.34a: the proliferate replacement doubles the action count, so the
    // first action opens a choice with `remaining: 1` parked behind it.
    let ability = ResolvedAbility::new(Effect::Proliferate, vec![], tekuthal, P0);
    let mut events = Vec::new();
    engine::game::effects::proliferate::resolve(runner.state_mut(), &ability, &mut events).unwrap();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ProliferateChoice { .. }
        ),
        "the doubled proliferate must open its first target choice"
    );
    assert_eq!(
        proliferate_frames(runner.state()),
        1,
        "the open choice is owned by exactly one proliferate frame"
    );

    // Drive both proliferate actions, answering each CR 616.1 ordering prompt
    // the counter placement raises. Bounded so a regression that wedges or
    // re-prompts forever fails loudly instead of hanging.
    let mut proliferate_choices = 0;
    let mut replacement_choices = 0;
    let mut all_events: Vec<GameEvent> = events;
    for _ in 0..12 {
        let result = match &runner.state().waiting_for {
            WaitingFor::ProliferateChoice { .. } => {
                proliferate_choices += 1;
                runner.act(GameAction::SelectTargets {
                    targets: vec![engine::types::ability::TargetRef::Object(grown)],
                })
            }
            WaitingFor::ReplacementChoice { .. } => {
                replacement_choices += 1;
                // THE key intermediate assertion: while the counter-placement
                // choice is open the proliferate frame must NOT be resident.
                // Pre-fix it was, buried under the `CounterAdditions` frame.
                assert_eq!(
                    proliferate_frames(runner.state()),
                    0,
                    "no proliferate frame may survive across a counter-placement choice"
                );
                runner.act(GameAction::ChooseReplacement { index: 0 })
            }
            _ => break,
        }
        .expect("every prompt raised by a paused proliferate must be answerable");
        all_events.extend(result.events);
    }

    let state = runner.state();
    assert_eq!(
        replacement_choices, 2,
        "each of the two proliferate actions places a counter and so raises its own \
         CR 616.1 ordering prompt — fewer means an action was skipped"
    );
    assert_eq!(
        proliferate_choices, 2,
        "CR 701.34a: a doubled proliferate performs TWO actions, each with its own \
         target choice (pre-fix the second never opened)"
    );

    // The panic's precondition, stated directly: nothing proliferate-shaped may
    // outlive the resolution anywhere in the stack (not merely at its top).
    assert_eq!(
        proliferate_frames(state),
        0,
        "a completed proliferate must leave no frame stranded on the resolution stack"
    );
    assert!(
        state.active_counter_additions().is_none(),
        "the parked counter additions must be fully drained"
    );

    // Reach-guard: the negatives above must not pass on a fixture where
    // proliferate never actually did anything.
    // Exact, not merely "grew": both `ChooseReplacement { index: 0 }` answers are
    // deterministic, so each action's single counter becomes +1 (Hardened Scales)
    // then doubled (Doubling Season) = 4. Starting at 1, two actions land 1+4+4.
    // An exact total is what discriminates a dropped or duplicated addition out
    // of the parked `remaining` queue, which an inequality cannot.
    assert_eq!(
        state.objects[&grown].counters[&CounterType::Plus1Plus1],
        9,
        "two proliferate actions, each placing one replacement-modified counter"
    );
    assert_eq!(
        starting_counters, 1,
        "the fixture's starting point, pinned so the total above stays derivable"
    );
    assert_eq!(
        count_events(&all_events, PlayerActionKind::Proliferate),
        2,
        "CR 701.34a: exactly one player-action event per proliferate action"
    );
    assert_eq!(
        all_events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::Proliferate,
                    ..
                }
            ))
            .count(),
        1,
        "the whole doubled proliferate resolves exactly once, after its last action"
    );
    assert!(
        !all_events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Proliferate,
                source_id: ObjectId(0),
                ..
            }
        )),
        "the completion must carry the real proliferate source, not ObjectId(0)"
    );
}

/// The reported crash itself: after a proliferate that paused on a counter
/// replacement, a later chain that parks a continuation must not blow up. The
/// reporter's trigger was a tutor — `SearchLibrary` with a trailing `Shuffle` —
/// whose tail is parked through `prepend_to_pending_continuation`, the
/// `.expect` that panicked.
#[test]
fn issue_7384_tutor_after_paused_proliferate_does_not_panic() {
    let mut scenario = GameScenario::new();
    scenario
        .add_creature(P0, "Doubling Season", 0, 0)
        .as_enchantment()
        .with_replacement_definition(counter_modifier(QuantityModification::DOUBLE));
    scenario
        .add_creature(P0, "Hardened Scales", 0, 0)
        .as_enchantment()
        .with_replacement_definition(counter_modifier(QuantityModification::Plus { value: 1 }));
    let grown = scenario
        .add_creature(P0, "Counter Carrier", 1, 1)
        .with_plus_counters(1)
        .id();
    scenario.add_card_to_library_top(P0, "Tutor Target");

    let mut runner = scenario.build();
    let source = grown;

    let ability = ResolvedAbility::new(Effect::Proliferate, vec![], source, P0);
    engine::game::effects::proliferate::resolve(runner.state_mut(), &ability, &mut Vec::new())
        .unwrap();
    runner
        .act(GameAction::SelectTargets {
            targets: vec![engine::types::ability::TargetRef::Object(grown)],
        })
        .expect("submit the proliferate targets");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "two applicable counter replacements must raise a CR 616.1 ordering prompt"
    );
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("answer the ordering prompt");
    assert_eq!(
        proliferate_frames(runner.state()),
        0,
        "the proliferate frame must not outlive the counter-placement choice"
    );

    // CR 701.23a: the tutor whose trailing `Shuffle` is parked as a
    // continuation. Pre-fix this panicked in `prepend_to_pending_continuation`
    // with PromptMismatch { frame: Proliferate, waiting_for: "SearchChoice" }.
    let mut search = ResolvedAbility::new(
        Effect::SearchLibrary {
            source_zones: vec![engine::types::zones::Zone::Library],
            filter: TargetFilter::Any,
            count: QuantityExpr::Fixed { value: 1 },
            reveal: false,
            target_player: None,
            selection_constraint: SearchSelectionConstraint::None,
            split: None,
        },
        vec![],
        source,
        P0,
    );
    search.sub_ability = Some(Box::new(ResolvedAbility::new(
        Effect::Shuffle {
            target: TargetFilter::Any,
        },
        vec![],
        source,
        P0,
    )));

    let mut events = Vec::new();
    engine::game::effects::resolve_ability_chain(runner.state_mut(), &search, &mut events, 0)
        .expect("the tutor chain must resolve, not panic on a stranded proliferate frame");

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::SearchChoice { .. }),
        "the tutor opens its search choice"
    );
}

/// CR 122.1 + CR 614.1a: `Effect::ProliferateTarget` (Skyship Plunderer's forced
/// single-target form) spells out the counter-add rather than using the
/// proliferate keyword action, so it must NEVER publish
/// `PlayerActionKind::Proliferate` — publishing it would fire "whenever you
/// proliferate" triggers off a card that does not proliferate.
///
/// It shares `apply_proliferate` with the chooser-driven form, and before this
/// fix that function hardcoded a completion of
/// `with_player_action(EffectKind::Proliferate, ObjectId(0), .., Proliferate)`.
/// So on the paused path it emitted the forbidden player action AND a second
/// `EffectResolved` — the effect had already pushed its own, eagerly, before the
/// counters landed.
#[test]
fn issue_7384_proliferate_target_paused_by_counter_replacement_emits_no_keyword_action() {
    let mut scenario = GameScenario::new();
    scenario
        .add_creature(P0, "Doubling Season", 0, 0)
        .as_enchantment()
        .with_replacement_definition(counter_modifier(QuantityModification::DOUBLE));
    scenario
        .add_creature(P0, "Hardened Scales", 0, 0)
        .as_enchantment()
        .with_replacement_definition(counter_modifier(QuantityModification::Plus { value: 1 }));
    let plunderer = scenario.add_creature(P0, "Skyship Plunderer", 2, 1).id();
    let grown = scenario
        .add_creature(P0, "Counter Carrier", 1, 1)
        .with_plus_counters(1)
        .id();

    let mut runner = scenario.build();
    let ability = ResolvedAbility::new(
        Effect::ProliferateTarget {
            target: TargetFilter::Any,
        },
        vec![engine::types::ability::TargetRef::Object(grown)],
        plunderer,
        P0,
    );

    let mut all_events = Vec::new();
    engine::game::effects::proliferate::resolve_target(
        runner.state_mut(),
        &ability,
        &mut all_events,
    )
    .unwrap();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "two applicable counter replacements must pause the targeted form too — \
         otherwise this row never reaches the completion it is about"
    );
    // The eager emit must NOT have fired yet: the effect has not finished.
    assert_eq!(
        all_events
            .iter()
            .filter(|event| matches!(event, GameEvent::EffectResolved { .. }))
            .count(),
        0,
        "a paused ProliferateTarget must not announce itself resolved before its \
         counters land"
    );

    let result = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("answer the CR 616.1 ordering prompt");
    all_events.extend(result.events);

    assert_eq!(
        count_events(&all_events, PlayerActionKind::Proliferate),
        0,
        "CR 122.1: the forced-target form must never publish the proliferate \
         keyword action, or it fires 'whenever you proliferate' triggers"
    );
    assert_eq!(
        all_events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::ProliferateTarget,
                    ..
                }
            ))
            .count(),
        1,
        "exactly one EffectResolved, carrying this effect's own kind"
    );
    assert!(
        !all_events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Proliferate,
                ..
            }
        )),
        "the shared completion must not mis-attribute the targeted form as the \
         chooser-driven Proliferate"
    );
    // Exact, for the same reason as the row above. `ChooseReplacement { index: 0 }`
    // is deterministic but the applicable set is ordered per fixture, and this
    // one resolves Doubling Season first: one counter doubled to 2, then +1 from
    // Hardened Scales = 3, onto a starting 1. (The doubled-proliferate row above
    // has a different permanent set and so resolves them the other way round —
    // which is why each row pins its own measured total rather than sharing a
    // constant.)
    assert_eq!(
        runner.state().objects[&grown].counters[&CounterType::Plus1Plus1],
        4,
        "reach-guard: the replacement-modified counter must actually have landed"
    );
}
