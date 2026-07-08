//! Regression for issue #4560: Winter Soldier, Reborn Avenger attack trigger must
//! reanimate a legal graveyard creature and grant Heroes an extra +1/+1 counter.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{Effect, EffectKind, TargetFilter, TypeFilter};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

const WINTER_SOLDIER_ORACLE: &str = "Whenever Winter Soldier attacks, return target creature card with mana value less than or equal to Winter Soldier's power from your graveyard to the battlefield. If a Hero enters this way, it enters with an additional +1/+1 counter on it.";

fn p1p1_count(runner: &GameRunner, id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .map(|obj| {
            obj.counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0)
        })
        .unwrap_or(0)
}

fn drive_attack_resolution_collecting_events(
    runner: &mut GameRunner,
    graveyard_target: ObjectId,
) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..80 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return events;
                }
                events.extend(
                    runner
                        .act(GameAction::PassPriority)
                        .expect("pass priority")
                        .events,
                );
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let count = triggers.len();
                events.extend(
                    runner
                        .act(GameAction::OrderTriggers {
                            order: (0..count).collect(),
                        })
                        .expect("order triggers")
                        .events,
                );
            }
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            }
            | WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let slot = &target_slots[selection.current_slot];
                let target = slot
                    .legal_targets
                    .iter()
                    .find(|t| {
                        matches!(t, engine::types::ability::TargetRef::Object(id) if *id == graveyard_target)
                    })
                    .or_else(|| slot.legal_targets.first())
                    .cloned();
                events.extend(
                    runner
                        .act(GameAction::ChooseTarget { target })
                        .expect("choose graveyard target")
                        .events,
                );
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                events.extend(
                    runner
                        .act(GameAction::DecideOptionalEffect { accept: true })
                        .expect("accept optional effect")
                        .events,
                );
            }
            other if runner.state().stack.is_empty() => {
                panic!("unexpected waiting state during attack resolution: {other:?}");
            }
            _ => {}
        }
    }
    panic!("attack trigger did not finish resolving");
}

fn drive_attack_resolution(runner: &mut GameRunner, graveyard_target: ObjectId) {
    let _ = drive_attack_resolution_collecting_events(runner, graveyard_target);
}

#[test]
fn winter_soldier_attack_trigger_parses_reanimation_and_hero_counter_rider() {
    let parsed = parse_oracle_text(
        WINTER_SOLDIER_ORACLE,
        "Winter Soldier, Reborn Avenger",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Hero".to_string()],
    );
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::Attacks)
        .expect("attacks trigger");
    let execute = trigger.execute.as_ref().expect("execute");
    assert!(
        matches!(execute.effect.as_ref(), Effect::ChangeZone { .. }),
        "head must reanimate from graveyard, got {:?}",
        execute.effect
    );
    assert!(
        execute.forward_result,
        "ChangeZone must forward the returned card"
    );
    let Effect::ChangeZone {
        conditional_enter_with_counters,
        ..
    } = execute.effect.as_ref()
    else {
        panic!("expected ChangeZone head");
    };
    assert_eq!(
        conditional_enter_with_counters.len(),
        1,
        "Hero counter rider must fold into conditional_enter_with_counters"
    );
    let (filter, counter_type, count) = &conditional_enter_with_counters[0];
    assert_eq!(*counter_type, CounterType::Plus1Plus1);
    assert!(
        matches!(
            count,
            engine::types::ability::QuantityExpr::Fixed { value: 1 }
        ),
        "expected one additional +1/+1 counter, got {count:?}"
    );
    let TargetFilter::Typed(typed) = filter else {
        panic!("expected Hero filter, got {filter:?}");
    };
    assert!(typed
        .type_filters
        .contains(&TypeFilter::Subtype("Hero".into())));
    assert!(
        execute.sub_ability.is_none(),
        "counter rider must not remain as a PutCounter sub-ability"
    );
}

#[test]
fn winter_soldier_reanimates_hero_with_extra_counter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let soldier = scenario
        .add_creature_from_oracle(
            P0,
            "Winter Soldier, Reborn Avenger",
            3,
            3,
            WINTER_SOLDIER_ORACLE,
        )
        .id();

    let hero = scenario
        .add_creature_to_graveyard(P0, "Fallen Hero", 2, 2)
        .with_subtypes(vec!["Hero"])
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::White],
        })
        .id();

    let mut runner = scenario.build();
    assert!(
        !runner.state().objects[&soldier]
            .trigger_definitions
            .is_empty(),
        "Winter Soldier must register its attack trigger from oracle text"
    );
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(soldier, AttackTarget::Player(P1))])
        .expect("attack with Winter Soldier");

    drive_attack_resolution(&mut runner, hero);

    assert_eq!(
        runner.state().objects[&hero].zone,
        Zone::Battlefield,
        "Hero must return from graveyard"
    );
    assert!(
        p1p1_count(&runner, hero) >= 1,
        "Hero reanimated this way must enter with an additional +1/+1 counter"
    );
}

/// CR 122.1 + CR 614.1c: The Hero counter must ride the battlefield-entry
/// pipeline, not a separate post-move `PutCounter` effect. A folded
/// `conditional_enter_with_counters` rider must not also resolve a
/// `PutCounter` sub-ability after the zone move.
#[test]
fn winter_soldier_hero_counter_is_entry_time_not_post_move_put_counter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let soldier = scenario
        .add_creature_from_oracle(
            P0,
            "Winter Soldier, Reborn Avenger",
            3,
            3,
            WINTER_SOLDIER_ORACLE,
        )
        .id();

    let hero = scenario
        .add_creature_to_graveyard(P0, "Fallen Hero", 2, 2)
        .with_subtypes(vec!["Hero"])
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::White],
        })
        .id();

    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(soldier, AttackTarget::Player(P1))])
        .expect("attack with Winter Soldier");

    let events = drive_attack_resolution_collecting_events(&mut runner, hero);

    assert_eq!(runner.state().objects[&hero].zone, Zone::Battlefield);
    assert!(
        p1p1_count(&runner, hero) >= 1,
        "Hero must still receive the +1/+1 counter"
    );
    assert!(
        !events.iter().any(|ev| {
            matches!(
                ev,
                GameEvent::EffectResolved {
                    kind: EffectKind::PutCounter,
                    source_id,
                } if *source_id == soldier
            )
        }),
        "Hero rider must fold into ChangeZone entry counters, not resolve a \
         separate PutCounter effect afterward (events: {events:#?})"
    );
}
