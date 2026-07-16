//! Production-pipeline regression for Power Leak's dynamic damage prevention
//! (PR #5742). This is the end-to-end companion to the helper-level unit tests
//! in `game/effects/pay.rs` (`power_leak_pattern_*`): those drive a synthesized
//! `ResolvedAbility` chain through `resolve_ability_chain`; this drives the REAL
//! parsed card through the REAL turn structure and `stack::resolve_top`.
//!
//! Power Leak's verbatim Oracle text (MTGJSON AtomicCards):
//!   "Enchant enchantment
//!    At the beginning of the upkeep of enchanted enchantment's controller, that
//!    player may pay any amount of mana. This Aura deals 2 damage to that player.
//!    Prevent X of that damage, where X is the amount of mana that player paid
//!    this way."
//!
//! Both PR fixes are exercised together, end-to-end:
//!   * Parser (commit "parse and resolve Power Leak's dynamic damage prevention"):
//!     the "deal 2 / prevent X" pair folds into ONE `DealDamage` with a computed
//!     `max(2 - X, 0)` amount (CR 615.1a/615.4 — a prevention shield can't
//!     retroactively edit an already-dealt amount, so the net is folded), and the
//!     upkeep trigger scopes to the enchanted enchantment's controller
//!     (`ParentTargetController`, CR 303.4e) rather than every player.
//!   * Runtime (commit "restore trigger-event context across a paused
//!     continuation resume"): `TargetFilter::TriggeringPlayer` still resolves to
//!     the enchanted controller (P1) even though the ability chain pauses
//!     mid-resolution on the "may pay any amount of mana" choice and resumes via
//!     `PendingContinuation` after `stack::resolve_top`'s CR 603.7c context
//!     cleanup. Before this fix the drained `DealDamage` fell back to the Aura's
//!     own controller (P0), so P0 wrongly took the damage.
//!
//! Setup: P0 controls Power Leak, attached to a plain enchantment host that P1
//! controls. We advance to P1's REAL upkeep so the phase trigger fires naturally
//! (CR 603.2b), let the stack resolve through the real priority/`resolve_top`
//! path, accept the optional payment for a nonzero X, submit it via the real
//! `GameAction::SubmitPayAmount`, and assert the enchanted controller P1 — never
//! the Aura controller P0 — takes exactly `max(2 - X, 0)` damage.

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::trigger_index::reindex_object_triggers;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

/// Verbatim Oracle text (matches the parser test `POWER_LEAK_ORACLE`).
const POWER_LEAK_ORACLE: &str = "Enchant enchantment\nAt the beginning of the upkeep of enchanted enchantment's controller, that player may pay any amount of mana. This Aura deals 2 damage to that player. Prevent X of that damage, where X is the amount of mana that player paid this way.";

/// Build a game where P0's Power Leak is attached to a plain enchantment host
/// controlled by P1, and it's P1's turn starting at the untap step so
/// `advance_to_upkeep` can drive into P1's real upkeep. Returns the runner and
/// the `(power_leak, host)` object ids.
fn setup() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);

    // A plain enchantment host under P1's control. `as_enchantment()` strips the
    // 0/0 creature type the creature-builder adds, so no CR 704.5f SBA death.
    let host = {
        let mut b = scenario.add_creature(P1, "Host Shrine", 0, 0);
        b.as_enchantment();
        b.id()
    };

    // Power Leak: an Aura enchantment under P0's control, real parsed triggers.
    let power_leak = {
        let mut b = scenario.add_creature_from_oracle(P0, "Power Leak", 0, 0, POWER_LEAK_ORACLE);
        b.as_enchantment();
        b.with_subtypes(vec!["Aura"]);
        b.id()
    };

    // Library padding so upkeep/draw advancement never decks anyone.
    for _ in 0..20 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Plains");
    }

    let mut runner = scenario.build();

    // It is P1's turn (their upkeep is the one that triggers Power Leak). Keep
    // the three turn/priority fields consistent so `advance_to_upkeep` opens a
    // clean P1 priority window.
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    // Attach Power Leak to the host enchantment P1 controls. `ParentTargetController`
    // (CR 303.4e) reads this host's controller, so the upkeep trigger fires only on
    // P1's upkeep. Set the pointer directly (both sides) and re-index so the parsed
    // phase trigger is consultable.
    runner
        .state_mut()
        .objects
        .get_mut(&power_leak)
        .unwrap()
        .attached_to = Some(AttachTarget::Object(host));
    runner
        .state_mut()
        .objects
        .get_mut(&host)
        .unwrap()
        .attachments
        .push(power_leak);
    reindex_object_triggers(runner.state_mut(), power_leak);

    (runner, power_leak, host)
}

/// Give P1 `n` colorless mana so the "pay any amount of mana" prompt has a
/// positive max. Added during P1's upkeep (after advancement) so the pool isn't
/// emptied by a step transition (CR 500.4) before the payment.
fn give_p1_mana(runner: &mut GameRunner, n: usize) {
    for _ in 0..n {
        runner.state_mut().players[P1.0 as usize]
            .mana_pool
            .add(ManaUnit::new(
                ManaType::Colorless,
                ObjectId(0),
                false,
                vec![],
            ));
    }
}

/// Pass priority (each side) until the Power Leak trigger's resolution pauses on
/// its optional "may pay" prompt, or panic if the stack drains without pausing.
fn advance_until_optional_choice(runner: &mut GameRunner) {
    for _ in 0..40 {
        match runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("PassPriority should succeed while draining the stack");
            }
            ref other => panic!("unexpected waiting state while draining: {other:?}"),
        }
    }
    panic!("did not reach OptionalEffectChoice within 40 iterations");
}

/// Drive from setup to P1's upkeep, confirm the real trigger fired, then walk the
/// real priority/`resolve_top` path to the optional-payment pause.
fn reach_optional_payment(runner: &mut GameRunner, power_leak: ObjectId) {
    runner.advance_to_upkeep();

    // Reach-guard: the REAL parsed upkeep trigger fired and is on the stack. This
    // proves the assertions below are not vacuous — the input reached resolution.
    let triggers_from_power_leak = runner
        .state()
        .stack
        .iter()
        .filter(|e| e.source_id == power_leak)
        .count();
    assert!(
        triggers_from_power_leak >= 1,
        "Power Leak's upkeep trigger must fire at the enchanted enchantment's \
         controller's (P1) upkeep and be on the stack; stack={:?}",
        runner.state().stack
    );

    give_p1_mana(runner, 5);
    advance_until_optional_choice(runner);
}

/// Primary discriminator. Pay X = 1 → the enchanted controller P1 loses exactly
/// `max(2 - 1, 0) = 1` life, and the Aura controller P0 loses ZERO. Reverting the
/// runtime `PendingContinuation.trigger_context` push/restore fix flips this so P0
/// takes the damage instead of P1.
#[test]
fn paying_one_mana_damages_enchanted_controller_for_one_not_the_aura_controller() {
    let (mut runner, power_leak, _host) = setup();
    reach_optional_payment(&mut runner, power_leak);

    // The "may pay any amount of mana" gate must prompt the triggering (enchanted)
    // player P1 — the payer is `TriggeringPlayer`, not the Aura controller P0.
    match &runner.state().waiting_for {
        WaitingFor::OptionalEffectChoice { player, .. } => assert_eq!(
            *player, P1,
            "the optional 'may pay' choice belongs to the enchanted controller P1"
        ),
        other => panic!("expected OptionalEffectChoice, got {other:?}"),
    }

    let p0_before = runner.state().players[P0.0 as usize].life;
    let p1_before = runner.state().players[P1.0 as usize].life;

    // Accept the optional payment through the real handler.
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept the optional 'may pay any amount of mana'");

    // The paused chain resumes into the {X} pay-amount prompt for P1.
    match &runner.state().waiting_for {
        WaitingFor::PayAmountChoice { player, .. } => assert_eq!(
            *player, P1,
            "the {{X}} payer is the enchanted controller P1, not the Aura controller P0"
        ),
        other => panic!("expected PayAmountChoice, got {other:?}"),
    }

    // Pay 1 mana → X = 1 → the folded DealDamage amount is max(2 - 1, 0) = 1.
    runner
        .act(GameAction::SubmitPayAmount { amount: 1 })
        .expect("submit the {X}=1 payment");

    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        p1_before - 1,
        "enchanted controller P1 must take exactly max(2 - 1, 0) = 1 damage after \
         the paused pay-amount continuation drains"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        p0_before,
        "the Aura controller P0 must take ZERO damage — reverting the \
         PendingContinuation trigger-context push/restore reproduces the bug by \
         damaging P0 here instead of P1"
    );
}

/// Sibling boundary case. Decline the payment → X = 0 → P1 loses exactly
/// `max(2 - 0, 0) = 2` life, still scoped to the enchanted controller (never P0).
/// Exercises the lower edge of the `max(2 - X, 0)` fold.
#[test]
fn declining_payment_deals_full_two_to_enchanted_controller() {
    let (mut runner, power_leak, _host) = setup();
    reach_optional_payment(&mut runner, power_leak);

    let p0_before = runner.state().players[P0.0 as usize].life;
    let p1_before = runner.state().players[P1.0 as usize].life;

    // Decline the optional payment → X = 0.
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("decline the optional 'may pay any amount of mana'");

    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        p1_before - 2,
        "declining leaves X = 0, so the enchanted controller P1 takes the full \
         max(2 - 0, 0) = 2 damage"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        p0_before,
        "the Aura controller P0 must take ZERO damage on the decline path"
    );
}

/// Sibling upper-edge case. Pay X = 3 (>= 2) → the whole 2 damage is prevented,
/// so P1 takes `max(2 - 3, 0) = 0` and neither player loses life. Confirms the
/// fold clamps at zero rather than dealing negative/heal damage.
#[test]
fn paying_three_mana_fully_prevents_the_damage() {
    let (mut runner, power_leak, _host) = setup();
    reach_optional_payment(&mut runner, power_leak);

    let p0_before = runner.state().players[P0.0 as usize].life;
    let p1_before = runner.state().players[P1.0 as usize].life;

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept the optional payment");
    match &runner.state().waiting_for {
        WaitingFor::PayAmountChoice { player, max, .. } => {
            assert_eq!(
                *player, P1,
                "the {{X}} payer is the enchanted controller P1"
            );
            assert!(
                *max >= 3,
                "P1 has 5 mana, so paying 3 must be legal; max={max}"
            );
        }
        other => panic!("expected PayAmountChoice, got {other:?}"),
    }

    runner
        .act(GameAction::SubmitPayAmount { amount: 3 })
        .expect("submit the {X}=3 payment");

    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        p1_before,
        "X = 3 prevents all of the 2 damage: max(2 - 3, 0) = 0, so P1 loses no life"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        p0_before,
        "the Aura controller P0 is never the damage recipient"
    );
}
