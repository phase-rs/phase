//! Runtime regression for GitHub issue #4369 — Ashling, the Limitless's delayed
//! "sacrifice it unless you pay {W}{U}{B}{R}{G}".
//!
//! Front-face Oracle: "Whenever you sacrifice a nontoken Elemental, create a
//! token that's a copy of it. ... At the beginning of your next end step,
//! sacrifice it unless you pay {W}{U}{B}{R}{G}."
//!
//! The reported bug was a runtime TIMING bug, not just AST placement: the
//! {W}{U}{B}{R}{G} alternative cost was demanded when the token COPY was created
//! (on the parent "Whenever you sacrifice" trigger) instead of at the next end
//! step on the delayed sacrifice. Root cause: the trigger-level
//! `extract_unless_pay_modifier` scanned the whole multi-sentence effect, found
//! the "unless" in the delayed end-step sentence, and hoisted the cost onto the
//! parent trigger (CR 118.12a payment surfaced at the wrong time). The cost
//! belongs to the `CreateDelayedTrigger`'s inner sacrifice (CR 603.7a).
//!
//! This drives the REAL parse → sacrifice-event → trigger → token → delayed-
//! trigger → end-step → unless-payment pipeline. Ashling is synthesized from
//! Oracle text (live parse, so the trigger reflects current parser source, not a
//! stored/stale card-data AST). A nontoken Elemental is sacrificed through the
//! production `sacrifice::resolve` seam; the parent trigger resolves and creates
//! the token. The discriminating checkpoints are:
//!   (1) NO `UnlessPayment` prompt surfaces when the token is created, and
//!   (2) the `{W}{U}{B}{R}{G}` `UnlessPayment` prompt surfaces only when the
//!       delayed trigger resolves at the next end step,
//! and both the paid (token survives) and declined (token sacrificed) paths
//! behave correctly.
//!
//! CR 603.7a: a delayed triggered ability triggers (and its cost is paid) only
//! when its condition is met, here "at the beginning of your next end step".
//! CR 118.12 / CR 118.12a: "unless you pay" is an alternative the controller may
//! decline; declining lets the otherwise-effect (the sacrifice) happen.

use engine::game::effects::sacrifice::resolve as resolve_sacrifice;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::triggers::process_triggers;
use engine::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ASHLING: &str = "Whenever you sacrifice a nontoken Elemental, create a token that's a copy of it. The token gains haste until end of turn. At the beginning of your next end step, sacrifice it unless you pay {W}{U}{B}{R}{G}.";

/// Build Ashling + a nontoken Elemental on P0's battlefield, sacrifice the
/// Elemental through the production seam, resolve the parent trigger, and
/// advance to the next end step where the delayed `UnlessPayment` prompt
/// surfaces. Returns the runner (parked at the prompt) and the created token id.
///
/// Asserts the discriminator along the way: NO payment is demanded when the
/// token is created (checkpoint 1).
fn run_to_end_step_unless_prompt() -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Ashling, built from Oracle text through the real parse + synthesis
    // pipeline so the (fixed) parent + delayed triggers are installed.
    let ashling = scenario
        .add_creature_from_oracle(P0, "Ashling, the Limitless", 3, 3, ASHLING)
        .with_subtypes(vec!["Elemental"])
        .id();

    // The sacrifice victim: a nontoken Elemental under P0's control.
    let elemental = scenario
        .add_creature(P0, "Fire Elemental", 5, 4)
        .with_subtypes(vec!["Elemental"])
        .id();

    let mut runner = scenario.build();

    // Sacrifice the Elemental through the production `sacrifice::resolve` seam.
    // `SpecificObject` targets exactly the victim (Ashling is itself an
    // Elemental, so a type filter would be ambiguous).
    let sac = ResolvedAbility::new(
        Effect::Sacrifice {
            target: TargetFilter::SpecificObject { id: elemental },
            count: QuantityExpr::Fixed { value: 1 },
            min_count: 0,
        },
        vec![TargetRef::Object(elemental)],
        ashling,
        P0,
    );
    let mut events = Vec::new();
    resolve_sacrifice(runner.state_mut(), &sac, &mut events).expect("sacrifice resolves");
    process_triggers(runner.state_mut(), &events);

    // The parent "Whenever you sacrifice a nontoken Elemental" trigger is queued.
    assert_eq!(
        runner.state().stack.len(),
        1,
        "sacrificing a nontoken Elemental must fire Ashling's parent trigger"
    );

    // Resolve the parent trigger: it creates the token copy and installs the
    // delayed end-step sacrifice trigger.
    runner.advance_until_stack_empty();

    // CHECKPOINT 1 (the discriminator): the {W}{U}{B}{R}{G} cost must NOT be
    // demanded now, at token creation. Before the fix the cost was hoisted onto
    // this parent trigger and `WaitingFor::UnlessPayment` surfaced here.
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "CR 603.7a: no payment may be demanded when the token is created — the \
         unless-cost belongs to the delayed end-step sacrifice; got {:?}",
        runner.state().waiting_for
    );

    // The parent trigger produced a token copy and installed the delayed trigger.
    let token = runner
        .state()
        .objects
        .values()
        .find(|o| o.controller == P0 && o.zone == Zone::Battlefield && o.is_token)
        .map(|o| o.id)
        .expect("the parent trigger must create a token copy of the sacrificed Elemental");
    assert!(
        !runner.state().delayed_triggers.is_empty(),
        "the delayed end-step 'sacrifice it unless you pay' trigger must be installed"
    );

    // Advance toward the next end step. Cross combat by declaring no attackers
    // (the harness surfaces `WaitingFor::DeclareAttackers` as a turn-based action
    // that plain priority passes cannot answer), then advance to the end step and
    // resolve the now-firing delayed trigger off the stack — it surfaces the
    // unless-payment prompt (CHECKPOINT 2).
    runner.advance_to_combat();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![],
            bands: vec![],
        })
        .expect("declare no attackers to cross combat");
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();

    // CHECKPOINT 2: the prompt is Ashling's controller paying at the end step.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::UnlessPayment { player: P0, .. }
        ),
        "the {{W}}{{U}}{{B}}{{R}}{{G}} unless-cost must be offered to P0 when the \
         delayed trigger resolves; got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().phase,
        Phase::End,
        "the cost is offered at the next end step (CR 603.7a)"
    );

    (runner, token)
}

/// Declining the {W}{U}{B}{R}{G} payment sacrifices the token (CR 118.12a).
#[test]
fn ashling_delayed_unless_pay_declined_sacrifices_token() {
    let (mut runner, token) = run_to_end_step_unless_prompt();

    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("declining the unless-cost must be accepted");
    runner.advance_until_stack_empty();

    // The token is sacrificed — no longer on the battlefield (a token in the
    // graveyard ceases to exist as a state-based action, CR 704.5d).
    assert!(
        runner
            .state()
            .objects
            .get(&token)
            .is_none_or(|o| o.zone != Zone::Battlefield),
        "declining the {{W}}{{U}}{{B}}{{R}}{{G}} cost must sacrifice the token"
    );
}

/// Paying the {W}{U}{B}{R}{G} keeps the token on the battlefield (CR 118.12).
#[test]
fn ashling_delayed_unless_pay_paid_keeps_token() {
    let (mut runner, token) = run_to_end_step_unless_prompt();

    // Fund P0's pool with one of each color to pay {W}{U}{B}{R}{G}.
    for color in [
        ManaType::White,
        ManaType::Blue,
        ManaType::Black,
        ManaType::Red,
        ManaType::Green,
    ] {
        runner
            .state_mut()
            .add_mana_to_pool(P0, ManaUnit::new(color, ObjectId(0), false, vec![]));
    }

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("paying the unless-cost must be accepted");
    runner.advance_until_stack_empty();

    // The token survives — the alternative cost was paid, so the sacrifice does
    // not happen.
    assert_eq!(
        runner.state().objects.get(&token).map(|o| o.zone),
        Some(Zone::Battlefield),
        "paying the {{W}}{{U}}{{B}}{{R}}{{G}} cost must keep the token on the battlefield"
    );
}
