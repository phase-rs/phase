//! CR 603.6a + CR 603.2c + CR 614.12a — a token that enters the battlefield AS A COPY, via a
//! choice made while it was entering (Embalm-as-copy), must be observed by enters-the-battlefield
//! abilities exactly like any other creature entering, exactly once.
//!
//! # The defect these rows were written against
//!
//! `handle_copy_target_choice`'s liminal-resume branch answered the `CopyTargetChoice` without
//! clearing it from `state.waiting_for`, so both of its tails echoed the spent prompt back. The
//! action never settled to `WaitingFor::Priority`, `apply_action` skipped
//! `run_post_action_pipeline`, and the entry pair flushed inside
//! `finish_copy_target_choice_entry` was never scanned for CR 603.6a triggers. A board Soul Warden
//! saw nothing.
//!
//! # Measurement predicates (stated, not implied — a figure needs its predicate, not just a value)
//!
//! * **life delta** — `runner.life(seat)` after minus before, per named seat. Never `>=`; the
//!   exactly-once row (CR 603.2c) is decided by the magnitude.
//! * **prompts** — one entry per prompt the ENGINE raised and this driver ANSWERED with a
//!   `runner.act` call. It is a count of prompts, **not** of instrument lines: a prompt observed by
//!   two probe sites is still one prompt. `Drive::prompts` is asserted as the WHOLE vector, never a
//!   slice, so a stale re-echoed prompt cannot hide past the end of the assertion.
//! * **ordering prompts** — entries of that vector whose label starts with `OrderTriggers`.
//! * **board pins** — `count_on_battlefield(state, name)` counts battlefield objects whose
//!   post-copy `name` matches; `token_count` counts battlefield objects with `is_token`.
//!
//! # PREMISE gates (spec §2), run inside every test that reads a delta
//!
//! `premise_p1_after_full_recalc` forces a full rules recalculation (PREMISE-P2) and then makes a
//! PLAIN creature enter through the production token resolver + the production trigger pipeline
//! (PREMISE-P1). A zero read after that is a measured zero; a zero read without it is
//! unattributable, because an observer that was silently discarded reads identically to the defect.
//!
//! # Discrimination (all three mutants measured, see the fix report)
//!
//! * MUTANT-DROP (delete the `state.waiting_for` clear) flips L1/L2/L3's life axis and L1/L2/L3's
//!   whole-vector prompt guard; C1/C2/C3 and every PREMISE-P1 stay green.
//! * MUTANT-TRIVIALIZE (also push the flushed entry event back into `deferred_entry_events`) flips
//!   the exactly-once row — a different row from DROP's.
//! * `p1_break_every_delta_row_reads_zero` breaks the observer itself; EVERY delta row must read 0,
//!   controls included. A row that survives it was never reading the observer — but only where the
//!   LIVE arm of that same board reads non-zero. An assertion whose live counterpart is also 0 is a
//!   pin, not a break control, and is labelled as such: the ordering-prompt axis is discriminated
//!   ONLY by the L2-shaped arm (two differing observers, live = 1, broken = 0), never by the
//!   single-observer boards, where both arms read 0.

use engine::game::layers::{flush_layers, mark_layers_full};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::{drain_order_triggers_with_identity, process_triggers};
use engine::types::ability::{
    Effect, PtValue, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Verbatim Oracle text (paraphrases can take a different parser branch and go green while the
/// real card stays broken).
const SOUL_WARDEN_ORACLE: &str = "Whenever another creature enters, you gain 1 life.";
const VIZIER_ORACLE: &str = "You may have this creature enter as a copy of any creature on the battlefield, except if this creature was embalmed, the token has no mana cost, it's white, and it's a Zombie in addition to its other types.\nEmbalm {3}{U}{U}";
const PAINTERS_SERVANT_ORACLE: &str = "As this creature enters, choose a color.\nAll cards that aren't on the battlefield, spells, and permanents are the chosen color in addition to their other colors.";
const SPARK_DOUBLE_ORACLE: &str = "You may have this creature enter as a copy of a creature or planeswalker you control, except it enters with an additional +1/+1 counter on it if it's a creature, it enters with an additional loyalty counter on it if it's a planeswalker, and it isn't legendary.";
const METAMORPHIC_ALTERATION_ORACLE: &str = "Enchant creature\nAs this Aura enters, choose a creature.\nEnchanted creature is a copy of the chosen creature.";
/// A SECOND enters-the-battlefield observer whose resolution function DIFFERS from Soul Warden's.
/// MEASURED necessity: `triggers::group_is_order_independent` auto-orders a group whose members
/// normalize to the same ability (CR 603.3b — permuting indistinguishable triggers is immaterial),
/// so two Soul Wardens raise NO ordering prompt. An `Untap` sibling makes the group genuinely
/// order-dependent, which is what surfaces the player-facing prompt L2 asserts.
const MIDNIGHT_GUARD_ORACLE: &str = "Whenever another creature enters, untap this creature.";

// ───────────────────────────────── observables ─────────────────────────────────

fn count_on_battlefield(state: &GameState, name: &str) -> usize {
    state
        .battlefield
        .iter()
        .filter(|id| state.objects.get(id).is_some_and(|o| o.name == name))
        .count()
}

/// CR 608.2i battlefield-entry rows recorded this turn for `name`.
///
/// This is the right "did it enter" pin for the DECLINED Embalm route: Vizier of Many Faces is a
/// printed 0/0, so its plain (non-copy) token dies to the CR 704.5f state-based action the instant
/// SBAs run. Board presence would read 0 for an entry that demonstrably happened.
fn entry_rows_for(state: &GameState, name: &str) -> usize {
    state
        .battlefield_entries_this_turn
        .iter()
        .filter(|record| record.name == name)
        .count()
}

fn token_count(state: &GameState) -> usize {
    state
        .battlefield
        .iter()
        .filter(|id| state.objects.get(id).is_some_and(|o| o.is_token))
        .count()
}

/// The variant name of a `WaitingFor`, with its payload dropped — the settlement axis.
fn wf_label(waiting_for: &WaitingFor) -> String {
    let rendered = format!("{waiting_for:?}");
    rendered
        .split_once(" {")
        .map_or(rendered.as_str(), |(head, _)| head)
        .to_string()
}

/// What one answered prompt did, recorded from outside the engine.
#[derive(Debug, Clone)]
struct Step {
    /// The prompt label this action answered.
    answered: String,
    /// `state.waiting_for` as observed from OUTSIDE the engine once the action returned — i.e. the
    /// POST-pipeline value, **not** the value `apply_action`'s `run_post_action_pipeline` gate
    /// tested. The two differ whenever the pipeline itself raises a prompt: L2 reads
    /// `OrderTriggers` here precisely BECAUSE the pipeline ran. It is still a sound settlement
    /// proxy for L1/L3, where a non-`Priority` reading can only be the un-cleared prompt (pre-fix
    /// this field reads `CopyTargetChoice`), but do not read it as "the gate saw this".
    waiting_after: String,
    /// Life of (P0, P1) after the action.
    life_after: (i32, i32),
}

#[derive(Debug)]
struct Drive {
    /// PREDICATE: one entry per prompt raised AND answered. Prompts, not instrument lines.
    prompts: Vec<String>,
    steps: Vec<Step>,
    /// The object the `CopyTargetChoice` was raised for (the Embalm token / the copy spell).
    copy_subject: Option<ObjectId>,
}

impl Drive {
    /// PREDICATE: ordering PROMPTS answered — one per `runner.act(OrderTriggers { .. })`, counted
    /// off the answered steps rather than off any instrument that merely observed one.
    fn ordering_prompts(&self) -> usize {
        self.steps
            .iter()
            .filter(|step| step.answered.starts_with("OrderTriggers"))
            .count()
    }

    /// Per-action life trace: `(prompt answered, P0 life after, P1 life after)`. Distinguishes
    /// "the observer fired during this action" from "it fired on a later beat".
    fn life_trace(&self) -> Vec<(&str, i32, i32)> {
        self.steps
            .iter()
            .map(|step| (step.answered.as_str(), step.life_after.0, step.life_after.1))
            .collect()
    }

    fn copy_subject(&self) -> ObjectId {
        self.copy_subject.unwrap_or_else(|| {
            panic!(
                "the copy-target prompt must be reached; prompts seen = {:?}",
                self.prompts
            )
        })
    }
}

// ───────────────────────────────── fixtures ─────────────────────────────────

/// A graveyard Vizier of Many Faces with its synthesized Embalm ability, plus the {3}{U}{U} the
/// activation costs. `with_mana_pool` ADDS to the pool, so calling this twice funds two Embalms.
fn stage_embalm_vizier(scenario: &mut GameScenario) -> ObjectId {
    let vizier = scenario
        .add_creature_to_graveyard(P0, "Vizier of Many Faces", 0, 0)
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Blue],
        })
        .from_oracle_text_with_keywords(&["Embalm"], VIZIER_ORACLE)
        .id();
    scenario.with_mana_pool(
        P0,
        [
            ManaType::Blue,
            ManaType::Blue,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ]
        .into_iter()
        .map(|m| ManaUnit::new(m, ObjectId(0), false, vec![]))
        .collect(),
    );
    vizier
}

/// Stage the ETB observer. `live == false` is the DELIBERATE P1 BREAK: the same card, the same
/// name, the same board position, with no Oracle text and therefore no trigger.
fn stage_observer(scenario: &mut GameScenario, seat: PlayerId, live: bool) {
    if live {
        scenario.add_creature_from_oracle(seat, "Soul Warden", 1, 1, SOUL_WARDEN_ORACLE);
    } else {
        scenario.add_creature(seat, "Soul Warden", 1, 1);
    }
}

/// Resolve one plain 1/1 creature token through the production token resolver and run the
/// production trigger pipeline over what it emitted, then let the trigger resolve.
fn enter_plain_creature(runner: &mut GameRunner) {
    let source = *runner
        .state()
        .battlefield
        .iter()
        .next()
        .expect("the premise board is non-empty");
    let ability = ResolvedAbility::new(
        Effect::Token {
            name: "Saproling".to_string(),
            power: PtValue::Fixed(1),
            toughness: PtValue::Fixed(1),
            types: vec!["Creature".to_string()],
            colors: Vec::new(),
            keywords: Vec::new(),
            tapped: false,
            count: QuantityExpr::Fixed { value: 1 },
            owner: TargetFilter::Controller,
            attach_to: None,
            enters_attacking: false,
            supertypes: Vec::new(),
            static_abilities: Vec::new(),
            enter_with_counters: Vec::new(),
        },
        Vec::new(),
        source,
        P0,
    );
    let mut events = Vec::new();
    engine::game::effects::token::resolve(runner.state_mut(), &ability, &mut events)
        .expect("the plain creature token resolves");
    process_triggers(runner.state_mut(), &events);
    drain_order_triggers_with_identity(runner.state_mut());
    runner.advance_until_stack_empty();
}

/// PREMISE-P2 then PREMISE-P1 (spec §2), in that order, on the LIVE board of the test that calls
/// it. Returns the (P0, P1) life delta a plain creature entry produced.
///
/// PREMISE-P2: `mark_layers_full` + `flush_layers` is a full rules recalculation. Running it BEFORE
/// P1 means P1's non-zero is measured on an observer that has already survived one.
fn premise_p1_after_full_recalc(runner: &mut GameRunner) -> (i32, i32) {
    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());
    let before = (runner.life(P0), runner.life(P1));
    enter_plain_creature(runner);
    (runner.life(P0) - before.0, runner.life(P1) - before.1)
}

// ───────────────────────────────── the driver ─────────────────────────────────

/// Answer every prompt until the action settles and the stack drains, recording each answer and
/// the `WaitingFor` it produced.
///
/// `copy_target` names the battlefield creature a `CopyTargetChoice` must pick; `None` DECLINES the
/// first `ReplacementChoice` (the "enter as a copy" offer), routing a token entry through the
/// ordinary emit path instead.
fn drive_prompts(runner: &mut GameRunner, copy_target: Option<&str>) -> Drive {
    let mut drive = Drive {
        prompts: Vec::new(),
        steps: Vec::new(),
        copy_subject: None,
    };
    let mut replacements_answered = 0_usize;
    for _ in 0..64 {
        let (label, action) = match runner.state().waiting_for.clone() {
            WaitingFor::ManaPayment { .. } | WaitingFor::Priority { .. } => {
                let entry_done = drive.copy_subject.is_some() || copy_target.is_none();
                if entry_done && runner.state().stack.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).expect("pass priority");
                continue;
            }
            WaitingFor::ReplacementChoice { candidates, .. } => {
                // The FIRST replacement choice is the optional "enter as a copy"; index 1 declines
                // it. Any later one is a CR 616.1 ordering between simultaneous replacements.
                let index = usize::from(replacements_answered == 0 && copy_target.is_none());
                replacements_answered += 1;
                (
                    format!("ReplacementChoice({})", candidates.len()),
                    GameAction::ChooseReplacement { index },
                )
            }
            WaitingFor::CopyTargetChoice {
                source_id,
                valid_targets,
                ..
            } => {
                let wanted = copy_target.expect("declining must not raise a copy-target prompt");
                let target = *valid_targets
                    .iter()
                    .find(|id| {
                        runner
                            .state()
                            .objects
                            .get(id)
                            .is_some_and(|object| object.name == wanted)
                    })
                    .unwrap_or_else(|| panic!("{wanted} must be a legal copy target"));
                drive.copy_subject = Some(source_id);
                (
                    "CopyTargetChoice".to_string(),
                    GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    },
                )
            }
            WaitingFor::NamedChoice { options, .. } => (
                format!("NamedChoice({})", options.len()),
                GameAction::ChooseOption {
                    choice: options
                        .first()
                        .expect("a mandatory named choice offers at least one option")
                        .clone(),
                },
            ),
            // CR 603.3b: simultaneous same-controller triggers surface an ordering prompt. This is
            // the row that proves the entry reached trigger COLLECTION as a genuine event.
            WaitingFor::OrderTriggers { triggers, .. } => (
                format!("OrderTriggers({})", triggers.len()),
                GameAction::OrderTriggers {
                    order: (0..triggers.len()).collect(),
                },
            ),
            other => {
                drive.prompts.push(wf_label(&other));
                break;
            }
        };
        runner
            .act(action)
            .unwrap_or_else(|err| panic!("answering {label} failed: {err:?}"));
        drive.prompts.push(label.clone());
        drive.steps.push(Step {
            answered: label,
            waiting_after: wf_label(&runner.state().waiting_for),
            life_after: (runner.life(P0), runner.life(P1)),
        });
    }
    runner.advance_until_stack_empty();
    drive
}

fn drive_embalm(runner: &mut GameRunner, vizier: ObjectId, copy_target: Option<&str>) -> Drive {
    let embalm_index = runner.state().objects[&vizier]
        .abilities
        .iter()
        .position(|ability| matches!(&*ability.effect, Effect::CopyTokenOf { .. }))
        .expect("the synthesized Embalm ability is on the graveyard Vizier");
    runner
        .act(GameAction::ActivateAbility {
            source_id: vizier,
            ability_index: embalm_index,
        })
        .expect("activate Embalm");
    drive_prompts(runner, copy_target)
}

/// The Embalm-as-copy board: a graveyard Vizier, a Grizzly Bears to copy (no ETB of its own, so
/// every fire counted below belongs to an observer), and one observer per named seat.
fn embalm_board(observer_seats: &[PlayerId], observers_live: bool) -> (GameRunner, ObjectId) {
    embalm_board_with(observer_seats, observers_live, None)
}

/// `extra_observer` stages one ADDITIONAL P0 observer from `(name, oracle_text)` — used only where
/// a trigger group must be order-DEPENDENT (see `MIDNIGHT_GUARD_ORACLE`).
fn embalm_board_with(
    observer_seats: &[PlayerId],
    observers_live: bool,
    extra_observer: Option<(&str, i32, i32, &str)>,
) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let vizier = stage_embalm_vizier(&mut scenario);
    scenario.add_creature(P0, "Grizzly Bears", 2, 2);
    for seat in observer_seats {
        stage_observer(&mut scenario, *seat, observers_live);
    }
    if let Some((name, power, toughness, oracle)) = extra_observer {
        if observers_live {
            scenario.add_creature_from_oracle(P0, name, power, toughness, oracle);
        } else {
            scenario.add_creature(P0, name, power, toughness);
        }
    }
    (scenario.build(), vizier)
}

/// Assert the copy token is on the battlefield under its post-copy name. This is L1/L2/L3's PINNED
/// co-observation: it holds in BOTH arms of the fix, so a measured 0 is "the observer did not
/// fire", never "nothing entered".
fn assert_copy_token_entered(runner: &GameRunner, drive: &Drive) {
    let token = drive.copy_subject();
    let object = runner
        .state()
        .objects
        .get(&token)
        .expect("the Embalm token object exists");
    assert_eq!(
        object.zone,
        Zone::Battlefield,
        "PIN: the copy token is on the battlefield; prompts = {:?}",
        drive.prompts
    );
    assert_eq!(
        object.name, "Grizzly Bears",
        "PIN: it entered as the chosen copy (CR 614.12a); prompts = {:?}",
        drive.prompts
    );
    assert!(object.is_token, "PIN: the entrant is the Embalm token");
}

/// Every battlefield object's name, tokens suffixed `*`, sorted. The board pin, printed.
fn board(runner: &GameRunner) -> String {
    let state = runner.state();
    let mut names: Vec<String> = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .map(|o| format!("{}{}", o.name, if o.is_token { "*" } else { "" }))
        .collect();
    names.sort();
    format!("[{}]", names.join(", "))
}

/// TEAM-RULE per-arm runtime signature, emitted from INSIDE the run: the axis under test plus the
/// pinned co-observations that must NOT move between arms. Without both halves, the axis alone
/// cannot separate "the switch worked" from "everything changed", and the pins alone cannot
/// separate "correctly pinned" from "nothing ran".
fn signature(row: &str, axis: &str, pins: &str) {
    eprintln!("FUAB-ROW {row} | AXIS {axis} | PINS {pins}");
}

// ───────────────────────────────── contract rows ─────────────────────────────────

/// **L1 + N1** — CR 603.6a: an Embalm-as-copy token entering the battlefield is checked against
/// every permanent for enters-the-battlefield triggers, so one board observer gains its controller
/// exactly 1 life. CR 603.2c: one entry is one occurrence, so the magnitude is 1 and never 2.
///
/// AXIS: P0 life delta across the Embalm drive, 0 → +1.
/// PINS (must not move between the pre-fix and post-fix arms): the copy token is on the
/// battlefield under its post-copy name; the observer count is 1; the token count is 2 (the
/// PREMISE-P1 Saproling plus the copy); P1's life is untouched.
#[test]
fn l1_liminal_copy_token_is_observed_exactly_once() {
    let (mut runner, vizier) = embalm_board(&[P0], true);

    // PREMISE-P2 then PREMISE-P1: the instrument is proven able to read non-zero, on this board,
    // in this run, after a full rules recalculation — before any zero below is admissible.
    let premise = premise_p1_after_full_recalc(&mut runner);
    let before = (runner.life(P0), runner.life(P1));
    let drive = drive_embalm(&mut runner, vizier, Some("Grizzly Bears"));
    let delta = (runner.life(P0) - before.0, runner.life(P1) - before.1);
    signature(
        "L1+N1",
        &format!(
            "life_delta_P0={} settled_after_copy_answer={} ordering_prompts={}",
            delta.0,
            drive.steps.get(1).map_or("<none>", |s| &s.waiting_after),
            drive.ordering_prompts()
        ),
        &format!(
            "premise_p1={premise:?} board={} observers={} tokens={} opp_life_delta={} \
             prompts={:?} life_trace={:?}",
            board(&runner),
            count_on_battlefield(runner.state(), "Soul Warden"),
            token_count(runner.state()),
            delta.1,
            drive.prompts,
            drive.life_trace()
        ),
    );

    assert_eq!(
        premise,
        (1, 0),
        "PREMISE-P1/P2: a plain creature entering must move the observer's controller by exactly 1"
    );
    // Whole-vector reach guard — never a slice. Pre-fix this vector carries a THIRD entry, the
    // stale `CopyTargetChoice` the unsettled action echoed back, and this assertion is what
    // MUTANT-DROP flips first.
    assert_eq!(
        drive.prompts,
        vec![
            "ReplacementChoice(2)".to_string(),
            "CopyTargetChoice".to_string(),
        ],
        "the copy-target answer is the LAST prompt: it settles, so the spent prompt is not echoed"
    );
    // The settlement axis, read directly: CR 603.3 owes the triggered abilities a priority
    // boundary, and `apply_action` only runs the trigger scan when the action reaches one.
    assert_eq!(
        drive.steps[1].waiting_after, "Priority",
        "the copy-target answer settles to Priority; steps = {:?}",
        drive.steps
    );

    assert_copy_token_entered(&runner, &drive);
    assert_eq!(
        count_on_battlefield(runner.state(), "Soul Warden"),
        1,
        "PIN: exactly one observer"
    );
    assert_eq!(
        token_count(runner.state()),
        2,
        "PIN: the PREMISE-P1 Saproling and the copy token"
    );
    assert_eq!(
        delta.0, 1,
        "L1 + N1 (CR 603.6a + CR 603.2c): exactly one fire — not 0, not 2; steps = {:?}",
        drive.steps
    );
    assert_eq!(delta.1, 0, "PIN: the opponent controls no observer");
}

/// **L2** — CR 603.3b: simultaneous same-controller triggers must be ORDERED by their controller.
/// A single fire can be explained by a resolution-time shortcut; a player-facing ordering prompt
/// can only arise if the entry reached trigger COLLECTION as a genuine event. This is the row that
/// proves the entry rejoined the normal path rather than being special-cased.
///
/// The board carries THREE observers, not two, and that is a measured requirement rather than a
/// preference: `triggers::group_is_order_independent` auto-orders a group whose members normalize
/// to one ability, so two Soul Wardens alone gain +2 and raise NOTHING (measured on the C1 route at
/// `8b46e2517`). Midnight Guard's `Untap` makes the group order-DEPENDENT. The two Soul Wardens
/// still carry the +2 magnitude the contract asks for.
///
/// AXIS: ordering prompts 0 → 1, P0 life delta 0 → +2.
/// PINS: all observers still on the battlefield; the copy token entered under its copy name.
#[test]
fn l2_two_observers_raise_the_ordering_prompt() {
    let (mut runner, vizier) = embalm_board_with(
        &[P0, P0],
        true,
        Some(("Midnight Guard", 2, 3, MIDNIGHT_GUARD_ORACLE)),
    );

    let premise = premise_p1_after_full_recalc(&mut runner);
    let before = (runner.life(P0), runner.life(P1));
    let drive = drive_embalm(&mut runner, vizier, Some("Grizzly Bears"));
    let delta = (runner.life(P0) - before.0, runner.life(P1) - before.1);
    signature(
        "L2",
        &format!(
            "life_delta_P0={} ordering_prompts={} settled_after_copy_answer={}",
            delta.0,
            drive.ordering_prompts(),
            drive.steps.get(1).map_or("<none>", |s| &s.waiting_after)
        ),
        &format!(
            "premise_p1={premise:?} board={} observers={} tokens={} opp_life_delta={} prompts={:?}",
            board(&runner),
            count_on_battlefield(runner.state(), "Soul Warden"),
            token_count(runner.state()),
            delta.1,
            drive.prompts
        ),
    );

    assert_eq!(
        premise,
        (2, 0),
        "PREMISE-P1/P2: two live observers move their controller by exactly 2 on a plain entry"
    );
    assert_eq!(
        drive.prompts,
        vec![
            "ReplacementChoice(2)".to_string(),
            "CopyTargetChoice".to_string(),
            "OrderTriggers(3)".to_string(),
        ],
        "the settled copy-target answer collects ALL THREE observer triggers off the one entry and \
         asks their controller for an order (CR 603.3b)"
    );
    // PREDICATE: prompts raised and answered, not instrument lines.
    assert_eq!(
        drive.ordering_prompts(),
        1,
        "exactly one CR 603.3b ordering prompt for the two simultaneous triggers"
    );

    assert_copy_token_entered(&runner, &drive);
    assert_eq!(
        count_on_battlefield(runner.state(), "Soul Warden"),
        2,
        "PIN: both observers"
    );
    assert_eq!(
        delta.0, 2,
        "L2: both observers see the same single entry (CR 603.6a); steps = {:?}",
        drive.steps
    );
}

/// **L3** — CR 603.6a checks ALL permanents on the battlefield, not just the entrant's
/// controller's. An opponent's observer must see the copy token enter.
///
/// AXIS: P1 life delta 0 → +1. PINS: P0's life unmoved; the copy token on the battlefield.
#[test]
fn l3_opponents_observer_sees_the_copy_token_enter() {
    let (mut runner, vizier) = embalm_board(&[P1], true);

    let premise = premise_p1_after_full_recalc(&mut runner);
    let before = (runner.life(P0), runner.life(P1));
    let drive = drive_embalm(&mut runner, vizier, Some("Grizzly Bears"));
    let delta = (runner.life(P0) - before.0, runner.life(P1) - before.1);
    signature(
        "L3",
        &format!(
            "life_delta_P1={} settled_after_copy_answer={}",
            delta.1,
            drive.steps.get(1).map_or("<none>", |s| &s.waiting_after)
        ),
        &format!(
            "premise_p1={premise:?} board={} observers={} controller_life_delta={} prompts={:?}",
            board(&runner),
            count_on_battlefield(runner.state(), "Soul Warden"),
            delta.0,
            drive.prompts
        ),
    );

    assert_eq!(
        premise,
        (0, 1),
        "PREMISE-P1/P2: the OPPONENT's observer is the live one on this board"
    );
    assert_eq!(
        drive.prompts,
        vec![
            "ReplacementChoice(2)".to_string(),
            "CopyTargetChoice".to_string(),
        ],
        "the copy-target answer settles; no stale prompt is echoed"
    );
    assert_copy_token_entered(&runner, &drive);
    assert_eq!(
        delta.1, 1,
        "L3: the opponent's observer gains ITS controller 1 life; steps = {:?}",
        drive.steps
    );
    assert_eq!(
        delta.0, 0,
        "PIN: the token's controller controls no observer"
    );
}

/// **C1** — the permanent-SPELL copy route (Spark Double entering as a copy of Painter's Servant),
/// which settles today. Its life delta must be unchanged by the fix. This is one of the arms that
/// proves the switch is live rather than constant: it reads +2 while L1 reads 0 pre-fix.
///
/// Its two observers are indistinguishable, so CR 603.3b's immaterial-choice optimization
/// auto-orders them and no ordering prompt reaches this driver (MEASURED 0 at `8b46e2517`, both
/// pre- and post-fix). That is why L2 needs a third, differing observer.
#[test]
fn c1_permanent_spell_copy_route_unchanged() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Painter's Servant", 1, 3, PAINTERS_SERVANT_ORACLE);
    stage_observer(&mut scenario, P0, true);
    stage_observer(&mut scenario, P0, true);
    let spark = scenario
        .add_creature_to_hand_from_oracle(P0, "Spark Double", 0, 0, SPARK_DOUBLE_ORACLE)
        .id();
    let mut runner = scenario.build();

    let premise = premise_p1_after_full_recalc(&mut runner);
    let before = (runner.life(P0), runner.life(P1));
    let card_id = runner.state().objects[&spark].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spark,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Spark Double");
    runner.advance_until_stack_empty();
    let drive = drive_prompts(&mut runner, Some("Painter's Servant"));
    let delta = (runner.life(P0) - before.0, runner.life(P1) - before.1);
    signature(
        "C1",
        &format!(
            "life_delta_P0={} ordering_prompts={}",
            delta.0,
            drive.ordering_prompts()
        ),
        &format!(
            "premise_p1={premise:?} board={} observers={} copy_name={} prompts={:?}",
            board(&runner),
            count_on_battlefield(runner.state(), "Soul Warden"),
            runner.state().objects[&spark].name,
            drive.prompts
        ),
    );

    assert_eq!(premise, (2, 0), "PREMISE-P1/P2: both observers are live");
    assert_eq!(
        drive.prompts,
        vec![
            "ReplacementChoice(2)".to_string(),
            "CopyTargetChoice".to_string(),
            "NamedChoice(5)".to_string(),
        ],
        "the permanent-spell copy route already settles; its observer triggers are collected and \
         ordered inside the drain, not surfaced as a prompt to this driver"
    );
    assert_eq!(
        runner.state().objects[&spark].name,
        "Painter's Servant",
        "PIN: the copy is on the battlefield as the chosen creature"
    );
    assert_eq!(
        count_on_battlefield(runner.state(), "Soul Warden"),
        2,
        "PIN: observer count unchanged"
    );
    assert_eq!(
        delta.0, 2,
        "C1: unchanged across the fix; steps = {:?}",
        drive.steps
    );
}

/// **C2** — a plain creature entering the battlefield with no copy and no mid-entry choice. The
/// same Embalm activation with its enter-as-a-copy replacement DECLINED puts an ordinary Zombie
/// Vizier token onto the battlefield through the emit path.
#[test]
fn c2_plain_creature_entry_unchanged() {
    let (mut runner, vizier) = embalm_board(&[P0], true);

    let premise = premise_p1_after_full_recalc(&mut runner);
    let before = (runner.life(P0), runner.life(P1));
    let drive = drive_embalm(&mut runner, vizier, None);
    let delta = (runner.life(P0) - before.0, runner.life(P1) - before.1);
    signature(
        "C2",
        &format!("life_delta_P0={}", delta.0),
        &format!(
            "premise_p1={premise:?} board={} observers={} entry_rows={} prompts={:?}",
            board(&runner),
            count_on_battlefield(runner.state(), "Soul Warden"),
            entry_rows_for(runner.state(), "Vizier of Many Faces"),
            drive.prompts
        ),
    );

    assert_eq!(premise, (1, 0), "PREMISE-P1/P2: the observer is live");
    assert_eq!(
        drive.prompts,
        vec!["ReplacementChoice(2)".to_string()],
        "declining the copy replacement raises no copy-target prompt"
    );
    assert_eq!(
        entry_rows_for(runner.state(), "Vizier of Many Faces"),
        1,
        "PIN: the declined token DID enter (CR 608.2i row) — it then died to CR 704.5f as a \
         printed 0/0, which is why board presence is not the pin here"
    );
    assert_eq!(
        delta.0, 1,
        "C2: unchanged across the fix; steps = {:?}",
        drive.steps
    );
}

/// **C3** — the deferral machinery is SHARED with a `CopyTargetChoice` that is not an entry at all.
/// Metamorphic Alteration's `PersistChosenAttribute` purpose answers the same prompt on the same
/// handler, but installs a copy onto an already-resident host; nothing enters the battlefield as a
/// creature, so no observer may fire and no spurious prompt may appear.
///
/// PIN (spec §4.1.1, the subtlest one): the choice WAS answered — the host really became the
/// chosen creature. Without it, "no effect" passes trivially for a run that did nothing.
///
/// SCOPE, stated so nobody reads more into a green C3 than it earns: `PersistChosenAttribute`
/// returns from this handler ABOVE the liminal branch this fix edits, so C3 cannot move under
/// either mutant and contributes **no** discrimination for THIS fix. It is kept because it does
/// discriminate a different fix shape — one that had loosened a shared gate instead of clearing a
/// spent prompt would start firing entry triggers here.
#[test]
fn c3_non_entry_copy_choice_raises_no_observer_effect() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let donor = scenario
        .add_creature_from_oracle(P0, "Serra Angel", 4, 4, "Flying, vigilance")
        .id();
    let host = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    stage_observer(&mut scenario, P0, true);
    let aura = scenario
        .add_spell_to_hand(P0, "Metamorphic Alteration", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Blue],
        })
        .from_oracle_text_with_keywords(&["enchant"], METAMORPHIC_ALTERATION_ORACLE)
        .id();
    scenario.with_mana_pool(
        P0,
        [
            ManaType::Blue,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ]
        .into_iter()
        .map(|m| ManaUnit::new(m, ObjectId(0), false, vec![]))
        .collect(),
    );
    let mut runner = scenario.build();

    let premise = premise_p1_after_full_recalc(&mut runner);
    let before = (runner.life(P0), runner.life(P1));
    runner
        .cast(aura)
        .target_object(host)
        .copy_target(donor)
        .resolve();
    let delta = (runner.life(P0) - before.0, runner.life(P1) - before.1);
    signature(
        "C3",
        &format!("life_delta_P0={} life_delta_P1={}", delta.0, delta.1),
        &format!(
            "premise_p1={premise:?} board={} host_name={} waiting_for={} (the choice WAS consumed)",
            board(&runner),
            runner.state().objects[&host].name,
            wf_label(&runner.state().waiting_for)
        ),
    );

    assert_eq!(premise, (1, 0), "PREMISE-P1/P2: the observer is live");
    assert_eq!(
        runner.state().objects[&host].name,
        "Serra Angel",
        "PIN: the copy choice WAS consumed and applied (CR 707.2c) — this is not an empty run"
    );
    assert_eq!(
        wf_label(&runner.state().waiting_for),
        "Priority",
        "C3: no spurious prompt is left resident"
    );
    assert_eq!(
        delta,
        (0, 0),
        "C3: a non-entry copy choice fires no enters-the-battlefield observer"
    );
}

/// **P1-BREAK control** — the observer installation is deliberately broken (same card, same board
/// position, no Oracle text and therefore no trigger). EVERY delta row must read 0, the controls
/// included. A row that still reads non-zero was never reading the observer, and every zero it
/// reports elsewhere is unattributable.
///
/// This closes the honest gap the spec carried for two rounds: it proves, by CONSTRUCTION rather
/// than by analogy with sibling arms, that these rows are wired to the observer.
#[test]
fn p1_break_every_delta_row_reads_zero() {
    // Row L1/N1 with a broken observer.
    let (mut runner, vizier) = embalm_board(&[P0], false);
    let premise = premise_p1_after_full_recalc(&mut runner);
    let before = runner.life(P0);
    let drive = drive_embalm(&mut runner, vizier, Some("Grizzly Bears"));
    let delta = runner.life(P0) - before;
    signature(
        "P1BREAK-L1",
        &format!(
            "life_delta_P0={delta} ordering_prompts={}",
            drive.ordering_prompts()
        ),
        &format!(
            "premise_p1={premise:?} board={} prompts={:?}",
            board(&runner),
            drive.prompts
        ),
    );
    assert_eq!(
        premise,
        (0, 0),
        "P1-BREAK: the premise gate itself must read 0 — that is the break working"
    );
    assert_copy_token_entered(&runner, &drive);
    // PIN, deliberately NOT the L2 axis: this board has ONE observer, so the LIVE arm reads 0 here
    // too (a lone trigger never needs ordering). Labelling it "the L2 axis" would be a vacuous
    // negative — an assertion that cannot fail when the observer is live. The real L2 break arm is
    // below, on a board whose live counterpart measures 1.
    assert_eq!(
        drive.ordering_prompts(),
        0,
        "PIN: one observer raises no ordering prompt in either arm"
    );
    assert_eq!(
        delta, 0,
        "P1-BREAK L1: the row reads 0 with the observer broken, while the entry PIN still holds"
    );

    // Row L3 with a broken opponent observer.
    let (mut runner, vizier) = embalm_board(&[P1], false);
    let before = runner.life(P1);
    let drive = drive_embalm(&mut runner, vizier, Some("Grizzly Bears"));
    let delta = runner.life(P1) - before;
    signature(
        "P1BREAK-L3",
        &format!("life_delta_P1={delta}"),
        &format!("board={} prompts={:?}", board(&runner), drive.prompts),
    );
    assert_copy_token_entered(&runner, &drive);
    assert_eq!(delta, 0, "P1-BREAK L3: the opponent row reads 0");

    // Row C2 (plain creature entry) with a broken observer.
    let (mut runner, vizier) = embalm_board(&[P0], false);
    let before = runner.life(P0);
    let drive = drive_embalm(&mut runner, vizier, None);
    let delta = runner.life(P0) - before;
    signature(
        "P1BREAK-C2",
        &format!("life_delta_P0={delta}"),
        &format!(
            "board={} entry_rows={} prompts={:?}",
            board(&runner),
            entry_rows_for(runner.state(), "Vizier of Many Faces"),
            drive.prompts
        ),
    );
    assert_eq!(
        entry_rows_for(runner.state(), "Vizier of Many Faces"),
        1,
        "PIN: the declined token still entered (CR 608.2i row)"
    );
    assert_eq!(
        delta, 0,
        "P1-BREAK C2: the plain-entry CONTROL reads 0 too — it was reading the observer, not luck"
    );

    // Row C1 (permanent-spell copy) with broken observers.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Painter's Servant", 1, 3, PAINTERS_SERVANT_ORACLE);
    stage_observer(&mut scenario, P0, false);
    stage_observer(&mut scenario, P0, false);
    let spark = scenario
        .add_creature_to_hand_from_oracle(P0, "Spark Double", 0, 0, SPARK_DOUBLE_ORACLE)
        .id();
    let mut runner = scenario.build();
    let before = runner.life(P0);
    let card_id = runner.state().objects[&spark].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spark,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Spark Double");
    runner.advance_until_stack_empty();
    let drive = drive_prompts(&mut runner, Some("Painter's Servant"));
    let delta = runner.life(P0) - before;
    signature(
        "P1BREAK-C1",
        &format!(
            "life_delta_P0={delta} ordering_prompts={}",
            drive.ordering_prompts()
        ),
        &format!(
            "board={} copy_name={} prompts={:?}",
            board(&runner),
            runner.state().objects[&spark].name,
            drive.prompts
        ),
    );
    assert_eq!(
        runner.state().objects[&spark].name,
        "Painter's Servant",
        "PIN: the copy still entered as the chosen creature"
    );
    // PIN, not an axis: C1's live arm also reads 0 ordering prompts (its two observers normalize to
    // one ability, so CR 603.3b auto-orders them — see the L2 doc). Stated so this is not misread as
    // a break control for the ordering axis.
    assert_eq!(
        drive.ordering_prompts(),
        0,
        "PIN: C1 raises no ordering prompt in either arm"
    );
    assert_eq!(
        delta, 0,
        "P1-BREAK C1: the permanent-spell CONTROL reads 0 too"
    );

    // Row L2 with broken observers — THE ordering-axis break control. This is the only P1-break
    // board whose live counterpart measures a non-zero ordering-prompt count (L2 reads 1), so it is
    // the only one that can discriminate the axis. Without it, L2's 0 → 1 axis had no break arm at
    // all and the suite's "a row that survives the break was never reading the observer" claim did
    // not hold for it.
    let (mut runner, vizier) = embalm_board_with(
        &[P0, P0],
        false,
        Some(("Midnight Guard", 2, 3, MIDNIGHT_GUARD_ORACLE)),
    );
    let premise = premise_p1_after_full_recalc(&mut runner);
    let before = runner.life(P0);
    let drive = drive_embalm(&mut runner, vizier, Some("Grizzly Bears"));
    let delta = runner.life(P0) - before;
    signature(
        "P1BREAK-L2",
        &format!(
            "life_delta_P0={delta} ordering_prompts={}",
            drive.ordering_prompts()
        ),
        &format!(
            "premise_p1={premise:?} board={} observers={} prompts={:?}",
            board(&runner),
            count_on_battlefield(runner.state(), "Soul Warden"),
            drive.prompts
        ),
    );
    assert_eq!(
        premise,
        (0, 0),
        "P1-BREAK: the premise gate reads 0 on the L2 board too — the break is working"
    );
    assert_copy_token_entered(&runner, &drive);
    assert_eq!(
        count_on_battlefield(runner.state(), "Soul Warden"),
        2,
        "PIN: both (broken) observers are still on the battlefield"
    );
    assert_eq!(
        drive.ordering_prompts(),
        0,
        "P1-BREAK L2 AXIS: the board that raises 1 ordering prompt live raises 0 broken"
    );
    assert_eq!(delta, 0, "P1-BREAK L2: the life row reads 0 as well");
}
