//! Regression coverage for the engine-authored selectable attack-target support
//! contract. The client receives only target pairs that participate in a complete
//! declaration accepted at the unchanged CR 508.1d free threshold; strict
//! declaration validation remains the authority for the submitted assignment.

use std::collections::HashSet;

use engine::game::combat::{build_declare_attackers_waiting_for, AttackTarget};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::parser::oracle_static::parse_static_line;
use engine::types::ability::StaticDefinition;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::{CombatAloneAction, CombatAloneRequirement, StaticMode};

const P2: PlayerId = PlayerId(2);

fn declare_attackers_targets(
    state: &engine::types::game_state::GameState,
) -> (
    Vec<engine::types::identifiers::ObjectId>,
    Vec<AttackTarget>,
    std::collections::HashMap<engine::types::identifiers::ObjectId, Vec<AttackTarget>>,
) {
    match build_declare_attackers_waiting_for(state) {
        WaitingFor::DeclareAttackers {
            valid_attacker_ids,
            valid_attack_targets,
            valid_attack_targets_by_attacker: Some(by_attacker),
            ..
        } => (valid_attacker_ids, valid_attack_targets, by_attacker),
        other => panic!("expected DeclareAttackers prompt, got {other:?}"),
    }
}

/// The forced-pair solver must not fall back to the empty declaration. This
/// exercises the interaction of a global cap with both CombatAlone directions:
/// the companion-dependent attacker has no selectable pair, while the sole-only
/// attacker still has its one-attacker witnesses. Every candidate remains a map
/// key, and the aggregate is exactly the sorted union of support values.
#[test]
fn selectable_target_support_respects_caps_and_combat_alone_without_empty_fallback() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::DeclareAttackers);
    let sole = scenario
        .add_creature(P0, "Only Alone", 2, 2)
        .with_static_definition(StaticDefinition::new(StaticMode::CombatAlone {
            action: CombatAloneAction::Attack,
            requirement: CombatAloneRequirement::MustBeSole,
        }))
        .id();
    let needs_companion = scenario
        .add_creature(P0, "Needs Company", 2, 2)
        .with_static_definition(StaticDefinition::new(StaticMode::CombatAlone {
            action: CombatAloneAction::Attack,
            requirement: CombatAloneRequirement::NeedsCompanion,
        }))
        .id();
    let companion = scenario.add_creature(P0, "Potential Companion", 2, 2).id();
    let _cap = scenario
        .add_creature(P1, "One Attacker Cap", 2, 2)
        .with_static_definition(StaticDefinition::new(StaticMode::MaxAttackersEachCombat {
            max: 1,
            defender: None,
        }))
        .id();
    let mut runner = scenario.build();

    let (candidates, aggregate, by_attacker) = declare_attackers_targets(runner.state());
    assert_eq!(
        by_attacker.keys().copied().collect::<HashSet<_>>(),
        candidates.into_iter().collect(),
        "every eligible candidate has an authoritative support entry, including empty support"
    );
    assert_eq!(
        by_attacker.get(&needs_companion),
        Some(&Vec::new()),
        "a forced companion-dependent pair has no complete witness under the one-attacker cap"
    );
    assert_eq!(
        aggregate,
        vec![AttackTarget::Player(P1), AttackTarget::Player(P2)],
        "the aggregate target list is the sorted union of selectable support"
    );

    runner.state_mut().waiting_for = build_declare_attackers_waiting_for(runner.state());
    assert!(
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![(needs_companion, AttackTarget::Player(P1))],
                bands: vec![],
            })
            .is_err(),
        "strict validation still rejects a companion-dependent creature attacking alone"
    );
    runner.state_mut().waiting_for = build_declare_attackers_waiting_for(runner.state());
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(sole, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("the sole-only attacker has a complete one-attacker witness");
    assert!(
        runner.state().combat.as_ref().is_some_and(|combat| combat
            .attackers
            .iter()
            .any(|attacker| attacker.object_id == sole)),
        "accepted support remains executable through the unchanged declaration validator"
    );
    assert!(
        by_attacker
            .get(&companion)
            .is_some_and(|targets| !targets.is_empty()),
        "the ordinary companion candidate retains independently selectable support"
    );
}

/// A taxed pair is still selectable when it appears in a complete declaration
/// that meets the free CR 508.1d bar. Taxes constrain payment, not target support.
#[test]
fn selectable_target_support_includes_voluntarily_taxed_witnesses() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::DeclareAttackers);
    let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
    let def = parse_static_line(
        "Creatures can't attack you unless their controller pays {2} for each creature they control that's attacking you.",
    )
    .expect("Ghostly Prison static should parse");
    scenario
        .add_creature(P2, "Ghostly Prison", 2, 2)
        .with_static_definition(def);
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&attacker)
        .expect("attacker exists")
        .goaded_by
        .insert(P1);

    let (_, aggregate, by_attacker) = declare_attackers_targets(runner.state());
    assert_eq!(
        by_attacker.get(&attacker),
        Some(&vec![AttackTarget::Player(P1), AttackTarget::Player(P2)]),
        "the exact full-universe witness includes the voluntarily taxed P2 attack, while the free bar remains P1's lower score"
    );
    assert_eq!(
        aggregate,
        vec![AttackTarget::Player(P1), AttackTarget::Player(P2)],
        "aggregate support retains the taxed target"
    );
}

const FIRKRAAG_ORACLE: &str = "Flying, haste\nWhenever one or more Dragons you control attack an opponent, goad target creature that player controls.\nWhenever a creature deals combat damage to one of your opponents, if that creature had to attack this combat, you put a +1/+1 counter on Firkraag, Cunning Instigator and you draw a card.";
const SEARSLICER_ORACLE: &str = "Raid — At the beginning of your end step, if you attacked this turn, create a 1/1 red Goblin creature token.";

/// Literal-card production path: Firkraag's actual triggered Oracle text targets
/// Searslicer, producing the goad designation that the next DeclareAttackers
/// prompt projects into selectable support. The goader is excluded, the other
/// opponent is included, and strict validation still decides submitted attacks.
#[test]
fn firkraag_goad_produces_selectable_target_support_for_searslicer() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::DeclareAttackers);
    let firkraag = scenario
        .add_creature_from_oracle(P2, "Firkraag, Cunning Instigator", 3, 3, FIRKRAAG_ORACLE)
        .with_subtypes(vec!["Dragon"])
        .id();
    let searslicer = scenario
        .add_creature_from_oracle(P0, "Searslicer Goblin", 2, 1, SEARSLICER_ORACLE)
        .with_subtypes(vec!["Goblin", "Warrior"])
        .id();
    let mut runner = scenario.build();

    runner.state_mut().active_player = P2;
    runner.state_mut().priority_player = P2;
    runner.state_mut().phase = Phase::DeclareAttackers;
    runner.state_mut().waiting_for = build_declare_attackers_waiting_for(runner.state());
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(firkraag, AttackTarget::Player(P0))],
            bands: vec![],
        })
        .expect("Firkraag attacks P0 and creates its real goad trigger");

    for _ in 0..32 {
        match &runner.state().waiting_for {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::TriggerTargetSelection {
                ref target_slots, ..
            } => {
                assert_eq!(target_slots.len(), 1, "Firkraag has one target slot");
                assert_eq!(
                    target_slots[0].legal_targets,
                    vec![engine::types::ability::TargetRef::Object(searslicer)],
                    "Firkraag's literal target restriction finds the attacked player's Searslicer"
                );
                runner
                    .choose_first_legal_target()
                    .expect("choose Searslicer for Firkraag's goad trigger");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            _ => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("advance the Firkraag trigger pipeline");
            }
        }
    }
    assert!(
        runner.state().objects[&searslicer].goaded_by.contains(&P2),
        "Firkraag's resolved trigger genuinely goads Searslicer"
    );

    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().phase = Phase::DeclareAttackers;
    runner.state_mut().waiting_for = build_declare_attackers_waiting_for(runner.state());
    let (_, _, by_attacker) = declare_attackers_targets(runner.state());
    assert_eq!(
        by_attacker.get(&searslicer),
        Some(&vec![AttackTarget::Player(P1)]),
        "P2 is the goader and therefore unsupported; P1 is the only maximum-score target"
    );

    assert!(
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            })
            .is_err(),
        "a goaded creature cannot be omitted when P1 is available"
    );
    runner.state_mut().waiting_for = build_declare_attackers_waiting_for(runner.state());
    assert!(
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![(searslicer, AttackTarget::Player(P2))],
                bands: vec![],
            })
            .is_err(),
        "attacking the goader scores below the unchanged CR 508.1d free threshold"
    );
    runner.state_mut().waiting_for = build_declare_attackers_waiting_for(runner.state());
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(searslicer, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("attacking P1 obeys every obtainable goad requirement");
}
