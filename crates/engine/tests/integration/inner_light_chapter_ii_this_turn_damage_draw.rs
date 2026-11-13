//! The Inner Light, chapter II — a this-turn combat-damage watch bound to the
//! chapter's target.
//!
//! Oracle text:
//! "(As this Saga enters and after your draw step, add a lore counter. Sacrifice after III.)
//! I — Return target green or white creature card from your graveyard to the battlefield.
//! II — Choose target creature. Whenever it deals combat damage to a player this turn, you draw that many cards.
//! III — You gain life equal to the greatest power among creatures you control. Create a Treasure token."
//!
//! CR 603.7b: "Whenever it deals combat damage to a player this turn" creates a
//! delayed triggered ability that lasts until end of turn. CR 608.2c: "it" is
//! the chapter's chosen target. CR 120.3: "that many" is the damage dealt.
//!
//! Driven through the production Saga pipeline (CR 714.3c: the lore counter
//! added after the draw step triggers chapter II), then real combat.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const INNER_LIGHT_ORACLE: &str = "(As this Saga enters and after your draw step, add a lore counter. Sacrifice after III.)\nI — Return target green or white creature card from your graveyard to the battlefield.\nII — Choose target creature. Whenever it deals combat damage to a player this turn, you draw that many cards.\nIII — You gain life equal to the greatest power among creatures you control. Create a Treasure token.";

/// The Saga (already on chapter I) and a 3-power attacker for P0.
fn setup() -> (GameRunner, ObjectId, ObjectId) {
    let (runner, saga, wurm, _) = setup_with_bystander(false);
    (runner, saga, wurm)
}

/// `setup`, plus (when `with_bystander`) a second 2-power P0 creature that is
/// not chapter II's chosen creature.
fn setup_with_bystander(
    with_bystander: bool,
) -> (GameRunner, ObjectId, ObjectId, Option<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let saga = scenario
        .add_creature(P0, "The Inner Light", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Saga"])
        .from_oracle_text(INNER_LIGHT_ORACLE)
        .id();
    let wurm = scenario.add_creature(P0, "Craw Wurm", 3, 4).id();
    let bystander = with_bystander.then(|| scenario.add_creature(P0, "Grizzly Bears", 2, 2).id());
    scenario.with_library_top(P0, &["Plains"; 20]);
    scenario.with_library_top(P1, &["Plains"; 20]);
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&saga)
        .unwrap()
        .counters
        .insert(CounterType::Lore, 1);
    (runner, saga, wurm, bystander)
}

/// Advance from P0's end step through P0's next draw step (CR 714.3c adds the
/// second lore counter), answer chapter II's target with `target`, and resolve.
fn resolve_chapter_two(runner: &mut GameRunner, saga: ObjectId, target: ObjectId) {
    {
        let state = runner.state_mut();
        state.turn_number = 1;
        state.active_player = P0;
        state.phase = Phase::End;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    runner.advance_to_phase(Phase::PreCombatMain);
    runner.pass_both_players();
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(
        runner.state().objects[&saga]
            .counters
            .get(&CounterType::Lore),
        Some(&2),
        "CR 714.3c: the second lore counter is added after the draw step"
    );
    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::TriggerTargetSelection { target_slots, .. } => {
                assert!(
                    target_slots[0]
                        .legal_targets
                        .contains(&TargetRef::Object(target)),
                    "chapter II must offer the creature as its target"
                );
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("choosing chapter II's target");
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            _ => break,
        }
    }
    assert!(runner.state().stack.is_empty(), "chapter II must resolve");
    assert!(
        !runner.state().delayed_triggers.is_empty(),
        "chapter II must create its this-turn delayed trigger"
    );
}

#[test]
fn chosen_creature_dealing_combat_damage_this_turn_draws_that_many() {
    let (mut runner, saga, wurm) = setup();
    resolve_chapter_two(&mut runner, saga, wurm);

    runner.advance_to_combat();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareAttackers { .. }
        ),
        "expected DeclareAttackers, got {:?}",
        runner.state().waiting_for
    );
    runner
        .declare_attackers(&[(wurm, AttackTarget::Player(P1))])
        .expect("Craw Wurm attacks");
    let outcome = runner.combat_damage();
    outcome.assert_life_delta(P1, -3);
    // CR 603.7b + CR 120.3: draw cards equal to the 3 combat damage dealt.
    outcome.assert_hand_drawn(P0, 3);
}

/// CR 603.7c + CR 608.2c: the delayed trigger watches only the chosen creature
/// ("it"). A different creature of ours dealing combat damage the same turn
/// draws nothing; the chosen one still draws its own damage. Both attack, so
/// the total drawn equals only the chosen creature's 3 damage, not the 5 dealt.
#[test]
fn a_non_chosen_creature_dealing_combat_damage_draws_nothing() {
    let (mut runner, saga, wurm, bystander) = setup_with_bystander(true);
    let bystander = bystander.expect("bystander staged");
    resolve_chapter_two(&mut runner, saga, wurm);

    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (wurm, AttackTarget::Player(P1)),
            (bystander, AttackTarget::Player(P1)),
        ])
        .expect("the chosen Craw Wurm and the non-chosen Grizzly Bears attack");
    let outcome = runner.combat_damage();
    // Reach guard: both creatures dealt their combat damage (3 + 2).
    outcome.assert_life_delta(P1, -5);
    // Only the chosen creature's 3 damage draws; the bystander's 2 adds nothing.
    outcome.assert_hand_drawn(P0, 3);
}

#[test]
fn combat_damage_on_a_later_turn_draws_nothing() {
    let (mut runner, saga, wurm) = setup();
    resolve_chapter_two(&mut runner, saga, wurm);
    let chapter_turn = runner.state().turn_number;

    // Let the turn end without attacking; CR 603.7b ends the watch at cleanup.
    for _ in 0..12 {
        if runner.state().turn_number > chapter_turn
            && runner.state().active_player == P0
            && runner.state().phase == Phase::DeclareAttackers
        {
            break;
        }
        if runner.state().phase == Phase::DeclareAttackers
            && matches!(
                runner.state().waiting_for,
                WaitingFor::DeclareAttackers { .. }
            )
        {
            runner.declare_attackers(&[]).expect("declare no attackers");
        }
        let next = if runner.state().phase == Phase::DeclareAttackers {
            Phase::End
        } else {
            Phase::DeclareAttackers
        };
        runner.advance_to_phase(next);
        runner.advance_until_stack_empty();
    }
    assert!(runner.state().turn_number > chapter_turn);
    assert_eq!(runner.state().active_player, P0);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareAttackers { .. }
        ),
        "expected P0's later DeclareAttackers, got {:?}",
        runner.state().waiting_for
    );

    runner
        .declare_attackers(&[(wurm, AttackTarget::Player(P1))])
        .expect("Craw Wurm attacks on the later turn");
    let outcome = runner.combat_damage();
    // Reach guard: the creature did deal its combat damage.
    outcome.assert_life_delta(P1, -3);
    // CR 603.7b: the chapter II watch expired with its turn.
    outcome.assert_hand_drawn(P0, 0);
}
