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
use engine::game::targeting::{find_legal_targets, find_legal_targets_for_ability};
use engine::game::triggers::trigger_source_context_for_latch;
use engine::game::zones::create_object;
use engine::types::ability::{
    ChoiceType, Effect, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, StackEntry, StackEntryKind, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::{Keyword, ProtectionTarget};
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

    // 1. Damage from Grizzly Bears to P0 is prevented (direct resolution).
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

    // 1b. CR 702.16e: Ordinary Effect::DealDamage from Grizzly Bears resolved through the stack.
    let ability_bear_stack = damage_ability(bear, P1, TargetRef::Player(P0));
    runner.state_mut().stack.push_back(StackEntry {
        id: ObjectId(999),
        kind: StackEntryKind::ActivatedAbility {
            source_id: bear,
            ability: Box::new(ability_bear_stack),
        },
        controller: P1,
        source_id: bear,
    });
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().players[0].life,
        20,
        "P0 life must remain 20 after stack-resolved ordinary DealDamage from Grizzly Bears is prevented"
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

const PYROHEMIA_ORACLE: &str = "At the beginning of the end step, if no creatures are on the battlefield, sacrifice this enchantment.\n{R}: This enchantment deals 1 damage to each creature and each player.";

/// P0 controls Runed Halo naming `chosen` and activates its own Pyrohemia
/// (a non-targeting `DamageAll` ability) with a tapped Mountain. Returns
/// `(runner, mountain, p1_creature)` after the ability has resolved.
fn activate_pyrohemia_with_halo_naming(
    chosen: &str,
) -> (engine::game::scenario::GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let halo = scenario
        .add_enchantment_from_oracle(P0, "Runed Halo", RUNED_HALO_ORACLE)
        .id();
    let pyro = scenario
        .add_enchantment_from_oracle(P0, "Pyrohemia", PYROHEMIA_ORACLE)
        .id();
    let mountain = scenario
        .add_land_from_oracle(P0, "Mountain", "{T}: Add {R}.")
        .id();
    let bear = scenario.add_creature(P1, "Bystander", 2, 2).id();
    let mut runner = scenario.build();

    runner
        .state_mut()
        .objects
        .get_mut(&halo)
        .unwrap()
        .chosen_attributes
        .push(engine::types::ability::ChosenAttribute::CardName(
            chosen.to_string(),
        ));

    let ability_index = runner.state().objects[&pyro]
        .abilities
        .iter()
        .position(|a| matches!(a.effect.as_ref(), Effect::DamageAll { .. }))
        .expect("Pyrohemia's {R} DamageAll ability");
    runner
        .act(GameAction::ActivateAbility {
            source_id: pyro,
            ability_index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activating Pyrohemia must succeed");
    assert!(
        runner.state().objects[&mountain].tapped,
        "the {{R}} activation cost must be paid by tapping the Mountain"
    );
    assert!(
        !runner.state().stack.is_empty(),
        "Pyrohemia's ability must be on the stack before resolution"
    );
    runner.advance_until_stack_empty();
    (runner, mountain, bear)
}

#[test]
fn runed_halo_prevents_nontargeted_damage_all_from_named_source() {
    let (runner, _, bear) = activate_pyrohemia_with_halo_naming("Pyrohemia");

    // Reach guard: the ability resolved (it damaged the other player and a creature).
    assert_eq!(
        runner.state().players[1].life,
        19,
        "Pyrohemia must have resolved, dealing 1 to the unprotected player"
    );
    assert_eq!(runner.state().objects[&bear].damage_marked, 1);
    // CR 702.16e: damage from the chosen-name source to the Halo controller is prevented.
    assert_eq!(
        runner.state().players[0].life,
        20,
        "P0 has protection from Pyrohemia and must take no damage from its DamageAll"
    );
}

#[test]
fn runed_halo_does_not_prevent_nontargeted_damage_all_from_other_name() {
    let (runner, _, _) = activate_pyrohemia_with_halo_naming("Grizzly Bears");

    assert_eq!(runner.state().players[1].life, 19);
    assert_eq!(
        runner.state().players[0].life,
        19,
        "a non-matching chosen name must not protect P0 from Pyrohemia"
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

/// CR 608.2h + CR 702.16e: Protection from the chosen card name prevents damage
/// from departed sources (such as a Clone in the graveyard or a ceased token)
/// by evaluating against the captured trigger source's LKI name.
#[test]
fn runed_halo_prevents_damage_from_departed_clone_and_token() {
    let mut scenario = GameScenario::new();
    let halo = scenario
        .add_enchantment_from_oracle(P0, "Runed Halo", RUNED_HALO_ORACLE)
        .id();

    // Creature on battlefield with protection from "Volatile Rig".
    let protected_creature = scenario.add_creature(P0, "Guardian", 3, 3).id();

    // Clone initially copying Volatile Rig on the battlefield.
    let clone_source = scenario.add_creature(P1, "Volatile Rig", 4, 4).id();

    let mut runner = scenario.build();

    // Set Halo's chosen name to "Volatile Rig".
    runner
        .state_mut()
        .objects
        .get_mut(&halo)
        .unwrap()
        .chosen_attributes
        .push(engine::types::ability::ChosenAttribute::CardName(
            "Volatile Rig".to_string(),
        ));

    // Grant the guardian protection from chosen card name "Volatile Rig".
    {
        let obj = runner
            .state_mut()
            .objects
            .get_mut(&protected_creature)
            .unwrap();
        obj.keywords
            .push(Keyword::Protection(ProtectionTarget::ChosenCardName));
        obj.chosen_attributes
            .push(engine::types::ability::ChosenAttribute::CardName(
                "Volatile Rig".to_string(),
            ));
    }

    // 1. Capture the trigger source context while the Clone is on the battlefield as Volatile Rig.
    let clone_trigger_ctx =
        trigger_source_context_for_latch(runner.state(), &runner.state().objects[&clone_source]);

    // Move Clone to the graveyard and rename it back to "Clone" (as copy effects end).
    {
        let obj = runner.state_mut().objects.get_mut(&clone_source).unwrap();
        obj.zone = Zone::Graveyard;
        obj.name = "Clone".to_string();
    }
    runner
        .state_mut()
        .battlefield
        .retain(|&id| id != clone_source);
    runner.state_mut().players[1]
        .graveyard
        .push_back(clone_source);

    // Create a dies damage ability from the Clone with the captured trigger source.
    let mut ability = ResolvedAbility::new(
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 4 },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        },
        vec![TargetRef::Player(P0)],
        clone_source,
        P1,
    );
    ability.trigger_source = Some(clone_trigger_ctx.clone());

    let mut events = Vec::new();
    deal_damage::resolve(runner.state_mut(), &ability, &mut events)
        .expect("resolving Clone damage to player");

    assert!(
        events
            .iter()
            .any(|e| matches!(e, GameEvent::DamagePrevented { .. })),
        "damage from Clone copying Volatile Rig must be prevented to P0 by Runed Halo"
    );
    assert_eq!(runner.state().players[0].life, 20, "P0 life must remain 20");

    // Also assert damage to protected creature is prevented.
    ability.targets = vec![TargetRef::Object(protected_creature)];
    let mut creature_events = Vec::new();
    deal_damage::resolve(runner.state_mut(), &ability, &mut creature_events)
        .expect("resolving Clone damage to protected creature");

    assert!(
        creature_events
            .iter()
            .any(|e| matches!(e, GameEvent::DamagePrevented { .. })),
        "damage from Clone copying Volatile Rig must be prevented to protected creature"
    );
    assert_eq!(
        runner.state().objects[&protected_creature].damage_marked,
        0,
        "protected creature must take no damage"
    );

    // 2. Token case: token dies and ceases to exist.
    let token_id = create_object(
        runner.state_mut(),
        CardId(999),
        P1,
        "Goblin".to_string(),
        Zone::Battlefield,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&token_id)
        .unwrap()
        .is_token = true;
    let token_trigger_ctx =
        trigger_source_context_for_latch(runner.state(), &runner.state().objects[&token_id]);

    // Token ceases to exist (removed from state.objects entirely).
    runner.state_mut().objects.remove(&token_id);
    runner.state_mut().battlefield.retain(|&id| id != token_id);

    // Set Halo's chosen name to "Goblin".
    runner
        .state_mut()
        .objects
        .get_mut(&halo)
        .unwrap()
        .chosen_attributes
        .clear();
    runner
        .state_mut()
        .objects
        .get_mut(&halo)
        .unwrap()
        .chosen_attributes
        .push(engine::types::ability::ChosenAttribute::CardName(
            "Goblin".to_string(),
        ));

    let mut token_ability = ResolvedAbility::new(
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 2 },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        },
        vec![TargetRef::Player(P0)],
        token_id,
        P1,
    );
    token_ability.trigger_source = Some(token_trigger_ctx);

    let mut token_events = Vec::new();
    deal_damage::resolve(runner.state_mut(), &token_ability, &mut token_events)
        .expect("resolving token damage");

    assert!(
        token_events
            .iter()
            .any(|e| matches!(e, GameEvent::DamagePrevented { .. })),
        "damage from ceased token Goblin must be prevented to P0 by Runed Halo"
    );
    assert_eq!(
        runner.state().players[0].life,
        20,
        "P0 life must remain 20 after token damage is prevented"
    );
}

/// CR 608.2h + CR 702.16b: When a targeted dies trigger comes from a token that has
/// ceased to exist, `find_legal_targets_for_ability` evaluates protection against
/// the captured trigger source projection, preventing the protected player from
/// being selected as a legal target.
#[test]
fn runed_halo_prevents_targeting_from_departed_token_source() {
    let mut scenario = GameScenario::new();
    let halo = scenario
        .add_enchantment_from_oracle(P0, "Runed Halo", RUNED_HALO_ORACLE)
        .id();

    let mut runner = scenario.build();

    // Set Halo's chosen name to "Goblin".
    runner
        .state_mut()
        .objects
        .get_mut(&halo)
        .unwrap()
        .chosen_attributes
        .push(engine::types::ability::ChosenAttribute::CardName(
            "Goblin".to_string(),
        ));

    // Create a token Goblin on battlefield.
    let token_id = create_object(
        runner.state_mut(),
        CardId(888),
        P1,
        "Goblin".to_string(),
        Zone::Battlefield,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&token_id)
        .unwrap()
        .is_token = true;
    let token_trigger_ctx =
        trigger_source_context_for_latch(runner.state(), &runner.state().objects[&token_id]);

    // Token ceases to exist (removed from state.objects entirely).
    runner.state_mut().objects.remove(&token_id);
    runner.state_mut().battlefield.retain(|&id| id != token_id);

    // Create a triggered ability targeting Any (players + permanents).
    let mut ability = ResolvedAbility::new(
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        },
        vec![],
        token_id,
        P1,
    );
    ability.trigger_source = Some(token_trigger_ctx);

    let legal_targets =
        find_legal_targets_for_ability(runner.state(), &TargetFilter::Any, &ability);

    assert!(
        !legal_targets.contains(&TargetRef::Player(P0)),
        "P0 must NOT be a legal target for ceased token Goblin due to Runed Halo protection, got {legal_targets:?}"
    );
    assert!(
        legal_targets.contains(&TargetRef::Player(P1)),
        "P1 must be a legal target for ceased token Goblin, got {legal_targets:?}"
    );
}
