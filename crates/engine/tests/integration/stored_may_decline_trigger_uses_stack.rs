//! CR 603.5: an optional ("may") triggered ability goes on the stack when it
//! triggers, regardless of whether its controller intends to exercise the
//! option; the choice is made when the ability resolves. A remembered
//! "don't ask again" decline is that resolution-time answer, so it must not
//! keep the trigger off the stack.
//!
//! Regression for issue #10536: a stored `AutoMayChoice::Decline` used to
//! discard the pending trigger before it was put on the stack, so nothing
//! could respond to it. With Soul's Attendant and Strionic Resonator on the
//! battlefield, a creature entering must leave the Attendant trigger on the
//! stack for the Resonator to copy (CR 707.10), and both the original and the
//! copy apply the remembered decline as they resolve.

use engine::game::casting::activated_ability_definitions;
use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::ability::{Effect, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{
    AutoMayChoice, CastPaymentMode, MayTriggerAutoChoiceScope, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::PlayerId;

const P0: PlayerId = PlayerId(0);

/// Verbatim Oracle text (Scryfall).
const SOULS_ATTENDANT: &str = "Whenever another creature enters, you may gain 1 life.";
const STRIONIC_RESONATOR: &str = "{2}, {T}: Copy target triggered ability you control. You may \
     choose new targets for the copy. (A triggered ability uses the words \"when,\" \
     \"whenever,\" or \"at.\")";

struct Board {
    runner: GameRunner,
    attendant: ObjectId,
    resonator: ObjectId,
    first_bear: ObjectId,
    second_bear: ObjectId,
}

/// Soul's Attendant and Strionic Resonator on P0's battlefield, two {1}
/// creatures in hand, and {4} in P0's pool: {1} + {1} for the creatures and
/// {2} for the Resonator.
fn board() -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attendant = scenario
        .add_creature_from_oracle(P0, "Soul's Attendant", 1, 1, SOULS_ATTENDANT)
        .id();
    let resonator = scenario
        .add_artifact_from_oracle(P0, "Strionic Resonator", STRIONIC_RESONATOR)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let first_bear = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let second_bear = scenario
        .add_creature_to_hand(P0, "Runeclaw Bear", 2, 2)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    Board {
        runner,
        attendant,
        resonator,
        first_bear,
        second_bear,
    }
}

fn colorless_in_pool(runner: &GameRunner) -> usize {
    runner.state().players[0].mana_pool.total()
}

/// Pass priority until the top stack object has resolved. Unlike
/// `GameRunner::resolve_top`, this stops as soon as that entry leaves the
/// stack, so a trigger it causes is left waiting for priority.
fn resolve_top_entry(runner: &mut GameRunner) {
    let top = runner
        .state()
        .stack
        .back()
        .expect("an object to resolve")
        .id;
    for _ in 0..4 {
        if !runner.state().stack.iter().any(|entry| entry.id == top) {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority can be passed");
    }
    panic!("the top stack object did not resolve");
}

/// Cast `creature` (a {1} creature spell) and pass priority until it resolves.
fn cast_and_resolve_creature(runner: &mut GameRunner, creature: ObjectId) {
    let card_id = runner.state().objects[&creature].card_id;
    let pool_before = colorless_in_pool(runner);
    runner
        .act(GameAction::CastSpell {
            object_id: creature,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P0 may cast the {1} creature");
    assert_eq!(
        colorless_in_pool(runner),
        pool_before - 1,
        "CR 601.2h: the creature's {{1}} is paid from the pool"
    );
    resolve_top_entry(runner);
    assert_eq!(
        runner.state().objects[&creature].zone,
        engine::types::zones::Zone::Battlefield,
        "reach: the creature spell resolved and the creature entered"
    );
}

/// Ids of the Soul's Attendant triggered abilities (originals and copies)
/// currently on the stack, bottom to top.
fn attendant_triggers_on_stack(runner: &GameRunner, attendant: ObjectId) -> Vec<ObjectId> {
    runner
        .state()
        .stack
        .iter()
        .filter(|entry| {
            matches!(
                &entry.kind,
                StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == attendant
            )
        })
        .map(|entry| entry.id)
        .collect()
}

/// Drive the first creature's entry to the Attendant's prompt and remember
/// "decline" for it, through the public remember action.
fn remember_decline(board: &mut Board) {
    let Board {
        runner,
        attendant,
        first_bear,
        ..
    } = board;
    cast_and_resolve_creature(runner, *first_bear);
    assert_eq!(
        attendant_triggers_on_stack(runner, *attendant).len(),
        1,
        "reach: the unanswered Attendant trigger is on the stack"
    );
    resolve_top_entry(runner);
    let WaitingFor::OptionalEffectChoice {
        may_trigger_key: Some(_),
        source_id,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "CR 603.5: the Attendant's 'may' is asked on resolution, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(source_id, *attendant);
    runner
        .act(GameAction::DecideOptionalEffectAndRemember {
            choice: AutoMayChoice::Decline,
            scope: MayTriggerAutoChoiceScope::ExactInstance,
        })
        .expect("the keyed prompt accepts a remembered decline");
    assert_eq!(runner.life(P0), 20, "the declined trigger gained no life");
    assert!(runner.state().stack.is_empty());
}

/// CR 603.5: with a remembered decline, the next Attendant trigger still goes
/// on the stack and P0 receives priority with it there; the decline is applied
/// only when it resolves.
#[test]
fn remembered_decline_keeps_the_may_trigger_on_the_stack() {
    let mut board = board();
    remember_decline(&mut board);
    let Board {
        mut runner,
        attendant,
        second_bear,
        ..
    } = board;

    cast_and_resolve_creature(&mut runner, second_bear);
    let triggers = attendant_triggers_on_stack(&runner, attendant);
    assert_eq!(
        triggers.len(),
        1,
        "CR 603.5: the remembered decline must not keep the trigger off the stack"
    );
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 },
        "CR 117.3b: players receive priority while the trigger waits on the stack"
    );

    resolve_top_entry(&mut runner);
    assert!(
        runner.state().stack.is_empty(),
        "reach: the Attendant trigger resolved"
    );
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 },
        "the remembered decline answers the 'may' without a prompt"
    );
    assert_eq!(
        runner.life(P0),
        20,
        "CR 603.5: the remembered decline is applied at resolution"
    );
}

/// CR 603.5 + CR 707.10: the Attendant trigger left on the stack by a
/// remembered decline is a legal target for Strionic Resonator. The copy's
/// "may" is answered when the copy resolves.
#[test]
fn strionic_resonator_copies_a_trigger_with_a_remembered_decline() {
    let mut board = board();
    remember_decline(&mut board);
    let Board {
        mut runner,
        attendant,
        resonator,
        second_bear,
        ..
    } = board;

    cast_and_resolve_creature(&mut runner, second_bear);
    let original = match attendant_triggers_on_stack(&runner, attendant).as_slice() {
        [original] => *original,
        other => {
            panic!("CR 603.5: exactly one Attendant trigger must wait on the stack, got {other:?}")
        }
    };

    let copy_idx = activated_ability_definitions(runner.state(), resonator)
        .into_iter()
        .find(|(_, ability)| matches!(ability.effect.as_ref(), Effect::CopySpell { .. }))
        .expect("Strionic Resonator exposes its copy ability")
        .0;
    let pool_before = colorless_in_pool(&runner);
    runner
        .act(GameAction::ActivateAbility {
            source_id: resonator,
            ability_index: copy_idx,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Strionic Resonator's {2}, {T} ability is activatable");
    if let WaitingFor::TargetSelection { target_slots, .. } = &runner.state().waiting_for {
        assert!(
            target_slots[0]
                .legal_targets
                .contains(&TargetRef::Object(original)),
            "the Attendant trigger is a legal copy target"
        );
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(original)],
            })
            .expect("the Attendant trigger is a legal copy target");
    }
    assert_eq!(
        colorless_in_pool(&runner),
        pool_before - 2,
        "CR 602.2b: the Resonator's {{2}} is paid"
    );
    assert!(
        runner.state().objects[&resonator].tapped,
        "CR 602.2b: the Resonator's {{T}} is paid"
    );
    assert_eq!(
        runner
            .state()
            .stack
            .back()
            .and_then(|entry| entry.ability())
            .map(|ability| ability.targets.clone()),
        Some(vec![TargetRef::Object(original)]),
        "CR 601.2c: the Resonator ability targets the Attendant trigger"
    );

    // Resolve the Resonator ability: the copy goes on the stack above the
    // original (CR 707.10).
    resolve_top_entry(&mut runner);
    let triggers = attendant_triggers_on_stack(&runner, attendant);
    assert_eq!(
        triggers.len(),
        2,
        "CR 707.10: the Resonator put a copy of the Attendant trigger on the stack, \
         got stack {:?}",
        runner.stack_names()
    );
    assert_eq!(triggers[0], original, "the original stays below its copy");

    runner.advance_until_stack_empty();
    assert!(
        runner.state().stack.is_empty(),
        "reach: the copy and the original both resolved"
    );
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 },
        "the remembered decline answers both 'may's without a prompt"
    );
    assert_eq!(
        runner.life(P0),
        20,
        "CR 603.5: the copy and the original each apply the remembered decline"
    );
}
