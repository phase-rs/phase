//! CR 113.6 — end-to-end board-delta fixtures for the `TriggerIndex` live-zone
//! guard, reproducing the two reported cards.
//!
//! The Locust God fired "Whenever you draw a card" from its owner's HAND, and
//! Lightning Rift fired "Whenever a player cycles a card" from a GRAVEYARD, in a
//! live four-player game. Both are ordinary battlefield triggers with no
//! `trigger_zones` opt-in, so under CR 113.6 they function only from the
//! battlefield. Two instances of one class, not two bugs.
//!
//! # Why this file is in-crate rather than under `tests/integration/`
//!
//! The stale-entry detector in `candidates_for_event` is gated
//! `#[cfg(all(debug_assertions, not(test)))]`. Integration binaries link the
//! engine WITHOUT `cfg(test)`, so the detector is live there and both negatives
//! below would panic before they could assert anything. In-crate `#[cfg(test)]`
//! modules compile with `cfg(test)` ON, so the panic is compiled out and the
//! board deltas are observable. This follows the established in-crate
//! scenario-test-module convention (`meld_tests`, `omnath_tests`,
//! `enters_with_unless_runtime_tests`) and adds no top-level test binary.
//!
//! # Every negative here is paired with a positive reach-guard
//!
//! A negative alone would be satisfied vacuously by a draw that silently failed
//! to happen. Each negative therefore has a sibling running the identical
//! scenario with the permanent left on the battlefield, asserting the trigger
//! DOES fire. Both are asserted on board deltas — token counts and stack
//! contents — never on AST or index internals.
//!
//! # Stability caveat, recorded deliberately
//!
//! These two negatives are stable only because `rebuild_from_battlefield`
//! iterates `battlefield_phased_in_ids`, which never reads `obj.zone`, so the
//! induced stale entry survives a rebuild. If a live-zone conjunct is ever added
//! there, a rebuild intervening before the consult would purge the induced entry
//! and both negatives would pass FOR THE WRONG REASON — and their positive
//! reach-guards would NOT catch it, because the positive still legitimately
//! fires. Re-verify then that these still fail with the guard reverted.

#![cfg(test)]

use crate::game::scenario::{GameRunner, GameScenario};
use crate::game::triggers::process_triggers;
use crate::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter};
use crate::types::events::GameEvent;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

const P0: PlayerId = PlayerId(0);

/// Verbatim Scryfall Oracle text. A paraphrase can take a different parser
/// branch and go green while the real card stays broken.
const LOCUST_GOD_ORACLE: &str = "Flying\n\
    Whenever you draw a card, create a 1/1 blue and red Insect creature token with flying and haste.\n\
    {2}{U}{R}: Draw a card, then discard a card.\n\
    When The Locust God dies, return it to its owner's hand at the beginning of the next end step.";

const LIGHTNING_RIFT_ORACLE: &str =
    "Whenever a player cycles a card, you may pay {1}. If you do, this enchantment deals 2 damage to any target.";

/// Count battlefield tokens — the board delta both Locust God fixtures read.
fn token_count(runner: &GameRunner) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            runner
                .state()
                .objects
                .get(id)
                .is_some_and(|obj| obj.is_token)
        })
        .collect::<Vec<_>>()
        .len()
}

/// The Locust God on P0's battlefield, indexed, with a card on top of library.
fn locust_god_runtime() -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    let locust = scenario
        .add_creature_from_oracle(P0, "The Locust God", 4, 4, LOCUST_GOD_ORACLE)
        .id();
    scenario.add_card_to_library_top(P0, "Some Card");
    (scenario.build(), locust)
}

/// Drive one draw for P0 through the production draw effect, then run the real
/// trigger pipeline over the events it emitted. No test-only seam: this is
/// `effects::draw::resolve`, the same function the resolver calls.
fn draw_one_and_process(runner: &mut GameRunner) {
    let ability = ResolvedAbility::new(
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
        vec![],
        ObjectId(100),
        P0,
    );
    let mut events: Vec<GameEvent> = Vec::new();
    crate::game::effects::draw::resolve(runner.state_mut(), &ability, &mut events)
        .expect("draw resolves");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, GameEvent::CardDrawn { .. })),
        "reach-guard: the draw must actually emit CardDrawn, or every assertion \
         downstream of it is vacuous"
    );
    process_triggers(runner.state_mut(), &events);
    // `process_triggers` only puts the trigger on the stack (CR 603.3). The
    // Insect is a board delta, so it exists only after the trigger RESOLVES.
    runner.advance_until_stack_empty();
}

/// POSITIVE REACH-GUARD for `locust_god_in_hand_does_not_create_insects_on_draw`.
/// Without this, a draw that silently failed to happen would satisfy the
/// negative vacuously.
#[test]
fn locust_god_on_battlefield_creates_one_insect_on_draw() {
    let (mut runner, _locust) = locust_god_runtime();
    let before = token_count(&runner);

    draw_one_and_process(&mut runner);

    assert_eq!(
        token_count(&runner) - before,
        1,
        "reach-guard: an on-battlefield Locust God must create exactly one Insect per draw"
    );
}

/// CR 113.6 — The Locust God's own third ability moves it battlefield →
/// graveyard → hand, which is exactly the transition sequence that stranded a
/// stale battlefield-membership record in the live game. With the live zone in
/// the hand, its draw trigger must not fire at all.
#[test]
fn locust_god_in_hand_does_not_create_insects_on_draw() {
    let (mut runner, locust) = locust_god_runtime();

    // Induce the desync directly: `state.battlefield` and the index are left
    // stale on purpose. `zones::move_to_zone` cannot be used — its CR 603.6c
    // hook would remove the index entry and there would be nothing to test.
    runner.state_mut().objects.get_mut(&locust).unwrap().zone = Zone::Hand;
    let before = token_count(&runner);

    draw_one_and_process(&mut runner);

    assert_eq!(
        token_count(&runner) - before,
        0,
        "CR 113.6: a Locust God whose live zone is the hand must create no Insects"
    );
}

/// POSITIVE REACH-GUARD for `lightning_rift_in_graveyard_does_not_trigger_on_cycle`.
#[test]
fn lightning_rift_on_battlefield_triggers_on_cycle() {
    let (mut runner, _rift, cycled) = lightning_rift_runtime();
    let before = runner.state().stack.len();

    cycle_and_process(&mut runner, cycled);

    assert!(
        runner.state().stack.len() > before,
        "reach-guard: an on-battlefield Lightning Rift must put its trigger on the stack"
    );
}

/// CR 113.6 — Lightning Rift fired from a graveyard in the live game. With the
/// live zone in the graveyard its cycling trigger must not reach the stack.
#[test]
fn lightning_rift_in_graveyard_does_not_trigger_on_cycle() {
    let (mut runner, rift, cycled) = lightning_rift_runtime();

    runner.state_mut().objects.get_mut(&rift).unwrap().zone = Zone::Graveyard;
    let before = runner.state().stack.len();

    cycle_and_process(&mut runner, cycled);

    assert_eq!(
        runner.state().stack.len(),
        before,
        "CR 113.6: a Lightning Rift whose live zone is the graveyard must not trigger"
    );
}

/// CR 603.10a — the guard must not eat look-back triggers. A dies-trigger's
/// source is, by definition, no longer on the battlefield when its own trigger
/// is collected, so a naive live-zone filter would silence every "when this
/// dies" ability in the engine.
///
/// It does not, because the look-back path never consults this index: the
/// departing permanent is dropped from the index at the mutation site by
/// `move_to_zone`'s CR 603.6c hook, and its own departure trigger is produced by
/// a dedicated block in `triggers.rs` fed from the `ZoneChangeRecord`. This
/// fixture drives the REAL zone-change pipeline and asserts the board delta, so
/// it fails if that separation is ever broken.
///
/// The four existing co-departure integration suites are the broader tripwire
/// for the same claim; this is the in-crate one.
#[test]
fn dies_trigger_still_fires_after_the_guard_lands() {
    let mut scenario = GameScenario::new();
    let dying = scenario
        .add_creature_from_oracle(
            P0,
            "Doomed Traveler",
            1,
            1,
            "When this creature dies, create a 1/1 white Spirit creature token with flying.",
        )
        .id();
    let mut runner = scenario.build();
    let before = token_count(&runner);

    // The real pipeline: this is what removes the object from the index.
    let mut events: Vec<GameEvent> = Vec::new();
    crate::game::zones::move_to_zone(runner.state_mut(), dying, Zone::Graveyard, &mut events);
    assert!(
        events.iter().any(|e| matches!(
            e,
            GameEvent::ZoneChanged {
                from: Some(Zone::Battlefield),
                ..
            }
        )),
        "reach-guard: the move must emit a battlefield-departure ZoneChanged"
    );
    process_triggers(runner.state_mut(), &events);
    runner.advance_until_stack_empty();

    assert_eq!(
        token_count(&runner) - before,
        1,
        "CR 603.10a: a dies-trigger must still fire even though its source's live \
         zone is the graveyard — look-back triggers do not come from the index"
    );
}

/// Lightning Rift on P0's battlefield, indexed, plus the card that gets cycled.
fn lightning_rift_runtime() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    let rift = scenario
        .add_enchantment_from_oracle(P0, "Lightning Rift", LIGHTNING_RIFT_ORACLE)
        .id();
    let cycled = scenario.add_card_to_library_top(P0, "Cycled Card");
    (scenario.build(), rift, cycled)
}

/// Emit the real `Cycled` event for P0 and run the trigger pipeline over it.
fn cycle_and_process(runner: &mut GameRunner, cycled: ObjectId) {
    let events = vec![GameEvent::Cycled {
        player_id: P0,
        object_id: cycled,
    }];
    process_triggers(runner.state_mut(), &events);
}
