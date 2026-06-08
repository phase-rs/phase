//! CR 702.68 — Frenzy N.
//!
//! "Frenzy N" means "Whenever this creature attacks and isn't blocked, it gets
//! +N/+0 until end of turn." (CR 702.68a). If a creature has multiple instances
//! of frenzy, each triggers separately (CR 702.68b).
//!
//! Before this change "Frenzy N" fell to `Keyword::Unknown` (a silent no-op) and
//! the Frenzy Sliver static grant ("All Sliver creatures have frenzy 1") fell to
//! `Effect::Unimplemented` because `parse_keyword_from_oracle("frenzy 1")`
//! returned `None`. The fix adds the `Keyword::Frenzy(u32)` variant, the
//! parser/`FromStr` arms, and the synthesis builder
//! (`build_frenzy_trigger` → `TriggerMode::AttackerUnblocked` self-pump).
//!
//! These tests drive the REAL combat pipeline through `apply`:
//! declare-attackers (and, where relevant, declare-blockers) → the
//! `BlockersDeclared` event → the `AttackerUnblocked` trigger → stack →
//! resolution → layer system. They assert the *post-layer* `power`/`toughness`
//! of the attacker (the layer system writes effective P/T into
//! `obj.power`/`obj.toughness`), never the parsed `Pump` effect — so a missing
//! synthesized trigger fails the assertion.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

use super::rules::AttackTarget;

/// A Frenzy 2 creature in the real MTGJSON Oracle form: the keyword line carries
/// the numeral plus reminder text. Paired with the `["frenzy"]` keyword-name
/// hint (the inline-Oracle analog of MTGJSON's keyword list), the parser's
/// `parse_keyword_from_oracle` recovers N = 2 from this line.
const FRENZY_2_ORACLE: &str = "Frenzy 2 (Whenever this creature attacks and isn't \
blocked, it gets +2/+0 until end of turn.)";
/// Frenzy Sliver's real Oracle text — a static grant of `frenzy 1` to all
/// Slivers. Before this change `parse_keyword_from_oracle("frenzy 1")` returned
/// `None`, so the grant fell to `Effect::Unimplemented` (a silent no-op).
const FRENZY_SLIVER_GRANT: &str = "All Sliver creatures have frenzy 1. \
(Whenever a Sliver attacks and isn't blocked, it gets +1/+0 until end of turn.)";
/// CR 702.68b: two instances of Frenzy (each triggers separately → +1/+0 twice
/// = +2/+0 total). Written as two bare `Frenzy` keyword lines so each infers to
/// `Frenzy(1)` and both survive `merge_extracted_keywords` (Frenzy carries no
/// MTGJSON-deduped multi-instance recovery, so the two printed instances stand).
const FRENZY_BARE_TWICE: &str = "Frenzy\nFrenzy";

/// Drive a runner from PreCombatMain through declaring `attacker` as an attacker
/// and then through the declare-blockers step with an EMPTY block, so the
/// attacker is unblocked and the `BlockersDeclared` event (which the
/// `AttackerUnblocked` matcher keys on) fires. Then resolve the stack so the
/// trigger applies.
///
/// `advance_until_stack_empty` alone is insufficient: `AttackerUnblocked` does
/// not fire at declare-attackers (unlike `Attacks`), so the stack is empty right
/// after `DeclareAttackers`. The defending player must have at least one
/// (non-blocking) creature so the `WaitingFor::DeclareBlockers` prompt appears;
/// we then declare no blockers to leave the attacker unblocked. (With zero
/// defending creatures the engine auto-resolves the empty-blockers step and
/// lands back in Priority, so the caller seeds an idle defender.)
fn declare_attacker_unblocked(runner: &mut engine::game::scenario::GameRunner, attacker: ObjectId) {
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");
    if matches!(
        runner.state().waiting_for,
        engine::types::game_state::WaitingFor::Priority { .. }
    ) {
        runner.pass_both_players();
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            engine::types::game_state::WaitingFor::DeclareBlockers { .. }
        ),
        "expected DeclareBlockers, got {:?}",
        runner.state().waiting_for
    );
    // CR 509.1: the defending player declares no blockers — the attacker is
    // unblocked. This emits `GameEvent::BlockersDeclared`.
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![],
        })
        .expect("empty DeclareBlockers should succeed");
    runner.advance_until_stack_empty();
}

/// CR 702.68a: a Frenzy 2 creature that attacks and isn't blocked gets +2/+0
/// until end of turn. A 2/2 becomes an effective 4/2 (power +2, toughness
/// unchanged) after the trigger resolves and the layer system applies the pump.
#[test]
fn frenzy_pumps_unblocked_attacker_power_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mut attacker_b = scenario.add_creature(P0, "Frenzied Bear", 2, 2);
    attacker_b.from_oracle_text_with_keywords(&["frenzy"], FRENZY_2_ORACLE);
    let attacker = attacker_b.id();
    // Idle defender so the DeclareBlockers prompt appears (it declares no blocks).
    scenario.add_creature(P1, "Idle Bystander", 1, 1);

    let mut runner = scenario.build();
    declare_attacker_unblocked(&mut runner, attacker);

    let obj = &runner.state().objects[&attacker];
    assert_eq!(
        obj.power,
        Some(4),
        "CR 702.68a: unblocked Frenzy 2 attacker is pumped +2 → power 4"
    );
    assert_eq!(
        obj.toughness,
        Some(2),
        "CR 702.68a: Frenzy is +N/+0 — toughness is unchanged at 2"
    );
}

/// CR 702.68a: a Frenzy 2 creature that attacks and IS blocked does not trigger
/// (the ability requires "isn't blocked"), so it receives no pump.
#[test]
fn frenzy_does_not_pump_blocked_attacker() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mut attacker_b = scenario.add_creature(P0, "Frenzied Bear", 2, 2);
    attacker_b.from_oracle_text_with_keywords(&["frenzy"], FRENZY_2_ORACLE);
    let attacker = attacker_b.id();
    let blocker = scenario.add_creature(P1, "Hostile Bear", 2, 2).id();

    let mut runner = scenario.build();
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");
    if matches!(
        runner.state().waiting_for,
        engine::types::game_state::WaitingFor::Priority { .. }
    ) {
        runner.pass_both_players();
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            engine::types::game_state::WaitingFor::DeclareBlockers { .. }
        ),
        "expected DeclareBlockers, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(blocker, attacker)],
        })
        .expect("DeclareBlockers should succeed");
    runner.advance_until_stack_empty();

    let obj = &runner.state().objects[&attacker];
    assert_eq!(
        obj.power,
        Some(2),
        "CR 702.68a: a BLOCKED Frenzy attacker does not trigger → power stays 2"
    );
    assert_eq!(
        obj.toughness,
        Some(2),
        "CR 702.68a: a BLOCKED Frenzy attacker does not trigger → toughness stays 2"
    );
}

/// CR 702.68b: two instances of Frenzy 1 each trigger separately. An unblocked
/// attacker with Frenzy 1 twice gets +1/+0 twice = +2/+0 → a 2/2 becomes 4/2.
#[test]
fn frenzy_multiple_instances_stack_per_cr_702_68b() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let attacker = scenario
        .add_creature_from_oracle(P0, "Doubly Frenzied Bear", 2, 2, FRENZY_BARE_TWICE)
        .id();
    // Idle defender so the DeclareBlockers prompt appears (it declares no blocks).
    scenario.add_creature(P1, "Idle Bystander", 1, 1);

    let mut runner = scenario.build();
    declare_attacker_unblocked(&mut runner, attacker);

    let obj = &runner.state().objects[&attacker];
    assert_eq!(
        obj.power,
        Some(4),
        "CR 702.68b: two Frenzy 1 instances each fire → +1/+0 twice → power 4"
    );
    assert_eq!(
        obj.toughness,
        Some(2),
        "CR 702.68a: each instance is +N/+0 — toughness unchanged at 2"
    );
}

/// CR 702.68a + CR 702.68 grant: Frenzy Sliver grants `frenzy 1` to all Slivers
/// via a static ability. A granted-frenzy Sliver that attacks unblocked gets
/// +1/+0. This exercises the grant → layer-system `AddKeyword` → synthesis
/// (`triggers_for` at `layers.rs:3417`) → combat path, and is the regression
/// guard that `map_keyword`/`parse_keyword_from_oracle` no longer drops
/// "frenzy 1" to Unknown/Unimplemented.
#[test]
fn frenzy_sliver_grant_pumps_unblocked_sliver() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Frenzy Sliver is itself a Sliver, so its own grant applies to it. A 1/1
    // attacking unblocked should become an effective 2/1 (+1/+0).
    let mut grantor =
        scenario.add_creature_from_oracle(P0, "Frenzy Sliver", 1, 1, FRENZY_SLIVER_GRANT);
    grantor.with_subtypes(vec!["Sliver"]);
    let grantor_id = grantor.id();
    // Idle defender so the DeclareBlockers prompt appears (it declares no blocks).
    scenario.add_creature(P1, "Idle Bystander", 1, 1);

    let mut runner = scenario.build();
    // Pass priority once so the static grant's layer modification (AddKeyword
    // Frenzy(1)) is applied and `triggers_for` installs the synthesized trigger
    // before combat.
    runner.act(GameAction::PassPriority).ok();
    declare_attacker_unblocked(&mut runner, grantor_id);

    let obj = &runner.state().objects[&grantor_id];
    assert_eq!(
        obj.power,
        Some(2),
        "granted frenzy 1 Sliver attacking unblocked is pumped +1 → power 2 \
         (regression: 'frenzy 1' grant must not fall to Unimplemented)"
    );
    assert_eq!(
        obj.toughness,
        Some(1),
        "CR 702.68a: granted frenzy is +1/+0 — toughness unchanged at 1"
    );
}
