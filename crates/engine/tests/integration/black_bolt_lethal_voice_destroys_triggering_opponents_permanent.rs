//! CR 115.1 + CR 603.2e + CR 608.2c — Black Bolt, Inhuman King (Lethal Voice).
//!
//! Verbatim Oracle text:
//!   Flying
//!   Whenever you cast a noncreature spell, Black Bolt gets +2/+2 until end of turn.
//!   Lethal Voice — Whenever Black Bolt becomes the target of a spell or ability an
//!     opponent controls, destroy target nonland permanent that player controls.
//!
//! "That player" is the controller of the *targeting* source — the opponent
//! (CR 608.2c, "apply the rules of English to the text," reads the effect's "that
//! player" anaphor as referring back to the player named by the trigger
//! condition; CR 603.2e fires on the "becomes the target" event; CR 115.1 fixes
//! the source's target when it is put on the stack). The parser previously
//! lowered "that player controls" to `ControllerRef::You`, so Lethal Voice
//! offered/destroyed one of BLACK BOLT'S OWN permanents. This test drives the
//! real pipeline: an opponent (P1) targets Black Bolt with a spell P1 controls,
//! and Lethal Voice must be able to destroy a nonland permanent P1 controls —
//! never one of P0's own.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const BLACK_BOLT_ORACLE: &str = "Flying\n\
Whenever you cast a noncreature spell, Black Bolt gets +2/+2 until end of turn.\n\
Lethal Voice — Whenever Black Bolt becomes the target of a spell or ability an \
opponent controls, destroy target nonland permanent that player controls.";

/// PRIMARY + hostile-fixture runtime discrimination.
///
/// Fixture (multi-authority): P0 controls Black Bolt AND a nonland permanent of
/// its own (Grizzly Bears). P1 controls TWO nonland permanents (so P0 must be
/// prompted to choose between them rather than the engine auto-selecting a lone
/// legal target) and the targeting spell. When P1 targets Black Bolt, the Lethal
/// Voice trigger's destroy slot must offer only P1's permanents, and destroying
/// one must resolve.
///
/// Revert-failing seam: the trigger's target-legality set is computed at
/// `game/filter.rs::controller_ref_player`'s `TriggeringPlayer` arm →
/// `triggering_event_player` → `extract_player_from_event`'s `BecomesTarget`
/// arm. Pre-fix (`ControllerRef::You`) that set is P0's permanents, so both the
/// candidate-list assertions and the destroy outcome flip.
#[test]
fn lethal_voice_destroys_a_permanent_the_targeting_opponent_controls() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let black_bolt = scenario
        .add_creature_from_oracle(P0, "Black Bolt, Inhuman King", 3, 3, BLACK_BOLT_ORACLE)
        .id();
    // P0's OWN nonland permanent — the pre-fix (`You`) mis-target would offer this.
    let own_bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    // The targeting opponent's nonland permanents — the correct target class. Two
    // of them force P0 to be prompted (no single-legal-target auto-select).
    let opp_ogre = scenario.add_creature(P1, "Onakke Ogre", 3, 3).id();
    let opp_wolf = scenario.add_creature(P1, "Timber Wolves", 1, 1).id();
    // A noncreature spell P1 (the opponent) controls, to target Black Bolt.
    let bolt = scenario.add_bolt_to_hand(P1);
    scenario.with_mana_pool(
        P1,
        vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])],
    );

    let mut runner = scenario.build();
    {
        // CR 601.2 / CR 117.1: give the opponent priority so it can cast.
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }

    let card_id = runner.state().objects[&bolt].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: bolt,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P1 casting its noncreature spell should succeed");

    // Drive: P1 declares the spell's target (Black Bolt); passing priority places
    // the pending Lethal Voice trigger on the stack (CR 603.3), which prompts P0
    // for its destroy target. Enumerate the offered targets at that P0 prompt —
    // the runtime revert guard — then destroy one of P1's permanents.
    let mut chose_spell_target = false;
    let mut enumerated_trigger_targets = false;
    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            // CR 601.2c: the opponent declares its spell's target = Black Bolt,
            // the trigger source.
            WaitingFor::TargetSelection { player, .. } if player == P1 && !chose_spell_target => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(black_bolt)),
                    })
                    .expect("targeting Black Bolt with the opponent's spell must be legal");
                chose_spell_target = true;
            }
            // CR 603.3d: Black Bolt's controller (P0) declares Lethal Voice's
            // destroy target as the trigger is put on the stack. A triggered
            // ability's target prompt is `TriggerTargetSelection`, distinct from a
            // spell's `TargetSelection`.
            WaitingFor::TriggerTargetSelection {
                player,
                target_slots,
                ..
            } if player == P0 => {
                let legal = &target_slots[0].legal_targets;
                // Positive reach guard: proves the trigger fired and put a real
                // target slot on the stack — a vacuous "not present" pair could
                // otherwise pass if the trigger silently failed to fire.
                assert!(
                    legal.contains(&TargetRef::Object(opp_ogre))
                        && legal.contains(&TargetRef::Object(opp_wolf)),
                    "the targeting opponent's permanents must be legal Lethal Voice targets; got {legal:?}",
                );
                // Revert-failing: pre-fix (`You`) these were the offered targets.
                assert!(
                    !legal.contains(&TargetRef::Object(own_bear)),
                    "P0's own permanent must NOT be a legal Lethal Voice target (pre-fix `You` bug); got {legal:?}",
                );
                assert!(
                    !legal.contains(&TargetRef::Object(black_bolt)),
                    "Black Bolt (P0's) must NOT be a legal Lethal Voice target; got {legal:?}",
                );
                enumerated_trigger_targets = true;
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(opp_ogre)),
                    })
                    .expect("destroying the targeting opponent's permanent must be legal");
                break;
            }
            // CR 603.3 / CR 117.3b: pass priority so the engine places the pending
            // trigger and surfaces P0's target prompt (and later drives resolution).
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority to place the pending trigger must be accepted");
            }
            other => panic!("unexpected state while declaring targets: {other:?}"),
        }
    }
    assert!(
        chose_spell_target && enumerated_trigger_targets,
        "both the spell's target (P1) and Lethal Voice's target (P0) must be declared",
    );

    runner.advance_until_stack_empty();

    // Primary revert-failing outcome: the chosen P1 permanent is destroyed; P0's
    // own permanent is untouched.
    assert_eq!(
        runner.state().objects[&opp_ogre].zone,
        Zone::Graveyard,
        "Lethal Voice must destroy the targeting opponent's nonland permanent",
    );
    assert_eq!(
        runner.state().objects[&own_bear].zone,
        Zone::Battlefield,
        "Lethal Voice must never destroy the controller's own permanent",
    );
}
