//! Issue #7197 (Moraug, Fury of Akoum): the landfall turn must grant a USABLE
//! additional combat phase — a full combat with its own DeclareAttackers step —
//! and "at the beginning of that combat, untap all creatures you control" must
//! untap attackers from an earlier combat so they can attack again.
//!
//! Oracle text verified against the card (see also the `that_combat` parser
//! test in `oracle_effect/lower.rs`, which quotes it verbatim):
//! "Each creature you control gets +1/+0 for each time it has attacked this
//! turn. Landfall — Whenever a land you control enters, if it's your main
//! phase, there's an additional combat phase after this phase. At the
//! beginning of that combat, untap all creatures you control."
//!
//! The pinned v0.86.0 evidence for #7197 shows the defect shape these tests
//! guard: an extra `BeginCombat` that collapses straight to `EndCombat`
//! (an "empty combat shell") with no usable declaration. On current main the
//! precombat-landfall shell in the evidence log is correct behavior (the lone
//! attacker taps in the extra combat, so the regular combat legitimately has
//! nothing to declare); the defect these tests pin is an extra combat that
//! stays empty while creatures the trigger should have untapped sit tapped.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::EffectKind;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::{Phase, PhaseGroup, TurnSegment};

const MORAUG: &str = "Each creature you control gets +1/+0 for each time it has attacked this turn.
Landfall — Whenever a land you control enters, if it's your main phase, there's an additional combat phase after this phase. At the beginning of that combat, untap all creatures you control.";

/// Drives until `done`, attacking with `attackers` at EVERY surfaced
/// DeclareAttackers window. Returns (phases entered, surfaced windows seen).
/// A combat whose declaration is auto-skipped (no legal attackers) surfaces no
/// window — so the window count is the usability signal, not the phase list.
fn drive_attacking(
    runner: &mut GameRunner,
    attackers: &[ObjectId],
    done: impl Fn(&GameRunner) -> bool,
) -> (Vec<Phase>, usize) {
    let mut entered = Vec::new();
    let mut windows = 0;
    for _ in 0..400 {
        if done(runner) {
            return (entered, windows);
        }
        let action = match runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => {
                windows += 1;
                GameAction::DeclareAttackers {
                    attacks: attackers
                        .iter()
                        .map(|id| (*id, AttackTarget::Player(P1)))
                        .collect(),
                    bands: vec![],
                }
            }
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            _ => GameAction::PassPriority,
        };
        let result = runner.act(action).expect("the turn advances");
        entered.extend(result.events.iter().filter_map(|event| match event {
            GameEvent::PhaseChanged { phase } => Some(*phase),
            _ => None,
        }));
    }
    panic!("the stop condition was never reached; entered {entered:?}");
}

/// Plays `land` from P0's hand and resolves the landfall trigger, returning
/// the resolution events. Panics unless the stack fully drains.
fn play_land_and_resolve(runner: &mut GameRunner, land: ObjectId) -> Vec<GameEvent> {
    let card_id = runner.state().objects[&land].card_id;
    let mut events = runner
        .act(GameAction::PlayLand {
            object_id: land,
            card_id,
        })
        .expect("the land is playable")
        .events;
    for _ in 0..8 {
        if runner.state().stack.is_empty() {
            break;
        }
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("the trigger resolves")
                .events,
        );
    }
    assert!(
        runner.state().stack.is_empty(),
        "the landfall trigger resolved"
    );
    events
}

fn resolved_additional_phase(events: &[GameEvent]) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::AdditionalPhase,
                ..
            }
        )
    })
}

fn is_combat_full(entered: &[Phase]) -> bool {
    [
        Phase::DeclareAttackers,
        Phase::DeclareBlockers,
        Phase::CombatDamage,
    ]
    .iter()
    .all(|step| entered.contains(step))
}

/// CR 500.8 + CR 603.7a: landfall resolving in the postcombat main phase adds
/// a combat after it, and "at the beginning of that combat, untap all
/// creatures you control" untaps the tapped attackers so the added combat is
/// a full usable combat — not the empty BeginCombat → EndCombat shell from
/// the #7197 evidence. The tapped-after-regular-combat assertion is the
/// non-vacuity guard: without it the untap would have nothing to do and the
/// test would pass even with the delayed trigger deleted.
#[test]
fn moraug_postcombat_landfall_untaps_attackers_for_a_full_added_combat() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // CR 704.5a: with Moraug's +1/+0-per-attack bonus the two combats deal
    // 10 then 12 damage, which would kill a 20-life defender before the End
    // phase and end the game mid-fixture. Headroom keeps the usability
    // assertions (not life totals) under test.
    scenario.with_life(P1, 40);
    let moraug = scenario
        .add_creature_from_oracle(P0, "Moraug, Fury of Akoum", 6, 6, MORAUG)
        .id();
    let bears = scenario.add_creature(P0, "Bears", 2, 2).id();
    let land = scenario.add_land_to_hand(P0, "Wastes").id();
    let mut runner = scenario.build();

    // Regular combat first: attack with both, so both tap.
    let (regular_entered, regular_windows) = drive_attacking(&mut runner, &[moraug, bears], |r| {
        r.state().phase == Phase::PostCombatMain && r.state().stack.is_empty()
    });
    assert_eq!(
        regular_windows, 1,
        "the regular combat surfaced its declaration"
    );
    assert!(
        is_combat_full(&regular_entered),
        "the regular combat ran all its steps; entered {regular_entered:?}"
    );
    assert!(
        runner.state().objects[&moraug].tapped && runner.state().objects[&bears].tapped,
        "reach: both attackers tapped in the regular combat, so the added \
         combat's untap has work to do"
    );

    // Landfall in the postcombat main phase: the added combat follows it.
    let events = play_land_and_resolve(&mut runner, land);
    assert!(
        resolved_additional_phase(&events),
        "the landfall trigger resolved its additional phase"
    );
    assert!(
        runner.state().extra_phases.iter().any(|entry| {
            entry.anchor == Phase::PostCombatMain
                && entry.segment == TurnSegment::Phase(PhaseGroup::Combat)
        }),
        "the added combat follows the postcombat main phase"
    );

    // The added combat must untap both attackers and run full steps.
    let (added_entered, added_windows) = drive_attacking(&mut runner, &[moraug, bears], |r| {
        r.state().phase == Phase::End
    });
    assert_eq!(
        added_windows, 1,
        "the added combat surfaced its declaration instead of collapsing to \
         an empty shell; entered {added_entered:?}"
    );
    assert!(
        is_combat_full(&added_entered),
        "the added combat ran all its steps; entered {added_entered:?}"
    );
    assert!(
        runner.state().objects[&moraug].tapped && runner.state().objects[&bears].tapped,
        "both creatures attacked again in the added combat"
    );
    assert!(runner.state().extra_phases.is_empty());
    assert!(runner.state().extra_phase_resume.is_empty());
}

/// CR 500.8: landfall resolving in the precombat main phase adds its combat
/// before the regular one. Holding one creature back proves both combats are
/// independently usable: the first window spends Moraug, the second still
/// opens for the fresh creature. (With a single tapper the second combat is
/// legitimately empty — that is the evidence log's shape under correct
/// behavior, not a defect — so this test brings two attackers.)
#[test]
fn moraug_precombat_landfall_both_combats_declare_with_fresh_attackers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let moraug = scenario
        .add_creature_from_oracle(P0, "Moraug, Fury of Akoum", 6, 6, MORAUG)
        .id();
    let bears = scenario.add_creature(P0, "Bears", 2, 2).id();
    let land = scenario.add_land_to_hand(P0, "Wastes").id();
    let mut runner = scenario.build();

    let events = play_land_and_resolve(&mut runner, land);
    assert!(
        resolved_additional_phase(&events),
        "the landfall trigger resolved its additional phase"
    );

    // First window: attack with Moraug only, holding Bears back.
    let mut windows = 0;
    let mut entered = Vec::new();
    for _ in 0..400 {
        if runner.state().phase == Phase::End {
            break;
        }
        let action = match runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => {
                windows += 1;
                let attackers = if windows == 1 {
                    &[moraug][..]
                } else {
                    &[bears][..]
                };
                GameAction::DeclareAttackers {
                    attacks: attackers
                        .iter()
                        .map(|id| (*id, AttackTarget::Player(P1)))
                        .collect(),
                    bands: vec![],
                }
            }
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            _ => GameAction::PassPriority,
        };
        let result = runner.act(action).expect("the turn advances");
        entered.extend(result.events.iter().filter_map(|event| match event {
            GameEvent::PhaseChanged { phase } => Some(*phase),
            _ => None,
        }));
    }
    assert_eq!(
        windows, 2,
        "both combats surfaced a declaration (Moraug first, Bears second); \
         entered {entered:?}"
    );
    assert_eq!(
        runner
            .state()
            .steps_started_this_turn
            .count(Phase::BeginCombat),
        2
    );
    assert!(
        runner.state().objects[&moraug].tapped && runner.state().objects[&bears].tapped,
        "each creature attacked in its combat"
    );
    assert!(runner.state().extra_phases.is_empty());
    assert!(runner.state().extra_phase_resume.is_empty());
}
