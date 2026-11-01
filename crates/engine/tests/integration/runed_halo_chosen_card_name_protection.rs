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
use engine::game::effects::deal_damage;
use engine::game::engine::EngineError;
use engine::game::game_object::AttachTarget;
use engine::game::sba::check_state_based_actions;
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::targeting::find_legal_targets;
use engine::types::ability::{
    ChoiceType, Effect, PreventionAmount, QuantityExpr, ReplacementDefinition, ReplacementMode,
    ResolvedAbility, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastPaymentMode, CastingVariant, CastingVariantFace, GameRng, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaPool, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use engine::types::ReplacementEvent;

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

fn red_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Red],
        generic: 0,
    }
}

fn halo_on_battlefield(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_enchantment_from_oracle(P0, "Runed Halo", RUNED_HALO_ORACLE)
        .id()
}

fn name_halo_choice(runner: &mut engine::game::scenario::GameRunner, halo: ObjectId, chosen: &str) {
    let obj = runner.state_mut().objects.get_mut(&halo).unwrap();
    obj.chosen_attributes
        .retain(|attr| !matches!(attr, engine::types::ability::ChosenAttribute::CardName(_)));
    obj.chosen_attributes
        .push(engine::types::ability::ChosenAttribute::CardName(
            chosen.to_string(),
        ));
}

#[test]
fn runed_halo_prevents_curse_of_oblivion_targeting_and_sba_detaches_aura() {
    // 1. Halo on battlefield naming "Curse of Oblivion". P1 attempts to cast Curse of Oblivion targeting P0 in 3-player game.
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let halo = halo_on_battlefield(&mut scenario);
    let db = crate::support::shared_card_db().expect("card db");
    let curse = scenario.add_real_card(P1, "Curse of Oblivion", Zone::Hand, db);
    scenario.with_mana_pool(
        P1,
        vec![
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    let mut runner = scenario.build();
    name_halo_choice(&mut runner, halo, "Curse of Oblivion");
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    let card_id = runner.state().objects[&curse].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: curse,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("initiating Curse of Oblivion cast");

    let WaitingFor::TargetSelection { target_slots, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected TargetSelection, got {}",
            runner.waiting_for_kind()
        );
    };
    assert!(
        !target_slots[0].legal_targets.contains(&TargetRef::Player(P0)),
        "CR 702.16b: P0 must NOT be a legal target for Curse of Oblivion when protected by Runed Halo"
    );
    assert!(
        target_slots[0]
            .legal_targets
            .contains(&TargetRef::Player(P1)),
        "P1 must be a legal target"
    );
    assert!(
        target_slots[0]
            .legal_targets
            .contains(&TargetRef::Player(PlayerId(2))),
        "P2 must be a legal target"
    );
    let err = runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P0)),
        })
        .unwrap_err();
    assert!(matches!(err, EngineError::InvalidAction(_)));

    // Cancel cast: stack is empty and priority returns.
    runner.act(GameAction::CancelCast).expect("cancel cast");
    assert!(
        runner.state().stack.is_empty(),
        "stack is empty after cancel"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1),
        "priority returned to P1"
    );

    // 2. Control: Halo naming a different card lets Curse of Oblivion target and attach to P0.
    name_halo_choice(&mut runner, halo, "Shock");
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    runner.state_mut().players[1].mana_pool = ManaPool::from_units(vec![
        ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
        ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
    ]);

    runner.cast(curse).target_player(P0).resolve();
    assert!(
        runner.state().stack.is_empty(),
        "stack is empty after Curse of Oblivion resolves"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "priority returned"
    );
    assert_eq!(
        runner.state().objects[&curse].attached_to,
        Some(AttachTarget::Player(P0)),
        "Curse of Oblivion must attach to P0 when Halo names Shock"
    );

    // 3. CR 704.5m: When Halo names "Curse of Oblivion", SBA detaches the Aura to graveyard.
    name_halo_choice(&mut runner, halo, "Curse of Oblivion");
    let mut sba_events = Vec::new();
    check_state_based_actions(runner.state_mut(), &mut sba_events);

    assert!(
        !runner.state().battlefield.contains(&curse),
        "illegally attached Aura must be detached from battlefield by SBA (CR 704.5m)"
    );
    assert!(
        runner.state().players[1].graveyard.contains(&curse),
        "detached Aura must be put into owner's graveyard"
    );
    assert!(runner.state().stack.is_empty());
}

const VOLATILE_RIG_ORACLE: &str = "Trample\nThis creature attacks each combat if able.\nWhenever this creature is dealt damage, flip a coin. If you lose the flip, sacrifice this creature.\nWhen this creature dies, flip a coin. If you lose the flip, it deals 4 damage to each creature and each player.";
const CLONE_ORACLE: &str =
    "You may have this creature enter as a copy of any creature on the battlefield.";
const MURDER_ORACLE: &str = "Destroy target creature.";

fn run_departed_clone_rig_sequence(halo_named: &str) -> (i32, i32) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let halo = halo_on_battlefield(&mut scenario);
    let rig = scenario
        .add_creature_from_oracle(P1, "Volatile Rig", 4, 4, VOLATILE_RIG_ORACLE)
        .as_artifact_creature()
        .id();
    let clone = scenario
        .add_spell_to_hand_from_oracle(P1, "Clone", false, CLONE_ORACLE)
        .as_creature()
        .with_subtypes(vec!["Shapeshifter"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 3,
        })
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", false, MURDER_ORACLE)
        .as_instant()
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
            generic: 1,
        })
        .id();
    scenario.with_mana_pool(
        P1,
        vec![
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    let mut runner = scenario.build();
    // Seed the coin flip so the flip loses (deals 4 damage). Seed 1 produces false on first flip.
    runner.state_mut().rng = GameRng::seed_from_u64(1);
    name_halo_choice(&mut runner, halo, halo_named);

    // 1. P1 casts Clone, choosing to copy Volatile Rig.
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    runner
        .cast(clone)
        .replacement_choice(0)
        .copy_target(rig)
        .resolve();
    assert_eq!(
        runner.state().objects[&clone].name,
        "Volatile Rig",
        "Clone entered copying Volatile Rig"
    );

    // 2. P0 casts Murder targeting the Clone.
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    runner.cast(murder).target_object(clone).commit();
    runner
        .act(GameAction::PassPriority)
        .expect("P0 pass Murder");
    runner
        .act(GameAction::PassPriority)
        .expect("P1 pass Murder");
    assert_eq!(runner.state().objects[&clone].zone, Zone::Graveyard);
    // In the graveyard, Clone has reverted to its printed card name "Clone"
    assert_eq!(runner.state().objects[&clone].name, "Clone");
    // Volatile Rig's dies trigger is on the stack.
    assert_eq!(runner.state().stack.len(), 1, "dies trigger on stack");

    // 3. Resolve the dies trigger.
    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty(), "stack is empty");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "priority returned"
    );

    (
        runner.state().players[0].life,
        runner.state().players[1].life,
    )
}

#[test]
fn runed_halo_prevents_damage_from_departed_clone_rig() {
    // Halo naming "Volatile Rig": P0 life is unchanged (20), P1 takes 4 damage (16).
    assert_eq!(run_departed_clone_rig_sequence("Volatile Rig"), (20, 16));
}

#[test]
fn runed_halo_does_not_prevent_damage_from_departed_clone_rig_for_other_name() {
    // Control: Halo naming "Shock": P0 takes 4 damage (16), P1 takes 4 damage (16).
    assert_eq!(run_departed_clone_rig_sequence("Shock"), (16, 16));
}

#[test]
fn runed_halo_prevents_targeting_from_ceased_token() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let halo = halo_on_battlefield(&mut scenario);
    let db = crate::support::shared_card_db().expect("card db");
    let arsonist = scenario.add_real_card(P1, "Goblin Arsonist", Zone::Battlefield, db);
    let bears = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", false, MURDER_ORACLE)
        .as_instant()
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
            generic: 1,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    let mut runner = scenario.build();
    name_halo_choice(&mut runner, halo, "Goblin Arsonist");

    // Mark arsonist as a token.
    runner
        .state_mut()
        .objects
        .get_mut(&arsonist)
        .unwrap()
        .is_token = true;

    // Murder the token Goblin Arsonist through a real action.
    runner.cast(murder).target_object(arsonist).commit();
    runner.act(GameAction::PassPriority).expect("P0 pass");
    runner.act(GameAction::PassPriority).expect("P1 pass");

    // CR 111.7: The token died and ceased to exist in the graveyard through SBA.
    assert!(
        !runner.state().objects.contains_key(&arsonist),
        "token must cease to exist after dying"
    );

    // The trigger requires a target: WaitingFor::TriggerTargetSelection.
    let WaitingFor::TriggerTargetSelection { target_slots, .. } =
        runner.state().waiting_for.clone()
    else {
        panic!(
            "expected TriggerTargetSelection, got {}",
            runner.waiting_for_kind()
        );
    };
    // CR 702.16b: P0 cannot be targeted by the ceased token whose LKI was Goblin Arsonist.
    assert!(
        !target_slots[0]
            .legal_targets
            .contains(&TargetRef::Player(P0)),
        "P0 must NOT be a legal target for ceased token Goblin Arsonist"
    );
    assert!(
        target_slots[0]
            .legal_targets
            .contains(&TargetRef::Player(P1)),
        "P1 must be a legal target"
    );
    assert!(
        target_slots[0]
            .legal_targets
            .contains(&TargetRef::Object(bears)),
        "Bears must be a legal target"
    );

    // Target P1 and put trigger on the stack.
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .expect("targeting P1");

    // Pass priority to resolve the trigger.
    runner
        .act(GameAction::PassPriority)
        .expect("P0 pass trigger");
    runner
        .act(GameAction::PassPriority)
        .expect("P1 pass trigger");

    // CR 603.5: "you may have it deal 1 damage" prompts during resolution.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "expected OptionalEffectChoice during resolution, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept optional damage");

    assert!(
        runner.state().stack.is_empty(),
        "stack is empty, but was {:?}",
        runner.state().stack
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "expected Priority, got {:?}",
        runner.state().waiting_for
    );
    // P1 took 1 damage, P0 life remains 20.
    assert_eq!(runner.state().players[0].life, 20);
    assert_eq!(runner.state().players[1].life, 19);
}

fn run_departed_epicure_sequence(halo_named: &str) -> i32 {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let halo = scenario
        .add_enchantment_from_oracle(P1, "Runed Halo", RUNED_HALO_ORACLE)
        .id();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    const EPICURE_ORACLE: &str =
        "When this creature enters, it deals 1 damage to each opponent. Create a Blood token.";
    let epicure = scenario
        .add_spell_to_hand_from_oracle(P0, "Voldaren Epicure", false, EPICURE_ORACLE)
        .as_creature()
        .with_mana_cost(red_cost())
        .id();
    const MIRRORWEAVE_ORACLE: &str =
        "Each other creature becomes a copy of target nonlegendary creature until end of turn.";
    let mirrorweave = scenario
        .add_spell_to_hand_from_oracle(P0, "Mirrorweave", false, MIRRORWEAVE_ORACLE)
        .as_instant()
        .with_mana_cost(ManaCost::Cost {
            shards: vec![
                ManaCostShard::White,
                ManaCostShard::White,
                ManaCostShard::White,
                ManaCostShard::White,
            ],
            generic: 0,
        })
        .id();
    const VILLAGE_RITES_ORACLE: &str =
        "As an additional cost to cast this spell, sacrifice a creature.\nDraw two cards.";
    let rites = scenario
        .add_spell_to_hand_from_oracle(P0, "Village Rites", true, VILLAGE_RITES_ORACLE)
        .as_instant()
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .id();
    scenario.with_library_top(P0, &["Card 1", "Card 2"]);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    name_halo_choice(&mut runner, halo, halo_named);

    // 1. Cast Voldaren Epicure and let the creature spell resolve; its ETB trigger is put on the stack.
    runner.cast(epicure).commit();
    runner.act(GameAction::PassPriority).expect("P0 pass");
    runner.act(GameAction::PassPriority).expect("P1 pass");
    assert_eq!(
        runner.state().stack.len(),
        1,
        "Epicure ETB trigger is on stack"
    );

    // 2. In response, cast Mirrorweave targeting Grizzly Bears.
    runner.cast(mirrorweave).target_object(bears).commit();
    runner.resolve_top();
    assert_eq!(
        runner.state().objects[&epicure].name,
        "Grizzly Bears",
        "Epicure is now named Grizzly Bears"
    );

    // 3. Cast Village Rites sacrificing Epicure (currently named Grizzly Bears).
    runner.cast(rites).sacrifice_with(&[epicure]).commit();
    runner.resolve_top();
    assert_eq!(
        runner.state().objects[&epicure].zone,
        Zone::Graveyard,
        "Epicure sacrificed to graveyard"
    );

    // 4. Resolve Epicure's ETB trigger.
    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty(), "stack must be empty");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "priority must return to active player"
    );

    runner.state().players[1].life
}

#[test]
fn departed_source_uses_exit_snapshot_preventing_damage_when_exit_name_matches() {
    // When Halo names "Grizzly Bears", P1 takes 0 damage because Epicure exited as Grizzly Bears.
    assert_eq!(run_departed_epicure_sequence("Grizzly Bears"), 20);
}

#[test]
fn departed_source_control_does_not_prevent_damage_when_exit_name_differs() {
    // Control: When Halo names "Voldaren Epicure", P1 takes 1 damage because Epicure exited as Grizzly Bears.
    assert_eq!(run_departed_epicure_sequence("Voldaren Epicure"), 19);
}

#[test]
fn runed_halo_naming_trouble_protects_against_fused_toil_trouble_and_control_allows_toil() {
    let db = crate::support::shared_card_db().expect("card db");

    // 1. Fused Toil // Trouble cast with Halo naming "Trouble" in 3-player game.
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let halo = halo_on_battlefield(&mut scenario);
    let toil = scenario.add_real_card(P1, "Toil", Zone::Hand, db);
    // Fund P1 for fused cost: Toil {2}{B} + Trouble {2}{R} = 6 mana
    scenario.with_mana_pool(
        P1,
        vec![
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    scenario.with_library_top(P1, &["Filler 1", "Filler 2", "Filler 3", "Filler 4"]);
    scenario.with_library_top(P0, &["Filler 1", "Filler 2"]);

    let mut runner = scenario.build();
    name_halo_choice(&mut runner, halo, "Trouble");
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    // P1 casts Toil // Trouble fused:
    let card_id = runner.state().objects[&toil].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: toil,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast spell");
    // Offer CastingVariant::Fuse
    let WaitingFor::CastingVariantChoice { options, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected CastingVariantChoice, got {}",
            runner.waiting_for_kind()
        );
    };
    let fuse_idx = options
        .iter()
        .position(|o| o.variant == CastingVariant::Fuse)
        .expect("Fuse option");
    runner
        .act(GameAction::ChooseCastingVariant { index: fuse_idx })
        .expect("choosing Fuse");

    // TargetSelection for slot 0 (Toil):
    let WaitingFor::TargetSelection {
        target_slots,
        selection,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected TargetSelection, got {}",
            runner.waiting_for_kind()
        );
    };
    // CR 709.4a + CR 709.4d + CR 702.16b: Because the fused spell carries both names ("Toil" and "Trouble"),
    // P0 has protection from the fused spell, so P0 is NOT a legal target for ANY part of the fused spell!
    assert!(
        !target_slots[selection.current_slot]
            .legal_targets
            .contains(&TargetRef::Player(P0)),
        "CR 702.16b: P0 must NOT be a legal target for fused spell carrying Trouble's name"
    );
    // P1 targets P1 for Toil
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .expect("targeting P1 for Toil");

    // TargetSelection for slot 1 (Trouble):
    let WaitingFor::TargetSelection {
        target_slots: slots2,
        selection: sel2,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected TargetSelection for Trouble, got {}",
            runner.waiting_for_kind()
        );
    };
    assert!(
        !slots2[sel2.current_slot]
            .legal_targets
            .contains(&TargetRef::Player(P0)),
        "CR 702.16b: P0 must NOT be a legal target for Trouble half"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .expect("targeting P1 for Trouble");

    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    // P0 was completely protected and took 0 damage/loss; P0 life remains 20.
    assert_eq!(runner.state().players[0].life, 20);

    // 2. Control: P1 casts Toil alone (unfused) aimed at P0. Halo still names "Trouble".
    let mut scenario2 = GameScenario::new();
    scenario2.at_phase(Phase::PreCombatMain);
    let halo2 = halo_on_battlefield(&mut scenario2);
    let toil2 = scenario2.add_real_card(P1, "Toil", Zone::Hand, db);
    scenario2.with_mana_pool(
        P1,
        vec![
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    scenario2.with_library_top(P0, &["Card A", "Card B"]);
    let mut runner2 = scenario2.build();
    name_halo_choice(&mut runner2, halo2, "Trouble");
    runner2.state_mut().active_player = P1;
    runner2.state_mut().priority_player = P1;
    runner2.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    // Toil alone is cast with CastingVariant::Normal, Left face.
    runner2
        .cast(toil2)
        .casting_variant_face(CastingVariant::Normal, CastingVariantFace::Left)
        .target_player(P0)
        .resolve();

    assert!(runner2.state().stack.is_empty());
    assert!(matches!(
        runner2.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    // Toil resolves on P0: P0 draws 2 cards and loses 2 life! Life becomes 18.
    assert_eq!(runner2.state().players[0].life, 18);
}

#[test]
fn remaining_damage_chain_nodes_keep_latched_trigger_source_across_pause() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let halo = halo_on_battlefield(&mut scenario);

    // 1. Volatile Rig on battlefield under P1.
    let rig = scenario
        .add_creature_from_oracle(P1, "Volatile Rig", 4, 4, VOLATILE_RIG_ORACLE)
        .as_artifact_creature()
        .id();

    // 2. Clone in P1's hand.
    let clone = scenario
        .add_spell_to_hand_from_oracle(P1, "Clone", false, CLONE_ORACLE)
        .as_creature()
        .with_subtypes(vec!["Shapeshifter"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 3,
        })
        .id();

    // 3. Shielded creature on P1 with an optional prevention shield to force a ReplacementChoice.
    let prevention = ReplacementDefinition::new(ReplacementEvent::DamageDone)
        .valid_card(TargetFilter::SelfRef)
        .prevention_shield(PreventionAmount::Next(1))
        .mode(ReplacementMode::Optional { decline: None })
        .description("You may prevent the next 1 damage to this creature.".to_string());
    let _shielded = scenario
        .add_creature(P1, "Shielded", 0, 10)
        .with_replacement_definition(prevention)
        .id();

    // 4. Swords to Plowshares to exile the original Volatile Rig before Clone is destroyed.
    let db = crate::support::shared_card_db().expect("card db");
    let swords = scenario.add_real_card(P0, "Swords to Plowshares", Zone::Hand, db);

    // 5. Murder in P0's hand to destroy the Clone.
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", false, MURDER_ORACLE)
        .as_instant()
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
            generic: 1,
        })
        .id();

    scenario.with_mana_pool(
        P1,
        vec![
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    // Seed the coin flip so the flip loses (deals 4 damage). Seed 1 produces false on first flip.
    runner.state_mut().rng = GameRng::seed_from_u64(1);
    name_halo_choice(&mut runner, halo, "Volatile Rig");

    // 1. P1 casts Clone, choosing to copy Volatile Rig.
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    runner
        .cast(clone)
        .replacement_choice(0)
        .copy_target(rig)
        .resolve();
    assert_eq!(
        runner.state().objects[&clone].name,
        "Volatile Rig",
        "Clone entered copying Volatile Rig"
    );

    // 2. P0 casts Swords to Plowshares exiling the original Volatile Rig.
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    runner.cast(swords).target_object(rig).commit();
    runner
        .act(GameAction::PassPriority)
        .expect("P0 pass Swords");
    runner
        .act(GameAction::PassPriority)
        .expect("P1 pass Swords");
    assert_eq!(
        runner.state().objects[&rig].zone,
        Zone::Exile,
        "original Rig exiled so its own dies trigger never fires"
    );
    // Swords' second instruction: the Rig's controller (P1) gains life equal to its power (4).
    assert_eq!(
        runner.state().players[1].life,
        24,
        "P1 gains 4 life from Swords to Plowshares"
    );

    // 3. P0 casts Murder targeting the Clone.
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    runner.cast(murder).target_object(clone).commit();
    runner
        .act(GameAction::PassPriority)
        .expect("P0 pass Murder");
    runner
        .act(GameAction::PassPriority)
        .expect("P1 pass Murder");
    assert_eq!(runner.state().objects[&clone].zone, Zone::Graveyard);
    // In the graveyard, Clone has reverted to its printed card name "Clone"
    assert_eq!(runner.state().objects[&clone].name, "Clone");
    // Volatile Rig's dies trigger is on the stack.
    assert_eq!(runner.state().stack.len(), 1, "dies trigger on stack");

    // The dies trigger resolves until the shielded creature's damage pauses on ReplacementChoice.
    let mut paused = false;
    for _ in 0..10 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ) {
            paused = true;
            break;
        }
        runner.resolve_top();
    }
    assert!(
        paused,
        "must pause on ReplacementChoice for shielded creature"
    );

    // Answer the replacement choice (index 0 is the optional prevention shield).
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("answering replacement choice");

    // The remaining damage continuation resumes, reaching P0 and P1.
    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty(), "stack is empty at end");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "priority returned"
    );

    // With Halo naming Volatile Rig:
    // - P0 (protected) stays at 20 (latched trigger source LKI retains name "Volatile Rig").
    // - P1 (unprotected) takes 4 damage (24 -> 20), proving the continuation ran.
    assert_eq!(runner.state().players[0].life, 20, "P0 life must remain 20");
    assert_eq!(
        runner.state().players[1].life,
        20,
        "P1 life must be 20 proving continuation ran"
    );
}

const BOLT_ORACLE: &str = "Lightning Bolt deals 3 damage to any target.";
const ORCISH_ARTILLERY_ORACLE: &str =
    "{T}: This creature deals 2 damage to any target and 3 damage to you.";

/// CR 702.16b: a printed Lightning Bolt cast through the runner is aimed at P0.
/// Returns `(P0 life, P1 life)` once the stack is empty.
fn cast_bolt_aimed_at_p0(chosen: &str) -> (i32, i32) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let halo = halo_on_battlefield(&mut scenario);
    scenario.with_mana_pool(
        P0,
        vec![engine::types::mana::ManaUnit::new(
            engine::types::mana::ManaType::Red,
            ObjectId(0),
            false,
            vec![],
        )],
    );
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P0, "Lightning Bolt", false, BOLT_ORACLE)
        .as_instant()
        .with_mana_cost(red_cost())
        .id();
    let mut runner = scenario.build();
    name_halo_choice(&mut runner, halo, chosen);

    assert_eq!(runner.state().objects[&bolt].zone, Zone::Hand);
    runner.cast(bolt).target_player(P0).resolve();
    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        runner.state().objects[&bolt].zone,
        Zone::Graveyard,
        "reach guard: the Bolt was cast and resolved"
    );
    assert_eq!(runner.state().players[0].mana_pool.total(), 0, "{{R}} paid");
    (
        runner.state().players[0].life,
        runner.state().players[1].life,
    )
}

#[test]
fn runed_halo_blocks_printed_bolt_from_targeting_its_controller() {
    // P0 is not a legal target, so the only legal target (P1) takes the Bolt.
    assert_eq!(cast_bolt_aimed_at_p0("Lightning Bolt"), (20, 17));
}

#[test]
fn runed_halo_naming_other_card_lets_printed_bolt_target_its_controller() {
    assert_eq!(cast_bolt_aimed_at_p0("Shock"), (17, 20));
}

/// CR 702.16e: Orcish Artillery's untargeted "3 damage to you" is prevented when the
/// controller has protection from "Orcish Artillery"; its targeted 2 damage still lands.
fn activate_artillery(chosen: &str) -> (i32, i32) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let halo = halo_on_battlefield(&mut scenario);
    let artillery = scenario
        .add_creature_from_oracle(P0, "Orcish Artillery", 1, 3, ORCISH_ARTILLERY_ORACLE)
        .id();
    let mut runner = scenario.build();
    name_halo_choice(&mut runner, halo, chosen);

    let index = runner.state().objects[&artillery]
        .abilities
        .iter()
        .position(|a| a.cost.is_some())
        .expect("Orcish Artillery's tap ability");
    runner
        .act(GameAction::ActivateAbility {
            source_id: artillery,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activating Orcish Artillery");
    if let WaitingFor::TargetSelection { .. } = runner.state().waiting_for.clone() {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Player(P1)),
            })
            .expect("targeting P1");
    }
    assert!(
        runner.state().objects[&artillery].tapped,
        "the tap cost is paid"
    );
    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty());
    (
        runner.state().players[0].life,
        runner.state().players[1].life,
    )
}

#[test]
fn runed_halo_prevents_artillery_self_damage_when_named() {
    assert_eq!(activate_artillery("Orcish Artillery"), (20, 18));
}

#[test]
fn runed_halo_does_not_prevent_artillery_self_damage_for_other_name() {
    assert_eq!(activate_artillery("Shock"), (17, 18));
}
