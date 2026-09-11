//! Regression for issue #2899 — Tainted Pact repeat-until loop and same-name
//! unless gate on the optional put-to-hand rider.
//!
//! https://github.com/phase-rs/phase/issues/2899
//!
//! # KNOWN REMAINING DEFECT — issue #8798
//!
//! The tests in this file prove TERMINATION only. Tainted Pact cast with an
//! empty library is still wrong: `ChangeZone { target: ParentTarget }` re-binds
//! to the source object when its producing `ExileTop` produced nothing
//! (issue #8798), so the engine still offers one "you may put that card into
//! your hand" prompt that CR 608.2d says cannot be offered, and accepting it
//! still moves Tainted Pact itself from the graveyard to its controller's hand
//! instead of leaving it there (CR 608.2n). Nothing here asserts the spell's
//! final zone, and nothing here drives the accept path — a green file does NOT
//! mean the card is correct.

use std::sync::mpsc;
use std::time::Duration;

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::zones::move_to_library_position;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, EffectKind, LibraryPosition, QuantityExpr, QuantityRef,
    RepeatContinuation, ResolvedAbility, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const TAINTED_PACT_ORACLE: &str = "Exile the top card of your library. You may put that card into your hand unless it has the same name as another card exiled this way. Repeat this process until you put a card into your hand or you exile two cards with the same name, whichever comes first.";

fn put_library_top(runner: &mut GameRunner, id: ObjectId) {
    let owner = runner.state().objects.get(&id).expect("object").owner;
    let mut events = Vec::new();
    move_to_library_position(runner.state_mut(), id, true, &mut events);
    assert_eq!(
        runner.state().players[owner.0 as usize].library[0],
        id,
        "precondition: card must be library top"
    );
}

fn resolve_tainted_pact(runner: &mut GameRunner, source: ObjectId, optional_accepts: &[bool]) {
    let def = parse_effect_chain(TAINTED_PACT_ORACLE, AbilityKind::Spell);
    let ability = build_resolved_from_def(&def, source, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0).unwrap();

    for &accept in optional_accepts {
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::OptionalEffectChoice { .. }
            ),
            "expected optional put prompt, got {:?}",
            runner.state().waiting_for
        );
        runner
            .act(GameAction::DecideOptionalEffect { accept })
            .expect("optional put decision");
    }
}

#[test]
fn tainted_pact_repeats_until_controller_puts_a_card_into_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let third = scenario
        .add_spell_to_library_top(P0, "Third Card", true)
        .id();
    let second = scenario
        .add_spell_to_library_top(P0, "Second Card", true)
        .id();
    let first = scenario
        .add_spell_to_library_top(P0, "First Card", true)
        .id();

    let mut runner = scenario.build();
    put_library_top(&mut runner, first);

    resolve_tainted_pact(&mut runner, ObjectId(900), &[false, false, true]);

    assert_eq!(
        runner.state().objects.get(&third).unwrap().zone,
        Zone::Hand,
        "accepting the third optional put must move the top card into hand"
    );
    assert_eq!(
        runner.state().objects.get(&first).unwrap().zone,
        Zone::Exile,
        "declined iteration must leave the card exiled"
    );
    assert_eq!(
        runner.state().objects.get(&second).unwrap().zone,
        Zone::Exile,
        "declined iteration must leave the card exiled"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "loop must finish after a card is put into hand, got {:?}",
        runner.state().waiting_for
    );
}

#[test]
fn tainted_pact_stops_when_two_exiled_cards_share_a_name() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let bolt_a = scenario
        .add_spell_to_library_top(P0, "Lightning Bolt", true)
        .id();
    let island = scenario.add_spell_to_library_top(P0, "Island", true).id();
    let bolt_b = scenario
        .add_spell_to_library_top(P0, "Lightning Bolt", true)
        .id();

    let mut runner = scenario.build();
    // Exile order: bolt_b, island, bolt_a.
    put_library_top(&mut runner, bolt_b);

    resolve_tainted_pact(&mut runner, ObjectId(900), &[false, false]);

    assert_eq!(
        runner.state().objects.get(&bolt_b).unwrap().zone,
        Zone::Exile
    );
    assert_eq!(
        runner.state().objects.get(&island).unwrap().zone,
        Zone::Exile
    );
    assert_eq!(
        runner.state().objects.get(&bolt_a).unwrap().zone,
        Zone::Exile
    );
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "unless gate must block the third optional put when names duplicate, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "duplicate-name stop must end the loop, got {:?}",
        runner.state().waiting_for
    );
}

/// `{1}{B}` worth of floating mana, so the cast is pool-funded and never
/// surfaces a `ManaPayment` window (CR 601.2g).
fn tainted_pact_mana() -> Vec<ManaUnit> {
    vec![
        ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
        ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
    ]
}

fn tainted_pact_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Black],
        generic: 1,
    }
}

fn chain_contains_unimplemented(def: &AbilityDefinition) -> bool {
    if matches!(*def.effect, Effect::Unimplemented { .. }) {
        return true;
    }
    [def.sub_ability.as_deref(), def.else_ability.as_deref()]
        .into_iter()
        .flatten()
        .any(chain_contains_unimplemented)
}

/// Positive reach-guard shared by the termination regressions: the card really
/// parses to the loop variant whose missing termination guarantee is under
/// test, with no `Effect::Unimplemented` anywhere in the chain. Without this a
/// "the loop ended" assertion would pass vacuously on a card that never parsed
/// into a loop at all.
fn assert_tainted_pact_parses_to_until_stop_conditions() {
    let def = parse_effect_chain(TAINTED_PACT_ORACLE, AbilityKind::Spell);
    assert!(
        !chain_contains_unimplemented(&def),
        "reach-guard: Tainted Pact must parse with no Effect::Unimplemented"
    );
    assert!(
        matches!(
            def.repeat_until,
            Some(RepeatContinuation::UntilStopConditions { .. })
        ),
        "reach-guard: Tainted Pact must parse to the UntilStopConditions repeat, got {:?}",
        def.repeat_until
    );
}

/// Positive reach-guard: the repeat's producer actually ran, so "the loop
/// ended" is not the trivially-true statement that it never started.
fn assert_exile_top_resolved(events: &[GameEvent]) {
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::ExileTop,
                ..
            }
        )),
        "reach-guard: the repeat body must have resolved at least one ExileTop"
    );
}

/// CR 104.4b + CR 101.3 + CR 609.3: an `UntilStopConditions` repeat whose
/// producer is starved from the very first iteration must terminate.
///
/// With an empty library "exile the top card of your library" is impossible and
/// is ignored, so neither printed stop predicate can ever become true and the
/// repeat has no printed way to stop. The no-progress witness ends it.
///
/// Reverting the guard leaves the engine parked on the spurious
/// `OptionalEffectChoice` with the repeat frame still live, so BOTH assertions
/// below flip.
#[test]
fn tainted_pact_empty_library_terminates_the_repeat() {
    assert_tainted_pact_parses_to_until_stop_conditions();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let pact = scenario
        .add_spell_to_hand_from_oracle(P0, "Tainted Pact", true, TAINTED_PACT_ORACLE)
        .with_mana_cost(tainted_pact_cost())
        .id();
    scenario.with_mana_pool(P0, tainted_pact_mana());

    let mut runner = scenario.build();
    assert!(
        runner.state().players[P0.0 as usize].library.is_empty(),
        "precondition: this regression is about a starved producer"
    );

    let outcome = runner.cast(pact).resolve();

    assert_exile_top_resolved(outcome.events());
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "a repeat that cannot advance must end the resolution, got {:?}",
        outcome.final_waiting_for()
    );
    assert!(
        outcome.state().active_repeat_until().is_none(),
        "the repeat-until frame must retire, not stay parked"
    );
}

/// CR 104.4b: a repeat that makes progress and THEN stalls must still
/// terminate.
///
/// SCOPE OF THIS ROW, stated precisely because an earlier draft overclaimed it:
/// this proves termination after real progress, and it is revert-failing for
/// that. It does NOT discriminate a baseline hoisted above the `loop` in
/// `resolve_ability_chain`. Tainted Pact's body pauses every iteration, so each
/// iteration exits through the drain and RE-ENTERS `resolve_ability_chain`,
/// which re-captures the baseline at function entry either way — the hoist is
/// invisible from here. Observing it needs a non-pausing body whose witness
/// genuinely grows, i.e. an `UntilStopConditions` ability carrying a
/// linked-exile consumer, which no fixture in this file builds. The
/// per-iteration capture is still the correct code, and it IS pinned against
/// that specific mutation — by
/// `until_stop_conditions_with_a_tracked_non_pausing_body_terminates_after_progress`
/// at the bottom of this file, not by this row.
#[test]
fn tainted_pact_terminates_when_the_library_empties_mid_repeat() {
    assert_tainted_pact_parses_to_until_stop_conditions();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let only_card = scenario
        .add_spell_to_library_top(P0, "Only Card", true)
        .id();
    let pact = scenario
        .add_spell_to_hand_from_oracle(P0, "Tainted Pact", true, TAINTED_PACT_ORACLE)
        .with_mana_cost(tainted_pact_cost())
        .id();
    scenario.with_mana_pool(P0, tainted_pact_mana());

    let mut runner = scenario.build();
    put_library_top(&mut runner, only_card);
    assert_eq!(
        runner.state().players[P0.0 as usize].library.len(),
        1,
        "precondition: exactly one card, so iteration 2 is the stalled one"
    );

    let outcome = runner.cast(pact).resolve();

    assert_exile_top_resolved(outcome.events());
    // Positive reach-guard: iteration 1 really ran and really exiled, so the
    // termination assertions below are not vacuous.
    outcome.assert_zone(&[only_card], Zone::Exile);
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "a repeat that stalls after making progress must still end, got {:?}",
        outcome.final_waiting_for()
    );
    assert!(
        outcome.state().active_repeat_until().is_none(),
        "the repeat-until frame must retire, not stay parked"
    );
}

/// CR 104.4b: the IN-LOOP guard arm, which the two cast-pipeline regressions
/// above never reach.
///
/// Tainted Pact's body raises a `WaitingFor` every iteration (the spurious
/// prompt of issue #8798), so both of those tests exit through
/// `drain_active_repeat_until`. This variant-level fixture gives the repeat a
/// body that CANNOT pause — bare `ExileTop`, no optional sub-ability — against
/// an empty library, which is exactly the shape #8798 creates for Tainted Pact
/// itself once the spurious prompt is suppressed. Without the in-loop guard the
/// `loop` in `resolve_ability_chain`'s `UntilStopConditions` arm never yields
/// and never returns.
///
/// Deliberately NOT a `/card-test` cast-pipeline row: no card in the corpus
/// prints this body, and `drive_resolution`'s 64-iteration bound only helps if
/// the engine yields between iterations — here it never does, so the spin would
/// happen inside a single `resolve()` call with the bound never consulted. The
/// skill's "never call the raw `resolve()` stack function directly" rule names
/// `stack::resolve_top` / `effect::resolve` and exists to preserve the
/// intervening-if recheck and the `cast_from_zone` carry-through; this fixture
/// never puts anything on the stack and never casts, so that rule has no
/// subject here.
///
/// BOUNDED HARNESS: the work runs on a spawned thread and the assertion waits
/// on `recv_timeout`, so a missing or incorrect guard FAILS instead of hanging
/// the suite. Residual, stated honestly: `recv_timeout` returning does not stop
/// the spawned thread. Under nextest's process-per-test isolation
/// (`.config/nextest.toml`, profile `ci`) the leaked thread dies with this
/// test's own process; under plain `cargo test`, which shares one process per
/// test binary, it keeps spinning and allocating until the binary exits.
#[test]
fn until_stop_conditions_with_a_non_pausing_body_terminates_in_loop() {
    let (tx, rx) = mpsc::channel();
    let _fixture = std::thread::spawn(move || {
        let mut state = GameState::new_two_player(2899);
        let mut ability = ResolvedAbility::new(
            Effect::ExileTop {
                player: TargetFilter::Controller,
                count: QuantityExpr::Fixed { value: 1 },
                position: LibraryPosition::Top,
                face_down: false,
            },
            vec![],
            ObjectId(900),
            P0,
        );
        ability.repeat_until = Some(RepeatContinuation::UntilStopConditions {
            stop_on_put_to_hand: true,
            stop_on_duplicate_exiled_names: false,
        });

        let mut events = Vec::new();
        let ok = resolve_ability_chain(&mut state, &ability, &mut events, 0).is_ok();
        let exile_tops = events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    GameEvent::EffectResolved {
                        kind: EffectKind::ExileTop,
                        ..
                    }
                )
            })
            .count();
        let _ = tx.send((
            ok,
            events.len(),
            exile_tops,
            state.active_repeat_until().is_some(),
        ));
    });

    let (ok, event_count, exile_tops, frame_still_parked) = rx
        .recv_timeout(Duration::from_secs(10))
        .unwrap_or_else(|err| {
            panic!(
                "the UntilStopConditions repeat never returned ({err:?}): the in-loop \
                 `repeat_until_should_terminate` arm in `resolve_ability_chain`'s \
                 UntilStopConditions dispatch is missing or incorrect, so the loop \
                 spins without ever yielding a WaitingFor"
            )
        });

    assert!(ok, "the repeat must resolve cleanly, not error out");
    // Positive reach-guard: a body that failed to build would also "return".
    assert!(
        exile_tops >= 1,
        "reach-guard: the repeat body must have resolved at least one ExileTop"
    );
    assert!(
        event_count < 64,
        "a terminating repeat emits a bounded event list, got {event_count}"
    );
    assert!(
        !frame_still_parked,
        "a non-pausing body must never park a repeat-until frame"
    );
}

/// CR 104.4b: the in-loop guard's baseline is captured PER ITERATION, not once
/// per repeat — the sibling of the test above, and the only row that pins that.
///
/// WHICH MUTATION THIS TEST PINS, stated exactly: hoisting
/// `resolve_ability_chain`'s `let progress_baseline = …repeat_until_stop_witness(…)`
/// out of the `UntilStopConditions` `loop` and above it. That turns the guard
/// into "nothing has been exiled since the repeat BEGAN", a strictly weaker
/// predicate, and this test then spins until its `recv_timeout` and FAILS.
///
/// NOTHING ELSE IN THIS FILE PINS IT.
/// `tainted_pact_terminates_when_the_library_empties_mid_repeat` has the same
/// progress-then-stall SHAPE but exits through `drain_active_repeat_until`,
/// which re-enters `resolve_ability_chain` and so re-captures the baseline at
/// function entry either way — the hoist is invisible from there.
/// `until_stop_conditions_with_a_non_pausing_body_terminates_in_loop` does reach
/// the in-loop arm, but its ability carries NO linked-exile consumer, so
/// `exile_links::should_track_exiled_by_source` is false, neither ledger is ever
/// written, and its witness is empty on every iteration — a hoisted baseline is
/// also empty, so the comparison is unchanged and the hoist is invisible there
/// too. Only a body that is BOTH non-pausing AND tracked discriminates.
///
/// The fixture: `ExileTop` chained to a genuine linked-exile consumer
/// (`QuantityRef::CardsExiledBySource` — "gain 1 life for each card exiled this
/// way"), which makes `should_track_exiled_by_source` true so the ledgers
/// actually grow, and which neither pauses nor moves a card out of exile. Run
/// against a ONE-CARD library: iteration 1 exiles the card and grows the
/// witness; iteration 2 finds the library empty, changes nothing, and must end
/// via the in-loop `repeat_until_should_terminate` arm. With the baseline
/// hoisted, iteration 2's witness (one row) never equals the repeat's start
/// (empty), `should_stop_repeat_until` stays false because the card is in exile
/// and not in hand, and the loop never ends.
///
/// Why this shape matters rather than being a synthetic curiosity: it is the
/// LIVE shape Tainted Pact acquires the moment issue #8798 suppresses the
/// spurious prompt. That is precisely what the #8798 ordering constraint exists
/// to prevent, so the guard that prevents it must be pinned before then.
///
/// Same bounded `std::thread` + `recv_timeout` harness as the test above, and
/// the same residual: `recv_timeout` returning does not stop the spawned
/// thread. Under nextest's process-per-test isolation the leaked thread dies
/// with this test's own process; under plain `cargo test` it keeps spinning and
/// allocating until the binary exits.
#[test]
fn until_stop_conditions_with_a_tracked_non_pausing_body_terminates_after_progress() {
    let (tx, rx) = mpsc::channel();
    let _fixture = std::thread::spawn(move || {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let only_card = scenario
            .add_spell_to_library_top(P0, "Only Card", true)
            .id();
        let source = scenario
            .add_spell_to_graveyard(P0, "Tracked Repeat Source", true)
            .id();
        let mut runner = scenario.build();
        assert_eq!(
            runner.state().players[P0.0 as usize]
                .library
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![only_card],
            "precondition: exactly one card, so iteration 2 is the stalled one"
        );
        // The same comparison the `UntilStopConditions` loop itself makes to
        // decide whether an iteration paused.
        let initial_waiting_for = runner.state().waiting_for.clone();

        let mut ability = ResolvedAbility::new(
            Effect::ExileTop {
                player: TargetFilter::Controller,
                count: QuantityExpr::Fixed { value: 1 },
                position: LibraryPosition::Top,
                face_down: false,
            },
            vec![],
            source,
            P0,
        );
        // CR 607.1: the linked-exile consumer that makes
        // `should_track_exiled_by_source` true, so the exile ledgers — and
        // therefore the witness — actually grow on iteration 1. It reads the
        // linked pool without pausing and without moving anything out of exile.
        ability.sub_ability = Some(Box::new(ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::CardsExiledBySource,
                },
                player: TargetFilter::Controller,
            },
            vec![],
            source,
            P0,
        )));
        ability.repeat_until = Some(RepeatContinuation::UntilStopConditions {
            stop_on_put_to_hand: true,
            stop_on_duplicate_exiled_names: false,
        });

        let mut events = Vec::new();
        let ok = resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0).is_ok();
        let state = runner.state();
        let tracked_this_turn = state
            .cards_exiled_with_source_this_turn
            .get(&source)
            .map_or(0, Vec::len);
        let linked = state
            .exile_links
            .iter()
            .filter(|link| link.source_id == source)
            .count();
        let _ = tx.send((
            ok,
            events.len(),
            state.objects.get(&only_card).map(|obj| obj.zone),
            tracked_this_turn,
            linked,
            state.active_repeat_until().is_some(),
            state.waiting_for == initial_waiting_for,
        ));
    });

    let (ok, event_count, card_zone, tracked_this_turn, linked, frame_still_parked, never_paused) =
        rx.recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|err| {
                panic!(
                    "the tracked UntilStopConditions repeat never returned ({err:?}): the \
                     `progress_baseline` capture in `resolve_ability_chain`'s \
                     UntilStopConditions arm must happen INSIDE the loop, once per \
                     iteration. Hoisted above the loop it measures against the repeat's \
                     start, so an iteration that stalls AFTER making progress never \
                     compares equal and the loop never ends"
                )
            });

    assert!(ok, "the repeat must resolve cleanly, not error out");
    // Positive reach-guards: iteration 1 really exiled, and really TRACKED what
    // it exiled — without both, the witness is empty every iteration and this
    // test degenerates into the non-tracked sibling above.
    assert_eq!(
        card_zone,
        Some(Zone::Exile),
        "reach-guard: iteration 1 must actually exile the only card"
    );
    assert_eq!(
        (tracked_this_turn, linked),
        (1, 1),
        "reach-guard: the linked-exile consumer must make both ledgers record \
         the exiled card, so the witness genuinely GREW on iteration 1"
    );
    assert!(
        event_count < 64,
        "a terminating repeat emits a bounded event list, got {event_count}"
    );
    assert!(
        !frame_still_parked,
        "a non-pausing body must never park a repeat-until frame"
    );
    assert!(
        never_paused,
        "precondition: `waiting_for` never changed, so no iteration parked and \
         the in-loop arm — not the drain — is what ended the repeat"
    );
}
