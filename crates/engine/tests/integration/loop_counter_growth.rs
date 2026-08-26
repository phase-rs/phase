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
use engine::game::derived_views::{CounterMagnitude, CounterRowView, ObjectCounterDisplay};
use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::ability::AbilityKind;
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

/// A FREE, voluntarily-repeatable activation that creates a token AND grows a `+1/+1` counter.
///
/// BOTH CLAUSES ARE LOAD-BEARING, and the token one is not decoration. `apply_action`'s
/// `ActivateAbility` arm bootstraps `last_loop_action_sequence` ONLY when the activated ability
/// `creates_token` (or when a period for the same controller is already open); any other
/// activation CLEARS it. Mana activations arm it through the separate
/// `record_mana_loop_action_step` path. So a counter-only activation can never open a period, and
/// the CR 732.2a offer — which requires a non-empty sequence — is unreachable without a carrier.
/// The `+1/+1` growth therefore rides a token-creating activation, which is also a realistic
/// shape: a token engine whose creature grows as it works.
const PLUS1_TOKEN_ENGINE: &str =
    "{0}: Create a 1/1 colorless Servo artifact creature token. Put a +1/+1 counter on this creature.";

fn plus1_of(runner: &GameRunner, id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .and_then(|o| o.counters.get(&CounterType::Plus1Plus1))
        .copied()
        .unwrap_or(0)
}

/// Drives the `PLUS1_TOKEN_ENGINE` rider to a DECLARED, not-yet-accepted CR 732.2a offer.
///
/// A pure extraction of what was this file's single `+1/+1` fixture: setup, the activation
/// drive, both reach-guards, and `DeclareShortcut`. Two tests share it so the matched pair
/// below differs in exactly ONE line (the counter clear) — one authority for a 100-line
/// drive, because two copies drift.
///
/// THE WINDOW THIS RETURNS IN IS LOAD-BEARING. CR 732.2b: the declaration fans the offer out
/// to each other player in APNAP order, and CR 732.2c: the shortcut is taken only once the
/// LAST of them accepts. So at the returned instant the shortcut is DECLARED and OFFERED and
/// nothing has been materialized — which is the only window in which a board edit still
/// reaches the accept-time re-derivation. The `RespondToShortcut` assert pins that: with a
/// single living opponent the fan-out lands here, but a future one-opponent-less shape
/// (a conceded seat, a solo proposer) would take CR 732.2c's "nobody else to poll" branch
/// and materialize AT declaration — silently degrading the cleared test below from a `0 -> 1`
/// test into an `N -> N+1` test that still passes. The assert makes that loud.
fn drive_plus1_token_engine_to_declared_offer() -> (GameRunner, ObjectId) {
    use engine::analysis::decision_template::IterationCount;

    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(PlayerId(1), 20);
    let rider = scenario
        .add_creature_from_oracle(P0, "Test Plus One Token Engine", 2, 2, PLUS1_TOKEN_ENGINE)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;

    // THE DRIVING SHAPE — two constraints, both MEASURED by building the fixture that violates
    // them and watching it fail, not inferred from the code:
    //
    // 1. It must be an ACTIVATION, not a trigger cascade. `try_offer_object_growth_shortcut`
    //    requires a non-empty `last_loop_action_sequence` whose every step is
    //    `is_voluntarily_repeatable()` (CR 601.2a / CR 602.2 / CR 605.3a — casting, activating, and
    //    mana abilities are each a voluntary choice at priority; the helper's own annotation names
    //    all three). A trigger cascade drives itself and records no
    //    action sequence, so it reaches only the Path-C silent mark — which registers no backing
    //    set at all. The cascade version of this fixture grew its counters and then sat at
    //    `Priority` with no offer.
    // 2. The activation must CREATE A TOKEN. `apply_action`'s `ActivateAbility` arm opens a period
    //    only for a token-creating ability (or continues one already open for this controller);
    //    every other activation CLEARS the sequence. A `{0}: Put a +1/+1 counter on this creature.`
    //    version therefore also sat at `Priority` — each activation wiped the very sequence the
    //    offer needs. Mana activations arm it by a different path entirely
    //    (`record_mana_loop_action_step`).
    //
    // So the reachable production shape for a `+1/+1` ∞ display registration is a counter growth
    // riding a token-creating or mana-producing carrier. That is a real constraint on the class,
    // worth stating: it is why no such fixture existed to reuse.
    let ability_index = runner
        .state()
        .objects
        .get(&rider)
        .and_then(|o| {
            o.abilities
                .iter()
                .position(|def| def.kind == AbilityKind::Activated)
        })
        .expect("the {0} activated ability parsed onto the rider");

    let mut offered = false;
    let mut activations = 0usize;
    let mut halt = String::from("ran to the iteration cap");
    for _ in 0..40 {
        if matches!(runner.state().waiting_for, WaitingFor::LoopShortcut { .. }) {
            offered = true;
            break;
        }
        match runner.act(GameAction::ActivateAbility {
            source_id: rider,
            ability_index,
        }) {
            Ok(_) => activations += 1,
            Err(e) => {
                halt = format!(
                    "activation #{} refused: {e:?} (waiting_for {:?})",
                    activations + 1,
                    runner.state().waiting_for
                );
                break;
            }
        }
        // Settle the activation off the stack; stop early if the offer surfaces mid-settle.
        for _ in 0..60 {
            match &runner.state().waiting_for {
                WaitingFor::LoopShortcut { .. } => break,
                WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
                _ => {}
            }
            if let Err(e) = runner.act(GameAction::PassPriority) {
                halt = format!(
                    "settle after activation #{activations} stalled: {e:?} (waiting_for {:?})",
                    runner.state().waiting_for
                );
                break;
            }
        }
    }
    offered |= matches!(runner.state().waiting_for, WaitingFor::LoopShortcut { .. });

    // (1) REACH-GUARD: the engine really executed and really grew a `+1/+1` counter, so an empty
    // target set below means "the registration missed the class" and not "no loop happened".
    //
    // THRESHOLD IS ONE, deliberately, and not the `>= 2` the charge cascades above use. Those
    // fixtures need the BOARD to iterate because they are witnessing a Path-C mark that only
    // recurrence can produce. This one witnesses an OFFER, and the offer fires as soon as a single
    // period is recorded and the clone-drive confirms it recurs — the real board never iterates
    // twice. Measured: with `>= 2` this guard failed at `got 1 counters after 1 activation(s)`
    // while the offer had already surfaced, i.e. the guard was rejecting a working fixture.
    // The halt reason rides along because a stalled driver and a broken registration otherwise
    // fail identically.
    let grown = plus1_of(&runner, rider);
    assert!(
        grown >= 1,
        "reach-guard: the +1/+1 engine must actually run; got {grown} counters after \
         {activations} activation(s) — {halt}"
    );

    // (2) REACH-GUARD: a real offer surfaced, so the accept below drives production's
    // `materialize_object_growth_shortcut` rather than a grafted stash.
    assert!(
        offered,
        "reach-guard: the +1/+1 growth loop must raise a natural CR 732.2a offer, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(1),
            template: None,
        })
        .expect("P0 (proposer) declares the +1/+1 growth shortcut");

    // (RG-0) The offer must be PENDING on a living opponent's response, not already taken.
    // See this helper's doc: this is what pins "materialization happens at accept, not at
    // declaration" (CR 732.2b fan-out reached, CR 732.2c completion not yet reached).
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::RespondToShortcut { .. }
        ),
        "the declared shortcut must be pending a living opponent's response (CR 732.2b), so \
         nothing is materialized yet (CR 732.2c); got {:?}",
        runner.state().waiting_for
    );

    (runner, rider)
}

/// THE `∞` DISPLAY CHANNEL REGISTERS A `+1/+1` GROWTH (CR 122.1 + CR 732.2a).
///
/// WHY THIS FIXTURE HAD TO BE BUILT rather than reused: a census of every test touching
/// `unbounded_counter_targets` found that all of them grow `charge` — a `Generic` counter, which
/// registers IDENTICALLY under the old cover partition and the current beneficial one. So the
/// whole suite passed byte-for-byte with or without the display/collapse consolidation, and no
/// existing test could distinguish the change from its absence.
///
/// REACHABILITY, measured rather than assumed — a `+1/+1` loop is detected by a DIFFERENT
/// disjunct than a charge loop, and it matters: `CounterType::Plus1Plus1
/// ::is_monotone_loop_resource()` is `true`, so `project_out_resources` strips it and the frames
/// read EQUAL under `loop_states_equal_modulo_resources`. (A charge loop cannot do that —
/// `Generic` is preserved, which is exactly why `loop_states_cover_modulo_counter_growth` exists.)
/// So this loop arrives through the base equality disjunct, is offered, and its `+1/+1` growth is
/// materialized at the boundary by `counter_is_beneficial_materializable` — while the DISPLAY
/// registration, when it was partitioned by the `Generic`-only ω-cover rule, saw nothing. That
/// gap is the defect: a real loop whose collapse lands and whose pills never render `∞`.
///
/// THE REVERT-PROBE (the evidence, run and recorded): restore the display registration to the
/// `Generic`-only derivation — i.e. re-point it at a `grown_generic_counter_targets`-shaped
/// filter instead of projecting `growths` — and assertion (3) flips to an EMPTY target set. Every
/// other assertion here holds under that revert, which is what makes (3) the discriminator rather
/// than a bystander.
///
/// DIVISION OF LABOUR, stated so neither half is overread: this fixture is DERIVED state (a
/// scenario-built loop), so it proves the registration covers the `+1/+1` class end-to-end
/// through a real offer and accept. It does NOT carry production-dump provenance; that burden is
/// `kilo_live_offer_from_real_dump`'s, on a real 4p dump.
#[test]
fn plus_one_counter_growth_registers_its_infinity_display_target() {
    use engine::analysis::loop_check::ShortcutResponse;
    use engine::game::derived_views::derive_views;

    let (mut runner, rider) = drive_plus1_token_engine_to_declared_offer();

    while matches!(
        runner.state().waiting_for,
        WaitingFor::RespondToShortcut { .. }
    ) {
        runner
            .act(GameAction::RespondToShortcut {
                response: ShortcutResponse::Accept,
            })
            .expect("the opponent accepts");
    }

    // (3) THE ASSERTION — the discriminator. The accept registered the `+1/+1` pair as an `∞`
    // DISPLAY target. Under the `Generic`-only registration this set is EMPTY.
    let targets = runner
        .state()
        .unbounded_counter_targets
        .get(&P0)
        .cloned()
        .unwrap_or_default();
    assert!(
        targets.contains(&(rider, CounterType::Plus1Plus1)),
        "(3) the accept must register the +1/+1 pair as an ∞ display target — this is the \
         assertion the Generic-only display partition failed; got {targets:?}"
    );

    // (4) …and it reaches the WIRE as a pill, which is the user-visible half of (3). Asserted
    // separately because (3) could hold while the projection filtered it back out.
    //
    // (A-3) THE MATCHED POSITIVE for the cleared sibling below. The row carries this object's
    // LIVE count, and the helper's `grown >= 1` reach-guard makes that count NONZERO here — so a
    // projector that hardcoded `count: 0` (the way to make the sibling's A-1 pass vacuously) reds
    // exactly here. The pair is across two tests because a SelfRef-only pump
    // ("…on this creature") registers exactly one object, so a zero-count row and a nonzero-count
    // row cannot coexist in one frame.
    let live = plus1_of(&runner, rider);
    let views = derive_views(runner.state(), None);
    assert!(
        live >= 1,
        "(A-3) reach-guard: the un-cleared rider must carry counters, or the count assertion \
         below is vacuous; got {live}"
    );
    assert_eq!(
        views.counter_display.get(&rider),
        Some(&ObjectCounterDisplay {
            pills: vec![CounterRowView {
                counter: CounterType::Plus1Plus1,
                count: live,
                magnitude: CounterMagnitude::Unbounded,
            }],
            loyalty: None,
        }),
        "(4)/(A-3) the +1/+1 ∞ pill must reach the wire carrying the rider's LIVE count \
         ({live}), got {:?}",
        views.counter_display
    );
}

/// THE `0 -> 1` HALF OF THE PAIR — a registered pair the live object carries NONE of still
/// renders (CR 122.1 + CR 732.2a).
///
/// THE DEFECT THIS PINS. `unbounded_counter_targets` is derived by diffing a SIMULATED one-period
/// frame against a clone of the LIVE state (`game::engine::drive_one_period_frames` feeding
/// `analysis::resource::grown_beneficial_counter_deltas`, which admits a pair on `a > b` with
/// `b = counters.get(ct).unwrap_or(0)`). So a counter growing `0 -> 1` across that period is
/// registered while the live bearer carries none of it. While the channel published bare counter
/// TYPES, the display could only draw such a mark by finding a matching row in the object's own
/// `counters` map — there was none — so a real, accepted, registered `∞` rendered NOWHERE. That
/// is the subsystem's own polarity violated: it may leave an `∞` standing one boundary too long,
/// never hide a real one.
///
/// HOW THE `0` IS REACHED, and what is production vs. harness — stated rather than blurred.
/// The MATERIALIZATION is entirely production: a real offer, a real `DeclareShortcut`, a real
/// `RespondToShortcut::Accept` driving `materialize_object_growth_shortcut` ->
/// `current_period_counter_growth` -> `register_unbounded_counter_targets`. The PRECONDITION —
/// the bearer sitting at zero — is harness-injected, and deliberately so: this pump is
/// `PutCounter{this creature}`, so the only object it can ever reach is the rider itself, and the
/// rider was necessarily present for the period that recorded it. The state is nonetheless a real
/// one the engine can reach (`AbilityCost::RemoveCounter` and CR 122.3's `+1/+1`/`-1/-1`
/// annihilation both decrease a battlefield permanent's counters), and it is legal on its own
/// terms: a 2/2 base creature with no `+1/+1` counters is a 2/2, so no state-based action fires.
/// It is injected as a faithful stand-in for that class, not smuggled in as production shape.
///
/// WHY THE CLEAR CANNOT BE UNDONE BY THE ACCEPT. The accept consumes the proposal already latched
/// in `WaitingFor::RespondToShortcut` and re-derives against LIVE state, which is the point: with
/// the rider cleared, `before` has no `Plus1Plus1` entry and `after` has one, so the `a > b`
/// admission is reached by production code. And `materialize_object_growth_shortcut` only STASHES
/// the concrete finite growth (`register_pending_materialization`) — it is applied at the next
/// phase/step boundary — so the rider's live count is still `0` at `derive_views`. RG-1 and the
/// post-accept re-check below assert both halves rather than assuming them.
#[test]
fn plus_one_counter_growth_registers_a_target_the_bearer_does_not_yet_carry() {
    use engine::analysis::loop_check::ShortcutResponse;
    use engine::game::derived_views::{derive_views, ClientGameStateRef};

    let (mut runner, rider) = drive_plus1_token_engine_to_declared_offer();

    // THE ONE LINE that differs from the sibling above. Placed AFTER `DeclareShortcut` and BEFORE
    // the first Accept — the only window that matters, because CR 732.2b's fan-out has happened
    // (so declaration-time handling already saw an unmutated board) while CR 732.2c's completion
    // has not (so nothing is materialized yet). The helper's RG-0 assert pins that window.
    runner
        .state_mut()
        .objects
        .get_mut(&rider)
        .expect("the rider is still on the battlefield at the accept beat")
        .counters
        .remove(&CounterType::Plus1Plus1);

    // (RG-1) MANDATORY PRECONDITION — the only thing separating `0 -> 1` from `N -> N+1`. Nothing
    // else in this test reds if it is removed, which is exactly why it is asserted, not assumed.
    // NEVER weaken it to make the test pass: RG-1 *is* the proof the `0 -> 1` path was driven.
    assert_eq!(
        plus1_of(&runner, rider),
        0,
        "RG-1: the bearer must carry ZERO +1/+1 counters at the accept beat, or this fixture is \
         an N -> N+1 test wearing a 0 -> 1 label"
    );

    while matches!(
        runner.state().waiting_for,
        WaitingFor::RespondToShortcut { .. }
    ) {
        runner
            .act(GameAction::RespondToShortcut {
                response: ShortcutResponse::Accept,
            })
            .expect("the opponent accepts");
    }

    // (RG-3) The registration happened. Separates "registration missed the 0 -> 1 pair" from
    // "the projection dropped it" — without this, a red A-1 has two explanations.
    let targets = runner
        .state()
        .unbounded_counter_targets
        .get(&P0)
        .cloned()
        .unwrap_or_default();
    assert!(
        targets.contains(&(rider, CounterType::Plus1Plus1)),
        "RG-3: the accept must register the +1/+1 pair even though the bearer carries none of it \
         — this is production's `a > b` admission with `b == 0`; got {targets:?} (waiting_for \
         {:?})",
        runner.state().waiting_for
    );

    // Premise re-check: the stashed growth is applied at the next phase/step boundary, so the
    // bearer is STILL at zero here. If a boundary had already run, A-1 would fail with a nonzero
    // count and this names the reason.
    assert_eq!(
        plus1_of(&runner, rider),
        0,
        "the accepted growth must still be STASHED, not applied, at the derived-view seam"
    );

    // (A-1) THE DISCRIMINATOR. The row exists and carries `count: 0`. Under the old bare-type
    // shape this row was unrenderable; under a projector that filtered on
    // `objects[id].counters.contains_key(ct)` it would be absent entirely.
    let views = derive_views(runner.state(), None);
    assert_eq!(
        views.counter_display.get(&rider),
        Some(&ObjectCounterDisplay {
            pills: vec![CounterRowView {
                counter: CounterType::Plus1Plus1,
                count: 0,
                magnitude: CounterMagnitude::Unbounded,
            }],
            loyalty: None,
        }),
        "(A-1) a registered pair the bearer carries NONE of must still publish a renderable row \
         with `count: 0`, got {:?}",
        views.counter_display
    );

    // (A-2) …and it survives to the real adapter-visible envelope, not just the in-process view.
    let envelope = serde_json::to_value(ClientGameStateRef::wrap(runner.state(), None))
        .expect("the client envelope serializes");
    let rows = envelope
        .get("derived")
        .and_then(|d| d.get("counter_display"))
        .and_then(|c| c.get(rider.0.to_string()))
        .unwrap_or_else(|| panic!("(A-2) no wire rows for the bearer; envelope={envelope}"));
    // The `"P1P1"` key is the serde authority's spelling (`CounterType::as_str`), written as a
    // literal deliberately: this is the adapter-visible contract the TS mirror matches against,
    // so deriving it from the enum here would make the assertion agree with itself.
    assert_eq!(
        rows,
        &serde_json::json!({
            "pills": [{ "counter": "P1P1", "count": 0, "magnitude": "Unbounded" }]
        }),
        "(A-2) the wire row the frontend actually reads must carry `count: 0`"
    );

    // (A-4) RULES STATE STAYED CLEAN. The display row must NOT have been bought by writing a
    // `{plus1plus1: 0}` entry into the object — `GameObject::counters` sits inside `PartialEq`
    // and these envelopes round-trip into the `.json` dumps engine tests reload, so a phantom
    // zero entry would corrupt CR 104.4b / CR 732.2a loop equality: the very subsystem this fix
    // repairs. This is what pins DISPLAY-only.
    let wire_counters = envelope
        .get("state")
        .and_then(|s| s.get("objects"))
        .and_then(|o| o.get(rider.0.to_string()))
        .and_then(|o| o.get("counters"))
        .expect("(A-4) the bearer is on the serialized board");
    assert!(
        wire_counters.get("P1P1").is_none(),
        "(A-4) the display row must not materialize into rules state, got {wire_counters}"
    );
}

/// CROSS-SEAT DEDUPE — one `(object, counter)` pair registered by TWO seats projects ONE row,
/// and a DISTINCT pair on the same object survives that collapse.
///
/// THE DEFECT THIS PINS. `GameState::unbounded_counter_targets` is
/// `BTreeMap<PlayerId, BTreeSet<(ObjectId, CounterType)>>`: the `BTreeSet` dedupes WITHIN a seat
/// and nothing dedupes ACROSS seats. The projector iterated `.values()` and pushed every pair
/// unconditionally, so two controllers whose accepted loops pump the same pair emitted the pair
/// TWICE. Both rows are byte-identical — the count is read from `state.objects` keyed only by
/// `(id, ct)`, with no seat input — so this is a duplicate, never a "whose count wins" question.
/// Downstream, all five render sites (`board/PermanentCard`, `card/ArtCropCard`,
/// `card/CardPreview`, `controls/AttackTargetPicker`, `hud/DialogAttachmentCard`) key their pill
/// on the counter TYPE alone, so a duplicate row is two React
/// children sharing one key: undefined reconciliation plus a dev warning. The frontend cannot fix
/// this — deduping engine-published game state in the display layer is exactly what this codebase
/// forbids — so the collapse belongs here, at the seam that owns the row set.
///
/// WHY A DIRECTLY-CONSTRUCTED STATE IS HONEST HERE. Reachability is STRUCTURAL, not scenario-
/// dependent: `register_unbounded_counter_targets` is the store's single write authority and it
/// keys strictly by the winning controller (`game::engine::materialize_object_growth_shortcut`
/// passes `proposal.proposer`), so "two seats hold the same pair" is reached by two accepted
/// proposals in either order and carries no per-seat state beyond the key. Driving two full
/// concurrent loop accepts would exercise the offer machinery, not this projection. The
/// registrations below therefore go through that same production write authority; only the
/// scheduling around them is harness-built.
///
/// THE NEGATIVE CONTROL IS IN THIS FIXTURE, NOT A SIBLING. A dedupe is only half-tested by a
/// state where every seat holds the SAME pair: that pins "collapses enough" while leaving
/// "collapses too much" free, so narrowing the set key to `ObjectId` alone would red nothing.
/// One seat here therefore also holds a DISTINCT pair on the SAME object, which must survive
/// alongside the collapsed one. Both directions are live against one state: dropping the dedupe
/// yields three rows, narrowing the key to the object yields one, and the assertion below names
/// exactly two. It also pins the merged multi-seat ORDERING — rows arrive sorted by
/// `(ObjectId, CounterType)`, which nothing else asserts beyond a single seat.
#[test]
fn two_seats_collapse_the_shared_pair_and_keep_the_distinct_one() {
    use engine::game::derived_views::derive_views;
    use engine::game::zones::create_object;
    use engine::types::identifiers::CardId;
    use engine::types::zones::Zone;

    const P1: PlayerId = PlayerId(1);

    let mut state = GameState::new_two_player(42);
    let bearer = create_object(
        &mut state,
        CardId(1),
        P0,
        "Shared Bearer".to_string(),
        Zone::Battlefield,
    );
    // NONZERO and DISTINCT on purpose: the counts make each surviving row discriminating, so a
    // "dedupe" that dropped rows and re-invented one from thin air cannot pass, and a collapse
    // that kept the wrong one of the two pairs cannot pass either.
    let charge = CounterType::Generic("charge".to_string());
    let bearer_counters = &mut state
        .objects
        .get_mut(&bearer)
        .expect("the bearer is on the board")
        .counters;
    bearer_counters.insert(CounterType::Plus1Plus1, 3);
    bearer_counters.insert(charge.clone(), 7);

    // Both seats register the SAME pair, through the store's real single write authority; one of
    // them also registers a DISTINCT pair on the SAME object (the over-collapse control).
    state.register_unbounded_counter_targets(P0, vec![(bearer, CounterType::Plus1Plus1)]);
    state.register_unbounded_counter_targets(
        P1,
        vec![(bearer, CounterType::Plus1Plus1), (bearer, charge.clone())],
    );

    // REACH-GUARD (the positive control). Without this, a registration that silently dropped the
    // second seat would make the collapse assertion below pass vacuously — the shared pair would
    // be projecting one row from one entry, which was never in doubt.
    let seats: Vec<PlayerId> = state
        .unbounded_counter_targets
        .iter()
        .filter(|(_, pairs)| pairs.contains(&(bearer, CounterType::Plus1Plus1)))
        .map(|(seat, _)| *seat)
        .collect();
    assert_eq!(
        seats,
        vec![P0, P1],
        "reach-guard: BOTH seats must really hold the pair, or the dedupe below is untested"
    );
    // REACH-GUARD (the over-collapse control's own positive control). If the distinct pair never
    // landed in the store, "it survives the collapse" below would be asserting nothing.
    assert!(
        state.unbounded_counter_targets[&P1].contains(&(bearer, charge.clone())),
        "reach-guard: the distinct pair must really be stored, or the over-collapse control is \
         vacuous"
    );

    // THE ASSERTION. Exactly two rows: the shared pair collapsed to ONE (not two identical rows
    // sharing a React key), the distinct pair NOT collapsed away with it, both sorted by
    // `(ObjectId, CounterType)` — `Plus1Plus1` is declared before `Generic`, so it comes first.
    let views = derive_views(&state, None);
    assert_eq!(
        views.counter_display.get(&bearer),
        Some(&ObjectCounterDisplay {
            pills: vec![
                CounterRowView {
                    counter: CounterType::Plus1Plus1,
                    count: 3,
                    magnitude: CounterMagnitude::Unbounded,
                },
                CounterRowView {
                    counter: charge.clone(),
                    count: 7,
                    magnitude: CounterMagnitude::Unbounded,
                },
            ],
            loyalty: None,
        }),
        "the (object, counter) pair held by two seats must project ONE row carrying the live \
         count (duplicates collide on the counter-type React key at every render site), while a \
         DISTINCT pair on the same object must survive that collapse in `(ObjectId, CounterType)` \
         order. Got {:?}",
        views.counter_display
    );
}

/// CR 122.2 + CR 110.1 — THE `∞` ROW DIES WITH ITS BEARER WHILE THE STORE DOES NOT.
///
/// The widened projection is not battlefield-gated for FINITE rows (see
/// `derived_views`' `counter_rows_survive_a_bearer_that_keeps_its_counters_off_the_battlefield`),
/// so the question this fixture answers is the OTHER half: an ordinary bearer's counters cease to
/// exist when it changes zones (CR 122.2) and it stops being a permanent (CR 110.1), so NEITHER
/// magnitude may publish a row — even though the `∞` store still names the pair.
///
/// THE SPECIFIC REGRESSION THIS PINS. The counter pass must NOT gain an
/// `!accepted_axes.contains_key(..)` KEEP conjunct copied from the axis-row loop: that would make
/// the accepted-collapse SCHEDULE decide a row's EXISTENCE, which the mirror invariant in
/// `derive_views` forbids. Arm 3 reds if it does.
///
/// Arm ORDER matters. Arm 1 asserts a POPULATED row in the same run, so arm 3's emptiness is a
/// measured transition rather than a fixture that never had rows. Arm 2 separates "the projection
/// gated it" from "the store was wiped" — without it arm 3 has two explanations and proves
/// neither. Arm 4 separates "the `∞` gate fired" from "the finite pass would have emitted a row
/// and something else suppressed it": it proves `zones::move_to_zone`, the single authority, did
/// the clearing and the projection merely declined to invent rows.
#[test]
fn unbounded_counter_row_dies_with_its_bearer_but_the_store_does_not() {
    use engine::analysis::loop_check::ShortcutResponse;
    use engine::game::derived_views::derive_views;
    use engine::game::zones::move_to_zone;
    use engine::types::zones::Zone;

    let (mut runner, rider) = drive_plus1_token_engine_to_declared_offer();

    while matches!(
        runner.state().waiting_for,
        WaitingFor::RespondToShortcut { .. }
    ) {
        runner
            .act(GameAction::RespondToShortcut {
                response: ShortcutResponse::Accept,
            })
            .expect("the opponent accepts");
    }

    // (1) POSITIVE CONTROL — matched, and FIRST so a regression on the negative cannot skip it.
    let live = plus1_of(&runner, rider);
    assert!(
        live >= 1,
        "(1) reach-guard: the bearer must carry counters here, or the populated row below is \
         vacuous; got {live}"
    );
    assert_eq!(
        derive_views(runner.state(), None)
            .counter_display
            .get(&rider),
        Some(&ObjectCounterDisplay {
            pills: vec![CounterRowView {
                counter: CounterType::Plus1Plus1,
                count: live,
                magnitude: CounterMagnitude::Unbounded,
            }],
            loyalty: None,
        }),
        "(1) the accepted pair really publishes an ∞ row before the departure"
    );

    // (2) REACH-GUARD / THE DISCRIMINATOR: the departure happens through the production
    // chokepoint, and the STORE keeps the pair — only the projection filters.
    let mut events: Vec<GameEvent> = Vec::new();
    move_to_zone(runner.state_mut(), rider, Zone::Graveyard, &mut events);
    assert!(
        !runner.state().battlefield.contains(&rider),
        "(2) reach-guard: the departure really happened"
    );
    assert!(
        runner
            .state()
            .unbounded_counter_targets
            .get(&P0)
            .is_some_and(|pairs| pairs.contains(&(rider, CounterType::Plus1Plus1))),
        "(2) the STORE must still hold the departed pair — the CR 500.5 boundary collapse reads \
         it — so arm 3 can only be explained by the projection's gate, got {:?}",
        runner.state().unbounded_counter_targets
    );

    // (3) THE ANSWER.
    assert_eq!(
        derive_views(runner.state(), None)
            .counter_display
            .get(&rider),
        None,
        "(3) the bearer is no longer a permanent and its counters ceased to exist, so no row of \
         either magnitude may be published"
    );

    // (4) THE POLARITY GUARD.
    assert!(
        runner.state().objects[&rider].counters.is_empty(),
        "(4) `move_to_zone` — the single authority — is what cleared the counters; the \
         projection merely declined to invent rows. Got {:?}",
        runner.state().objects[&rider].counters
    );
}
