//! CR 608.2h + CR 111.7 + CR 603.4: an intervening-if condition must still be
//! answerable when the triggered ability's SOURCE has ceased to exist.
//!
//! Oracle text (Delta Bloodflies, verbatim):
//!   "Flying
//!    Whenever this creature attacks, if you control a creature with a counter on
//!    it, each opponent loses 1 life."
//!
//! THE DEFECT this discriminates. `FilterContext::from_source` derived the
//! context's `source_controller` from LIVE state only:
//!
//!     state.objects.get(&source_id).map(|o| o.controller)
//!
//! A token that leaves the battlefield ceases to exist (CR 111.7 / CR 704.5d) and
//! is purged from `state.objects` outright. The trigger is COLLECTED while the
//! token is still present, goes on the stack, and the CR 111.7 SBA then purges the
//! source. At resolution the engine re-checks the intervening-if (CR 603.4,
//! stack.rs) via `check_trigger_condition` → `TriggerCondition::ControlsType` →
//! `FilterContext::from_source`, which now yields `source_controller: None`. The
//! filter "a creature you control with a counter on it" carries
//! `ControllerRef::You`, which is then unresolvable, so the condition reads FALSE
//! and the ability is silently removed from the stack. The opponent never loses
//! the life.
//!
//! CR 608.2h is explicit that this is wrong: "If the effect requires information
//! from a specific object, INCLUDING THE SOURCE OF THE ABILITY ITSELF, the effect
//! uses the current information of that object if it's in the public zone it was
//! expected to be in; if it's no longer in that zone ... the effect uses the
//! object's LAST KNOWN INFORMATION." `state.lki_cache` holds exactly that
//! snapshot (captured on battlefield exit by `apply_zone_exit_cleanup`), and per
//! CR 113.7a the ability on the stack exists independently of its source — the
//! source's death does not make "you" unanswerable.
//!
//! The DISCRIMINATING vector is the CR 111.7 purge, not an ordinary trip to the
//! graveyard: a nontoken card keeps its `controller` across a zone change and stays
//! in `state.objects`, so it still answers on the live path. `purged_token_source`
//! is therefore RED before the fix while `nontoken_source_is_unaffected` is GREEN
//! before and after — that asymmetry isolates the defect to the purge.
//!
//! This drives the REAL pipeline end to end: the card is synthesized from verbatim
//! Oracle text, the token is killed through the real zone-change pipeline
//! (`move_to_zone`, which snapshots LKI) and purged by the real SBA
//! (`check_state_based_actions`), and the trigger resolves off the real stack. The
//! observable is the opponent's life total, not an AST shape.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::{sba, zones};
use engine::types::counter::CounterType;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

/// Delta Bloodflies — 1/2 Insect. Verbatim Oracle text.
const DELTA_BLOODFLIES: &str = "Flying\nWhenever this creature attacks, if you control a creature with a counter on it, each opponent loses 1 life.";

/// Whether the attacking Delta Bloodflies is a token (a token COPY of it — e.g.
/// from "create a token that's a copy of target creature") or the printed card.
/// Only the token ceases to exist under CR 111.7.
#[derive(Clone, Copy, PartialEq)]
enum SourceKind {
    Token,
    Nontoken,
}

/// Whether a creature with a counter on it is present, i.e. whether the
/// intervening-if condition is genuinely TRUE at resolution.
#[derive(Clone, Copy, PartialEq)]
enum CounterBearer {
    Present,
    Absent,
}

/// Build the scenario, attack with Delta Bloodflies, kill it in response to its
/// own attack trigger, and resolve. Returns P1's life total after resolution.
///
/// The attack trigger's condition is TRUE when it triggers (CR 603.4 first check)
/// whenever `bearer == Present`; the question under test is whether it is still
/// answerable at the CR 603.4 re-check at RESOLUTION, once the source is gone.
fn attack_then_kill_source(kind: SourceKind, bearer: CounterBearer) -> i32 {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let flies = scenario
        .add_creature_from_oracle(P0, "Delta Bloodflies", 1, 2, DELTA_BLOODFLIES)
        .id();

    // In BOTH arms the controller keeps a second creature on the battlefield, so the
    // filter always has a live candidate to test. Only the COUNTER differs. Without
    // this, the `Absent` arm would leave the controller with no creatures at all once
    // the source is purged, and "no match" could mean "empty battlefield" rather than
    // "the counter predicate is genuinely false" — a control that cannot tell those
    // apart is not a control.
    let host = scenario.add_creature(P0, "Counter Bearer", 2, 2).id();
    if bearer == CounterBearer::Present {
        scenario.with_counter(host, CounterType::Plus1Plus1, 1);
    }

    let mut runner = scenario.build();

    // CR 111.7: only a token ceases to exist on leaving the battlefield. This is
    // the single axis that separates the two source kinds.
    if kind == SourceKind::Token {
        runner
            .state_mut()
            .objects
            .get_mut(&flies)
            .expect("source must exist at setup")
            .is_token = true;
    }

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(flies, AttackTarget::Player(P1))])
        .expect("Delta Bloodflies has no summoning sickness and may attack");

    // The attack trigger is now on the stack. Kill the source in response, through
    // the REAL zone-change pipeline (this is what snapshots LKI) and the REAL SBA
    // (this is what purges a token under CR 111.7 / CR 704.5d).
    let mut events = Vec::new();
    zones::move_to_zone(runner.state_mut(), flies, Zone::Graveyard, &mut events);
    sba::check_state_based_actions(runner.state_mut(), &mut events);

    assert!(
        runner.state().lki_cache.contains_key(&flies),
        "CR 400.7: battlefield exit must snapshot LKI for the source in both arms"
    );
    match kind {
        SourceKind::Token => assert!(
            !runner.state().objects.contains_key(&flies),
            "CR 111.7: the token source must have CEASED TO EXIST — if it is still \
             in state.objects this test is vacuous and proves nothing"
        ),
        SourceKind::Nontoken => assert!(
            runner.state().objects.contains_key(&flies),
            "a nontoken source stays in state.objects (in the graveyard) — this arm \
             is the live-path control"
        ),
    }

    runner.advance_until_stack_empty();
    runner.life(P1)
}

/// PRIMARY WITNESS — RED before the `from_source` LKI fallback.
///
/// A token copy of Delta Bloodflies attacks, then dies in response to its own
/// attack trigger. Its controller still controls a creature with a counter on it,
/// so the intervening-if is TRUE and the opponent must lose 1 life (CR 603.4:
/// the condition is re-checked at resolution and it still holds; CR 113.7a: the
/// ability does not care that its source is gone).
///
/// Before the fix `FilterContext::from_source` could not resolve the purged
/// source's controller, `ControllerRef::You` was unanswerable, the condition read
/// FALSE, and the ability was silently removed from the stack — P1 kept 20 life.
#[test]
fn purged_token_source_still_answers_intervening_if_via_lki() {
    let life = attack_then_kill_source(SourceKind::Token, CounterBearer::Present);
    assert_eq!(
        life, 19,
        "CR 608.2h: the intervening-if must resolve against the purged source's LAST \
         KNOWN controller — 'you control a creature with a counter on it' is still true, \
         so each opponent loses 1 life"
    );
}

/// NEGATIVE CONTROL (live path unchanged). The printed card — not a token — dies
/// the same way. It stays in `state.objects` (graveyard) so the live lookup always
/// answered, and behavior must be IDENTICAL before and after the fix.
///
/// Non-vacuity: this control CAN fail — `purged_token_source_condition_false_stays_false`
/// runs the same harness and asserts 20, so a harness that could never produce a
/// life change would fail that test instead.
#[test]
fn nontoken_source_is_unaffected() {
    let life = attack_then_kill_source(SourceKind::Nontoken, CounterBearer::Present);
    assert_eq!(
        life, 19,
        "a nontoken source that dies with its trigger on the stack was never broken \
         and must keep resolving its intervening-if"
    );
}

/// NEGATIVE CONTROL (the fallback must not fabricate a match). Same purged-token
/// source, and the controller still controls a creature — but that creature has NO
/// counter on it, so the intervening-if is genuinely FALSE at resolution. CR 603.4
/// removes the ability from the stack and no life is lost.
///
/// The creature is present on purpose: it gives the filter a live candidate to
/// reject, so a pass here means "the counter predicate was evaluated and was false",
/// not "there was nothing on the battlefield to match".
///
/// This is what makes the primary witness non-vacuous: the LKI fallback restores
/// the ability to ANSWER the question, it does not make the answer unconditionally
/// "yes". If the fallback were implemented as "purged source ⇒ filter matches",
/// this test goes red.
#[test]
fn purged_token_source_condition_false_stays_false() {
    let life = attack_then_kill_source(SourceKind::Token, CounterBearer::Absent);
    assert_eq!(
        life, 20,
        "CR 603.4: with no counter-bearing creature the intervening-if is genuinely \
         false and the ability does nothing — the LKI fallback must not fabricate a match"
    );
}

/// Guard the premise the whole file rests on: the trigger really does carry an
/// intervening-if condition, and it really is the source-controller-relative
/// `ControlsType` shape routed through `FilterContext::from_source`. If the parser
/// ever stops emitting a condition here, every assertion above would pass for the
/// wrong reason (no condition ⇒ no re-check ⇒ no purged-source lookup).
#[test]
fn premise_delta_bloodflies_trigger_carries_a_controller_relative_intervening_if() {
    use engine::types::ability::TriggerCondition;

    let mut scenario = GameScenario::new();
    let flies = scenario
        .add_creature_from_oracle(P0, "Delta Bloodflies", 1, 2, DELTA_BLOODFLIES)
        .id();
    let runner: GameRunner = scenario.build();
    let obj = runner
        .state()
        .objects
        .get(&flies)
        .expect("source must exist");

    let condition = obj
        .trigger_definitions
        .iter_unchecked()
        .find_map(|t| t.definition.condition.as_ref())
        .expect("the attack trigger must carry an intervening-if condition (CR 603.4)");

    let TriggerCondition::ControlsType { filter } = condition else {
        panic!("expected the 'if you control a …' intervening-if to lower to ControlsType, got {condition:?}");
    };
    assert!(
        format!("{filter:?}").contains("You"),
        "the filter must be controller-relative (ControllerRef::You) — that is the \
         predicate that goes unanswerable when the source is purged; got {filter:?}"
    );
}
