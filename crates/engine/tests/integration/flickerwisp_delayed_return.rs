//! Bug-triage issue #478 — Flickerwisp delayed `AtNextPhase{End}` return
//! trigger never fires at runtime.
//!
//! Flickerwisp: "When this creature enters, exile another target permanent.
//! Return that card to the battlefield under its owner's control at the
//! beginning of the next end step."
//!
//! The parser AST is verified-correct: the ETB `ChangesZone` trigger's
//! `execute` is `ChangeZone -> Exile` with a `CreateDelayedTrigger` sub-ability
//! whose condition is `AtNextPhase{End}` and whose effect is
//! `ChangeZone -> Battlefield(ParentTarget)`. The delayed-trigger primitive was
//! traced end-to-end and every isolated link works — yet the production cast
//! never returns the exiled permanent.
//!
//! This is a **discriminator** test. It drives the real Flickerwisp card
//! through the real `apply()` pipeline and has three ordered checkpoints; the
//! first one to fail localizes the bug to a single concrete fix (see
//! `.planning/bug-triage/issue-478/PLAN.md`):
//!   (a) after the ETB trigger resolves, the victim is in `Zone::Exile`.
//!   (b) before the End step, exactly one delayed trigger is installed and its
//!       `ability.targets` snapshot is `[Object(victim)]`.
//!   (c) after the End step's return trigger resolves, the victim is back on
//!       the battlefield under its owner's control.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;
use engine::types::game_state::CastPaymentMode;

/// Give P0 the mana to cast Flickerwisp ({1}{W}{W}).
fn add_flickerwisp_mana(runner: &mut engine::game::scenario::GameRunner) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool;
    for m in [ManaType::White, ManaType::White, ManaType::Colorless] {
        pool.add(ManaUnit::new(m, dummy, false, vec![]));
    }
}

#[test]
fn flickerwisp_delayed_return_fires_at_end_step() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P0 casts Flickerwisp; P1's creature is the exile victim. The `Another`
    // property on the ETB trigger's filter excludes Flickerwisp itself, and a
    // distinct-owner victim makes checkpoint (c)'s owner assertion meaningful.
    let flickerwisp = scenario.add_real_card(P0, "Flickerwisp", Zone::Hand, db);
    let victim = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);

    // Pad both libraries so neither player decks out while priority is passed
    // through the turn — keeps the game alive to reach the End step.
    for _ in 0..20 {
        scenario.add_real_card(P0, "Plains", Zone::Library, db);
        scenario.add_real_card(P1, "Plains", Zone::Library, db);
    }

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    add_flickerwisp_mana(&mut runner);

    // Cast Flickerwisp and let it resolve onto the battlefield.
    let card_id = runner.state().objects[&flickerwisp].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: flickerwisp,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Flickerwisp cast should be accepted");

    // Drive the cast through the real `apply()` pipeline. The ETB
    // `ChangesZone` trigger has a single `Another` permanent target; if it
    // surfaces an interactive `TriggerTargetSelection`, choose P1's creature,
    // otherwise let the engine assign the only legal target. Pass priority
    // until the ETB trigger has resolved — observed by the delayed trigger
    // being installed.
    let mut guard = 0;
    while runner.state().delayed_triggers.is_empty() {
        guard += 1;
        assert!(
            guard < 64,
            "Flickerwisp's ETB trigger never resolved (no delayed trigger \
             installed); last waiting_for = {:?}",
            runner.state().waiting_for
        );
        match &runner.state().waiting_for {
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(victim)),
                    })
                    .expect("ETB trigger should accept the chosen permanent target");
            }
            _ => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass while resolving the ETB trigger should succeed");
            }
        }
    }

    // Checkpoint (a): the targeted permanent is exiled.
    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Exile,
        "checkpoint (a): Flickerwisp's ETB trigger must exile the targeted \
         permanent"
    );

    // Checkpoint (b): exactly one delayed trigger installed, snapshotting the
    // exiled victim — NOT empty, NOT Flickerwisp itself.
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "checkpoint (b): exactly one AtNextPhase{{End}} delayed trigger must be \
         installed after the ETB trigger resolves"
    );
    assert_eq!(
        runner.state().delayed_triggers[0].ability.targets,
        vec![TargetRef::Object(victim)],
        "checkpoint (b): the delayed trigger must snapshot the exiled victim as \
         its target (CR 603.7c) — not empty, not Flickerwisp itself"
    );

    // Advance toward P0's End step by passing priority — this MUST exercise
    // `pass_priority_once_with_pipeline` so Suspect B's auto-advance-only path
    // is correctly distinguished. No raw `state.phase =` jump. The
    // `AtNextPhase{End}` delayed trigger fires when the End step is entered;
    // pass priority until it is consumed (and any return trigger it pushed has
    // resolved off the stack).
    let mut guard = 0;
    while !runner.state().delayed_triggers.is_empty() || !runner.state().stack.is_empty() {
        guard += 1;
        assert!(
            guard < 256,
            "the AtNextPhase{{End}} delayed trigger never fired/resolved; \
             phase = {:?}, waiting_for = {:?}, dt = {}, stack = {}",
            runner.state().phase,
            runner.state().waiting_for,
            runner.state().delayed_triggers.len(),
            runner.state().stack.len(),
        );
        // The End step's delayed return trigger carries a `ParentTarget`
        // snapshot and needs no fresh target choice — passing priority both
        // advances phases and resolves the trigger off the stack.
        if runner.act(GameAction::PassPriority).is_err() {
            panic!(
                "priority pass stalled before the delayed trigger fired; \
                 phase = {:?}, dt = {}",
                runner.state().phase,
                runner.state().delayed_triggers.len(),
            );
        }
    }

    // Checkpoint (c): the victim is back on the battlefield under its owner's
    // (P1's) control, and the delayed trigger has been consumed.
    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Battlefield,
        "checkpoint (c): the exiled permanent must return to the battlefield at \
         the next end step"
    );
    assert_eq!(
        runner.state().objects[&victim].controller,
        P1,
        "checkpoint (c): the returned permanent must be under its owner's control"
    );
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "checkpoint (c): the one-shot delayed trigger must be consumed after firing"
    );
}

/// Bug-triage issue #485 — CR 603.7c discriminator.
///
/// A delayed `AtNextPhase{End}` return snapshots the exiled victim's `ObjectId`
/// but, per CR 603.7c, must only return it if it is *still in the zone it was
/// expected to be in* (Exile). If the victim leaves Exile before the End step
/// (here: Exile -> Graveyard via the real zone-move pipeline), the delayed
/// trigger still fires once and is consumed (CR 603.7b) but resolves to a no-op
/// move — it must NOT drag the victim onto the battlefield.
///
/// This is discriminating: with the parser-side `origin` stamp reverted the
/// delayed `ChangeZone` carries `origin: None`, the resolver's CR 400.7 guard is
/// skipped, and the victim is wrongly moved to the battlefield — the assertions
/// below fail. With the fix (`origin: Some(Exile)`) the guard fires and the
/// victim stays in the graveyard.
#[test]
fn flickerwisp_delayed_return_skipped_when_victim_left_exile() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let flickerwisp = scenario.add_real_card(P0, "Flickerwisp", Zone::Hand, db);
    let victim = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);

    for _ in 0..20 {
        scenario.add_real_card(P0, "Plains", Zone::Library, db);
        scenario.add_real_card(P1, "Plains", Zone::Library, db);
    }

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    add_flickerwisp_mana(&mut runner);

    let card_id = runner.state().objects[&flickerwisp].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: flickerwisp,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Flickerwisp cast should be accepted");

    // Drive the ETB trigger to resolution so the victim is exiled and the
    // delayed trigger is installed (same drive loop as the happy-path test).
    let mut guard = 0;
    while runner.state().delayed_triggers.is_empty() {
        guard += 1;
        assert!(
            guard < 64,
            "Flickerwisp's ETB trigger never resolved; last waiting_for = {:?}",
            runner.state().waiting_for
        );
        match &runner.state().waiting_for {
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(victim)),
                    })
                    .expect("ETB trigger should accept the chosen permanent target");
            }
            _ => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass while resolving the ETB trigger should succeed");
            }
        }
    }

    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Exile,
        "precondition: the targeted permanent must be exiled"
    );

    // Discriminating mutation: drive the victim OUT of Exile before the End
    // step, through the real zone-move pipeline.
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), victim, Zone::Graveyard, &mut events);
    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Graveyard,
        "precondition: the victim must have left Exile for the graveyard"
    );

    // Pass priority past P0's End step. The one-shot delayed trigger still fires
    // and is consumed (CR 603.7b) but resolves to a no-op move (CR 603.7c).
    let mut guard = 0;
    while !runner.state().delayed_triggers.is_empty() || !runner.state().stack.is_empty() {
        guard += 1;
        assert!(
            guard < 256,
            "the AtNextPhase{{End}} delayed trigger never fired/resolved; \
             phase = {:?}, dt = {}, stack = {}",
            runner.state().phase,
            runner.state().delayed_triggers.len(),
            runner.state().stack.len(),
        );
        if runner.act(GameAction::PassPriority).is_err() {
            panic!(
                "priority pass stalled before the delayed trigger fired; phase = {:?}",
                runner.state().phase,
            );
        }
    }

    // CR 603.7c: the victim left its expected zone (Exile) — the delayed return
    // must not move it. It stays in the graveyard.
    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Graveyard,
        "CR 603.7c: a victim that left Exile must NOT be dragged to the \
         battlefield by the delayed return"
    );
    assert_ne!(
        runner.state().objects[&victim].zone,
        Zone::Battlefield,
        "CR 603.7c: explicit negative — the victim must not be on the battlefield"
    );
    // CR 603.7b: the one-shot delayed trigger is consumed even though it moved
    // nothing.
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "CR 603.7b: the one-shot delayed trigger must be consumed even when it \
         affects nothing"
    );
}
