//! Runed Halo gives a player protection from the chosen card name.
//!
//! CR 702.16a + CR 702.16b + CR 702.16c + CR 702.16e + CR 201.2 + CR 614.1c.
//!
//! Oracle text:
//! "As this enchantment enters, choose a card name.
//! You have protection from the chosen card name. (You can't be targeted, dealt damage, or enchanted by anything with that name.)"
//!
//! The as-enters replacement prompts for a card-name choice (`ChoiceType::CardName`)
//! before the permanent enters the battlefield. The static ability lowers to
//! `StaticMode::PlayerProtection(ProtectionTarget::ChosenCardName)`, which protects
//! the controller from any source whose name matches the chosen card name:
//! - Damage dealt by matching sources is prevented (CR 702.16e).
//! - The player cannot be targeted by spells or abilities from matching sources (CR 702.16b).
//! - The player cannot be enchanted by Auras with the matching name (CR 702.16c).

use engine::game::combat::AttackTarget;
use engine::game::effects::attach::attach_to_player;
use engine::game::effects::deal_damage;
use engine::game::game_object::AttachTarget;
use engine::game::sba::check_state_based_actions;
use engine::game::scenario::{GameScenario, P0};
use engine::game::targeting::find_legal_targets;
use engine::game::zones::create_object;
use engine::types::ability::{
    ChoiceType, Effect, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P1: PlayerId = PlayerId(1);
const RUNED_HALO_ORACLE: &str = "As this enchantment enters, choose a card name.\nYou have protection from the chosen card name. (You can't be targeted, dealt damage, or enchanted by anything with that name.)";

fn damage_ability(source_id: ObjectId, controller: PlayerId, target: TargetRef) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 3 },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        },
        vec![target],
        source_id,
        controller,
    )
}

#[test]
fn runed_halo_casts_pauses_for_name_choice_and_persists() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let halo = scenario
        .add_spell_to_hand_from_oracle(P0, "Runed Halo", false, RUNED_HALO_ORACLE)
        .as_enchantment()
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White, ManaCostShard::White],
            generic: 0,
        })
        .id();
    scenario.add_land_from_oracle(P0, "Plains 1", "{T}: Add {W}.");
    scenario.add_land_from_oracle(P0, "Plains 2", "{T}: Add {W}.");
    scenario.with_library_top(P0, &["Draw Step Filler"]);

    let mut runner = scenario.build();

    let card_id = runner.state().objects[&halo].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: halo,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Runed Halo must succeed");

    runner.advance_until_stack_empty();

    // CR 614.1c: As-enters choice pauses before the permanent is on the battlefield.
    assert!(
        !runner.state().battlefield.contains(&halo),
        "Runed Halo must NOT be on the battlefield while as-enters choice is pending"
    );

    let WaitingFor::NamedChoice { choice_type, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "Runed Halo as-enters replacement must produce a card-name choice, got {}",
            runner.waiting_for_kind()
        );
    };
    assert!(
        matches!(choice_type, ChoiceType::CardName),
        "expected ChoiceType::CardName, got {choice_type:?}"
    );

    runner.state_mut().all_card_names = std::sync::Arc::from(["Grizzly Bears".to_string()]);

    runner
        .act(GameAction::ChooseOption {
            choice: "Grizzly Bears".to_string(),
        })
        .expect("choosing card name must succeed");

    // Now Runed Halo is on the battlefield with the chosen card name recorded.
    assert!(
        runner.state().battlefield.contains(&halo),
        "Runed Halo must be on the battlefield after choice is resolved"
    );
    assert_eq!(
        runner.state().objects[&halo].chosen_card_name(),
        Some("Grizzly Bears"),
        "chosen card name must persist on Runed Halo"
    );
}

#[test]
fn runed_halo_prevents_combat_damage_from_named_card() {
    let mut scenario = GameScenario::new();

    // Runed Halo on P0's battlefield naming "Grizzly Bears".
    let halo = scenario
        .add_enchantment_from_oracle(P0, "Runed Halo", RUNED_HALO_ORACLE)
        .id();

    // P1 controls an attacker named "Grizzly Bears" and an attacker named "Hill Giant".
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let giant = scenario.add_creature(P1, "Hill Giant", 3, 3).id();
    scenario.with_library_top(P0, &["Filler"]);
    scenario.with_library_top(P1, &["Filler"]);
    scenario.at_phase(Phase::DeclareAttackers);

    let mut runner = scenario.build();

    runner
        .state_mut()
        .objects
        .get_mut(&halo)
        .unwrap()
        .chosen_attributes
        .push(engine::types::ability::ChosenAttribute::CardName(
            "Grizzly Bears".to_string(),
        ));

    runner.state_mut().active_player = P1;
    runner.state_mut().waiting_for = WaitingFor::DeclareAttackers {
        player: P1,
        valid_attacker_ids: vec![bear, giant],
        valid_attack_targets: vec![AttackTarget::Player(P0)],
        valid_attack_targets_by_attacker: None,
        attacker_constraints: Default::default(),
    };

    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![
                (bear, AttackTarget::Player(P0)),
                (giant, AttackTarget::Player(P0)),
            ],
            bands: vec![],
        })
        .expect("declare attackers must succeed");

    // Advance through declare blockers and combat damage step.
    for _ in 0..100 {
        if matches!(
            runner.state().phase,
            Phase::EndCombat | Phase::PostCombatMain
        ) {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                let _ = runner.act(GameAction::PassPriority);
            }
            WaitingFor::DeclareBlockers { .. } => {
                let _ = runner.act(GameAction::DeclareBlockers {
                    assignments: vec![],
                });
            }
            _ => break,
        }
    }

    // P0 started at 20 life.
    // Hill Giant (3 power) dealt 3 damage.
    // Grizzly Bears (2 power) had its damage prevented (CR 702.16e).
    // Final life must be 17.
    assert_eq!(
        runner.state().players[0].life,
        17,
        "P0 must only take damage from Hill Giant (3 damage); Grizzly Bears (2 damage) must be prevented"
    );
}

#[test]
fn runed_halo_prevents_noncombat_damage_from_named_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let halo = scenario
        .add_spell_to_hand_from_oracle(P0, "Runed Halo", false, RUNED_HALO_ORACLE)
        .as_enchantment()
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White, ManaCostShard::White],
            generic: 0,
        })
        .id();
    scenario.add_land_from_oracle(P0, "Plains 1", "{T}: Add {W}.");
    scenario.add_land_from_oracle(P0, "Plains 2", "{T}: Add {W}.");
    scenario.with_library_top(P0, &["Draw Step Filler"]);

    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let giant = scenario.add_creature(P1, "Hill Giant", 3, 3).id();

    let mut runner = scenario.build();

    let card_id = runner.state().objects[&halo].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: halo,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Runed Halo must succeed");
    runner.advance_until_stack_empty();
    runner.state_mut().all_card_names = std::sync::Arc::from(["Grizzly Bears".to_string()]);
    runner
        .act(GameAction::ChooseOption {
            choice: "Grizzly Bears".to_string(),
        })
        .expect("choosing card name must succeed");

    // 1. Damage from Grizzly Bears to P0 is prevented.
    let mut events = Vec::new();
    let ability_bear = damage_ability(bear, P1, TargetRef::Player(P0));
    deal_damage::resolve(runner.state_mut(), &ability_bear, &mut events)
        .expect("resolving damage from Grizzly Bears");

    assert!(
        events
            .iter()
            .any(|e| matches!(e, GameEvent::DamagePrevented { .. })),
        "damage from Grizzly Bears must be prevented, got {events:?}"
    );
    assert_eq!(
        runner.state().players[0].life,
        20,
        "P0 life must remain 20 after prevented damage"
    );

    // 2. Damage from Hill Giant to P0 is NOT prevented.
    let mut events = Vec::new();
    let ability_giant = damage_ability(giant, P1, TargetRef::Player(P0));
    deal_damage::resolve(runner.state_mut(), &ability_giant, &mut events)
        .expect("resolving damage from Hill Giant");

    assert!(
        !events
            .iter()
            .any(|e| matches!(e, GameEvent::DamagePrevented { .. })),
        "damage from Hill Giant must NOT be prevented, got {events:?}"
    );
    assert_eq!(
        runner.state().players[0].life,
        17,
        "P0 life must decrease by 3 from Hill Giant damage"
    );
}

#[test]
fn runed_halo_prevents_targeting_from_named_card() {
    let mut scenario = GameScenario::new();
    let halo = scenario
        .add_enchantment_from_oracle(P0, "Runed Halo", RUNED_HALO_ORACLE)
        .id();

    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let giant = scenario.add_creature(P1, "Hill Giant", 3, 3).id();

    let mut runner = scenario.build();

    runner
        .state_mut()
        .objects
        .get_mut(&halo)
        .unwrap()
        .chosen_attributes
        .push(engine::types::ability::ChosenAttribute::CardName(
            "Grizzly Bears".to_string(),
        ));

    // CR 702.16b: P0 cannot be targeted by spells/abilities from Grizzly Bears.
    let bear_targets = find_legal_targets(runner.state(), &TargetFilter::Player, P1, bear);
    assert!(
        !bear_targets.contains(&TargetRef::Player(P0)),
        "P0 must NOT be a legal target for Grizzly Bears source, got {bear_targets:?}"
    );
    assert!(
        bear_targets.contains(&TargetRef::Player(P1)),
        "P1 must remain a legal target for Grizzly Bears source"
    );

    // P0 CAN be targeted by spells/abilities from Hill Giant.
    let giant_targets = find_legal_targets(runner.state(), &TargetFilter::Player, P1, giant);
    assert!(
        giant_targets.contains(&TargetRef::Player(P0)),
        "P0 must be a legal target for Hill Giant source, got {giant_targets:?}"
    );
}

#[test]
fn runed_halo_prevents_enchanting_and_sba_detaches_aura() {
    let mut scenario = GameScenario::new();
    let halo = scenario
        .add_enchantment_from_oracle(P0, "Runed Halo", RUNED_HALO_ORACLE)
        .id();

    let mut runner = scenario.build();

    runner
        .state_mut()
        .objects
        .get_mut(&halo)
        .unwrap()
        .chosen_attributes
        .push(engine::types::ability::ChosenAttribute::CardName(
            "Curse of the Werebear".to_string(),
        ));

    // 1. Check attach_to_player directly.
    let curse_named = create_object(
        runner.state_mut(),
        CardId(101),
        P1,
        "Curse of the Werebear".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = runner.state_mut().objects.get_mut(&curse_named).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.card_types.subtypes.push("Aura".to_string());
    }

    let curse_other = create_object(
        runner.state_mut(),
        CardId(102),
        P1,
        "Curse of Oblivion".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = runner.state_mut().objects.get_mut(&curse_other).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.card_types.subtypes.push("Aura".to_string());
    }

    // CR 702.16c: P0 cannot be enchanted by Aura with the chosen name.
    attach_to_player(runner.state_mut(), curse_named, P0);
    assert_eq!(
        runner.state().objects[&curse_named].attached_to,
        None,
        "attach_to_player must fail for Aura with chosen card name"
    );
    attach_to_player(runner.state_mut(), curse_other, P0);
    assert_eq!(
        runner.state().objects[&curse_other].attached_to,
        Some(AttachTarget::Player(P0)),
        "attach_to_player must succeed for Aura with non-matching name"
    );

    // 2. CR 704.5m: If the Aura was already attached, SBA detaches it to graveyard.
    {
        let obj = runner.state_mut().objects.get_mut(&curse_named).unwrap();
        obj.attached_to = Some(AttachTarget::Player(P0));
    }
    let mut events = Vec::new();
    check_state_based_actions(runner.state_mut(), &mut events);

    assert!(
        !runner.state().battlefield.contains(&curse_named),
        "illegally attached Aura must be detached from battlefield by SBA (CR 704.5m)"
    );
    assert!(
        runner.state().players[1].graveyard.contains(&curse_named),
        "detached Aura must be put into owner's graveyard"
    );
}
