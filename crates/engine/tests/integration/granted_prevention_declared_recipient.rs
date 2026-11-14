//! CR 615.1 + CR 115.1 + CR 602.2b: a one-shot prevention whose "would deal damage"
//! clause names a DECLARED recipient ("to target creature") protects only the
//! recipient chosen as the ability is activated. Samite Blessing grants that
//! ability to the enchanted creature; Charm Peddler prints it. Both used to lose
//! the "target creature" restriction (`PreventDamage.target = Any`), which minted
//! no target slot and shielded the chosen source's damage to EVERY recipient.
//!
//! Every runtime assertion drives the printed ability through the action pipeline:
//! activation, target announcement, source choice, then real damage activations.

use engine::game::effects::attach::attach_to;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityKind, Effect, StaticDefinition, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

// Verbatim Oracle text.
const SAMITE_BLESSING: &str = "Enchant creature\nEnchanted creature has \"{T}: The next time a source of your choice would deal damage to target creature this turn, prevent that damage.\"";
const CHARM_PEDDLER: &str = "{W}, {T}, Discard a card: The next time a source of your choice would deal damage to target creature this turn, prevent that damage.";
const HELLKITE: &str = "Flying (This creature can't be blocked except by creatures with flying or reach.)\n{1}{R}: This creature deals 1 damage to any target.";

fn add_mana(runner: &mut GameRunner, player: PlayerId, color: ManaType, count: usize) {
    for _ in 0..count {
        runner
            .state_mut()
            .add_mana_to_pool(player, ManaUnit::new(color, ObjectId(0), false, vec![]));
    }
}

fn hellkite_ping(runner: &mut GameRunner, source: ObjectId, target: ObjectId) {
    let index = runner.state().objects[&source]
        .abilities
        .iter()
        .position(|a| {
            a.kind == AbilityKind::Activated
                && matches!(a.effect.as_ref(), Effect::DealDamage { .. })
        })
        .expect("Hellkite exposes its printed damage activation");
    if !matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1) {
        runner.act(GameAction::PassPriority).expect("pass to P1");
    }
    add_mana(runner, P1, ManaType::Red, 2);
    runner
        .activate(source, index)
        .target_object(target)
        .resolve();
}

/// Activate the (granted or printed) prevention ability on `host`, announce
/// `recipient`, resolve it, and choose `source` as the damage source.
fn shield_recipient_from_source(
    runner: &mut GameRunner,
    host: ObjectId,
    recipient: ObjectId,
    legal: &[ObjectId],
    illegal: &[ObjectId],
    source: ObjectId,
) {
    let index = runner.state().objects[&host]
        .abilities
        .iter()
        .position(|a| {
            a.kind == AbilityKind::Activated
                && matches!(a.effect.as_ref(), Effect::PreventDamage { .. })
        })
        .expect("host exposes the prevention activation (reach guard: it was granted/parsed)");
    runner
        .act(GameAction::ActivateAbility {
            source_id: host,
            ability_index: index,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("activate the prevention ability");
    // CR 115.1 + CR 602.2b: "target creature" is declared at activation.
    let WaitingFor::TargetSelection { target_slots, .. } = &runner.state().waiting_for else {
        panic!(
            "the printed 'target creature' must surface a target slot, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(target_slots.len(), 1);
    for id in legal {
        assert!(target_slots[0]
            .legal_targets
            .contains(&TargetRef::Object(*id)));
    }
    for id in illegal {
        assert!(!target_slots[0]
            .legal_targets
            .contains(&TargetRef::Object(*id)));
    }
    assert!(!target_slots[0]
        .legal_targets
        .iter()
        .any(|t| matches!(t, TargetRef::Player(_))));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(recipient)),
        })
        .expect("choose the recipient creature");
    // Charm Peddler's printed "Discard a card" cost: pay it with the only choice.
    if let WaitingFor::PayCost { choices, .. } = runner.state().waiting_for.clone() {
        runner
            .act(GameAction::SelectCards {
                cards: vec![choices[0]],
            })
            .expect("pay the discard cost");
    }
    for _ in 0..8 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DamageSourceChoice { .. }
        ) {
            break;
        }
        runner.act(GameAction::PassPriority).expect("resolve");
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DamageSourceChoice { .. }
        ),
        "reach guard: resolution must prompt for the damage source, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseDamageSource { source })
        .expect("choose the source");
}

/// Parser class: both cards keep the declared creature restriction.
#[test]
fn declared_creature_recipient_survives_in_printed_and_granted_prevention() {
    let peddler = parse_oracle_text(
        CHARM_PEDDLER,
        "Charm Peddler",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let Effect::PreventDamage { target, .. } = peddler.abilities[0].effect.as_ref() else {
        panic!("Charm Peddler must parse to PreventDamage");
    };
    assert!(matches!(target, TargetFilter::Typed(_)), "got {target:?}");

    let samite = parse_oracle_text(
        SAMITE_BLESSING,
        "Samite Blessing",
        &[],
        &["Enchantment".to_string()],
        &["Aura".to_string()],
    );
    let granted: Vec<&StaticDefinition> = samite.statics.iter().collect();
    assert_eq!(granted.len(), 1);
    let json = serde_json::to_value(&granted[0].modifications).unwrap();
    let target = &json[0]["definition"]["effect"]["target"];
    assert_eq!(target["type"], "Typed", "got {target}");
}

/// Samite Blessing end to end: damage from the chosen source to the announced
/// creature is prevented, damage from the same source to any other creature is not.
#[test]
fn samite_blessing_protects_only_the_announced_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Blessed", 1, 9).id();
    let aura = scenario
        .add_enchantment_from_oracle(P0, "Samite Blessing", SAMITE_BLESSING)
        .with_subtypes(vec!["Aura"])
        .id();
    let protected = scenario.add_creature(P0, "Protected", 0, 9).id();
    let bystander = scenario.add_creature(P0, "Bystander", 0, 9).id();
    let land = scenario.add_basic_land(P0, ManaColor::White);
    let source = scenario
        .add_creature_from_oracle(P1, "Shivan Hellkite", 5, 5, HELLKITE)
        .id();
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), aura, host);

    shield_recipient_from_source(
        &mut runner,
        host,
        protected,
        &[host, protected, bystander, source],
        &[land],
        source,
    );

    // Positive reach guard: the source's damage is real and lands on an unannounced creature.
    hellkite_ping(&mut runner, source, bystander);
    assert_eq!(
        runner.state().objects[&bystander].damage_marked,
        1,
        "the shield must NOT cover creatures other than the announced recipient"
    );
    // CR 615.1: the announced recipient is shielded from that same chosen source.
    hellkite_ping(&mut runner, source, protected);
    assert_eq!(
        runner.state().objects[&protected].damage_marked,
        0,
        "damage from the chosen source to the announced creature is prevented"
    );
}

/// Charm Peddler (printed form): the same recipient restriction, and an opponent's
/// creature is a legal announcement because the text says "target creature".
#[test]
fn charm_peddler_announces_any_creature_and_protects_only_it() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let peddler = scenario
        .add_creature_from_oracle(P0, "Charm Peddler", 1, 1, CHARM_PEDDLER)
        .id();
    let protected = scenario.add_creature(P0, "Protected", 0, 9).id();
    let bystander = scenario.add_creature(P0, "Bystander", 0, 9).id();
    let land = scenario.add_basic_land(P0, ManaColor::White);
    let source = scenario
        .add_creature_from_oracle(P1, "Shivan Hellkite", 5, 5, HELLKITE)
        .id();
    scenario.add_spell_to_hand(P0, "Discard fodder", false);
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, ManaType::White, 1);

    shield_recipient_from_source(
        &mut runner,
        peddler,
        protected,
        &[peddler, protected, bystander, source],
        &[land],
        source,
    );
    hellkite_ping(&mut runner, source, bystander);
    assert_eq!(runner.state().objects[&bystander].damage_marked, 1);
    hellkite_ping(&mut runner, source, protected);
    assert_eq!(runner.state().objects[&protected].damage_marked, 0);
}
