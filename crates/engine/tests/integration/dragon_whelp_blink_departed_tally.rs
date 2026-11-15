//! F3: the announced activation tally survives the source's zone exit.
//!
//! CR 400.7 + CR 608.2h: `apply_zone_exit_cleanup` migrates the erased live
//! journal entries into the departed-side tables tagged with the departing
//! incarnation, and the `Activated` tally arm of `evaluate_condition` falls
//! back to the departed turn tally on a live miss. A resolving ability
//! announced by the old object therefore still reads its count, while the
//! returned object's live journals start at zero.
//!
//! Verbatim Oracle text (Scryfall, verified 2026-09-09): Dragon Whelp —
//! "Flying\n{R}: This creature gets +1/+0 until end of turn. If this ability
//! has been activated four or more times this turn, sacrifice this creature
//! at the beginning of the next end step." Cloudshift — "Exile target
//! creature you control, then return that card to the battlefield under your
//! control." (CLOUDSHIFT const below).
//!
//! Revert detectors: reverting the migration (or the condition fallback)
//! returns tally 0 for the post-blink resolution, so no delayed trigger is
//! created and every `delayed_triggers` non-emptiness assertion below flips.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::{AbilityUsageKey, CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

// Verbatim Oracle text (Scryfall, verified 2026-09-09). Nalathni Dragon,
// Farrelite Priest and Initiates of the Ebon Hand print the same second
// sentence; only the first clause differs.
const DRAGON_WHELP_ORACLE: &str = "Flying\n{R}: This creature gets +1/+0 until end of turn. If this ability has been activated four or more times this turn, sacrifice this creature at the beginning of the next end step.";
const CLOUDSHIFT: &str =
    "Exile target creature you control, then return that card to the battlefield under your control.";

fn red_mana(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
        .collect()
}

fn whelp_board(red: usize, cloudshifts: usize) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, red_mana(red));
    let whelp = scenario
        .add_creature_from_oracle(P0, "Dragon Whelp", 2, 3, DRAGON_WHELP_ORACLE)
        .id();
    let mut shifts = Vec::new();
    for _ in 0..cloudshifts {
        shifts.push(
            scenario
                .add_spell_to_hand_from_oracle(P0, "Cloudshift", true, CLOUDSHIFT)
                .with_mana_cost(ManaCost::zero())
                .id(),
        );
    }
    (scenario.build(), whelp, shifts)
}

fn activate(runner: &mut GameRunner, whelp: ObjectId, nth: usize) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: whelp,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap_or_else(|e| panic!("activation {nth} must be payable: {e:?}"));
}

/// Announce an activation and leave it on the stack (no resolution).
fn announce(runner: &mut GameRunner, whelp: ObjectId, nth: usize) {
    activate(runner, whelp, nth);
    assert!(
        !runner.state().stack.is_empty(),
        "reach-guard: activation {nth} sits on the stack"
    );
}

/// Resolve the stack top (one activation).
fn resolve_top(runner: &mut GameRunner) {
    runner.resolve_top();
}

/// Cast a zero-cost Cloudshift at the Whelp and resolve it, leaving older
/// stack entries (announced activations) in place.
fn blink(runner: &mut GameRunner, cloudshift: ObjectId, whelp: ObjectId) {
    {
        let mut commit = runner.cast(cloudshift).target_object(whelp).commit();
        commit
            .act(GameAction::PassPriority)
            .expect("first pass over Cloudshift");
        commit
            .act(GameAction::PassPriority)
            .expect("second pass resolves Cloudshift");
    }
    assert_eq!(
        runner.state().objects[&cloudshift].zone,
        Zone::Graveyard,
        "reach-guard: Cloudshift resolved"
    );
}

/// CR 602.5c: the activation counter keyed by the resolved acquisition,
/// derived through the production constructor.
fn usage_key(
    state: &engine::types::game_state::GameState,
    id: ObjectId,
    idx: usize,
) -> AbilityUsageKey {
    engine::game::restrictions::ability_usage_key(state, id, idx)
        .expect("the tested slot resolves through the activation lookup")
}

fn live_turn_count(runner: &GameRunner, whelp: ObjectId) -> u32 {
    let key = usage_key(runner.state(), whelp, 0);
    runner
        .state()
        .activated_abilities_this_turn
        .get(&key)
        .copied()
        .unwrap_or(0)
}

fn departed_turn_count(runner: &GameRunner, whelp: ObjectId) -> u32 {
    runner
        .state()
        .departed_activated_abilities_this_turn
        .iter()
        .filter(|((key, _), _)| key.source == whelp)
        .map(|(_, count)| *count)
        .sum()
}

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

/// Cross to the end step and resolve whatever fires. Routes straight to the
/// end step: after a blink the Whelp is summoning-sick, so no attacker is
/// eligible and passing through combat would auto-submit the empty
/// declaration and sail past it.
fn cross_to_resolved_end_step(runner: &mut GameRunner) {
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().phase,
        Phase::End,
        "the delayed sacrifice is timed to the next end step (CR 603.7a)"
    );
}

/// CR 400.7 + CR 608.2h: three activations resolve below threshold, the
/// fourth is announced, the Whelp blinks, and the fourth resolves against
/// the departed tally — creating exactly one delayed sacrifice (the earlier
/// three resolved pre-blink at counts 1–3 and armed nothing).
///
/// Reverting the migration returns tally 0 for the post-blink resolution, so
/// no delayed trigger is created and the non-emptiness assertion flips.
#[test]
fn four_pre_blink_activations_arm_the_delayed_sacrifice() {
    let (mut runner, whelp, shifts) = whelp_board(4, 1);

    for nth in 1..=3 {
        activate(&mut runner, whelp, nth);
        resolve_top(&mut runner);
    }
    assert_eq!(
        live_turn_count(&runner, whelp),
        3,
        "three activations must be recorded on the CR 602.2a ledger"
    );
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "counts 1-3 sit below the printed four-or-more threshold"
    );

    // The fourth activation is announced (live count 4) but left unresolved.
    announce(&mut runner, whelp, 4);
    let incarnation_before = runner.state().objects[&whelp].incarnation;
    blink(&mut runner, shifts[0], whelp);
    assert!(
        runner.state().objects[&whelp].incarnation != incarnation_before,
        "reach-guard: the blinked Whelp is a new object (CR 400.7)"
    );
    assert_eq!(
        live_turn_count(&runner, whelp),
        0,
        "reach-guard: the returned object starts with no live history"
    );
    assert_eq!(
        departed_turn_count(&runner, whelp),
        4,
        "reach-guard: the departed incarnation's tally migrated (not copied)"
    );

    // The announced fourth activation resolves against the departed tally.
    resolve_top(&mut runner);
    assert_eq!(
        runner.state().stack.len(),
        0,
        "reach-guard: the fourth activation resolved"
    );
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "the post-blink resolution must install exactly one delayed end-step \
         sacrifice (the departed tally reads 4)"
    );

    cross_to_resolved_end_step(&mut runner);
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "the delayed trigger fired at the end step"
    );
    // CR 603.7c + CR 400.7: the delayed trigger was created by the
    // pre-blink incarnation's activation, so its SelfRef names that departed
    // object — not the returned Whelp, which is a new object. The sacrifice
    // epoch guard rejects the successor and the returned Whelp survives.
    assert_eq!(
        zone_of(&runner, whelp),
        Zone::Battlefield,
        "the fired delayed trigger must not sacrifice the returned Whelp"
    );
}

/// CR 608.2h: three activations plus a blink arm nothing. The departed table
/// holds the true sub-threshold count (migration preserves it), and the
/// threshold — not the blink — is what gates the rider.
#[test]
fn three_activations_plus_blink_arm_nothing() {
    let (mut runner, whelp, shifts) = whelp_board(3, 1);

    for nth in 1..=3 {
        activate(&mut runner, whelp, nth);
        resolve_top(&mut runner);
    }
    blink(&mut runner, shifts[0], whelp);

    assert_eq!(
        departed_turn_count(&runner, whelp),
        3,
        "reach-guard: the departed tally kept the true count 3"
    );
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "three activations sit below the printed threshold — no delayed \
         sacrifice may be installed"
    );

    cross_to_resolved_end_step(&mut runner);
    assert_eq!(
        zone_of(&runner, whelp),
        Zone::Battlefield,
        "the Whelp survives below its threshold"
    );
}

/// CR 400.7: two departures key their tallies by incarnation and must not
/// collide — the second blink finds an empty live journal, so the departed
/// table still holds exactly the first incarnation's entry.
#[test]
fn double_blink_does_not_collide_departed_entries() {
    let (mut runner, whelp, shifts) = whelp_board(4, 2);

    for nth in 1..=3 {
        activate(&mut runner, whelp, nth);
        resolve_top(&mut runner);
    }
    announce(&mut runner, whelp, 4);
    blink(&mut runner, shifts[0], whelp);
    blink(&mut runner, shifts[1], whelp);
    assert_eq!(
        runner
            .state()
            .departed_activated_abilities_this_turn
            .iter()
            .filter(|((key, _), _)| key.source == whelp)
            .count(),
        1,
        "two departures must not collide: exactly one departed entry survives"
    );

    resolve_top(&mut runner);
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "the announced activation still reads count 4 through the single \
         departed entry"
    );
}

/// CR 400.7: the returned Whelp has an independent activation history. After
/// a pre-activation blink, one activation reads live count 1 (the live path
/// wins; the departed table is never consulted for the new incarnation), so
/// no rider arms and the Whelp survives its end step.
///
/// A live+departed merge (or a departed-first read) would yield 4+ here and
/// sacrifice the Whelp — the survival assertion flips on that defect.
#[test]
fn returned_whelp_has_independent_activation_history() {
    let (mut runner, whelp, shifts) = whelp_board(1, 1);

    let incarnation_before = runner.state().objects[&whelp].incarnation;
    blink(&mut runner, shifts[0], whelp);
    assert!(
        runner.state().objects[&whelp].incarnation != incarnation_before,
        "reach-guard: the Whelp blinked before any activation"
    );
    assert!(
        runner
            .state()
            .departed_activated_abilities_this_turn
            .is_empty(),
        "reach-guard: nothing was migrated — the departed table starts empty"
    );

    activate(&mut runner, whelp, 1);
    resolve_top(&mut runner);
    assert_eq!(
        live_turn_count(&runner, whelp),
        1,
        "the returned Whelp's first activation reads live count 1"
    );
    assert_eq!(
        departed_turn_count(&runner, whelp),
        0,
        "no departed entry exists for the new incarnation"
    );
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "count 1 sits below the printed threshold"
    );
    // The per-game twin migrates symmetrically and reads independently too.
    let game_key = usage_key(runner.state(), whelp, 0);
    assert_eq!(
        runner
            .state()
            .activated_abilities_this_game
            .get(&game_key)
            .copied(),
        Some(1),
        "the per-game live journal counts the new incarnation's activation"
    );
    assert!(
        runner
            .state()
            .departed_activated_abilities_this_game
            .is_empty(),
        "the per-game departed table stays empty for the new incarnation"
    );

    cross_to_resolved_end_step(&mut runner);
    assert_eq!(
        zone_of(&runner, whelp),
        Zone::Battlefield,
        "the returned Whelp survives its end step on an independent count of 1"
    );
}

/// CR 400.7 + CR 608.2h: the announced incarnation selects the tally
/// table. Four activations are announced, the Whelp blinks, and the returned
/// Whelp activates once more under the same shared key. The new activation
/// resolves first and reads live count 1 (no rider); each old activation
/// then reads the departed incarnation's 4 (arming four delayed triggers) —
/// the live hit must not override the announced incarnation's history.
///
/// Reverting the table selection makes the old resolutions read the live 1,
/// so no delayed trigger is ever installed and the count assertion flips.
#[test]
fn interleaved_incarnations_read_their_own_tallies() {
    let (mut runner, whelp, shifts) = whelp_board(5, 1);

    for nth in 1..=4 {
        announce(&mut runner, whelp, nth);
    }
    assert_eq!(
        runner.state().stack.len(),
        4,
        "reach-guard: four activations sit on the stack"
    );
    blink(&mut runner, shifts[0], whelp);
    assert_eq!(
        live_turn_count(&runner, whelp),
        0,
        "reach-guard: the blink migrated the live journal"
    );
    assert_eq!(
        departed_turn_count(&runner, whelp),
        4,
        "reach-guard: the departed incarnation holds count 4"
    );

    // The returned Whelp activates under the same shared key; its resolution
    // reads the live table's 1.
    activate(&mut runner, whelp, 5);
    assert_eq!(
        live_turn_count(&runner, whelp),
        1,
        "reach-guard: the successor's activation is live history"
    );
    resolve_top(&mut runner);
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "the returned Whelp's ability reads live count 1 — below threshold"
    );

    // Each pre-blink activation reads the departed 4, not the live 1.
    for _ in 0..4 {
        resolve_top(&mut runner);
    }
    assert_eq!(
        runner.state().delayed_triggers.len(),
        4,
        "each old activation reads four uses and arms its delayed sacrifice"
    );
}

/// CR 400.7: the turn boundary clears departed per-turn tallies
/// beside their live twins, while per-game entries persist on both sides.
#[test]
fn turn_boundary_clears_departed_per_turn_tallies() {
    let (mut runner, whelp, shifts) = whelp_board(4, 1);

    for nth in 1..=3 {
        activate(&mut runner, whelp, nth);
        resolve_top(&mut runner);
    }
    announce(&mut runner, whelp, 4);
    blink(&mut runner, shifts[0], whelp);
    resolve_top(&mut runner);
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach-guard: the post-blink resolution armed the rider"
    );

    cross_to_resolved_end_step(&mut runner);
    runner.advance_to_upkeep();
    assert!(
        runner.state().activated_abilities_this_turn.is_empty(),
        "the live per-turn journal cleared at the turn boundary"
    );
    assert!(
        runner
            .state()
            .departed_activated_abilities_this_turn
            .is_empty(),
        "the departed per-turn table cleared with its live twin (no cross-turn leak)"
    );
    assert_eq!(
        departed_turn_count(&runner, whelp),
        0,
        "no per-turn count survives the boundary for the Whelp"
    );
    // Per-game entries persist on both sides, mirroring the live journal.
    assert!(
        !runner
            .state()
            .departed_activated_abilities_this_game
            .is_empty(),
        "the per-game departed table persists across the turn boundary"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            || runner.state().phase == Phase::Upkeep,
        "the game advanced past the turn boundary, got {:?}",
        runner.state().waiting_for
    );
}
