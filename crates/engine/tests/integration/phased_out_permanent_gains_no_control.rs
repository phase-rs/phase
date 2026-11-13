//! CR 702.26b: a phased-out permanent can't be affected, so no control-change
//! instruction (gain control or exchange of control) applies to it.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{ContinuousModification, EffectKind, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const AKROAN_HORSE: &str =
    "Defender\nWhen this creature enters, an opponent gains control of it.\n\
At the beginning of your upkeep, each opponent creates a 1/1 white Soldier creature token.";
const AVARICE_TOTEM: &str = "{5}: Exchange control of this artifact and target nonland permanent.";
const GILDED_DRAKE: &str = "Flying\nWhen this creature enters, exchange control of this creature \
and up to one target creature an opponent controls. If you don't or can't make an exchange, \
sacrifice this creature. This ability still resolves if its target becomes illegal.";
const REALITY_RIPPLE: &str = "Target artifact, creature, or land phases out. (While it's phased \
out, it's treated as though it doesn't exist. It phases in before its controller untaps during \
their next untap step.)";

fn has_control_effect(runner: &GameRunner, id: ObjectId) -> bool {
    runner
        .state()
        .transient_continuous_effects
        .iter()
        .any(|effect| {
            effect.affected == TargetFilter::SpecificObject { id }
                && effect
                    .modifications
                    .iter()
                    .any(|m| matches!(m, ContinuousModification::ChangeController))
        })
}

fn phase_out(runner: &mut GameRunner, ripple: ObjectId, target: ObjectId) {
    let _ = runner.cast(ripple).target_objects(&[target]).commit();
    runner.resolve_top();
}

/// Returns the Horse's (phased out, has a control effect, controller, `GainControl` resolutions,
/// `ControllerChanged` count, back at priority with an empty stack).
fn akroan(phase: bool) -> (bool, bool, PlayerId, usize, usize, bool) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let horse = scenario
        .add_creature_to_hand_from_oracle(P0, "Akroan Horse", 0, 4, AKROAN_HORSE)
        .id();
    let ripple = scenario
        .add_spell_to_hand_from_oracle(P0, "Reality Ripple", true, REALITY_RIPPLE)
        .id();
    let mut runner = scenario.build();
    let _ = runner.cast(horse).commit();
    runner.act(GameAction::PassPriority).unwrap();
    runner.act(GameAction::PassPriority).unwrap();
    assert_eq!(runner.state().objects[&horse].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the ETB trigger is on the stack"
    );
    if phase {
        phase_out(&mut runner, ripple, horse);
    }
    let mut events = Vec::new();
    for _ in 0..20 {
        let result = match runner.state().waiting_for.clone() {
            WaitingFor::NamedChoice { options, .. } => runner.act(GameAction::ChooseOption {
                choice: options[0].clone(),
            }),
            _ if runner.state().stack.is_empty() => break,
            _ => runner.act(GameAction::PassPriority),
        };
        events.extend(result.unwrap().events);
    }
    let obj = &runner.state().objects[&horse];
    (
        obj.is_phased_out(),
        has_control_effect(&runner, horse),
        obj.controller,
        events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    GameEvent::EffectResolved { kind: EffectKind::GainControl, source_id, .. }
                        if *source_id == horse
                )
            })
            .count(),
        events
            .iter()
            .filter(|event| matches!(event, GameEvent::ControllerChanged { .. }))
            .count(),
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty(),
    )
}

#[test]
fn phased_out_akroan_horse_gains_no_control() {
    assert_eq!(akroan(true), (true, false, P0, 1, 0, true));
}

#[test]
fn akroan_horse_reach_gives_control_to_opponent() {
    assert_eq!(akroan(false), (false, true, P1, 1, 1, true));
}

/// Returns (Totem phased out, Totem has a control effect, Bears have a control effect, the
/// Totem's `ExchangeControl` resolved, `ControllerChanged` count).
fn totem(phase: bool) -> (bool, bool, bool, bool, usize) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..5)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    let totem = scenario
        .add_artifact_from_oracle(P0, "Avarice Totem", AVARICE_TOTEM)
        .id();
    let bears = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let ripple = scenario
        .add_spell_to_hand_from_oracle(P0, "Reality Ripple", true, REALITY_RIPPLE)
        .id();
    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: totem,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activation is accepted");
    for _ in 0..8 {
        match &runner.state().waiting_for {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(bears)),
                    })
                    .expect("target accepted");
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the exchange is on the stack"
    );
    if phase {
        phase_out(&mut runner, ripple, totem);
    }
    let mut events = Vec::new();
    for _ in 0..10 {
        if runner.state().stack.is_empty() {
            break;
        }
        events.extend(runner.act(GameAction::PassPriority).unwrap().events);
    }
    assert!(runner.state().stack.is_empty());
    (
        runner.state().objects[&totem].is_phased_out(),
        has_control_effect(&runner, totem),
        has_control_effect(&runner, bears),
        events.iter().any(|event| {
            matches!(
                event,
                GameEvent::EffectResolved { kind: EffectKind::ExchangeControl, source_id, .. }
                    if *source_id == totem
            )
        }),
        events
            .iter()
            .filter(|event| matches!(event, GameEvent::ControllerChanged { .. }))
            .count(),
    )
}

#[test]
fn phased_out_avarice_totem_exchanges_nothing() {
    assert_eq!(totem(true), (true, false, false, true, 0));
}

#[test]
fn avarice_totem_reach_exchanges_control() {
    assert_eq!(totem(false), (false, true, true, true, 2));
}

#[test]
fn phased_out_gilded_drake_exchanges_nothing_and_is_not_sacrificed() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let drake = scenario
        .add_creature_to_hand_from_oracle(P0, "Gilded Drake", 3, 3, GILDED_DRAKE)
        .id();
    let bears = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let ripple = scenario
        .add_spell_to_hand_from_oracle(P0, "Reality Ripple", true, REALITY_RIPPLE)
        .id();
    let mut runner = scenario.build();
    let _ = runner.cast(drake).commit();
    for _ in 0..6 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(bears)),
                    })
                    .unwrap();
            }
            WaitingFor::Priority { .. }
                if runner.state().objects[&drake].zone == Zone::Battlefield
                    && !runner.state().stack.is_empty() =>
            {
                break
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).unwrap();
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the ETB trigger is on the stack"
    );
    phase_out(&mut runner, ripple, drake);
    let mut events = Vec::new();
    for _ in 0..10 {
        if runner.state().stack.is_empty() {
            break;
        }
        events.extend(runner.act(GameAction::PassPriority).unwrap().events);
    }
    assert!(runner.state().stack.is_empty());
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved { kind: EffectKind::ExchangeControl, source_id, .. }
                if *source_id == drake
        )),
        "the Drake's exchange instruction resolves"
    );
    assert!(!events
        .iter()
        .any(|event| matches!(event, GameEvent::ControllerChanged { .. })));

    let d = &runner.state().objects[&drake];
    assert_eq!(
        (
            d.zone,
            d.is_phased_out(),
            has_control_effect(&runner, drake),
            has_control_effect(&runner, bears),
            runner.state().objects[&bears].controller,
        ),
        (Zone::Battlefield, true, false, false, P1),
    );
}
