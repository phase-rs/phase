//! A resolution-time "tap X untapped <permanents> you control" payment defines X for the rest
//! of the resolving chain (CR 107.3f + CR 107.3i + CR 118.12), only when the prompt's selection
//! mode is `VariableX`.
//!
//! Two fixture shapes:
//! * (T) a real attack trigger parsed from a referent-free Battlesphere-grammar clause;
//! * (C) a hand-built typed chain, resolved through `resolve_ability_chain`, so the Fixed and
//!   Aggregate selection modes can be observed without a trigger event.
use engine::game::ability_utils::build_resolved_from_def;
use engine::game::combat::AttackTarget;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, Effect, TapCreaturesRequirement, TapCreaturesSelectionMode,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{CostResume, PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

/// Referent-free recipient ("to you"): the X pump and X damage grammar of Myr Battlesphere.
const TEXT: &str = "Whenever this creature attacks, you may tap X untapped Myr you control. If you do, this creature gets +X/+0 until end of turn and deals X damage to you.";

/// What the (T) fixture observed after the PayCost answer.
struct Outcome {
    power: Option<i32>,
    life_delta: i32,
    tapped: usize,
    untapped: usize,
}

/// Drive the (T) fixture: the sphere attacks P1, the optional is accepted, `pick` of the four Myr
/// are chosen at the VariableX prompt. `cost_x_paid` is the hostile cast-time authority.
fn run_trigger(pick: usize, cost_x_paid: Option<u32>) -> Outcome {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let sphere = sc.add_creature_from_oracle(P0, "Sphere", 1, 9, TEXT).id();
    let myr: Vec<ObjectId> = (0..4)
        .map(|i| {
            sc.add_creature(P0, &format!("Myr {i}"), 1, 1)
                .with_subtypes(vec!["Myr"])
                .id()
        })
        .collect();
    for _ in 0..20 {
        sc.add_card_to_library_top(P0, "Plains");
        sc.add_card_to_library_top(P1, "Plains");
    }
    let mut r: GameRunner = sc.build();
    if cost_x_paid.is_some() {
        r.state_mut().objects.get_mut(&sphere).unwrap().cost_x_paid = cost_x_paid;
    }

    // Parse pin: the trigger's execute is the optional tap-X PayCost with the Pump and
    // DealDamage riders, all reading the Variable "X" the payment must define. The
    // "any number" tap count of a VariableX payment serializes as `u32::MAX`.
    let any_number = u32::MAX.to_string();
    let parsed = serde_json::to_string(&r.state().objects[&sphere].trigger_definitions).unwrap();
    for needle in [
        "PayCost",
        "TapCreatures",
        any_number.as_str(),
        "Myr",
        "Pump",
        "DealDamage",
        "\"X\"",
        "OptionalEffectPerformed",
    ] {
        assert!(
            parsed.contains(needle),
            "grammar parsed differently: missing {needle} in {parsed}"
        );
    }

    r.advance_to_combat();
    r.declare_attackers(&[(sphere, AttackTarget::Player(P1))])
        .unwrap();
    let life0 = r.state().players[0].life;

    let mut reached_optional = false;
    let mut reached_prompt = false;
    for _ in 0..60 {
        match r.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                r.act(GameAction::OrderTriggers {
                    order: (0..triggers.len()).collect(),
                })
                .unwrap();
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                reached_optional = true;
                r.act(GameAction::DecideOptionalEffect { accept: true })
                    .unwrap();
            }
            WaitingFor::PayCost {
                kind,
                choices,
                min_count,
                resume,
                ..
            } => {
                reached_prompt = true;
                // Reach guard: the shown prompt is the resolution VariableX tap prompt with all
                // four Myr eligible and a zero floor.
                assert_eq!(
                    kind,
                    PayCostKind::TapCreatures {
                        mode: TapCreaturesSelectionMode::VariableX
                    }
                );
                assert!(matches!(resume, CostResume::Resolution));
                assert_eq!(choices.len(), 4);
                assert_eq!(min_count, 0);
                // Frame guard: the active frame is this payment's own rider chain (head Pump),
                // with X still unbound.
                let frame = r
                    .state()
                    .active_ability_continuation()
                    .expect("rider chain parked while the payment prompt is shown");
                assert!(matches!(frame.chain.effect, Effect::Pump { .. }));
                assert_eq!(frame.chain.chosen_x, None);
                let sub = frame
                    .chain
                    .sub_ability
                    .as_deref()
                    .expect("DealDamage rider");
                assert!(matches!(sub.effect, Effect::DealDamage { .. }));
                assert_eq!(sub.chosen_x, None);

                let cards: Vec<ObjectId> = choices.iter().copied().take(pick).collect();
                r.act(GameAction::SelectCards { cards }).unwrap();
            }
            WaitingFor::Priority { .. } => {
                if r.state().stack.is_empty() {
                    break;
                }
                r.act(GameAction::PassPriority).unwrap();
            }
            other => panic!("unexpected prompt {other:?}"),
        }
    }
    assert!(reached_optional, "optional prompt never reached");
    assert!(reached_prompt, "VariableX tap prompt never reached");
    let tapped = myr.iter().filter(|m| r.state().objects[*m].tapped).count();
    Outcome {
        power: r.state().objects[&sphere].power,
        life_delta: r.state().players[0].life - life0,
        tapped,
        untapped: myr.len() - tapped,
    }
}

// Tapping three of four Myr, with no cast-time X, defines X as 3.
#[test]
fn trigger_tap_three_defines_x_for_pump_and_damage() {
    let o = run_trigger(3, None);
    assert_eq!((o.tapped, o.untapped), (3, 1));
    assert_eq!(o.power, Some(4), "1 printed + X(3)");
    assert_eq!(o.life_delta, -3, "X damage to you");
}

// A hostile cast-time X on the source loses to the chosen count.
#[test]
fn trigger_chosen_count_beats_cast_time_x() {
    let o = run_trigger(2, Some(5));
    assert_eq!(o.tapped, 2);
    assert_eq!(o.power, Some(3));
    assert_eq!(o.life_delta, -2, "chosen count, not cost_x_paid");
}

// Tapping zero at the shown prompt stamps X = 0, not the cast-time value.
#[test]
fn trigger_tap_zero_stamps_x_zero_over_cast_time_x() {
    let o = run_trigger(0, Some(5));
    assert_eq!(o.tapped, 0);
    assert_eq!(o.power, Some(1));
    assert_eq!(o.life_delta, 0);
}

// Sibling of the tap-three and tap-zero rows above: tapping zero with no cast-time X.
#[test]
fn trigger_tap_zero_without_cast_time_x() {
    let o = run_trigger(0, None);
    assert_eq!(o.tapped, 0);
    assert_eq!(o.power, Some(1));
    assert_eq!(o.life_delta, 0);
}

// ---------------------------------------------------------------------------------------------
// (C) typed chains
// ---------------------------------------------------------------------------------------------

fn chain_def(requirement: &TapCreaturesRequirement, with_rider: bool) -> AbilityDefinition {
    let rider = if with_rider {
        serde_json::json!({
            "kind": "Spell",
            "effect": {"type": "DealDamage",
                "amount": {"type": "Ref", "qty": {"type": "Variable", "name": "X"}},
                "target": {"type": "Controller"}},
            "cost": null, "sub_ability": null, "duration": null, "description": null,
            "target_prompt": null,
            "condition": {"type": "EffectOutcome", "signal": "OptionalEffectPerformed"},
            "optional_targeting": false, "optional": false, "forward_result": false
        })
    } else {
        serde_json::Value::Null
    };
    let json = serde_json::json!({
        "kind": "Spell",
        "effect": {"type": "PayCost",
            "cost": {"type": "TapCreatures",
                "requirement": serde_json::to_value(requirement).unwrap(),
                "filter": {"type": "Typed", "type_filters": [{"Subtype": "Myr"}],
                    "controller": "You", "properties": []}},
            "payer": {"type": "Controller"}},
        "cost": null, "sub_ability": rider, "duration": null, "description": null,
        "target_prompt": null, "condition": null, "optional_targeting": false,
        "optional": true, "forward_result": false
    });
    serde_json::from_value(json).expect("chain definition")
}

struct ChainOutcome {
    mode: TapCreaturesSelectionMode,
    life_delta: i32,
    tapped: usize,
    drained: bool,
}

/// Resolve the (C) chain, accept the optional, answer the tap prompt with `pick` Myr.
fn run_chain(
    requirement: &TapCreaturesRequirement,
    with_rider: bool,
    pick: usize,
    power: i32,
) -> ChainOutcome {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let src = sc.add_creature(P0, "Src", 1, 9).id();
    let myr: Vec<ObjectId> = (0..4)
        .map(|i| {
            sc.add_creature(P0, &format!("Myr {i}"), power, 2)
                .with_subtypes(vec!["Myr"])
                .id()
        })
        .collect();
    for _ in 0..20 {
        sc.add_card_to_library_top(P0, "Plains");
        sc.add_card_to_library_top(P1, "Plains");
    }
    let mut r: GameRunner = sc.build();
    let ability = build_resolved_from_def(&chain_def(requirement, with_rider), src, P0);
    let life0 = r.state().players[0].life;
    let mut events = Vec::new();
    resolve_ability_chain(r.state_mut(), &ability, &mut events, 0).unwrap();

    r.act(GameAction::DecideOptionalEffect { accept: true })
        .expect("optional prompt accepted");
    let WaitingFor::PayCost {
        kind: PayCostKind::TapCreatures { mode },
        choices,
        resume,
        ..
    } = r.state().waiting_for.clone()
    else {
        panic!("tap prompt expected, got {:?}", r.state().waiting_for);
    };
    assert!(matches!(resume, CostResume::Resolution));
    if with_rider {
        // Frame guard: the parked frame is the DealDamage rider with X unbound.
        let frame = r
            .state()
            .active_ability_continuation()
            .expect("rider parked");
        assert!(matches!(frame.chain.effect, Effect::DealDamage { .. }));
        assert_eq!(frame.chain.chosen_x, None);
    }
    let cards: Vec<ObjectId> = choices.iter().copied().take(pick).collect();
    r.act(GameAction::SelectCards { cards }).unwrap();
    ChainOutcome {
        mode,
        life_delta: r.state().players[0].life - life0,
        tapped: myr.iter().filter(|m| r.state().objects[*m].tapped).count(),
        drained: r.state().active_ability_continuation().is_none()
            && matches!(r.state().waiting_for, WaitingFor::Priority { .. }),
    }
}

// A VariableX payment with no trigger event: the chosen count alone defines X.
#[test]
fn chain_variable_x_defines_x() {
    let o = run_chain(
        &TapCreaturesRequirement::Count { count: u32::MAX },
        true,
        2,
        2,
    );
    assert_eq!(o.mode, TapCreaturesSelectionMode::VariableX);
    assert_eq!(o.tapped, 2);
    assert!(
        o.drained,
        "rider drained, so the zero in the sibling rows is not vacuous"
    );
    assert_eq!(o.life_delta, -2);
}

// Sibling of the VariableX chain row: a Fixed tap count must not define X.
#[test]
fn chain_fixed_count_does_not_define_x() {
    let o = run_chain(&TapCreaturesRequirement::Count { count: 2 }, true, 2, 2);
    assert_eq!(o.mode, TapCreaturesSelectionMode::Fixed);
    assert_eq!(o.tapped, 2);
    assert!(o.drained);
    assert_eq!(o.life_delta, 0, "unbound X reads 0");
}

// Sibling of the VariableX chain row: a power-threshold (Crew/Saddle shape) tap must not define X.
#[test]
fn chain_aggregate_does_not_define_x() {
    let requirement = TapCreaturesRequirement::total_power_at_least(4);
    for pick in [2, 3] {
        let o = run_chain(&requirement, true, pick, 2);
        assert!(matches!(o.mode, TapCreaturesSelectionMode::Aggregate(_)));
        assert_eq!(o.tapped, pick);
        assert!(o.drained);
        assert_eq!(
            o.life_delta, 0,
            "aggregate selection never binds X (pick {pick})"
        );
    }
}

// Sibling: no rider and nothing parked, so the stamp is a no-op.
#[test]
fn chain_without_rider_binds_nothing_and_does_not_panic() {
    let o = run_chain(
        &TapCreaturesRequirement::Count { count: u32::MAX },
        false,
        2,
        2,
    );
    assert_eq!(o.mode, TapCreaturesSelectionMode::VariableX);
    assert_eq!(o.tapped, 2);
    assert!(o.drained);
    assert_eq!(o.life_delta, 0);
}
