//! Regression for issue #935 — Amphin Mutineer ETB exile + token for exiled creature's controller.
//!
//! Oracle: "When this creature enters, exile up to one target non-Salamander creature.
//! That creature's controller creates a 4/3 blue Salamander Warrior creature token."

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{Effect, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const AMPHIN_FULL: &str = "When this creature enters, exile up to one target non-Salamander creature. That creature's controller creates a 4/3 blue Salamander Warrior creature token.\nEncore {4}{U}{U} ({4}{U}{U}, Exile this card from your graveyard: For each opponent, create a token copy that attacks that opponent this turn if able. They gain haste. Sacrifice them at the beginning of the next end step. Activate only as a sorcery.)";

const AMPHIN_ETB: &str = "When this creature enters, exile up to one target non-Salamander creature. That creature's controller creates a 4/3 blue Salamander Warrior creature token.";

fn floating_mana(generic: usize, blue: usize) -> Vec<ManaUnit> {
    let mut pool = Vec::new();
    for _ in 0..generic {
        pool.push(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    for _ in 0..blue {
        pool.push(ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]));
    }
    pool
}

fn cast_and_resolve_etb(
    scenario: GameScenario,
    mutineer: ObjectId,
    prey: ObjectId,
) -> engine::game::scenario::GameRunner {
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&mutineer].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: mutineer,
            card_id,
            targets: vec![],
        })
        .expect("cast Amphin Mutineer");

    for _ in 0..60 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(prey)],
                    })
                    .expect("select exile target");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    if runner.act(GameAction::PassPriority).is_err() {
                        break;
                    }
                    if runner.state().stack.is_empty()
                        && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
                    {
                        break;
                    }
                } else if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }
    runner
}

#[test]
fn amphin_mutineer_full_card_etb_has_token_sub_ability() {
    let parsed = parse_oracle_text(
        AMPHIN_FULL,
        "Amphin Mutineer",
        &["encore".to_string()],
        &[],
        &[],
    );
    let etb = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::ChangesZone)
        .expect("ETB trigger");
    let execute = etb.execute.as_ref().expect("execute");
    let token_ability = execute.sub_ability.as_ref().expect("token sub_ability");
    assert!(matches!(
        token_ability.effect.as_ref(),
        Effect::Token {
            owner: TargetFilter::ParentTargetController,
            ..
        }
    ));
}

/// CR 111.2: Exiling an opponent's non-Salamander creature creates a Salamander Warrior under that creature's controller.
#[test]
fn amphin_mutineer_etb_exile_and_token_for_target_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mutineer = scenario
        .add_creature_to_hand_from_oracle(P0, "Amphin Mutineer", 3, 3, AMPHIN_ETB)
        .id();
    let prey = scenario.add_creature(P1, "Grizzly Bear", 2, 2).id();
    scenario.with_mana_pool(P0, floating_mana(3, 1));

    let runner = cast_and_resolve_etb(scenario, mutineer, prey);
    let state = runner.state();

    assert_eq!(
        state.objects.get(&prey).map(|o| o.zone),
        Some(Zone::Exile),
        "target should be exiled"
    );

    let salamander_tokens: Vec<_> = state
        .objects
        .values()
        .filter(|o| {
            o.zone == Zone::Battlefield
                && o.controller == P1
                && o.is_token
                && o.power == Some(4)
                && o.toughness == Some(3)
                && o.card_types.subtypes.iter().any(|s| s == "Salamander")
        })
        .collect();
    assert!(
        !salamander_tokens.is_empty(),
        "P1 should control a 4/3 Salamander Warrior token"
    );
}
