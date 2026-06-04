//! Regression test for the Steadfast Armasaur Last-Known-Information bug.
//!
//! Judge report: "I'm attacking with Steadfast Armasaur, it gets blocked, I
//! activate its ability, and in response the opponent casts Go for the Throat
//! destroying Steadfast. When the ability resolves the blocker should take 3
//! (Steadfast's toughness) using last known information — phase dealt 0."
//!
//! Root cause: "deals damage equal to **its toughness**" parsed to
//! `QuantityRef::Toughness { scope: ObjectScope::Anaphoric }`. The
//! subject-injection rewrite that rebinds the pronoun "its" to the ability
//! source only matched `Power`, so `Toughness { Anaphoric }` survived to
//! runtime — where an activated ability with no effect-context / trigger /
//! cost referent resolves it to 0.
//!
//! Fix: split the rebindable pronoun (`ObjectScope::Anaphoric`) from the fixed
//! demonstrative possessive (`ObjectScope::Demonstrative`, "that creature's
//! toughness") and generalize the rebind to every per-object characteristic, so
//! "its toughness" with a `SelfRef` subject binds to `Toughness { Source }`.
//! The existing `Source` resolver already LKI-falls-back (CR 113.7a) when the
//! source has left the battlefield, so the blocker now takes 3.
//!
//! CR 113.7a: an ability on the stack still exists though its source has left
//! its zone; that source's last known information is used.
//! CR 608.2b: an ability resolves even if its source (a permanent) has left the
//! battlefield. (verified: docs/MagicCompRules.txt)
//!
//! The damage target is simplified to "target creature" (the printed card
//! restricts to "attacking or blocking creature") and the activation cost to
//! "{T}" (printed "{1}{W}, {T}") — the fix touches only the damage-AMOUNT parse
//! path, which is independent of both the target filter and the mana cost, so
//! this preserves the discriminating behavior without staging combat or a mana
//! pool.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const STEADFAST_TEXT: &str =
    "{T}: Steadfast Armasaur deals damage equal to its toughness to target creature.";

/// Announce the ability and drive to the post-announcement Priority window
/// (the ability is on the stack, target chosen). Answers target selection;
/// finalizes any mana window from the (empty) pool defensively. Bounded loop.
fn activate_to_stack(runner: &mut GameRunner, source: ObjectId, target: ObjectId) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("activating Steadfast Armasaur's ability must succeed");

    for _ in 0..16 {
        match &runner.state().waiting_for {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("choosing the damage target must succeed");
            }
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("finalizing the (tap-only) cost must succeed");
            }
            WaitingFor::Priority { .. } => return,
            other => panic!("unexpected prompt before the ability hit the stack: {other:?}"),
        }
    }
    panic!("ability never reached the on-stack Priority window");
}

/// CR 113.7a + CR 608.2b — the headline test. Steadfast's `{T}` ability is on
/// the stack targeting a blocker; the source is destroyed in response; on
/// resolution the blocker must take damage equal to Steadfast's LKI toughness
/// (3), not 0.
#[test]
fn steadfast_deals_lki_toughness_after_source_destroyed_in_response() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Steadfast Armasaur is a 1/3; its damage equals its TOUGHNESS (3).
    let steadfast = scenario
        .add_creature_from_oracle(P0, "Steadfast Armasaur", 1, 3, STEADFAST_TEXT)
        .id();
    // Target survives 3 damage (toughness 5) so the exact marked damage is
    // observable — 0 (bug), 1 (power misread), or 3 (toughness via LKI).
    let blocker = scenario.add_creature(P1, "Goblin Blocker", 2, 5).id();

    let mut runner = scenario.build();

    activate_to_stack(&mut runner, steadfast, blocker);

    // CR 113.7a setup: destroy the source while the ability is on the stack.
    // `move_to_zone` runs `apply_zone_exit_cleanup`, which snapshots the
    // leaving permanent's LKI (toughness 3) keyed by its battlefield id — the
    // id the on-stack ability still references as its source.
    engine::game::zones::move_to_zone(
        runner.state_mut(),
        steadfast,
        Zone::Graveyard,
        &mut Vec::new(),
    );
    assert_eq!(
        runner.state().objects[&steadfast].zone,
        Zone::Graveyard,
        "Steadfast must be destroyed (in the graveyard) before the ability resolves",
    );

    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&blocker].damage_marked,
        3,
        "the blocker must take 3 damage (Steadfast's LKI toughness), not 0 (the \
         bug) and not 1 (its power) — proving 'its toughness' bound to Source and \
         resolved through last-known information",
    );
}

/// Control: with the source still on the battlefield, the same ability deals 3
/// from the live object. Guards against the rebind accidentally over-reaching
/// (e.g. always reading 0) in the non-LKI path.
#[test]
fn steadfast_deals_toughness_with_source_alive() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let steadfast = scenario
        .add_creature_from_oracle(P0, "Steadfast Armasaur", 1, 3, STEADFAST_TEXT)
        .id();
    let target = scenario.add_creature(P1, "Goblin Blocker", 2, 5).id();
    let mut runner = scenario.build();

    activate_to_stack(&mut runner, steadfast, target);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&target].damage_marked,
        3,
        "with the source alive the ability must still deal toughness (3)",
    );
}
