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
    AbilityDefinition, AbilityKind, DelayedTriggerCondition, DelayedTriggerLifetime, Effect,
    QuantityExpr, QuantityRef, ReplacementDefinition, ResolvedAbility, TargetFilter,
    TriggerDefinition,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{DelayedTrigger, LoopDetectSample, StackEntryKind};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::triggers::TriggerMode;

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

/// The "you gain twice that much life instead" half of the non-commuting pair.
const DOUBLER: &str = "Rhox Faithmender";
/// The "you gain that much life plus 1 instead" half of the non-commuting pair.
const OFFSET: &str = "Leyline of Hope";

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

/// Answer every CR 616.1 ordering prompt that opens, applying the effect from
/// the source named `first_source` FIRST. CR 616.1f repeats the process until
/// no applicable effects remain; later prompts take index 0, because with one
/// applicable effect left there is only one.
///
/// The candidate is located by NAME rather than by a hardcoded position.
/// `WaitingFor::ReplacementChoice.candidates` is ordered by the replacement
/// index's scan ordinal (`ReplacementIndexEntry.ordinal`), which is NOT
/// creation order: the same two sources, created in the same order, were
/// OBSERVED at `[Leyline, Rhox]` on the single-attacker board and at
/// `[Rhox, Leyline]` on the two-controller board. A positional premise would
/// therefore pin an incidental registration order rather than the CR 616.1
/// choice these tests exist to discriminate, and would go silently red on any
/// unrelated change to that order.
fn answer_ordering_prompts(runner: &mut GameRunner, first_source: &str) -> (usize, Vec<GameEvent>) {
    let mut answered = 0;
    let mut events = Vec::new();
    for _ in 0..12 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ReplacementChoice { candidates, .. } => {
                // CR 616.1: the affected player chooses one applicable effect to
                // apply; `continue_replacement` applies `candidates[index]`
                // immediately and then repeats (CR 616.1f).
                //
                // `first_source` is selected EVERY time it is applicable, not
                // just on this call's first prompt. One call can span more than
                // one life-gain event — a batch with two lifelink sources
                // (CR 702.15e), or a first-strike batch whose resume runs the
                // regular sub-step (CR 510.4) — and each such event opens its
                // own first prompt. Keying on "the first prompt I see" would
                // silently answer the SECOND event's opening choice with a bare
                // index 0 and make each event's total depend on candidate order
                // again.
                //
                // When `first_source` is absent the prompt is a CR 616.1f repeat:
                // CR 614.5 gives each effect one opportunity per event, so the
                // already-applied effect is no longer among the candidates and
                // index 0 is the sole remaining effect.
                let index = match candidates
                    .iter()
                    .position(|candidate| candidate.source_name == first_source)
                {
                    Some(index) => index,
                    None => {
                        assert!(
                            answered > 0,
                            "reach guard: CR 616.1 — {first_source} must be an applicable \
                             candidate on an event's opening prompt; got {candidates:?}"
                        );
                        0
                    }
                };
                let result = runner
                    .act(GameAction::ChooseReplacement { index })
                    .expect("the CR 616.1 ordering choice must be answerable");
                events.extend(result.events);
                answered += 1;
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

/// Install a one-shot delayed LifeGained observer through the engine's delayed
/// trigger machinery. Its counter receipt lets the test distinguish collection
/// from eventual resolution without creating another life-change event.
fn install_delayed_life_gain_counter_receipt(runner: &mut GameRunner, source: ObjectId) {
    let mut ability = ResolvedAbility::new(
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::SelfRef,
        },
        vec![],
        source,
        P0,
    );
    let source_context = engine::game::triggers::trigger_source_context_for_latch(
        runner.state(),
        runner
            .state()
            .objects
            .get(&source)
            .expect("delayed LifeGained receipt source"),
    );
    ability.set_trigger_source_recursive(source_context);

    runner
        .state_mut()
        .delayed_triggers
        .push(DelayedTrigger::new(
            DelayedTriggerCondition::WhenNextEvent {
                trigger: Box::new(
                    TriggerDefinition::new(TriggerMode::LifeGained)
                        .valid_target(TargetFilter::Controller),
                ),
                or_trigger: None,
                lifetime: DelayedTriggerLifetime::ThisTurn,
            },
            Box::new(ability),
            P0,
            source,
            true,
        ));
}

/// Resolve the CR 616.1 replacement choices, but stop at the first trigger
/// ordering prompt so the caller can inspect the one CR 603.3b transaction.
fn answer_replacements_until_first_trigger_order(
    runner: &mut GameRunner,
    first_source: &str,
) -> usize {
    let mut answered = 0;
    for _ in 0..12 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ReplacementChoice { candidates, .. } => {
                let index = candidates
                    .iter()
                    .position(|candidate| candidate.source_name == first_source)
                    .unwrap_or_else(|| {
                        assert!(
                            answered > 0,
                            "reach guard: {first_source} must be applicable on the first CR 616.1 choice; got {candidates:?}"
                        );
                        0
                    });
                runner
                    .act(GameAction::ChooseReplacement { index })
                    .expect("the CR 616.1 replacement choice must be answerable");
                answered += 1;
            }
            WaitingFor::OrderTriggers { .. } => return answered,
            waiting_for => panic!(
                "expected a CR 616.1 replacement choice or the initial trigger ordering prompt, got {waiting_for:?}"
            ),
        }
    }
    panic!("the combat batch did not reach its first trigger ordering prompt");
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

    let (answered, _) = answer_ordering_prompts(&mut runner, DOUBLER);
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

    let _ = answer_ordering_prompts(&mut runner, OFFSET);

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

    let (answered, _) = answer_ordering_prompts(&mut runner, DOUBLER);
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

    let _ = answer_ordering_prompts(&mut runner, DOUBLER);
    advance_through_combat_damage(&mut runner);
    let _ = answer_ordering_prompts(&mut runner, DOUBLER);

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
    let magpie = scenario
        .add_creature_from_oracle(P0, "Thieving Magpie", 1, 3, MAGPIE)
        .id();
    let pridemate = scenario
        .add_creature_from_oracle(P0, "Ajani's Pridemate", 2, 2, PRIDEMATE)
        .id();
    let blocker = scenario.add_creature(P1, "Chump", 1, 1).id();
    install_competing_life_gain_replacements(&mut scenario, P0);
    // CR 704.5b: Thieving Magpie is UNBLOCKED here, so its combat-damage trigger
    // draws. Without a library that draw eliminates P0 the moment SBAs next run,
    // ending the game before the CR 119.9 observer below can resolve — OBSERVED:
    // the identical board without these cards reaches a GameOver wait with the
    // Pridemate trigger still on the stack. The cards exist only to keep P0 in the
    // game; nothing in this test reads them.
    scenario.with_library_top(P0, &["Filler A", "Filler B", "Filler C", "Filler D"]);
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

    let _ = answer_ordering_prompts(&mut runner, DOUBLER);
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
    // CR 119.9 + CR 603.3b: both observers are placed by the one resolved
    // batch. Their effects have not resolved yet, so source-scoped stack
    // entries are the rule-correct receipt here.
    let count_triggers = |source| {
        runner
            .state()
            .stack
            .iter()
            .filter(|entry| {
                matches!(
                    entry.kind,
                    StackEntryKind::TriggeredAbility { source_id, .. } if source_id == source
                )
            })
            .count()
    };
    assert_eq!(
        count_triggers(pridemate),
        1,
        "CR 119.9: the resumed life gain creates exactly one Pridemate trigger"
    );
    assert_eq!(
        count_triggers(magpie),
        1,
        "CR 603.2: the combat-damage observer joins the same batch exactly once"
    );
}

/// CR 510.3a + CR 603.3b + CR 603.7b: an independently installed delayed
/// LifeGained observer and ordinary combat observers from the completed damage
/// batch must enter the *first* ordering transaction together. This observes
/// the live pending group before submitting `OrderTriggers`; a later priority
/// scan would be too late to prove simultaneous trigger collection.
#[test]
fn delayed_life_gain_trigger_joins_the_first_combat_ordering_transaction() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Magpie Draw"]);
    let lifelinker = add_lifelinker(&mut scenario, P0, "Lifelinker", 3, 3);
    let magpie = scenario
        .add_creature_from_oracle(P0, "Thieving Magpie", 1, 3, MAGPIE)
        .id();
    let pridemate = scenario
        .add_creature_from_oracle(P0, "Ajani's Pridemate", 2, 2, PRIDEMATE)
        .id();
    let delayed_receipt = scenario.add_creature(P0, "Delayed Receipt", 1, 1).id();
    let blocker = scenario.add_creature(P1, "Chump", 1, 1).id();
    install_competing_life_gain_replacements(&mut scenario, P0);
    let mut runner = scenario.build();
    install_delayed_life_gain_counter_receipt(&mut runner, delayed_receipt);

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
        "reach guard: the lifelink replacement must pause before trigger collection"
    );

    let replacement_choices = answer_replacements_until_first_trigger_order(&mut runner, DOUBLER);
    assert!(
        replacement_choices >= 1,
        "reach guard: the CR 616.1 replacement process must have been exercised"
    );

    let order = runner
        .state()
        .pending_trigger_order
        .as_ref()
        .expect("the first OrderTriggers prompt must retain its pending group");
    let group = order
        .groups
        .iter()
        .find(|group| group.controller == P0)
        .expect("P0 must own the combined combat trigger group");
    assert!(
        group.triggers.iter().any(|context| {
            context.pending.source_id == pridemate
                && context.dispatch_origin
                    == engine::game::triggers::PendingTriggerDispatchOrigin::Normal
        }),
        "reach guard: the ordinary LifeGained observer must be in the initial ordering group"
    );
    assert!(
        group.triggers.iter().any(|context| {
            context.pending.source_id == magpie
                && context.dispatch_origin
                    == engine::game::triggers::PendingTriggerDispatchOrigin::Normal
        }),
        "reach guard: the ordinary combat-damage observer must be in the initial ordering group"
    );
    assert!(
        group.triggers.iter().any(|context| {
            context.pending.source_id == delayed_receipt
                && context.dispatch_origin
                    == engine::game::triggers::PendingTriggerDispatchOrigin::Delayed
        }),
        "CR 603.3b + CR 603.7b: the delayed LifeGained observer must join this FIRST group; the phase-only collector leaves it absent"
    );
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "CR 603.7b: the matching one-shot delayed trigger is consumed during collection, before ordering"
    );

    let order = match runner.state().waiting_for.clone() {
        WaitingFor::OrderTriggers { triggers, .. } => (0..triggers.len()).collect(),
        waiting_for => panic!("expected the inspected OrderTriggers prompt, got {waiting_for:?}"),
    };
    runner
        .act(GameAction::OrderTriggers { order })
        .expect("the combined trigger ordering must be answerable");

    let mut stack_drained = false;
    for _ in 0..96 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => runner
                .act(GameAction::OrderTriggers {
                    order: (0..triggers.len()).collect(),
                })
                .expect("follow-up trigger ordering must be answerable"),
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => {
                stack_drained = true;
                break;
            }
            WaitingFor::Priority { .. } => runner
                .act(GameAction::PassPriority)
                .expect("priority pass must resolve the combined trigger batch"),
            waiting_for => panic!(
                "unexpected wait while resolving the combined combat trigger batch: {waiting_for:?}"
            ),
        };
    }
    assert!(
        stack_drained,
        "the combined trigger batch must settle within its safety bound"
    );
    assert_eq!(
        runner
            .state()
            .objects
            .get(&delayed_receipt)
            .expect("delayed receipt source remains on the battlefield")
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        1,
        "the independently delayed LifeGained receipt resolves exactly once"
    );
    assert_eq!(
        runner
            .state()
            .objects
            .get(&pridemate)
            .expect("Pridemate remains on the battlefield")
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        1,
        "the ordinary Pridemate receipt still resolves exactly once"
    );
}

/// CR 119.9 + CR 603.2c: the resumed combat batch's ordinary collection is
/// claimed before priority, so its generic ordinary scan cannot fire this one
/// observer twice. The CR 616.1 pause is required to reach that scheduler path.
#[test]
fn resumed_lifelink_gain_triggers_single_pridemate_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lifelinker = add_lifelinker(&mut scenario, P0, "Lifelinker", 3, 3);
    let pridemate = scenario
        .add_creature_from_oracle(P0, "Ajani's Pridemate", 2, 2, PRIDEMATE)
        .id();
    let blocker = scenario.add_creature(P1, "Chump", 1, 1).id();
    install_competing_life_gain_replacements(&mut scenario, P0);
    let mut runner = scenario.build();

    attack_into_damage(&mut runner, &[lifelinker], P1, &[(blocker, lifelinker)]);
    advance_through_combat_damage(&mut runner);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "reach guard: the lifelink gain must pause for a real CR 616.1 choice"
    );

    let _ = answer_ordering_prompts(&mut runner, DOUBLER);
    let mut stack_drained = false;
    for _ in 0..96 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("trigger ordering must be answerable");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => {
                stack_drained = true;
                break;
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass must resolve the trigger");
            }
            waiting_for => panic!("unexpected wait while draining Pridemate: {waiting_for:?}"),
        }
    }
    assert!(
        stack_drained,
        "the test must drain the stack within its safety bound"
    );

    assert!(
        runner.life(P0) > 20,
        "the resumed lifelink gain must be positive"
    );
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::GameOver { .. }),
        "the completed combat must not end the game"
    );
    assert_eq!(
        runner
            .state()
            .objects
            .get(&pridemate)
            .expect("Pridemate remains on the battlefield")
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        1,
        "CR 119.9: exactly one life-gain trigger firing adds one counter"
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

    let _ = answer_ordering_prompts(&mut runner, DOUBLER);

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
    let (_, events) = answer_ordering_prompts(&mut runner, DOUBLER);

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

    let _ = answer_ordering_prompts(&mut runner, DOUBLER);

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

/// H2 — a lifelink attacker that deals NO combat damage causes no life-gain
/// event, so the competing replacements have nothing to replace (CR 614.7a)
/// and no prompt is raised. The first production branch reached is `remaining`
/// being EMPTY — nothing is pushed into `lifelink_by_source` unless
/// `actual_amount > 0` — so `pop_front()` returns `None` on the first iteration
/// and `apply_life_gain` is never called.
///
/// The zero comes from the creature's PRINTED power (CR 510.1a: a creature that
/// would assign 0 or less combat damage assigns none). It deliberately does not
/// come from writing `object.power` directly: that field is a projected
/// characteristic, and the continuous-effects recalculation restores it from the
/// base power before combat damage is assigned — OBSERVED, a poked `Some(0)`
/// reads back as `Some(3)` by the time the batch runs, so such a fixture proves
/// nothing about the zero-damage branch.
///
/// PAIRED POSITIVE CONTROL in the same test: the identical board with a nonzero
/// power does raise the prompt, so the negative cannot pass vacuously.
#[test]
fn zero_damage_lifelink_raises_no_prompt() {
    // Negative: the attacker assigns no combat damage at all.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lifelinker = add_lifelinker(&mut scenario, P0, "Powerless Lifelinker", 0, 3);
    install_competing_life_gain_replacements(&mut scenario, P0);
    let mut runner = scenario.build();

    attack_into_damage(&mut runner, &[lifelinker], P1, &[]);
    advance_through_combat_damage(&mut runner);

    // Reach guard for the MECHANISM: the branch under test is "no damage was
    // dealt". If the attacker had somehow dealt damage, the negatives below
    // would be testing a different board.
    assert_eq!(
        runner.life(P1),
        20,
        "CR 510.1a: a 0-power attacker assigns no combat damage"
    );
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "CR 614.7a: an event that never happens has nothing to replace"
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
    assert_eq!(
        control.life(P1),
        17,
        "positive control: the identical board WITH power does deal damage"
    );
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
    // CR 704.5b: this is the only test here that runs a SECOND turn, so it is the
    // only one whose seats reach a draw step. A scenario seat starts with an empty
    // library and a player who drew from an empty library loses the game — without
    // these cards P1 is eliminated on its own draw step and the game ends
    // (OBSERVED: `GameOver { winner: P2 }` at `Phase::Draw`) before the following
    // turn's combat damage can be reached at all.
    scenario.with_library_top(P1, &["Filler A", "Filler B", "Filler C"]);
    scenario.with_library_top(P2, &["Filler A", "Filler B", "Filler C"]);
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

    // (i) the record must not outlive its combat. The abandonment authority is
    // `turns::enter_phase`, so the record is cleared when the game actually
    // ENTERS the next step — not by the `Concede` action itself, which only
    // performs the CR 800.4a departure and reconciles priority (the phase is
    // still `CombatDamage` and `active_player` is still the departed seat when
    // that action returns). The assertion is therefore made once the concede has
    // settled through `skip_eliminated_active_turn` -> `advance_phase_once` ->
    // `start_next_turn` -> `enter_phase`, exactly as the plan's H3 specifies.

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
    assert!(
        runner.state().pending_combat_lifelink.is_none(),
        "CR 500.4: entering another step abandons the combat-damage batch"
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

/// H4 — the latched-chooser teardown in `elimination::handle_player_left_game`,
/// and the anti-livelock drain.
///
/// SCOPE CORRECTION, stated rather than implied: this row does NOT reach the
/// per-entry `record.remaining.retain(..)` there, despite earlier docstrings
/// claiming it. The departing seat P1 controls exactly ONE lifelink source, so it
/// owns exactly one gain, and that gain is the one that PARKED — which
/// `drain_combat_lifelink` deliberately never re-queues into `remaining`. P1
/// therefore owns no entry in `remaining` at the concede and the `retain` filters
/// nothing on this board. `departed_seat_forfeits_a_still_queued_gain_and_the_batch_still_completes`
/// below is the row that reaches it.
///
/// What this row DOES cover: the competing replacements sit on the NON-ACTIVE
/// blocker's controller (P1), so P1 is the latched CR 616.1 chooser. P1 leaves
/// while the prompt is open, which clears `pending_replacement` and abandons its
/// post-replacement continuation; the parked gain is forfeited with it (a leaving
/// player gains no life). The batch must then still drain to completion so its
/// CR 603.3b triggers fire — a non-draining guard would hang on `priority.rs`'s
/// completeness gate — and the surviving active seat P0's gain must still land.
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

    // CR 800.4a departs the seat and reconciles priority; it does not itself
    // re-enter `resolve_combat_damage`, so the record is still parked when the
    // action returns. The NEXT priority window re-enters, and the re-entry
    // guard's completion branch finishes the batch. Pump a bounded number of
    // windows and record the completion state at the exact beat the record is
    // consumed — this is the anti-livelock assertion, made on the drained
    // record and the sub-step flag rather than on wall-clock.
    let mut completion_at_drain = None;
    for _ in 0..16 {
        if runner.state().pending_combat_lifelink.is_none() {
            completion_at_drain = Some(
                runner
                    .state()
                    .combat
                    .as_ref()
                    .map(|c| c.regular_damage_done),
            );
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
            other => panic!("no further prompt is owed after the departure: {other:?}"),
        }
    }
    assert_eq!(
        completion_at_drain,
        Some(Some(true)),
        "CR 800.4a: the batch drains rather than stranding — no livelock — and \
         the sub-step it owns is marked complete on its OWN live CombatState"
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
}

// ---------------------------------------------------------------------------
// H5 — CR 800.4a: the departing seat owns a STILL-QUEUED gain
// ---------------------------------------------------------------------------

/// H5 — the discriminating owner of the per-entry `retain` in
/// `elimination::handle_player_left_game`
/// (`record.remaining.retain(|gain| gain.controller != player)`).
///
/// H4 above cannot reach that `retain` — see its own docstring. This board gives
/// the departing seat a gain that is still IN `remaining` when it concedes:
///
///   * `lifelink_attacker` (P0, lifelink) is blocked by `first_blocker` -> P0 is
///     owed 3;
///   * `plain_attacker` (P0, NO lifelink) is blocked by `second_blocker`, so it
///     contributes no gain of its own and no CR 510.1c damage-division choice
///     opens — CR 510.1c only asks the attacker's controller to divide when TWO
///     or more creatures block it, and here each attacker has exactly one;
///   * `first_blocker` and `second_blocker` (both P1, both lifelink) are each
///     owed 2.
///
/// Three CR 702.15b gains, TWO of them P1's. The competing replacements sit on
/// P1, so the FIRST of P1's two gains to be drained parks (CR 616.1) and is
/// deliberately never re-queued — which leaves P1's SECOND gain sitting in
/// `remaining`, whatever order the batch drained in. That is the entry the
/// `retain` exists to drop, and the reach guard below asserts it is really there
/// before the concede rather than assuming it.
///
/// REVERT-FAILING ASSERTION: `runner.life(P1) == 20` / the empty
/// `positive_life_changes(.., P1)`. Delete the `retain` and P1's still-queued
/// gain is applied on the resume to a seat that has left the game.
///
/// The departed seat's gain is asserted SPECIFICALLY — on P1's own life total and
/// on P1-keyed `LifeChanged` events — never on a batch total that P0's surviving
/// gain could satisfy on its own.
#[test]
fn departed_seat_forfeits_a_still_queued_gain_and_the_batch_still_completes() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let lifelink_attacker = add_lifelinker(&mut scenario, P0, "Attacking Lifelinker", 3, 6);
    let plain_attacker = scenario.add_creature(P0, "Plain Attacker", 1, 6).id();
    let first_blocker = add_lifelinker(&mut scenario, P1, "First Blocking Lifelinker", 2, 5);
    let second_blocker = add_lifelinker(&mut scenario, P1, "Second Blocking Lifelinker", 2, 5);
    install_competing_life_gain_replacements(&mut scenario, P1);
    let mut runner = scenario.build();

    attack_into_damage(
        &mut runner,
        &[lifelink_attacker, plain_attacker],
        P1,
        &[
            (first_blocker, lifelink_attacker),
            (second_blocker, plain_attacker),
        ],
    );
    advance_through_combat_damage(&mut runner);

    let chooser = match runner.state().waiting_for.clone() {
        WaitingFor::ReplacementChoice { player, .. } => player,
        other => panic!("expected P1's CR 616.1 prompt, got {other:?}"),
    };
    assert_eq!(
        chooser, P1,
        "reach guard: the NON-ACTIVE seat must be the one that parks"
    );

    // THE reach guard that separates this row from H4: the departing seat must
    // still OWN an entry in `remaining`, or the `retain` filters nothing and this
    // fixture would certify a guard it never reaches.
    let queued_for_departing: Vec<u32> = runner
        .state()
        .pending_combat_lifelink
        .as_ref()
        .expect("reach guard: the batch is parked, so the record exists")
        .remaining
        .iter()
        .filter(|gain| gain.controller == P1)
        .map(|gain| gain.amount)
        .collect();
    assert!(
        !queued_for_departing.is_empty(),
        "reach guard: the departing seat must own a STILL-QUEUED gain — this is the \
         entry the CR 800.4a per-entry retain exists to drop, and H4 never has one"
    );

    let departure = runner
        .act(GameAction::Concede { player_id: P1 })
        .expect("CR 800.4: a player may leave at any time");
    let mut life_events = departure.events;

    let mut completed = false;
    for _ in 0..16 {
        if runner.state().pending_combat_lifelink.is_none() {
            completed = true;
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                match runner.act(GameAction::OrderTriggers { order }) {
                    Ok(result) => life_events.extend(result.events),
                    Err(_) => break,
                }
            }
            WaitingFor::Priority { .. } => match runner.act(GameAction::PassPriority) {
                Ok(result) => life_events.extend(result.events),
                Err(_) => break,
            },
            other => panic!("no further prompt is owed after the departure: {other:?}"),
        }
    }
    assert!(
        completed,
        "CR 800.4a: the batch drains rather than stranding — no livelock"
    );

    assert_eq!(
        positive_life_changes(&life_events, P1),
        Vec::<i32>::new(),
        "CR 800.4a: the departed seat's STILL-QUEUED gain must be dropped, not applied \
         — no life-gain event may be emitted for it"
    );
    assert_eq!(
        runner
            .state()
            .players
            .iter()
            .find(|player| player.id == P1)
            .map(|player| player.life)
            .expect("the departed seat keeps its row in the fixed seat vector"),
        20,
        "CR 800.4a: a leaving player gains no life — asserted on the DEPARTED seat's own \
         total, which a surviving seat's gain cannot satisfy"
    );
    assert_eq!(
        runner.life(P0),
        23,
        "the surviving controller's gain still lands — the retain is per-entry, never a \
         blanket null"
    );
    assert!(
        runner
            .state()
            .combat
            .as_ref()
            .map(|combat| combat.regular_damage_done)
            .unwrap_or(false),
        "CR 510.2: the sub-step this batch owns is marked complete on its own live \
         CombatState"
    );
}

// ---------------------------------------------------------------------------
// P1 — the prevention rider crosses the pause with the batch
// ---------------------------------------------------------------------------

/// Weeping Angel's prevention shield, verbatim from
/// `weeping_angel_combat_prevention.rs` (itself pinned by
/// `weeping_angel_prevention_scopes_to_creature_and_rewrites_anaphors`), so this
/// exercises the real parse -> combat -> prevention pipeline.
const PREVENTION_SHIELD: &str =
    "If this creature would deal combat damage to a creature, prevent that \
     damage and that creature's owner shuffles it into their library.";

/// P1 — CR 615.5 + CR 615.13: the batch's aggregate prevention tally survives a
/// CR 616.1 pause and Phase D fires it exactly once, in the drain's completion
/// tail, AFTER the resume.
///
/// This covers the seam this change actually modified:
/// `fire_combat_prevention_riders`' parameter became
/// `&[(AppliedReplacementKey, i32)]` and its call site now reads
/// `&record.prevention_tally` / `&mut record.batch_events` — i.e. the tally is
/// carried across the park inside `PendingCombatLifelink` instead of living in
/// two locals that never crossed a pause. A zero-damage fixture cannot reach it:
/// nothing is prevented, so the tally is empty and Phase D has nothing to fire.
///
/// One batch, two attackers:
///   * the 3/3 lifelinker is unblocked -> 3 damage to P1, whose lifelink gain
///     meets the non-commuting pair and PARKS (CR 616.1);
///   * the 4/4 shielded attacker is blocked by a 1/1, so its 4 combat damage to
///     that creature is fully prevented -> the tally holds 4.
///
/// REVERT-FAILING ASSERTIONS: `last_effect_count` is NOT yet the prevented total
/// at the pause but IS after the resume, and the aggregate `DamagePrevented`
/// appears exactly once in the ANSWERING action's events. Drop
/// `prevention_tally` from the parked record (or fire Phase D before the drain
/// rather than in its completion tail) and both go red.
#[test]
fn prevention_tally_survives_the_pause_and_fires_once_after_the_resume() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // CR 615.5: the shield's rider shuffles a library, so give both seats cards
    // for it to operate on.
    for &pid in &[P0, P1] {
        scenario.with_library_top(pid, &["Card A", "Card B", "Card C"]);
    }
    let lifelinker = add_lifelinker(&mut scenario, P0, "Lifelinker", 3, 3);
    // 4/4 so it survives the 1/1 blocker's damage and the batch is not perturbed
    // by a state-based death.
    let shielded = scenario
        .add_creature_from_oracle(P0, "Weeping Angel", 4, 4, PREVENTION_SHIELD)
        .id();
    let victim = scenario.add_creature(P1, "Potential Victim", 1, 1).id();
    install_competing_life_gain_replacements(&mut scenario, P0);
    let mut runner = scenario.build();

    attack_into_damage(
        &mut runner,
        &[lifelinker, shielded],
        P1,
        &[(victim, shielded)],
    );
    // Capture the PAUSING action's own events, so "Phase D had not fired yet" is
    // an observation rather than an absence nobody looked for.
    let mut pause_events = Vec::new();
    for _ in 0..96 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                match runner.act(GameAction::OrderTriggers { order }) {
                    Ok(result) => pause_events.extend(result.events),
                    Err(_) => break,
                }
            }
            WaitingFor::Priority { .. } => {
                if matches!(
                    runner.state().phase,
                    Phase::EndCombat | Phase::PostCombatMain
                ) {
                    break;
                }
                match runner.act(GameAction::PassPriority) {
                    Ok(result) => pause_events.extend(result.events),
                    Err(_) => break,
                }
            }
            _ => break,
        }
    }

    // Reach guards: the batch really did park, with damage already dealt and the
    // gain not yet applied.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "reach guard: the lifelink gain must park; got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().pending_combat_lifelink.is_some(),
        "reach guard: the unfinished tail — including the prevention tally — is parked"
    );
    assert_eq!(runner.life(P1), 17, "CR 510.2: the damage is already dealt");
    assert_eq!(
        runner.life(P0),
        20,
        "no life is gained while the prompt is open"
    );

    // The discriminating half: Phase D lives in the drain's COMPLETION tail, so
    // NO aggregate `DamagePrevented` may have been emitted by the action that
    // parked the batch.
    assert!(
        !pause_events
            .iter()
            .any(|event| matches!(event, GameEvent::DamagePrevented { .. })),
        "CR 615.5: Phase D must not fire while the CR 616.1 prompt is open — the \
         parking action emitted an aggregate DamagePrevented: {pause_events:?}"
    );

    let (answered, events) = answer_ordering_prompts(&mut runner, DOUBLER);
    assert!(answered >= 1, "the ordering choice was answered");

    // CR 615.13: exactly ONE aggregate `DamagePrevented` for the shield, carrying
    // the whole batch total, emitted in the ANSWERING action — i.e. Phase D ran
    // from the parked tally after the resume, not from a lost local.
    let prevented: Vec<u32> = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::DamagePrevented { amount, .. } => Some(*amount),
            _ => None,
        })
        .collect();
    assert_eq!(
        prevented,
        vec![4],
        "CR 615.5 + CR 615.13: the parked tally fires exactly once, after the \
         resume, against the batch's whole prevented amount"
    );

    // The prevention did not disturb the CR 616.1 ordering the player chose.
    assert_eq!(
        runner.life(P0),
        27,
        "CR 616.1: doubler first then +1 — 3 -> 6 -> 7 — alongside the prevention"
    );
    assert!(
        runner.state().pending_combat_lifelink.is_none(),
        "the batch completes and the record is consumed"
    );
    // CR 615.1a: the shield really did prevent, rather than the attacker simply
    // dealing no damage — the blocked creature left the battlefield through the
    // shield's own CR 615.5 follow-up.
    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Library,
        "CR 615.5: the shield's follow-up moved the blocked creature to its \
         owner's library, so the prevention genuinely applied"
    );
}

// ---------------------------------------------------------------------------
// V9 — the CR 732.2a loop-detection ring observes the batch's own life movement
// ---------------------------------------------------------------------------

/// Seed one frame into the CR 732.2a shortcut sampler so a later CLEAR is
/// observable. `GameState::record_loop_detect_sample` is `pub(crate)` and out of
/// reach from an integration binary, so the frame is built directly; the ring's
/// CONTENTS are irrelevant here — every consumer scans it for a satisfying prior,
/// so all this fixture needs is a non-empty ring whose disappearance is a verdict.
fn seed_loop_detect_ring(runner: &mut GameRunner) {
    let snapshot = runner.state().clone();
    runner
        .state_mut()
        .loop_detect_ring
        .push_back(std::sync::Arc::new(LoopDetectSample {
            normalized: snapshot.clone(),
            live: snapshot,
        }));
}

/// Declare `attacker` as the lone attacker against `P1` and return IMMEDIATELY,
/// with combat damage NOT yet dealt. `P1` controls no creatures on these boards,
/// so no `DeclareBlockers` window ever opens (OBSERVED: the engine goes from the
/// declare-attackers answer to a `Priority` window and then straight into the
/// CR 510.2 batch) — this helper therefore stops at the declaration itself, which
/// is the last beat guaranteed to precede the damage.
fn declare_lone_attacker(runner: &mut GameRunner, attacker: ObjectId) {
    for _ in 0..8 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareAttackers { .. }
        ) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("passing to the declare-attackers window must succeed");
    }
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");
}

/// V9 — CR 732.2a + CR 119.3: the life snapshot the loop-detection ring is
/// invalidated against is taken BEFORE the batch deals damage and is carried in
/// `PendingCombatLifelink`, so the guard call on the PAUSE path still observes
/// the CR 120.3a life loss this batch has already caused.
///
/// A CR 616.1 ordering prompt can stay open arbitrarily long. CR 732.2a lets a
/// player propose a shortcut only from "predictable results", and a ring frame
/// recorded before the batch no longer describes the board once combat damage
/// has moved a life total — so the ring must not survive the pause.
///
/// REVERT-FAILING ASSERTION: `loop_detect_ring.is_empty()` at the prompt.
/// Snapshot `lives_before` at `drain_combat_lifelink`'s entry instead of carrying
/// the pre-batch vector in the record and the drain-entry vector already equals
/// the post-damage board, the pause-path guard observes NO move, and the stale
/// ring survives the prompt.
///
/// SCOPE, stated rather than implied: this owns the PAUSE-path guard call only.
/// The completion-path call is NOT observable through the action API on a resume
/// — `apply()` unconditionally clears `loop_detect_ring` for every action that is
/// neither `PassPriority`/`OrderTriggers`/`BeginResolveAll`/`Respond`- or
/// `RevokeResolveAllConsent` nor an answer to a `WaitingFor::is_forced_cascade_window`,
/// and `WaitingFor::ReplacementChoice` is in neither set. So `ChooseReplacement`
/// empties the ring before `drain_combat_lifelink` is ever re-entered, and any
/// post-resume ring assertion would be vacuously green. See fix-round-2's probe
/// (e) entry.
#[test]
fn parked_batch_invalidates_the_loop_ring_against_the_pre_batch_snapshot() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lifelinker = add_lifelinker(&mut scenario, P0, "Lifelinker", 3, 3);
    install_competing_life_gain_replacements(&mut scenario, P0);
    let mut runner = scenario.build();

    declare_lone_attacker(&mut runner, lifelinker);

    // Seed AFTER the declaration: every action from here to the prompt is a
    // `PassPriority`, which is exempt from `apply()`'s blanket ring clear, so the
    // fate of the ring at the prompt is attributable to the guard alone.
    seed_loop_detect_ring(&mut runner);
    assert_eq!(
        (runner.life(P0), runner.life(P1)),
        (20, 20),
        "reach guard: the seed must predate the batch — CR 510.2 damage is not yet dealt"
    );
    assert!(
        !runner.state().loop_detect_ring.is_empty(),
        "reach guard: the ring is seeded, else the assertion below is vacuous"
    );

    advance_through_combat_damage(&mut runner);

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "reach guard: the batch must PARK on the CR 616.1 ordering choice; got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.life(P1),
        17,
        "reach guard: CR 120.3a — the batch has already moved a life total"
    );
    assert_eq!(
        runner.life(P0),
        20,
        "reach guard: CR 702.15b — the lifelink gain has NOT been applied yet, so the \
         only delta the pause-path guard can see is the damage"
    );

    assert!(
        runner.state().loop_detect_ring.is_empty(),
        "CR 732.2a: the batch moved a life total (CR 120.3a) before parking, so the \
         pre-batch ring frame no longer predicts the board and must be dropped for the \
         duration of the CR 616.1 pause"
    );
}

/// ZERO-CENSUS POSITIVE CONTROL for the row above: the identical drive over a
/// batch that moves NO life must RETAIN the ring. Without this, a ring cleared by
/// the pumping machinery itself — rather than by
/// `invalidate_loop_ring_on_unobserved_life_move` — would read as a pass.
///
/// CR 510.1a: a 0-power attacker assigns no combat damage, so no life moves, no
/// life-gain event is raised, and no CR 616.1 prompt opens. The drain runs with an
/// empty queue and its completion guard compares an unchanged board.
#[test]
fn a_batch_that_moves_no_life_retains_the_loop_ring() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let powerless = add_lifelinker(&mut scenario, P0, "Powerless Lifelinker", 0, 3);
    install_competing_life_gain_replacements(&mut scenario, P0);
    let mut runner = scenario.build();

    declare_lone_attacker(&mut runner, powerless);
    seed_loop_detect_ring(&mut runner);

    advance_through_combat_damage(&mut runner);

    assert_eq!(
        (runner.life(P0), runner.life(P1)),
        (20, 20),
        "reach guard: CR 510.1a — a 0-power attacker moved no life total at all"
    );
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "reach guard: no life-gain event, so no CR 616.1 prompt"
    );
    assert!(
        !runner.state().loop_detect_ring.is_empty(),
        "positive control: the guard observed NO life move, so the ring must SURVIVE \
         this identical drive — this is what proves the row above records a verdict \
         rather than an incidental wipe by the pump"
    );
}

// ---------------------------------------------------------------------------
// The fix's own claim, measured where it is made: on the stack, at the pause
// boundary — no drain, no library, no resolution
// ---------------------------------------------------------------------------

/// CR 603.3b + CR 510.3a: the batch's observers reach the stack TOGETHER, each
/// exactly once, only after the CR 616.1 answer settles.
///
/// This is the durable guard for what the gating change actually produces.
/// `life_gain_trigger_joins_the_combat_damage_trigger_batch` is the end-to-end
/// row and has to drain the stack and keep P0 alive to read a resolved receipt;
/// this row reads the stack itself, so it is immune to both of those hazards and
/// to anything that happens during resolution.
///
/// REVERT-FAILING ASSERTION: the pause-time `stack.is_empty()`. Delete the
/// `|| state.pending_replacement.is_some()` disjunct in `engine_priority.rs` and
/// the damage observer is put on the stack DURING the pause, so the count is 1
/// before the answer instead of 0.
#[test]
fn combat_batch_observers_reach_the_stack_once_as_one_group() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lifelinker = add_lifelinker(&mut scenario, P0, "Lifelinker", 3, 3);
    let magpie = scenario
        .add_creature_from_oracle(P0, "Thieving Magpie", 1, 3, MAGPIE)
        .id();
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
        "reach guard: the batch must PARK on the CR 616.1 choice; got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.life(P0),
        20,
        "reach guard: CR 702.15b — the gain has not been applied yet"
    );
    assert!(
        runner.state().stack.is_empty(),
        "CR 704.3: no triggered ability may be put on the stack while the CR 616.1 \
         prompt is open — no player has received priority"
    );

    let _ = answer_ordering_prompts(&mut runner, DOUBLER);

    // Identities, not just a count: a count alone cannot tell "both observers,
    // once each" from "one observer twice".
    let mut names = runner.stack_names();
    names.sort();
    assert_eq!(
        names,
        vec![
            "Ajani's Pridemate".to_string(),
            "Thieving Magpie".to_string()
        ],
        "CR 510.3a + CR 603.3b: the CR 119.9 gain observer and the CR 603.2 \
         combat-damage observer reach the stack together, each EXACTLY once"
    );
    assert!(
        runner.state().deferred_triggers.is_empty(),
        "nothing may be left parked in the deferred queue once the batch completes"
    );
}

/// CR 119.9 on the UNPAUSED combat-damage batch: one lifelink source causes one
/// life-gain event, so a single "whenever you gain life" observer fires exactly
/// once. No competing replacements, so no CR 616.1 choice is raised and the batch
/// never parks.
///
/// The same board WITH a CR 616.1 pause also fires exactly once: the paused-path
/// regression exercises that receipt through
/// `resumed_lifelink_gain_triggers_single_pridemate_once`.
#[test]
fn single_observer_gain_receipt_fires_once_without_a_pause() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lifelinker = add_lifelinker(&mut scenario, P0, "Lifelinker", 3, 3);
    scenario.add_creature_from_oracle(P0, "Ajani's Pridemate", 2, 2, PRIDEMATE);
    let mut runner = scenario.build();

    attack_into_damage(&mut runner, &[lifelinker], P1, &[]);
    advance_through_combat_damage(&mut runner);
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "reach guard: with no competing pair there is no CR 616.1 choice to make"
    );
    assert_eq!(
        runner.life(P0),
        23,
        "reach guard: CR 702.15b — the gain applied inline, so a life-gain event \
         really did occur for the observer to see"
    );
    for _ in 0..48 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
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
            other => panic!("no further prompt is owed while draining: {other:?}"),
        }
    }
    let counters: u32 = runner
        .state()
        .objects
        .values()
        .filter(|obj| obj.name == "Ajani's Pridemate")
        .map(|obj| {
            obj.counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0)
        })
        .sum();
    assert_eq!(
        counters, 1,
        "CR 119.9: one source, one life-gain event, one fire — on the path that \
         never parks"
    );
}
