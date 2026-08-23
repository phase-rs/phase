//! Issue #7692 reach-guard: the engine's classified target-choice kind must be
//! reached from a REAL announcement, not only from hand-built `WaitingFor`
//! fixtures.
//!
//! The unit tests in `game::derived_views` construct their prompts directly.
//! That leaves exactly one thing unproven, and it is the thing this file
//! exists for: that the ordinary cast/trigger pipeline installs a prompt the
//! projection classifies, and that `derive_views` publishes the answer on the
//! way out. Anything that stops populating the field reds here.
//!
//! The recipe is reused from `issue_3681_inferno_titan_divided_damage`, a
//! committed, passing witness that this flow reaches
//! `WaitingFor::TriggerTargetSelection` for this card. This test observes the
//! announcement; it never submits targets, because the thing under test is the
//! prompt rather than the outcome.

use engine::game::derived_views::{derive_views, TargetChoiceKind, TargetObjectCategory};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

/// Verbatim Oracle text, byte-identical to the string the committed Inferno
/// Titan test pins. A paraphrase can take a different parser branch and pass
/// while the real card stays broken.
const INFERNO_ORACLE: &str =
    "Whenever this creature enters or attacks, it deals 3 damage divided as you choose among one, two, or three targets.";

/// Give `player` `count` red mana units so the {4}{R}{R} cast auto-pays.
fn add_red_mana(
    runner: &mut engine::game::scenario::GameRunner,
    player: engine::types::PlayerId,
    count: usize,
) {
    let dummy = engine::types::identifiers::ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .expect("the player exists")
        .mana_pool;
    for _ in 0..count {
        pool.add(ManaUnit::new(ManaType::Red, dummy, false, vec![]));
    }
}

/// Advance through the cast/payment flow until the ETB trigger surfaces its
/// target-selection prompt, passing priority as needed.
fn advance_to_trigger_target_selection(runner: &mut engine::game::scenario::GameRunner) {
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(
            guard < 80,
            "Inferno Titan's ETB trigger never surfaced a target prompt; last waiting_for = {:?}",
            runner.state().waiting_for
        );
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } => return,
            WaitingFor::Priority { .. } => runner.pass_both_players(),
            other => panic!("unexpected waiting_for while reaching the ETB trigger: {other:?}"),
        }
    }
}

/// CR 115.1 + CR 601.2c: a real "any target" trigger announcement (CR 115.4)
/// offering both objects and players publishes
/// `ObjectsAndPlayers { category: Creature }` on `DerivedViews`.
///
/// **The expected category is a property of THIS FIXTURE, not of the rule.**
/// CR 115.4's "any target" also admits planeswalkers and battles; the answer is
/// `Creature` because the scenario contains none of those, and because the
/// Titan is in its own offer and is itself a creature. Adding a planeswalker to
/// this scenario moves the expected value to `Permanent`.
#[test]
fn a_real_trigger_announcement_publishes_the_classified_target_kind() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Two opposing creatures, so the "any target" offer holds objects as well
    // as the two players.
    scenario.add_creature(P1, "Bear", 2, 2);
    scenario.add_creature(P1, "Elf", 1, 1);

    let titan = scenario
        .add_creature_to_hand_from_oracle(P0, "Inferno Titan", 6, 6, INFERNO_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red, ManaCostShard::Red],
            generic: 4,
        })
        .id();

    let mut runner = scenario.build();
    add_red_mana(&mut runner, P0, 8);

    let card_id = runner.state().objects[&titan].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: titan,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Inferno Titan should be accepted");

    advance_to_trigger_target_selection(&mut runner);

    // ASSERT #1 — POSITIVE REACH-GUARD, BEFORE THE CLASSIFICATION ASSERTION.
    // Without this ordering, a `None` answer below could mean "the projection
    // is broken" or "no announcement was ever reached", and the test could not
    // tell those apart.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "reach-guard: a real target announcement must be live, got {:?}",
        runner.state().waiting_for
    );

    // ASSERT #2 — the classification, read off the public projection rather
    // than off any private helper, so this also covers the projection wiring.
    assert_eq!(
        derive_views(runner.state(), Some(P0)).current_target_kind,
        Some(TargetChoiceKind::ObjectsAndPlayers {
            category: TargetObjectCategory::Creature
        }),
        "a live 'any target' announcement offering creatures and players must publish both \
         halves; this is the projection #7692 exists to add"
    );
}
