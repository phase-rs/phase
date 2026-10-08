//! Regression for GitHub issue #5999 — Auras must go to the graveyard when the
//! permanent they are attached to is exiled.
//!
//! CR 704.5m: An Aura attached to an illegal object or player, or that is no
//! longer attached to anything legal, is put into its owner's graveyard.
//! CR 303.4c: An enchanted object that no longer exists, or is not on the
//! battlefield (for standard Auras), is an illegal attachment.

use engine::game::effects::attach::attach_to;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const PATH_TO_EXILE: &str = "Exile target creature. Its controller may search their library for a basic land card, put that card onto the battlefield tapped, then shuffle.";
const SIMPLE_EXILE: &str = "Exile target creature.";

fn drive_spell_until_settled(runner: &mut engine::game::scenario::GameRunner, host: ObjectId) {
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(host)],
                    })
                    .expect("target the enchanted creature");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .expect("decline the optional land search");
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority to resolve the stack");
            }
            _ if runner.state().stack.is_empty() => break,
            _ => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority while resolving");
            }
        }
    }
}

#[test]
fn issue_5999_aura_goes_to_graveyard_when_host_is_exiled_by_simple_exile_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let host = scenario.add_creature(P1, "Enchanted Host", 3, 3).id();
    let aura = scenario
        .add_creature(P0, "Pacifism", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text("Enchant creature\nEnchanted creature can't attack.")
        .id();
    let exile = scenario
        .add_spell_to_hand_from_oracle(P0, "Simple Exile", true, SIMPLE_EXILE)
        .id();

    let mut runner = scenario.build();
    attach_to(runner.state_mut(), aura, host);

    runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
        ManaType::White,
        ObjectId(0),
        false,
        vec![],
    ));

    let card_id = runner.state().objects[&exile].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: exile,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast simple exile");

    drive_spell_until_settled(&mut runner, host);

    assert_eq!(runner.state().objects[&host].zone, Zone::Exile);
    assert_eq!(
        runner.state().objects[&aura].zone,
        Zone::Graveyard,
        "CR 704.5m: unconditional exile must graveyard the Aura once the spell settles"
    );
}

#[test]
fn issue_5999_aura_stays_on_battlefield_until_optional_rider_resolves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let host = scenario.add_creature(P1, "Enchanted Host", 3, 3).id();
    let aura = scenario
        .add_creature(P0, "Pacifism", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text("Enchant creature\nEnchanted creature can't attack.")
        .id();
    let path = scenario
        .add_spell_to_hand_from_oracle(P0, "Path to Exile", true, PATH_TO_EXILE)
        .id();

    let mut runner = scenario.build();
    attach_to(runner.state_mut(), aura, host);

    runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
        ManaType::White,
        ObjectId(0),
        false,
        vec![],
    ));

    let card_id = runner.state().objects[&path].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: path,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Path to Exile");

    let mut saw_optional = false;
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(host)],
                    })
                    .expect("target the enchanted creature");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                saw_optional = true;
                assert_eq!(
                    runner.state().objects[&host].zone,
                    Zone::Exile,
                    "host must already be exiled before the optional rider prompts"
                );
                assert_eq!(
                    runner.state().objects[&aura].zone,
                    Zone::Battlefield,
                    "CR 704.4: SBAs must not graveyard the Aura while the spell is still resolving"
                );
                runner
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .expect("decline the optional land search");
                break;
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority to resolve the stack");
            }
            _ if runner.state().stack.is_empty() && saw_optional => break,
            _ => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority while resolving");
            }
        }
    }

    assert!(
        saw_optional,
        "Path to Exile must pause on its controller's optional land-search rider"
    );
    assert_eq!(
        runner.state().objects[&aura].zone,
        Zone::Graveyard,
        "CR 704.5m: the Aura must graveyard once the optional rider is declined and resolution settles"
    );
}

#[test]
fn issue_5999_aura_goes_to_graveyard_when_host_is_exiled_directly() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let host = scenario.add_creature(P1, "Enchanted Host", 3, 3).id();
    let aura = scenario
        .add_creature(P0, "Pacifism", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text("Enchant creature\nEnchanted creature can't attack.")
        .id();

    let mut runner = scenario.build();
    attach_to(runner.state_mut(), aura, host);

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), host, Zone::Exile, &mut events);
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut events);

    assert_eq!(runner.state().objects[&host].zone, Zone::Exile);
    assert_eq!(
        runner.state().objects[&aura].zone,
        Zone::Graveyard,
        "CR 704.5m: direct exile + SBA must graveyard the Aura"
    );
}

#[test]
fn issue_5999_aura_goes_to_graveyard_when_enchanted_creature_is_exiled() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let host = scenario.add_creature(P1, "Enchanted Host", 3, 3).id();
    let aura = scenario
        .add_creature(P0, "Pacifism", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text("Enchant creature\nEnchanted creature can't attack.")
        .id();
    let path = scenario
        .add_spell_to_hand_from_oracle(P0, "Path to Exile", true, PATH_TO_EXILE)
        .id();

    let mut runner = scenario.build();
    attach_to(runner.state_mut(), aura, host);
    assert_eq!(
        runner.state().objects[&aura]
            .attached_to
            .and_then(|t| t.as_object()),
        Some(host),
        "precondition: aura must be attached to host"
    );

    runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
        ManaType::White,
        ObjectId(0),
        false,
        vec![],
    ));

    let card_id = runner.state().objects[&path].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: path,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Path to Exile");

    drive_spell_until_settled(&mut runner, host);

    assert_eq!(
        runner.state().objects[&host].zone,
        Zone::Exile,
        "the enchanted creature must be exiled"
    );

    assert_eq!(
        runner.state().objects[&aura].zone,
        Zone::Graveyard,
        "CR 704.5m: the Aura must be put into its owner's graveyard when its host leaves the battlefield"
    );
    assert!(
        runner.state().objects[&aura].attached_to.is_none(),
        "the Aura must not retain a dangling attachment pointer"
    );
    assert!(
        !runner.state().objects[&aura]
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "the Aura must not remain on the battlefield"
    );
}
