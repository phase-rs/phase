//! Painbringer (FUT) — "{T}, Exile any number of cards from your graveyard:
//! Target creature gets -X/-X until end of turn, where X is the number of
//! cards exiled this way."
//!
//! Drives the REAL pipeline (parser + `GameRunner::act` activation →
//! interactive `PayCost { ExileFromZone }` → resolution) for a bare-zone
//! exile-any-number ACTIVATED cost: the typed sentinel, the (0, max) payment
//! bounds, and the cost→effect tracked-set binding (CR 608.2c) that X reads
//! off the stack.

use engine::game::scenario::{GameRunner, GameScenario};
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::AbilityCost;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

const PAINBRINGER: &str = "{T}, Exile any number of cards from your graveyard: Target creature gets -X/-X until end of turn, where X is the number of cards exiled this way.";

fn painbringer_parse() -> engine::parser::oracle::ParsedAbilities {
    parse_oracle_text(
        PAINBRINGER,
        "Painbringer",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Minion".to_string()],
    )
}

/// Painbringer's cost must parse to tap plus the bare-zone any-number exile
/// sentinel — not the EffectCost fallback.
///
/// Revert probe: with the pre-change parser the exile leg is
/// `EffectCost{ChangeZone}` and the `matches!` below fails.
#[test]
fn painbringer_parses_to_tap_plus_bare_any_number_exile() {
    let parsed = painbringer_parse();
    let ability = parsed
        .abilities
        .iter()
        .find(|a| a.cost.is_some())
        .expect("Painbringer activated ability");
    match ability.cost.as_ref().expect("cost") {
        AbilityCost::Composite { costs } => {
            assert!(
                costs.iter().any(|c| matches!(c, AbilityCost::Tap)),
                "tap leg survives, got {costs:?}"
            );
            assert!(
                costs.iter().any(|c| matches!(
                    c,
                    AbilityCost::Exile {
                        count,
                        zone: Some(Zone::Graveyard),
                        filter: None,
                    } if *count == engine::types::ability::EXILE_COST_ANY_NUMBER
                )),
                "bare any-number exile leg survives, got {costs:?}"
            );
        }
        other => panic!("expected Composite cost, got {other:?}"),
    }
}

fn setup() -> (GameRunner, ObjectId, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    let painbringer = scenario
        .add_creature_from_oracle(P0, "Painbringer", 2, 2, PAINBRINGER)
        .id();
    let target = scenario.add_creature(P1, "Target Bear", 3, 3).id();
    let mut runner = scenario.build();
    let gy: Vec<ObjectId> = (0..3)
        .map(|i| {
            create_object(
                runner.state_mut(),
                CardId(500 + i),
                P0,
                format!("Gy Filler {i}"),
                Zone::Graveyard,
            )
        })
        .collect();
    (runner, painbringer, target, gy)
}

/// Drive prompts until the stack drains: choose `target` for targeting,
/// exile `exile` for the cost, pass priority otherwise.
fn drive(runner: &mut GameRunner, target: ObjectId, exile: &[ObjectId]) {
    for _ in 0..60 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(engine::types::ability::TargetRef::Object(target)),
                    })
                    .expect("choose target");
            }
            WaitingFor::PayCost {
                kind: engine::types::game_state::PayCostKind::ExileFromZone { .. },
                ..
            } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: exile.to_vec(),
                    })
                    .expect("exile selection");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return;
                }
                runner.act(GameAction::PassPriority).expect("pass");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    panic!("drive loop did not drain the stack");
}

/// Exiling two of three graveyard cards gives the 3/3 target -2/-2.
///
/// Revert probes: (a) without the typed parse the activation is refused as
/// unpayable; (b) without the activation resume routing the prompt forces
/// all three cards; (c) without the concrete-id tracked-set binding X reads
/// 0 and the target stays 3/3.
#[test]
fn painbringer_exile_two_shrinks_target_by_two() {
    let (mut runner, painbringer, target, gy) = setup();
    runner
        .act(GameAction::ActivateAbility {
            source_id: painbringer,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Painbringer activates");
    drive(&mut runner, target, &gy[..2]);
    let state = runner.state();
    assert_eq!(state.objects[&target].power, Some(1));
    assert_eq!(state.objects[&target].toughness, Some(1));
    assert_eq!(state.objects[&gy[0]].zone, Zone::Exile);
    assert_eq!(state.objects[&gy[1]].zone, Zone::Exile);
    assert_eq!(state.objects[&gy[2]].zone, Zone::Graveyard);
    assert!(state.objects[&painbringer].tapped);
}

/// Exiling zero is legal (CR 601.2b: "any number" admits zero): the ability
/// resolves as -0/-0 and the graveyard is untouched.
#[test]
fn painbringer_exile_zero_resolves_without_shrinking() {
    let (mut runner, painbringer, target, gy) = setup();
    runner
        .act(GameAction::ActivateAbility {
            source_id: painbringer,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Painbringer activates");
    drive(&mut runner, target, &[]);
    let state = runner.state();
    assert_eq!(state.objects[&target].power, Some(3));
    assert_eq!(state.objects[&target].toughness, Some(3));
    for id in &gy {
        assert_eq!(state.objects[id].zone, Zone::Graveyard);
    }
    assert!(state.objects[&painbringer].tapped);
    assert!(runner.state().stack.is_empty());
}

/// Fence: a mixed fixed + any-number composite activation cost routes each
/// leg to its own bounds — the fixed leg stays exact-count, the sentinel leg
/// stays (0, max) — with no printed-card witness (no printed card pairs a
/// fixed exile leg with an any-number exile leg; nearest printed card:
/// Painbringer, single any-number leg). The legs draw from disjoint zones so
/// the joint leg model (which fails closed when an unmodeled demand competes
/// for objects) admits the activation; the any-number leg attaches itself to
/// its resume so dispatch needs no zone/count re-derivation. This test pins
/// both dispatches end to end.
#[test]
fn mixed_fixed_and_any_number_exile_legs_route_independently() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter,
    };
    use engine::types::game_state::CostResume;

    let mut scenario = GameScenario::new();
    let source = scenario.add_creature(P0, "Composite Exiler", 2, 2).id();
    let mut runner = scenario.build();
    let hand: Vec<ObjectId> = (0..2)
        .map(|i| {
            create_object(
                runner.state_mut(),
                CardId(600 + i),
                P0,
                format!("Hand Filler {i}"),
                Zone::Hand,
            )
        })
        .collect();
    let gy: Vec<ObjectId> = (0..3)
        .map(|i| {
            create_object(
                runner.state_mut(),
                CardId(610 + i),
                P0,
                format!("Gy Filler {i}"),
                Zone::Graveyard,
            )
        })
        .collect();
    let mut ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
    );
    ability.cost = Some(AbilityCost::Composite {
        costs: vec![
            AbilityCost::Exile {
                count: 2,
                zone: Some(Zone::Hand),
                filter: None,
            },
            AbilityCost::Exile {
                count: engine::types::ability::EXILE_COST_ANY_NUMBER,
                zone: Some(Zone::Graveyard),
                filter: None,
            },
        ],
    });
    runner
        .state_mut()
        .objects
        .get_mut(&source)
        .expect("source")
        .abilities = std::sync::Arc::new(vec![ability]);
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("composite activates");

    // Leg one: the fixed leg prompts for exactly two with a bare resume, and
    // a short payment is refused (no misroute to the any-number handler).
    match &runner.state().waiting_for {
        WaitingFor::PayCost {
            kind: engine::types::game_state::PayCostKind::ExileFromZone { .. },
            count: 2,
            resume: CostResume::Spell { .. },
            ..
        } => {}
        other => panic!("expected exact-count fixed prompt, got {other:?}"),
    }
    assert!(
        runner
            .act(GameAction::SelectCards {
                cards: hand[..1].to_vec(),
            })
            .is_err(),
        "one card must not satisfy the fixed two-card leg"
    );
    runner
        .act(GameAction::SelectCards {
            cards: hand[..2].to_vec(),
        })
        .expect("two cards satisfy the fixed leg");

    // Leg two: the sentinel leg prompts over the graveyard with its leg
    // attached, and a partial payment is accepted (no exact-count trap).
    match &runner.state().waiting_for {
        WaitingFor::PayCost {
            kind:
                engine::types::game_state::PayCostKind::ExileFromZone {
                    zone: engine::types::zones::ExileCostSourceZone::Graveyard,
                },
            count: 3,
            min_count: 0,
            resume: CostResume::SpellCost { cost: attached, .. },
            ..
        } => {
            assert!(
                matches!(
                    attached.as_ref(),
                    AbilityCost::Exile { count, .. }
                        if *count == engine::types::ability::EXILE_COST_ANY_NUMBER
                ),
                "resume carries the sentinel leg, got {attached:?}"
            );
        }
        other => panic!("expected attached any-number prompt, got {other:?}"),
    }
    runner
        .act(GameAction::SelectCards {
            cards: gy[..1].to_vec(),
        })
        .expect("one card satisfies the any-number leg");
    let state = runner.state();
    assert_eq!(state.objects[&hand[0]].zone, Zone::Exile);
    assert_eq!(state.objects[&hand[1]].zone, Zone::Exile);
    assert_eq!(state.objects[&gy[0]].zone, Zone::Exile);
    assert_eq!(state.objects[&gy[1]].zone, Zone::Graveyard);
    assert_eq!(state.objects[&gy[2]].zone, Zone::Graveyard);
}

/// Same-zone mixed legs share one object pool, so the joint leg model fails
/// closed and the activation is refused — the dispatch below is unreachable
/// and no misroute can manifest. Pins the design boundary, not a behavior.
#[test]
fn mixed_same_zone_exile_legs_refuse_activation_fail_closed() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter,
    };

    let mut scenario = GameScenario::new();
    let source = scenario.add_creature(P0, "Composite Exiler", 2, 2).id();
    let mut runner = scenario.build();
    for i in 0..4 {
        create_object(
            runner.state_mut(),
            CardId(620 + i),
            P0,
            format!("Hand Filler {i}"),
            Zone::Hand,
        );
    }
    let mut ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
    );
    ability.cost = Some(AbilityCost::Composite {
        costs: vec![
            AbilityCost::Exile {
                count: 2,
                zone: Some(Zone::Hand),
                filter: None,
            },
            AbilityCost::Exile {
                count: engine::types::ability::EXILE_COST_ANY_NUMBER,
                zone: Some(Zone::Hand),
                filter: None,
            },
        ],
    });
    runner
        .state_mut()
        .objects
        .get_mut(&source)
        .expect("source")
        .abilities = std::sync::Arc::new(vec![ability]);
    assert!(
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .is_err(),
        "competing unmodeled demand must refuse activation"
    );
}

/// Exiling all three gives -3/-3, killing the 3/3 — the readback counts
/// the whole paid set, not a stale or partial one.
#[test]
fn painbringer_exile_all_kills_three_toughness_target() {
    let (mut runner, painbringer, target, gy) = setup();
    runner
        .act(GameAction::ActivateAbility {
            source_id: painbringer,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Painbringer activates");
    drive(&mut runner, target, &gy);
    let state = runner.state();
    assert_eq!(state.objects[&target].zone, Zone::Graveyard);
    for id in &gy {
        assert_eq!(state.objects[id].zone, Zone::Exile);
    }
    assert!(state.objects[&painbringer].tapped);
}
