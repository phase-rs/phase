//! CR 608.2c + CR 613.1b: Coveted Falcon draws one card for each permanent whose control actually changed.
//! Two-player boards with no restriction on targeting the opponent: the sole opponent receives the
//! permanents. The recipient is never announced as a target (#9766).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    ContinuousModification, Effect, EffectKind, QuantityExpr, QuantityRef, ResolvedAbility,
    TargetFilter, TargetRef, ThisWayCause,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const COVETED_FALCON: &str = "Flying\n\
Whenever this creature attacks, gain control of target permanent you own but don't control.\n\
Disguise {1}{U}\n\
When this creature is turned face up, target opponent gains control of any number of target \
permanents you control. Draw a card for each one they gained control of this way.";
const REALITY_RIPPLE: &str = "Target artifact, creature, or land phases out. (While it's phased \
out, it's treated as though it doesn't exist. It phases in before its controller untaps during \
their next untap step.)";

fn draw_count(ability: &ResolvedAbility) -> Option<&QuantityExpr> {
    match &ability.effect {
        Effect::Draw { count, .. } => Some(count),
        _ => ability.sub_ability.as_deref().and_then(draw_count),
    }
}

/// The controllers of `id`'s control-changing transient effects.
fn control_effect_controllers(runner: &GameRunner, id: ObjectId) -> Vec<PlayerId> {
    runner
        .state()
        .transient_continuous_effects
        .iter()
        .filter(|effect| {
            effect.affected == TargetFilter::SpecificObject { id }
                && effect
                    .modifications
                    .iter()
                    .any(|m| matches!(m, ContinuousModification::ChangeController))
        })
        .map(|effect| effect.controller)
        .collect()
}

type FalconOutcome = (
    usize,
    PlayerId,
    PlayerId,
    Vec<PlayerId>,
    Vec<PlayerId>,
    Vec<(&'static str, PlayerId)>,
    usize,
);

/// Two players: turns a face-down Falcon face up and hands over two of P0's Bears, A and B,
/// optionally phasing B out first. Returns (cards drawn, A's controller, B's controller, A's and
/// B's control-effect controllers, each `ControllerChanged` as ("A" | "B" | "other", new
/// controller), the Falcon's `GiveControl` resolutions).
fn falcon(phase_b: bool) -> FalconOutcome {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Library 1", "Library 2", "Library 3", "Library 4"]);
    let falcon = scenario
        .add_creature_to_hand_from_oracle(P0, "Coveted Falcon", 1, 4, COVETED_FALCON)
        .id();
    let a = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let b = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let ripple = scenario
        .add_spell_to_hand_from_oracle(P0, "Reality Ripple", true, REALITY_RIPPLE)
        .id();
    let mut runner = scenario.build();
    for _ in 0..10 {
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            ManaType::Blue,
            ObjectId(0),
            false,
            vec![],
        ));
    }

    let card_id = runner.state().objects[&falcon].card_id;
    runner
        .act(GameAction::PlayFaceDown {
            object_id: falcon,
            card_id,
        })
        .unwrap();
    for _ in 0..10 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner.act(GameAction::PassPriority).unwrap();
    }
    runner
        .act(GameAction::TurnFaceUp {
            object_id: falcon,
            x: 0,
        })
        .unwrap();
    let mut picks = [a, b].into_iter();
    for _ in 0..6 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: picks.next().map(TargetRef::Object),
                    })
                    .unwrap();
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    assert!(!runner.state().objects[&falcon].face_down);
    assert_eq!(runner.state().stack.len(), 1, "the trigger is on the stack");
    let StackEntryKind::TriggeredAbility { ability, .. } = &runner.state().stack[0].kind else {
        panic!("the stack entry is the Falcon's trigger");
    };
    assert!(ability.targets.contains(&TargetRef::Object(a)));
    assert!(ability.targets.contains(&TargetRef::Object(b)));
    assert!(matches!(
        draw_count(ability.as_ref()),
        Some(QuantityExpr::Ref {
            qty: QuantityRef::FilteredTrackedSetSize {
                caused_by: Some(ThisWayCause::ControlGained),
                ..
            }
        })
    ));

    if phase_b {
        let _ = runner.cast(ripple).target_objects(&[b]).commit();
        runner.resolve_top();
        assert!(runner.state().objects[&b].is_phased_out());
    }
    let hand_before = runner.state().players[0].hand.len();
    let mut events = Vec::new();
    for _ in 0..12 {
        if runner.state().stack.is_empty() {
            break;
        }
        events.extend(runner.act(GameAction::PassPriority).unwrap().events);
    }
    assert!(runner.state().stack.is_empty());
    (
        runner.state().players[0].hand.len() - hand_before,
        runner.state().objects[&a].controller,
        runner.state().objects[&b].controller,
        control_effect_controllers(&runner, a),
        control_effect_controllers(&runner, b),
        events
            .iter()
            .filter_map(|event| match event {
                GameEvent::ControllerChanged {
                    object_id,
                    new_controller,
                    ..
                } => Some((
                    match *object_id {
                        id if id == a => "A",
                        id if id == b => "B",
                        _ => "other",
                    },
                    *new_controller,
                )),
                _ => None,
            })
            .collect(),
        events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    GameEvent::EffectResolved { kind: EffectKind::GiveControl, source_id, .. }
                        if *source_id == falcon
                )
            })
            .count(),
    )
}

#[test]
fn falcon_draws_for_each_permanent_handed_over() {
    assert_eq!(
        falcon(false),
        (2, P1, P1, vec![P1], vec![P1], vec![("A", P1), ("B", P1)], 1)
    );
}

/// CR 608.2b + CR 702.26b: phased-out B leaves the target list at revalidation, so only A is
/// handed over and drawn for.
#[test]
fn falcon_draws_only_for_the_target_still_phased_in() {
    assert_eq!(
        falcon(true),
        (1, P1, P0, vec![P1], vec![], vec![("A", P1)], 1)
    );
}
