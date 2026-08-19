//! CR 616.1 + CR 510.2 + CR 702.15b — a simultaneous combat-damage batch may
//! park on a lifelink life-gain ordering choice and must resume without losing
//! the gain, the rest of the batch, or CR 603.3b trigger simultaneity.
//!
//! The defect: `apply_combat_damage` called `apply_life_gain`, and when two or
//! more co-applicable life-gain replacements made the CR 616.1 ordering
//! material, that call returned `Err(ReplacementDeferred::ReplacementChoice)`
//! having applied NOTHING. The old code rolled `waiting_for` back and dropped
//! 100% of that source's gain — plus every later lifelink source in the same
//! batch — on the false premise that CR 510.2 forbids combat pausing. CR 510.2
//! forbids *casting spells and activating abilities* between combat damage
//! being assigned and dealt; a CR 616.1 choice is neither.
//!
//! This is a MECHANIC class, not a card class: the fix keys on the typed
//! `ReplacementDeferred` outcome, so it covers every board where
//! `replacement_ordering_is_material` is true for a `ProposedEvent::LifeGain`
//! raised from combat damage — the "gain twice that much life instead" family
//! (Rhox Faithmender, Boon Reflection, Alhammarret's Archive) crossed with the
//! "that much life plus N instead" family (Leyline of Hope, Cleric Class L2),
//! crossed with the whole lifelink pool.
//!
//! `Multiply{2}` and `Offset{+1}` deliberately do not commute
//! (2(n+1) != 2n+1), which is exactly what makes `replacement_ordering_is_material`
//! return true and forces a real player choice (CR 616.1).

use super::rules::{AttackTarget, GameRunner, GameScenario, Phase, WaitingFor, Zone, P0, P1};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, QuantityExpr, QuantityRef, ReplacementDefinition,
    TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;

const P2: PlayerId = PlayerId(2);

/// Ajani's Pridemate — the CR 119.9 life-gain receipt the user's board showed
/// never firing. Verbatim modern Oracle text.
const PRIDEMATE: &str = "Whenever you gain life, put a +1/+1 counter on this creature.";

/// Thieving Magpie — a combat-damage-to-a-player trigger, so one batch carries
/// both a CR 119.9 observer and a CR 603.2 combat-damage observer.
const MAGPIE: &str = "Flying\nWhenever this creature deals combat damage to a player, draw a card.";

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

fn event_amount() -> QuantityExpr {
    QuantityExpr::Ref {
        qty: QuantityRef::EventContextAmount,
    }
}

/// "If you would gain life, you gain twice that much life instead."
fn doubler() -> QuantityExpr {
    QuantityExpr::Multiply {
        factor: 2,
        inner: Box::new(event_amount()),
    }
}

/// "If you would gain life, you gain that much life plus 1 instead."
fn plus_one() -> QuantityExpr {
    QuantityExpr::Offset {
        inner: Box::new(event_amount()),
        offset: 1,
    }
}

fn gain_life_replacement(amount: QuantityExpr) -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::GainLife).execute(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount,
            player: TargetFilter::Controller,
        },
    ))
}

/// Install the non-commuting CR 616.1 pair on `player`.
fn install_competing_life_gain_replacements(scenario: &mut GameScenario, player: PlayerId) {
    scenario
        .add_creature(player, "Rhox Faithmender", 1, 5)
        .with_replacement_definition(gain_life_replacement(doubler()));
    scenario
        .add_creature(player, "Leyline of Hope", 1, 1)
        .with_replacement_definition(gain_life_replacement(plus_one()));
}

fn add_lifelinker(
    scenario: &mut GameScenario,
    player: PlayerId,
    name: &str,
    power: i32,
    toughness: i32,
) -> ObjectId {
    let mut builder = scenario.add_creature(player, name, power, toughness);
    builder.with_keyword(Keyword::Lifelink);
    builder.id()
}

/// Pass priority (draining CR 603.3b ordering prompts) until a CR 616.1
/// ordering prompt opens or combat is over. Stops on any other prompt so the
/// caller can assert on it rather than looping past it.
fn advance_through_combat_damage(runner: &mut GameRunner) {
    for _ in 0..96 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                if runner.act(GameAction::OrderTriggers { order }).is_err() {
                    return;
                }
            }
            WaitingFor::Priority { .. } => {
                if matches!(
                    runner.state().phase,
                    Phase::EndCombat | Phase::PostCombatMain
                ) {
                    return;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
            _ => return,
        }
    }
    panic!("combat damage did not settle within the bounded pump");
}

/// Declare `attackers` against `defender` and drive to the combat-damage step,
/// submitting `blocks` when the engine asks for blockers.
fn attack_into_damage(
    runner: &mut GameRunner,
    attackers: &[ObjectId],
    defender: PlayerId,
    blocks: &[(ObjectId, ObjectId)],
) {
    for _ in 0..8 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareAttackers { .. }
        ) || runner.state().phase == Phase::DeclareAttackers
        {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
    runner
        .act(GameAction::DeclareAttackers {
            attacks: attackers
                .iter()
                .map(|&id| (id, AttackTarget::Player(defender)))
                .collect(),
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    for _ in 0..24 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: blocks.to_vec(),
                    })
                    .expect("DeclareBlockers should succeed");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                if runner.act(GameAction::OrderTriggers { order }).is_err() {
                    return;
                }
            }
            WaitingFor::Priority { .. } => {
                if runner.state().phase == Phase::CombatDamage
                    || runner.state().phase == Phase::EndCombat
                {
                    return;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
            _ => return,
        }
    }
}

/// Answer every CR 616.1 ordering prompt that opens, starting with `first`
/// (CR 616.1f repeats the process until no applicable effects remain). Later
/// prompts take index 0 — with one candidate left there is only one.
fn answer_ordering_prompts(runner: &mut GameRunner, first: usize) -> (usize, Vec<GameEvent>) {
    let mut answered = 0;
    let mut events = Vec::new();
    let mut index = first;
    for _ in 0..12 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ReplacementChoice { .. } => {
                let result = runner
                    .act(GameAction::ChooseReplacement { index })
                    .expect("the CR 616.1 ordering choice must be answerable");
                events.extend(result.events);
                answered += 1;
                index = 0;
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                match runner.act(GameAction::OrderTriggers { order }) {
                    Ok(result) => events.extend(result.events),
                    Err(_) => return (answered, events),
                }
            }
            _ => return (answered, events),
        }
    }
    (answered, events)
}

fn positive_life_changes(events: &[GameEvent], player: PlayerId) -> Vec<i32> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::LifeChanged { player_id, amount } if *player_id == player && *amount > 0 => {
                Some(*amount)
            }
            _ => None,
        })
        .collect()
}

/// Drive T1's board to the CR 616.1 pause and return `(runner, lifelinker)`.
fn parked_board() -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lifelinker = add_lifelinker(&mut scenario, P0, "Lifelinker", 3, 3);
    install_competing_life_gain_replacements(&mut scenario, P0);
    let mut runner = scenario.build();

    attack_into_damage(&mut runner, &[lifelinker], P1, &[]);
    advance_through_combat_damage(&mut runner);

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "reach guard: the board must actually raise the CR 616.1 prompt; got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().pending_combat_lifelink.is_some(),
        "reach guard: the unfinished batch tail must be parked"
    );
    (runner, lifelinker)
}

// ---------------------------------------------------------------------------
// T1 / T2 — the ordering discriminator
// ---------------------------------------------------------------------------

/// T1 — CR 616.1 + CR 702.15b: the gain survives the pause and honors
/// "doubler first": 3 damage -> 3*2 = 6, +1 = 7. P0 ends at 27.
///
/// REVERT-FAILING ASSERTION: `runner.life(P0) == 27`. At base
/// `resolve_combat_damage` returns `None`, no prompt is raised and P0's life
/// stays at 20 — the reported bug.
#[test]
fn gain_survives_and_honors_doubler_first() {
    let (mut runner, _) = parked_board();
    assert_eq!(
        runner.life(P0),
        20,
        "no life may be gained while the ordering choice is open"
    );
    assert_eq!(runner.life(P1), 17, "CR 510.2: the damage is already dealt");

    let (answered, _) = answer_ordering_prompts(&mut runner, 0);
    assert!(
        answered >= 1,
        "CR 616.1f: the process repeats until no applicable effects remain"
    );

    assert_eq!(
        runner.life(P0),
        27,
        "CR 616.1: doubler first then +1 — 3 -> 6 -> 7"
    );
    assert!(
        runner.state().pending_combat_lifelink.is_none(),
        "the batch completes and the record is consumed"
    );
}

/// T2 — the same board, the opposite order: +1 first then doubled,
/// (3+1)*2 = 8. P0 ends at 28.
///
/// T1 ∧ T2 is the ordering discriminator: two indices, two different totals
/// (27 vs 28). A fix that gains life but auto-picks an order passes one and
/// fails the other, and a test asserting only `life > 20` cannot tell a correct
/// ordering from a wrong one.
#[test]
fn gain_survives_and_honors_offset_first() {
    let (mut runner, _) = parked_board();
    let candidate_count = match runner.state().waiting_for.clone() {
        WaitingFor::ReplacementChoice {
            candidate_count, ..
        } => candidate_count,
        other => panic!("expected the CR 616.1 prompt, got {other:?}"),
    };
    assert_eq!(
        candidate_count, 2,
        "CR 616.1: both life-gain replacements are co-applicable candidates"
    );

    let _ = answer_ordering_prompts(&mut runner, 1);

    assert_eq!(
        runner.life(P0),
        28,
        "CR 616.1: +1 first then doubled — 3 -> 4 -> 8"
    );
    assert_ne!(
        runner.life(P0),
        27,
        "the player's ordering choice must be material, not cosmetic"
    );
}

// ---------------------------------------------------------------------------
// T3 — the rest of the batch survives, including a second deferral
// ---------------------------------------------------------------------------

/// T3 — CR 702.15e: two lifelink sources in one simultaneous batch are two
/// separate life-gain events. The first parks; after the answer the SECOND
/// raises its own CR 616.1 prompt; after that answer both gains have landed.
///
/// REVERT-FAILING ASSERTION: P0's life reflects BOTH sources. At base both
/// gains are dropped (the first parks and rolls back, and the loop then drops
/// every later source too).
#[test]
fn second_lifelink_source_in_batch_is_not_lost() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let big = add_lifelinker(&mut scenario, P0, "Lifelinker A", 3, 3);
    let small = add_lifelinker(&mut scenario, P0, "Lifelinker B", 2, 2);
    install_competing_life_gain_replacements(&mut scenario, P0);
    let mut runner = scenario.build();

    attack_into_damage(&mut runner, &[big, small], P1, &[]);
    advance_through_combat_damage(&mut runner);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "the first source's gain parks"
    );

    let (answered, _) = answer_ordering_prompts(&mut runner, 0);
    assert!(
        answered >= 2,
        "CR 702.15e: each source's gain is its own event and raises its own \
         CR 616.1 prompt — answered {answered}"
    );

    // Doubler first for both sources: 3 -> 7 and 2 -> 5.
    assert_eq!(
        runner.life(P0),
        20 + 7 + 5,
        "both lifelink sources' gains must land"
    );
    assert_eq!(runner.life(P1), 20 - 5, "CR 510.2: 3 + 2 damage was dealt");
    assert!(runner.state().pending_combat_lifelink.is_none());
}

// ---------------------------------------------------------------------------
// T4 — CR 510.4: a paused first-strike sub-step still runs the regular one
// ---------------------------------------------------------------------------

/// T4 — CR 510.4: the second combat-damage step is mandatory. A double-strike
/// lifelinker parks in the FIRST-STRIKE batch; after the resume the defender
/// must have taken both sub-steps' damage and P0 must have gained twice.
///
/// REVERT-FAILING ASSERTION: `regular_damage_done` and the defender at 14.
/// Omitting `resume_pending_combat_lifelink`'s `resolve_combat_damage`
/// re-entry leaves the regular sub-step unrun.
#[test]
fn first_strike_pause_still_runs_the_regular_sub_step() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let striker = {
        let mut builder = scenario.add_creature(P0, "Double Striker", 3, 3);
        builder.with_keyword(Keyword::Lifelink);
        builder.with_keyword(Keyword::DoubleStrike);
        builder.id()
    };
    install_competing_life_gain_replacements(&mut scenario, P0);
    let mut runner = scenario.build();

    attack_into_damage(&mut runner, &[striker], P1, &[]);
    advance_through_combat_damage(&mut runner);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "the first-strike batch parks"
    );
    assert!(
        !runner
            .state()
            .combat
            .as_ref()
            .expect("combat is live")
            .regular_damage_done,
        "CR 510.4: the regular sub-step has not run yet"
    );

    let _ = answer_ordering_prompts(&mut runner, 0);
    advance_through_combat_damage(&mut runner);
    let _ = answer_ordering_prompts(&mut runner, 0);

    assert_eq!(
        runner.life(P1),
        14,
        "CR 510.4 + CR 702.4b: double strike deals 3 in each of the two sub-steps"
    );
    assert_eq!(
        runner.life(P0),
        20 + 7 + 7,
        "CR 702.15b: each sub-step's damage causes its own life gain"
    );
    assert!(runner.state().pending_combat_lifelink.is_none());
}

// ---------------------------------------------------------------------------
// T5 — CR 603.3b: the resumed gain joins the batch's own trigger batch
// ---------------------------------------------------------------------------

/// T5 — the direct regression for the user's missing Cleric Class receipt, and
/// premise P2's falsifier.
///
/// At the pause (CR 704.3): no player has received priority, so no state-based
/// actions have run — the lethally-damaged blocker is STILL on the battlefield
/// and neither trigger is on the stack. After the answer the "whenever you gain
/// life" (CR 119.9) observer and the combat-damage (CR 603.2) observer reach
/// the stack in the SAME CR 603.3b batch, each exactly once.
#[test]
fn life_gain_trigger_joins_the_combat_damage_trigger_batch() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lifelinker = add_lifelinker(&mut scenario, P0, "Lifelinker", 3, 3);
    let magpie = {
        let mut builder = scenario.add_creature_from_oracle(P0, "Thieving Magpie", 1, 3, MAGPIE);
        builder.id()
    };
    scenario.add_creature_from_oracle(P0, "Ajani's Pridemate", 2, 2, PRIDEMATE);
    let blocker = scenario.add_creature(P1, "Chump", 1, 1).id();
    install_competing_life_gain_replacements(&mut scenario, P0);
    let mut runner = scenario.build();

    attack_into_damage(
        &mut runner,
        &[lifelinker, magpie],
        P1,
        &[(blocker, lifelinker)],
    );
    advance_through_combat_damage(&mut runner);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "the lifelink gain parks; got {:?}",
        runner.state().waiting_for
    );

    // CR 704.3: no player gets priority for a CR 616.1 choice, so no SBAs run.
    assert_eq!(
        runner
            .state()
            .objects
            .get(&blocker)
            .expect("the blocker object still exists")
            .zone,
        Zone::Battlefield,
        "CR 704.3: state-based actions do not run while the prompt is open"
    );
    assert!(
        runner.state().stack.is_empty(),
        "no trigger may be put on the stack before the batch completes (CR 603.3b)"
    );

    let _ = answer_ordering_prompts(&mut runner, 0);

    assert!(
        runner.life(P0) > 20,
        "the lifelink gain lands once the ordering choice is answered"
    );
    assert_eq!(
        runner
            .state()
            .objects
            .get(&blocker)
            .map(|obj| obj.zone)
            .unwrap_or(Zone::Graveyard),
        Zone::Graveyard,
        "CR 704.5g: the lethally-damaged blocker dies once SBAs finally run"
    );
    // CR 119.9: the life-gain receipt the user's board never got.
    assert!(
        runner
            .state()
            .objects
            .values()
            .any(|obj| obj.name == "Ajani's Pridemate"
                && obj
                    .counters
                    .get(&CounterType::Plus1Plus1)
                    .copied()
                    .unwrap_or(0)
                    >= 1),
        "CR 119.9: the 'whenever you gain life' observer must fire exactly once \
         for the resumed gain"
    );
}

// ---------------------------------------------------------------------------
// T6 — the record's lifecycle: never stale, never stranded, never a bare Priority
// ---------------------------------------------------------------------------

/// T6 — B1/B2/M5's paths specifically.
///
/// (i) `resolve_combat_damage` called while the prompt is live returns THAT
///     prompt — never a bare `Priority` — without consuming the record and
///     without re-dealing damage (the batch is one CR 510.2 event).
/// (ii) `pending_phase_transition_progress` is `None` at the pause and again
///     before the wrapper's drain (premise P1's falsifier).
/// (iii) `state.combat` is still `Some` at re-entry, and the record is gone once
///     the step is left.
#[test]
fn resume_keeps_the_batch_whole_through_every_door() {
    let (mut runner, _) = parked_board();

    // (ii) premise P1: a parked record and a parked phase transition cannot
    // co-occur, which is what closes the `auto_advance` door at the epilogue.
    assert!(
        runner.state().pending_phase_transition_progress.is_none(),
        "premise P1: no phase-transition progress may be parked with a combat record"
    );
    // (iii) the record is reachable ahead of `state.combat.as_ref()?`.
    assert!(
        runner.state().combat.is_some(),
        "combat is still live while the batch is parked"
    );

    let defender_life_before = runner.life(P1);
    let mut events = Vec::new();
    let waiting =
        engine::game::combat_damage::resolve_combat_damage(runner.state_mut(), &mut events);

    // (i) the guard surfaces the live prompt, never a bare `Priority`.
    assert!(
        matches!(waiting, Some(WaitingFor::ReplacementChoice { .. })),
        "the re-entry guard must surface the open prompt, got {waiting:?}"
    );
    assert!(
        runner.state().pending_combat_lifelink.is_some(),
        "surfacing the prompt must not consume the parked record"
    );
    assert_eq!(
        runner.life(P1),
        defender_life_before,
        "CR 510.2: the batch is ONE event and must never be re-dealt"
    );

    assert!(
        runner.state().pending_phase_transition_progress.is_none(),
        "premise P1 still holds immediately before the drain"
    );

    let _ = answer_ordering_prompts(&mut runner, 0);

    assert!(
        runner.state().pending_combat_lifelink.is_none(),
        "the record is consumed by the completing drain"
    );
    assert_eq!(
        runner.life(P0),
        27,
        "the gain still lands after the re-entry"
    );
}

// ---------------------------------------------------------------------------
// T7 — no leaked pause state, and no double gain
// ---------------------------------------------------------------------------

/// T7 — CR 616.1: after a full resume neither `pending_replacement` nor
/// `pending_combat_lifelink` survives, a stray `ChooseReplacement` is rejected
/// rather than consuming a stale record, and the batch produced exactly ONE
/// positive life-gain event for P0 — the assertion that fails loudly if the
/// paused source were wrongly re-queued into `remaining`.
#[test]
fn no_pending_replacement_or_parked_record_leaks() {
    let (mut runner, _) = parked_board();
    let (_, events) = answer_ordering_prompts(&mut runner, 0);

    assert_eq!(
        positive_life_changes(&events, P0),
        vec![7],
        "CR 702.15e: exactly ONE positive life-gain event for the resumed source \
         — a re-queued paused source would emit a second"
    );
    assert!(
        runner.state().pending_replacement.is_none(),
        "CR 616.1: the answered replacement must not survive its round trip"
    );
    assert!(
        runner.state().pending_combat_lifelink.is_none(),
        "the parked batch must not survive its completion"
    );
    assert_eq!(runner.life(P0), 27, "exactly one gain, correctly ordered");

    let stray = runner.act(GameAction::ChooseReplacement { index: 0 });
    assert!(
        stray.is_err(),
        "a stray ChooseReplacement must be rejected, not consume a stale record"
    );
    assert_eq!(
        runner.life(P0),
        27,
        "the stray action must not gain the life a second time"
    );
}

// ---------------------------------------------------------------------------
// H1 — multi-authority: each gain credits its own snapshotted controller
// ---------------------------------------------------------------------------

/// H1 — CR 702.15b binds "that source's controller". Two lifelink sources with
/// DIFFERENT controllers trade damage in one batch and the competing
/// replacements sit on P0, so P0's gain parks. P1's gain must still land and
/// must credit P1 — not the pausing player and not `state.active_player`.
#[test]
fn two_lifelink_controllers_credit_their_own_snapshotted_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = add_lifelinker(&mut scenario, P0, "Attacking Lifelinker", 3, 3);
    let blocker = add_lifelinker(&mut scenario, P1, "Blocking Lifelinker", 2, 4);
    install_competing_life_gain_replacements(&mut scenario, P0);
    let mut runner = scenario.build();

    attack_into_damage(&mut runner, &[attacker], P1, &[(blocker, attacker)]);
    advance_through_combat_damage(&mut runner);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "P0's gain parks on the CR 616.1 choice; got {:?}",
        runner.state().waiting_for
    );

    let _ = answer_ordering_prompts(&mut runner, 0);

    assert_eq!(
        runner.life(P1),
        22,
        "CR 702.15b: the blocker's controller gains its OWN 2 life, unmodified \
         by the replacements P0 controls"
    );
    assert_eq!(
        runner.life(P0),
        27,
        "P0's own gain is doubled then offset (3 -> 6 -> 7)"
    );
    assert!(runner.state().pending_combat_lifelink.is_none());
}

// ---------------------------------------------------------------------------
// H2 — CR 614.7's actual subject: an event that never happens
// ---------------------------------------------------------------------------

/// H2 — a fully prevented lifelink attacker deals no damage, so there is no
/// life-gain event to replace (CR 614.7) and no prompt is raised. The first
/// production branch reached is `remaining` being EMPTY — nothing is pushed
/// into `lifelink_by_source` unless `actual_amount > 0` — so `pop_front()`
/// returns `None` on the first iteration and `apply_life_gain` is never called.
///
/// PAIRED POSITIVE CONTROL in the same test: the identical board WITHOUT the
/// prevention does raise the prompt, so the negative cannot pass vacuously.
#[test]
fn fully_prevented_lifelink_raises_no_prompt() {
    // Negative: damage prevented by a protection-style total gate.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lifelinker = add_lifelinker(&mut scenario, P0, "Lifelinker", 3, 3);
    install_competing_life_gain_replacements(&mut scenario, P0);
    let mut runner = scenario.build();
    // CR 510.1a: a 0-power attacker assigns no combat damage, so no damage is
    // dealt and no life-gain event exists to replace. Same first production
    // branch as a fully prevented source: nothing is pushed into
    // `lifelink_by_source` unless `actual_amount > 0`.
    runner
        .state_mut()
        .objects
        .get_mut(&lifelinker)
        .expect("the attacker exists")
        .power = Some(0);

    attack_into_damage(&mut runner, &[lifelinker], P1, &[]);
    advance_through_combat_damage(&mut runner);

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "CR 614.7: an event that never happens has nothing to replace"
    );
    assert!(
        runner.state().pending_combat_lifelink.is_none(),
        "no batch may park when no life-gain event occurs"
    );
    assert_eq!(runner.life(P0), 20, "no damage, no lifelink, no gain");

    // Positive control: the same board with a real power raises the prompt.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lifelinker = add_lifelinker(&mut scenario, P0, "Lifelinker", 3, 3);
    install_competing_life_gain_replacements(&mut scenario, P0);
    let mut control = scenario.build();
    attack_into_damage(&mut control, &[lifelinker], P1, &[]);
    advance_through_combat_damage(&mut control);
    assert!(
        matches!(
            control.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "positive control: the identical board WITH damage does raise the prompt"
    );
}

// ---------------------------------------------------------------------------
// H3 — CR 800.4: the ACTIVE attacker concedes with the prompt open
// ---------------------------------------------------------------------------

/// H3 — the abandonment authority in `turns::enter_phase`.
///
/// Three seats, so the game continues after a departure (CR 800.4). P0 is the
/// active player, attacks with the lifelinker, and controls the competing
/// replacements — so the pausing controller IS the active player. P0 then
/// concedes with the CR 616.1 prompt still open. `auto_advance_once` bails at
/// its CR 800.4 eliminated-active-player arm and leaves the combat-damage step
/// WITHOUT calling `resolve_combat_damage`, so the re-entry guard never runs:
/// only the phase-entry abandonment can clear the record.
///
/// REVERT PROBE: delete `state.pending_combat_lifelink = None;` from
/// `turns::enter_phase`. Assertion (ii) is the load-bearing half — a variant
/// that cleared the record somewhere harmless would satisfy (i) while the next
/// turn's combat damage was still being skipped, because the stale record's
/// drain writes `regular_damage_done` on the NEW turn's `CombatState`.
#[test]
fn conceding_active_attacker_does_not_skip_the_next_turns_combat_damage() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let lifelinker = add_lifelinker(&mut scenario, P0, "Lifelinker", 3, 3);
    install_competing_life_gain_replacements(&mut scenario, P0);
    let p1_attacker = scenario.add_creature(P1, "Next Turn Attacker", 2, 2).id();
    let mut runner = scenario.build();
    assert_eq!(
        runner.state().active_player,
        P0,
        "reach guard: the pausing controller must be the ACTIVE player"
    );

    attack_into_damage(&mut runner, &[lifelinker], P1, &[]);
    advance_through_combat_damage(&mut runner);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "reach guard: the prompt must be open when P0 concedes; got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().pending_combat_lifelink.is_some(),
        "reach guard: the record must be parked when P0 concedes"
    );

    runner
        .act(GameAction::Concede { player_id: P0 })
        .expect("CR 800.4: a player may concede at any time");

    // (i) the record must not outlive its combat.
    assert!(
        runner.state().pending_combat_lifelink.is_none(),
        "CR 500.4: entering another step abandons the combat-damage batch"
    );

    // (ii) the FOLLOWING turn's combat damage must actually be dealt.
    for _ in 0..64 {
        if runner.state().active_player == P1 && runner.state().phase == Phase::PreCombatMain {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                if runner.act(GameAction::OrderTriggers { order }).is_err() {
                    break;
                }
            }
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }
    assert_eq!(
        runner.state().active_player,
        P1,
        "CR 800.4: the game continues with the next player's turn"
    );

    let p2_life_before = runner.life(P2);
    attack_into_damage(&mut runner, &[p1_attacker], P2, &[]);
    advance_through_combat_damage(&mut runner);

    assert_eq!(
        runner.life(P2),
        p2_life_before - 2,
        "CR 510.2 + CR 510.4: a stale record must not skip THIS turn's combat damage"
    );
    assert!(
        runner
            .state()
            .combat
            .as_ref()
            .map(|combat| combat.regular_damage_done)
            .unwrap_or(true),
        "this turn's own CombatState records its own completed damage"
    );
}

// ---------------------------------------------------------------------------
// H4 — CR 800.4a: a NON-ACTIVE controller leaves; the batch still completes
// ---------------------------------------------------------------------------

/// H4 — the per-entry `retain` in `elimination`.
///
/// The competing replacements sit on the NON-ACTIVE blocker's controller (P1),
/// so P1 is the chooser and P1's gain is the one that parks. P1 leaves the game
/// while the prompt is open; the active player P0 stays. P1's owed gain is
/// dropped (a leaving player gains no life) while P0's still lands and the
/// batch completes — a blanket null would lose P0's gain too, and a
/// non-draining guard would hang on `priority.rs`'s completeness gate.
///
/// Termination is asserted on the drained record and the completion flag, never
/// on wall-clock.
#[test]
fn departed_nonactive_controller_forfeits_its_gain_and_the_batch_still_completes() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = add_lifelinker(&mut scenario, P0, "Attacking Lifelinker", 3, 3);
    let blocker = add_lifelinker(&mut scenario, P1, "Blocking Lifelinker", 2, 5);
    install_competing_life_gain_replacements(&mut scenario, P1);
    let mut runner = scenario.build();

    attack_into_damage(&mut runner, &[attacker], P1, &[(blocker, attacker)]);
    advance_through_combat_damage(&mut runner);
    let chooser = match runner.state().waiting_for.clone() {
        WaitingFor::ReplacementChoice { player, .. } => player,
        other => panic!("expected P1's CR 616.1 prompt, got {other:?}"),
    };
    assert_eq!(
        chooser, P1,
        "reach guard: the NON-ACTIVE seat must be the one that parks"
    );

    let departure = runner
        .act(GameAction::Concede { player_id: P1 })
        .expect("CR 800.4: a player may leave at any time");
    assert!(
        positive_life_changes(&departure.events, P1).is_empty(),
        "CR 800.4a: no life-gain event may be emitted for the departed seat"
    );

    assert!(
        runner.state().pending_combat_lifelink.is_none(),
        "CR 800.4a: the batch drains rather than stranding — no livelock"
    );
    assert_eq!(
        runner
            .state()
            .players
            .iter()
            .find(|p| p.id == P1)
            .map(|p| p.life)
            .unwrap_or(20),
        20,
        "CR 800.4a: a leaving player gains no life"
    );
    assert_eq!(
        runner.life(P0),
        23,
        "the surviving controller's gain still lands — the retain is per-entry, \
         never a blanket null"
    );
    assert!(
        runner
            .state()
            .combat
            .as_ref()
            .map(|combat| combat.regular_damage_done)
            .unwrap_or(true),
        "the combat-damage sub-step completes rather than re-entering forever"
    );
}
