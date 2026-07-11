//! Regression for **Mechtitan Core** (no linked GitHub issue) —
//!
//! > {5}, Exile this Vehicle and four other artifact creatures and/or Vehicles
//! > you control: Create Mechtitan, a legendary 10/10 Construct artifact creature
//! > token … When that token leaves the battlefield, return all cards exiled with
//! > this Vehicle except this card to the battlefield tapped under their owners'
//! > control.
//!
//! At runtime the activated ability exiles the 4 others with `TrackedBySource`
//! links (`source_id = Mechtitan Core`) — the self-exiled Core is NOT self-linked,
//! so "except this card" falls out for free — and installs a delayed trigger
//! `WhenLeavesPlayFiltered{token} → ChangeZoneAll{ Exile→Battlefield,
//! ExiledBySource, enter_tapped: Tapped }` whose `source_id` is Core.
//!
//! THE BUG: Core self-exiles paying its own cost. Before the fix,
//! `apply_zone_exit_cleanup` pruned every `TrackedBySource` link keyed to Core the
//! instant Core left the battlefield, and the only fallback (`linked_exile_lki`)
//! is cleared on every phase/step transition. So when the token left many turns
//! later, `linked_exile_cards_for_source(Core)` returned `[]` and NOTHING came
//! back. This test reproduces that exact runtime shape (source self-exiles → a
//! phase passes → the token leaves) and asserts the pile returns TAPPED.
//!
//! THE FIX (`game/zones.rs`): preserve `TrackedBySource` links when a source
//! leaves the battlefield TO EXILE (CR 607.2a: the pile stays the linked-ability
//! referent while the source sits in exile with a stable ObjectId per CR 400.7),
//! and reset them only if that source RE-ENTERS the battlefield.
//!
//! DISCRIMINATOR: revert the exit-prune change → Core's self-exile drops the
//! links → the `== Battlefield` / `tapped` assertions flip to "still in Exile".
//!
//! CR references (verified against `docs/MagicCompRules.txt`):
//!   - CR 607.2a: an "exile …" ability and a "cards exiled with [this object]"
//!     ability are linked; the second refers to the exile-zone cards the first put
//!     there — the association does not depend on where the source now is.
//!   - CR 400.7: an object that changes zones becomes a new object; ObjectId is
//!     stable storage, so the self-exiled Core keeps its id in exile.
//!   - CR 614.12: an effect that modifies how a permanent enters the battlefield
//!     (here, tapped) — the mass return honors `enter_tapped`.

use engine::game::players::linked_exile_cards_for_source;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::check_delayed_triggers;
use engine::game::zones::{create_object, move_to_zone};
use engine::types::ability::{DelayedTriggerCondition, Effect, ResolvedAbility, TargetFilter};
use engine::types::game_state::{DelayedTrigger, ExileLink, ExileLinkKind, GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::zones::{EtbTapState, Zone};

/// Build the delayed trigger the parser produces for Mechtitan Core, bound to a
/// concrete `token` (condition) and `core` (effect source). Mirrors the stored
/// AST observed in a real game-state export.
fn install_mechtitan_return_trigger(state: &mut GameState, core: ObjectId, token: ObjectId) {
    let ability = ResolvedAbility::new(
        Effect::ChangeZoneAll {
            origin: Some(Zone::Exile),
            destination: Zone::Battlefield,
            target: TargetFilter::ExiledBySource,
            enters_under: None,
            enter_tapped: EtbTapState::Tapped,
            enter_with_counters: vec![],
            face_down_profile: None,
            library_position: None,
            random_order: false,
        },
        vec![],
        core,
        P0,
    );
    state.delayed_triggers.push(DelayedTrigger {
        condition: DelayedTriggerCondition::WhenLeavesPlayFiltered {
            filter: TargetFilter::SpecificObject { id: token },
        },
        ability,
        controller: P0,
        source_id: core,
        one_shot: true,
    });
}

/// Drive priority passes until the stack drains so the fired delayed trigger's
/// mass return resolves onto the battlefield.
fn drain_stack(runner: &mut GameRunner) {
    for _ in 0..200 {
        if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
            engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            continue;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            _ => {
                if runner
                    .act(engine::types::actions::GameAction::PassPriority)
                    .is_err()
                {
                    break;
                }
            }
        }
    }
}

#[test]
fn mechtitan_core_returns_exiled_pile_tapped_after_token_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Mechtitan Core on the battlefield (the exile source / delayed-trigger source).
    let core = scenario.add_creature(P0, "Mechtitan Core", 2, 4).id();
    // The Mechtitan token on the battlefield.
    let token = scenario.add_creature(P0, "Mechtitan", 10, 10).id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    let state = runner.state_mut();

    // The four "other" cards exiled with Core (two per owner — proves per-owner
    // routing on the return). All carry `TrackedBySource` links keyed to Core,
    // exactly as the activation cost's `EffectZoneChoice` produces them.
    let p0_a = create_object(state, CardId(900), P0, "Ally A".to_string(), Zone::Exile);
    let p0_b = create_object(state, CardId(901), P0, "Ally B".to_string(), Zone::Exile);
    let p1_a = create_object(state, CardId(902), P1, "Foe A".to_string(), Zone::Exile);
    let p1_b = create_object(state, CardId(903), P1, "Foe B".to_string(), Zone::Exile);
    let pile = [p0_a, p0_b, p1_a, p1_b];
    for exiled in pile {
        state.exile_links.push(ExileLink {
            exiled_id: exiled,
            source_id: core,
            kind: ExileLinkKind::TrackedBySource,
        });
    }
    // A control card exiled by a DIFFERENT source must never come back.
    let unrelated = create_object(state, CardId(904), P0, "Unrelated".to_string(), Zone::Exile);
    let other_source = create_object(
        state,
        CardId(905),
        P0,
        "Other Source".to_string(),
        Zone::Battlefield,
    );
    state.exile_links.push(ExileLink {
        exiled_id: unrelated,
        source_id: other_source,
        kind: ExileLinkKind::TrackedBySource,
    });

    install_mechtitan_return_trigger(state, core, token);

    // 1) Core self-exiles paying its own cost (battlefield -> exile). The fix must
    //    preserve Core's TrackedBySource links here.
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), core, Zone::Exile, &mut events);
    assert_eq!(
        runner.state().objects[&core].zone,
        Zone::Exile,
        "precondition: Core self-exiled"
    );
    // DISCRIMINATOR (revert the exit-prune → this is empty): the linked pile
    // survives Core's departure and is still resolvable from exile.
    assert_eq!(
        linked_exile_cards_for_source(runner.state(), core).len(),
        4,
        "all four exiled-with-Core cards must survive Core's self-exile (CR 607.2a)"
    );

    // 2) A phase passes — proves the (turn-scoped) linked_exile_lki fallback is
    //    irrelevant; durability comes from the preserved live links.
    let mut phase_events = Vec::new();
    engine::game::turns::advance_phase(runner.state_mut(), &mut phase_events);

    // 3) The token leaves the battlefield → fire the delayed return trigger.
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), token, Zone::Graveyard, &mut events);
    check_delayed_triggers(runner.state_mut(), &events);
    drain_stack(&mut runner);

    // DISCRIMINATORS: the four exiled cards return to the battlefield, TAPPED,
    // under their own owners' control; Core (this card) stays in exile.
    for exiled in pile {
        assert_eq!(
            runner.state().objects[&exiled].zone,
            Zone::Battlefield,
            "exiled-with-Core card {exiled:?} must return to the battlefield"
        );
        assert!(
            runner.state().objects[&exiled].tapped,
            "returned card {exiled:?} must enter tapped"
        );
    }
    assert_eq!(runner.state().objects[&p0_a].controller, P0);
    assert_eq!(runner.state().objects[&p0_b].controller, P0);
    assert_eq!(
        runner.state().objects[&p1_a].controller,
        P1,
        "return is under each card's OWNER's control"
    );
    assert_eq!(runner.state().objects[&p1_b].controller, P1);

    // "except this card": Core itself is not linked to itself, so it stays exiled.
    assert_eq!(
        runner.state().objects[&core].zone,
        Zone::Exile,
        "Mechtitan Core itself must NOT return (\"except this card\")"
    );
    // NEGATIVE: a card exiled by a different source is untouched.
    assert_eq!(
        runner.state().objects[&unrelated].zone,
        Zone::Exile,
        "a card exiled by a different source must not be returned"
    );
}
