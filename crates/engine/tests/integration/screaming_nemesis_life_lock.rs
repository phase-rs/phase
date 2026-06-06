//! End-to-end test for Screaming Nemesis's "can't gain life" rider, driving
//! the FULL production path (Oracle parse -> DamageReceived trigger -> redirect
//! target selection -> sub-ability resolution), NOT a hand-built
//! `ResolvedAbility`.
//!
//! Oracle:
//!   "Haste
//!    Whenever this creature is dealt damage, it deals that much damage to any
//!    other target. If a player is dealt damage this way, they can't gain life
//!    for the rest of the game."
//!
//! The parser lowers the rider into a sub-ability of the redirect: the trigger
//! executes `DealDamage { amount: EventContextAmount, target: Typed(Another) }`
//! with `sub_ability = GenericEffect { static_abilities: [CantGainLife,
//! affected: ParentTarget], duration: Permanent, target: None, sub_link:
//! SequentialSibling }`.
//!
//! The player-gating depends on the rider's `affected: ParentTarget` resolving,
//! AT RUNTIME, to the redirect's CHOSEN target. The sub-ability carries no
//! targets of its own, so it must INHERIT the parent redirect's target via
//! chain target-propagation (CLAUDE.md: "resolve_ability_chain walks the chain;
//! when a parent ability has targets but the sub-ability does not, targets
//! propagate automatically"). These tests are discriminating: they pick the
//! redirect target through the real `TriggerTargetSelection` prompt and assert
//! the lock lands on EXACTLY that player, and on no player when the redirect
//! hits a creature.
//!
//! CR references:
//!   * CR 119.7 - "If an effect says that a player can't gain life, ... a
//!     replacement effect that would replace a life gain event affecting that
//!     player won't do anything." The lock is a player-only restriction, which
//!     is why redirecting to a creature locks no one.
//!   * CR 611.2a - a continuous effect from a resolving ability with no stated
//!     duration "lasts until the end of the game" (here: `Duration::Permanent`).
//!   * CR 603.3b / CR 603.3d - a triggered ability that targets chooses its
//!     target as it's put on the stack; the chosen target is the redirect's
//!     "any other target".

use engine::game::effects::life::apply_life_gain;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::static_abilities::player_has_cant_gain_life;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const NEMESIS_TEXT: &str = "Haste\n\
     Whenever this creature is dealt damage, it deals that much damage to any \
     other target. If a player is dealt damage this way, they can't gain life \
     for the rest of the game.";

/// A 2-damage instant used to deal damage to Screaming Nemesis. Two damage is
/// non-lethal to the 3/3, so Nemesis survives and its trigger resolves with the
/// source still on the battlefield - keeping the test focused on the rider, not
/// on death-timing.
const ZAP_TEXT: &str = "Zap deals 2 damage to target creature.";

/// Drive the full cast -> damage -> trigger pipeline. A single bounded loop
/// handles every interactive prompt the production engine surfaces: the damage
/// spell's cast-time `TargetSelection` (aimed at Nemesis), and the
/// DamageReceived trigger's redirect `TriggerTargetSelection` (the caller's
/// chosen "any other target"). `redirect_to` is the redirect target the trigger
/// must offer and we pick. Returns `true` iff the redirect prompt fired and was
/// answered.
fn drive_nemesis_pipeline(
    runner: &mut GameRunner,
    nemesis: ObjectId,
    redirect_to: TargetRef,
) -> bool {
    let mut redirected = false;
    for _ in 0..60 {
        match runner.state().waiting_for.clone() {
            // The damage spell (and only it) can target Nemesis itself; the
            // redirect is "any OTHER target", so Nemesis is never legal there.
            WaitingFor::TargetSelection { target_slots, .. } => {
                assert!(
                    target_slots[0]
                        .legal_targets
                        .contains(&TargetRef::Object(nemesis)),
                    "the damage spell must be able to target Screaming Nemesis"
                );
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(nemesis)],
                    })
                    .expect("targeting Nemesis with the damage spell must succeed");
            }
            WaitingFor::TriggerTargetSelection { target_slots, .. } => {
                assert!(
                    target_slots[0].legal_targets.contains(&redirect_to),
                    "the chosen redirect target must be legal for 'any other target'"
                );
                assert!(
                    !target_slots[0]
                        .legal_targets
                        .contains(&TargetRef::Object(nemesis)),
                    "Nemesis itself must NOT be a legal redirect target ('any OTHER target')"
                );
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![redirect_to.clone()],
                    })
                    .expect("redirecting Nemesis's damage must succeed");
                redirected = true;
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() && redirected {
                    break;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            other => panic!("unexpected waiting state during Nemesis pipeline: {other:?}"),
        }
    }
    redirected
}

/// CR 119.7 + CR 611.2a: redirecting Screaming Nemesis's damage to PLAYER B
/// must lock exactly B against life gain, and must NOT lock controller A. This
/// drives the real card: the rider's `affected: ParentTarget` is bound only by
/// inheriting the redirect's chosen target through chain propagation. If that
/// propagation regressed, the lock would bind no player and both asserts below
/// would flip - this is a fail-first discriminating test.
#[test]
fn screaming_nemesis_redirect_to_player_locks_that_player_only() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let nemesis = scenario
        .add_creature_from_oracle(P0, "Screaming Nemesis", 3, 3, NEMESIS_TEXT)
        .id();

    let zap = scenario
        .add_spell_to_hand_from_oracle(P0, "Zap", true, ZAP_TEXT)
        .id();

    let mut runner = scenario.build();
    let zap_card_id = runner.state().objects[&zap].card_id;

    assert!(!player_has_cant_gain_life(runner.state(), P0));
    assert!(!player_has_cant_gain_life(runner.state(), P1));

    runner
        .act(GameAction::CastSpell {
            object_id: zap,
            card_id: zap_card_id,
            targets: vec![],
        })
        .expect("casting the 2-damage instant must succeed");

    let redirected = drive_nemesis_pipeline(&mut runner, nemesis, TargetRef::Player(P1));
    assert!(
        redirected,
        "the DamageReceived trigger must surface a redirect target prompt"
    );
    runner.advance_until_stack_empty();

    assert!(
        runner.state().battlefield.contains(&nemesis),
        "Screaming Nemesis (3/3) survives 2 non-lethal damage"
    );

    // CR 119.7: the redirect target (player B) is locked against life gain; the
    // controller (player A) is NOT - proving the lock bound the redirect's
    // CHOSEN target via `ParentTarget` inheritance, not a parser guess.
    assert!(
        player_has_cant_gain_life(runner.state(), P1),
        "redirect target B must be locked against life gain"
    );
    assert!(
        !player_has_cant_gain_life(runner.state(), P0),
        "controller A must NOT be locked - only the redirect target is"
    );

    // Drive a real life-gain through the production path and confirm B's life
    // does not move (CR 119.7: the life-gain event is suppressed entirely).
    let b_life_before = runner.life(P1);
    let mut events: Vec<GameEvent> = Vec::new();
    let gained = apply_life_gain(runner.state_mut(), P1, 5, &mut events)
        .expect("apply_life_gain must not defer for a locked player");
    assert_eq!(gained, 0, "a locked player gains 0 life (CR 119.7)");
    assert_eq!(
        runner.life(P1),
        b_life_before,
        "player B's life total must be unchanged while locked"
    );

    // Controller A is unaffected: a life-gain for A still works.
    let a_life_before = runner.life(P0);
    let mut events_a: Vec<GameEvent> = Vec::new();
    let gained_a = apply_life_gain(runner.state_mut(), P0, 5, &mut events_a)
        .expect("apply_life_gain for the unlocked controller must succeed");
    assert_eq!(gained_a, 5, "unlocked player A gains the full 5 life");
    assert_eq!(
        runner.life(P0),
        a_life_before + 5,
        "player A's life total must increase by 5 (A is not locked)"
    );
}

/// CR 119.7 (player-only gating): redirecting Screaming Nemesis's damage to a
/// CREATURE must lock NO player. This is the gating discriminator - if the rider
/// bound to "the controller of the redirect target" instead of strictly a
/// player target, the creature's controller would be wrongly locked.
#[test]
fn screaming_nemesis_redirect_to_creature_locks_no_player() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let nemesis = scenario
        .add_creature_from_oracle(P0, "Screaming Nemesis", 3, 3, NEMESIS_TEXT)
        .id();

    // A creature controlled by player B to receive the redirected damage. A 4/4
    // survives the 2 redirected damage so it stays a valid bound object.
    let bear = scenario.add_creature(P1, "Grizzly Bears", 4, 4).id();

    let zap = scenario
        .add_spell_to_hand_from_oracle(P0, "Zap", true, ZAP_TEXT)
        .id();

    let mut runner = scenario.build();
    let zap_card_id = runner.state().objects[&zap].card_id;

    assert!(!player_has_cant_gain_life(runner.state(), P0));
    assert!(!player_has_cant_gain_life(runner.state(), P1));

    runner
        .act(GameAction::CastSpell {
            object_id: zap,
            card_id: zap_card_id,
            targets: vec![],
        })
        .expect("casting the 2-damage instant must succeed");

    let redirected = drive_nemesis_pipeline(&mut runner, nemesis, TargetRef::Object(bear));
    assert!(redirected, "the redirect target prompt must have fired");
    runner.advance_until_stack_empty();

    // CR 119.7 is a player-only restriction: redirecting to a creature must lock
    // NO player - neither the creature's controller (B) nor anyone else.
    assert!(
        !player_has_cant_gain_life(runner.state(), P0),
        "no player is locked when the damage is redirected to a creature"
    );
    assert!(
        !player_has_cant_gain_life(runner.state(), P1),
        "the creature's controller (B) must NOT be locked (CR 119.7 is player-only)"
    );

    // Confirm via the production gain-life path that B can still gain life.
    let b_life_before = runner.life(P1);
    let mut events: Vec<GameEvent> = Vec::new();
    let gained = apply_life_gain(runner.state_mut(), P1, 5, &mut events)
        .expect("apply_life_gain must succeed for an unlocked player");
    assert_eq!(
        gained, 5,
        "player B gains the full 5 life - B is not locked"
    );
    assert_eq!(
        runner.life(P1),
        b_life_before + 5,
        "player B's life total must increase by 5"
    );
}
