//! PR-7 — live preserved-`Generic` counter-growth loop detection (Path C).
//!
//! Companion to `loop_shortcut.rs`'s B5 revocable-∞ tests. Covers the live
//! `interactive_loop_bridge` Path-C arm for a self-refilling OPTIONAL cascade that
//! grows a `Generic` charge counter each cycle (CR 122.1) — the axis
//! `loop_states_cover_modulo_counter_growth` was built for. Because the growing charge
//! is a PRESERVED counter, the constant-depth `loop_states_equal_modulo_resources`
//! disjunct FAILS on this fixture, so the Path-C mark can only land via the new
//! counter-growth disjunct: reverting that disjunct makes `drive_until_marked` time out
//! (the revert-failing assertion).
//!
//! The live proliferate loop (Pentad Prism cast + Kilo/Freed/Relic) is NOT sampled by
//! construction — a `ProliferateChoice` beat every cycle hits the sampler's ring-CLEAR
//! arm (see `loop_shortcut.rs` docs). That acceptance path is covered OFFLINE by
//! `drive_offline_pentad_prism` in `corpus_tests.rs`. This file uses the sampler-visible
//! shape: a self-refilling trigger cascade whose per-cycle charge-put resolves with no
//! prompt.

use engine::analysis::resource::{CounterClass, ResourceAxis};
use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, LoopDetectionMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);

/// A SINGLE self-refilling trigger that both grows a `Generic` charge counter and
/// re-gains life in ONE resolution. The trailing "You gain 1 life." re-triggers the
/// same ability (like `SELF_LIFE_ENGINE`), so the stack stays NON-SHRINKING across the
/// resolution — the shape the live loop-detect sampler records. A separate leaf
/// charge-put trigger would shrink the stack on resolution and hit the sampler's
/// ring-CLEAR arm, so the counter-put must ride the self-refilling resolution itself.
const CHARGE_LIFE_ENGINE: &str =
    "Whenever you gain life, put a charge counter on this creature. You gain 1 life.";
const KICKOFF: &str = "You gain 1 life.";

fn charge_of(runner: &GameRunner, id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .and_then(|o| o.counters.get(&CounterType::Generic("charge".to_string())))
        .copied()
        .unwrap_or(0)
}

/// 2-player OPTIONAL beneficial cascade controlled by P0 that grows a `Generic` charge
/// counter each cycle. One creature carries `CHARGE_LIFE_ENGINE` (a single self-refilling
/// trigger that puts a charge counter AND re-gains life in one resolution — the
/// sampler-visible non-shrinking shape). P1 holds a castable Bolt off an untapped Mountain
/// (a meaningful priority action) so the loop is OPTIONAL (`mandatory == false`) ⇒ Path C,
/// not the Path-B draw. Nobody loses life ⇒ Path A finds no faller. Returns runner +
/// (kickoff sorcery id, engine creature id — the charge-counter bearer).
fn setup_2p_optional_charge_growth(mode: LoopDetectionMode) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(PlayerId(1), 20);
    let engine_creature = scenario
        .add_creature_from_oracle(P0, "Test Charge Life Engine", 2, 2, CHARGE_LIFE_ENGINE)
        .id();
    scenario.add_basic_land(PlayerId(1), ManaColor::Red);
    scenario.add_bolt_to_hand(PlayerId(1));
    let kickoff = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Lifegain Kickoff", false, KICKOFF)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = mode;
    (runner, kickoff, engine_creature)
}

/// Drive `PassPriority`/`OrderTriggers` beats, collecting every emitted event, until
/// `controller`'s revocable-∞ capability is marked (Path C is a SILENT mark — it never
/// changes `waiting_for`, so callers poll `unbounded_resources` directly). Returns the
/// accumulated events and whether the mark landed.
fn drive_until_marked_collecting(
    runner: &mut GameRunner,
    controller: PlayerId,
    cap: usize,
) -> (Vec<GameEvent>, bool) {
    let mut events = Vec::new();
    let marked = |s: &GameState| s.unbounded_resources.contains_key(&controller);
    for _ in 0..cap {
        if marked(runner.state()) {
            return (events, true);
        }
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::OrderTriggers { triggers, .. } => GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            },
            _ => return (events, marked(runner.state())),
        };
        match runner.act(action) {
            Ok(r) => events.extend(r.events),
            Err(_) => return (events, marked(runner.state())),
        }
    }
    (events, marked(runner.state()))
}

/// PR-7 #6 (live Path-C, revert-failing): an OPTIONAL self-refilling cascade that grows a
/// `Generic` charge counter each cycle is marked as a revocable-∞ capability naming the
/// charge counter axis — and NEVER produces a `GameOver` (CR 104.4b: an optional loop is
/// not a draw; Path C is a silent mark).
///
/// REVERT-FAILING assertion (`marked`): the growing charge is a PRESERVED counter, so the
/// constant-depth `loop_states_equal_modulo_resources` Path-C disjunct FAILS on this
/// fixture (contrast `b5_optional_beneficial_marks_revocable_unbounded`, whose pure-life
/// loop marks via that equality disjunct). The mark can land ONLY via the new
/// `loop_states_cover_modulo_counter_growth` disjunct; reverting it makes the recurrence
/// gate fail and `drive_until_marked_collecting` returns `false`.
#[test]
fn live_optional_charge_growth_marks_counter_advantage_no_gameover() {
    let (mut runner, kickoff, rider) =
        setup_2p_optional_charge_growth(LoopDetectionMode::Interactive);
    let _ = runner.cast(kickoff).resolve();

    let (events, marked) = drive_until_marked_collecting(&mut runner, P0, 500);
    assert!(
        marked,
        "the optional charge-growth cascade must reach the Path-C revocable-∞ mark \
         (only reachable via loop_states_cover_modulo_counter_growth — the growing charge \
         breaks the constant-depth equality disjunct)"
    );

    // Non-vacuity reach-guard: the charge counter genuinely grew (≥2 ⇒ the CHARGE_RIDER
    // trigger parsed AND the loop ran multiple cycles), so the mark is not a degenerate
    // empty capability.
    let charge = charge_of(&runner, rider);
    assert!(
        charge >= 2,
        "reach-guard: the rider must have accrued ≥2 charge counters (loop actually ran); got {charge}"
    );

    // The marked capability names the charge counter axis (CounterClass::Other = a Generic
    // charge counter). This axis appears ONLY because the counter-growth disjunct fired.
    let axes = runner
        .state()
        .unbounded_resources
        .get(&P0)
        .cloned()
        .unwrap_or_default();
    assert!(
        axes.iter()
            .any(|a| matches!(a, ResourceAxis::Counter(CounterClass::Other, _))),
        "P0's revocable-∞ capability must include the Generic charge counter axis; got {axes:?}"
    );

    // Revocability bound: Path C is a silent mark — the game continues at live priority,
    // never a GameOver (neither waiting_for nor an emitted event).
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "an optional beneficial loop must fall through to live priority, not GameOver; got {:?}",
        runner.state().waiting_for
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, GameEvent::GameOver { .. })),
        "no GameOver event may be emitted for a revocable optional beneficial loop"
    );
    assert!(
        runner.state().players.iter().all(|p| !p.is_eliminated),
        "a no-loss beneficial loop eliminates no player"
    );
}

/// PR-7 #7 (#4603 OFF gate): under `LoopDetectionMode::Off` the SAME charge-growth
/// fixture never marks a revocable capability — the detector is fully dormant (the
/// sampler never records under Off), restoring exact pre-feature behavior. Paired with
/// #6 (Interactive marks), this proves the user-controllable toggle gates the feature.
#[test]
fn live_charge_growth_off_never_marks() {
    let (mut runner, kickoff, rider) = setup_2p_optional_charge_growth(LoopDetectionMode::Off);
    let _ = runner.cast(kickoff).resolve();

    // Drive a bounded number of beats; Off must never mark, and (being a beneficial
    // no-loss loop) must never reach a GameOver.
    let (events, marked) = drive_until_marked_collecting(&mut runner, P0, 500);
    assert!(
        !marked,
        "Off must never mark a revocable-∞ capability (Interactive-only, #4603)"
    );
    assert!(
        runner.state().unbounded_resources.is_empty(),
        "Off must leave unbounded_resources empty; got {:?}",
        runner.state().unbounded_resources
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, GameEvent::GameOver { .. })),
        "Off must not synthesize a GameOver for this beneficial loop"
    );

    // Reach-guard: the loop still physically ran under Off (charge grew) — so "never
    // marks" is attributable to the OFF gate, not to the loop failing to execute.
    let charge = charge_of(&runner, rider);
    assert!(
        charge >= 2,
        "reach-guard: the cascade must still run under Off (charge grew); got {charge}"
    );
}
