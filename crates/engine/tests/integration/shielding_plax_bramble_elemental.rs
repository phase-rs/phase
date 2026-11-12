//! Integration tests for Bramble Elemental and Shielding Plax:
//!
//! Bramble Elemental:
//! "Whenever an Aura becomes attached to this creature, create two 1/1 green Saproling creature tokens."
//!
//! Shielding Plax:
//! "Enchant creature
//! When this Aura enters, draw a card.
//! Enchanted creature can't be the target of spells or abilities your opponents control."

use engine::game::engine::EngineError;
use engine::game::game_object::AttachTarget;
use engine::game::keywords::has_keyword;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{FilterProp, StaticDefinition, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::{ProhibitionScope, StaticMode};
use engine::types::zones::Zone;
use engine::types::ObjectId;
use engine::types::TargetRef;

const BRAMBLE_ELEMENTAL: &str =
    "Whenever an Aura becomes attached to this creature, create two 1/1 green Saproling creature tokens.";

const SHIELDING_PLAX: &str = "Enchant creature\nWhen this Aura enters, draw a card.\nEnchanted creature can't be the target of spells or abilities your opponents control.";

/// Synthetic grammar fixture, not a printed card. No printed Aura has the
/// compound "gets +N/+N and can't be the target of spells or abilities your
/// opponents control" shape: per Scryfall the only Auras with this restriction
/// are Shielding Plax and Canopy Cover, each stating it as its own sentence (so
/// they parse to the direct `CantBeTargeted` static, not this compound
/// continuous grant). The nearest printed compound is the kicked Vines of
/// Vastwood, a spell (see `vines_of_vastwood_scopes_opponents_from_its_caster`).
const COMPOUND_AURA: &str = "Enchant creature\nEnchanted creature gets +1/+1 and can't be the target of spells or abilities your opponents control.";

const ENORMOUS_ENERGY_BLADE: &str = "Equipped creature gets +4/+0.\nWhenever this Equipment becomes attached to a creature, tap that creature.\nEquip {0}";

const GRAFTED_WARGEAR: &str = "Equipped creature gets +3/+2.\nWhenever Grafted Wargear becomes unattached from a permanent, sacrifice that permanent.\nEquip {0}";

/// Whether `def` is the opponent-scoped targeting restriction on "enchanted
/// creature" (`CantBeTargeted { who: Opponents }` affecting `EnchantedBy`).
fn is_enchanted_creature_opponent_restriction(def: &StaticDefinition) -> bool {
    matches!(
        def.mode,
        StaticMode::CantBeTargeted {
            who: ProhibitionScope::Opponents
        }
    ) && matches!(
        &def.affected,
        Some(TargetFilter::Typed(typed)) if typed.properties.contains(&FilterProp::EnchantedBy)
    )
}

fn saproling_count(runner: &GameRunner, player: PlayerId) -> usize {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|obj| {
            obj.controller == player
                && obj.card_types.subtypes.iter().any(|s| s == "Saproling")
                && obj.power == Some(1)
                && obj.toughness == Some(1)
        })
        .count()
}

#[test]
fn bramble_elemental_creates_tokens_when_enchanted_by_shielding_plax() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest"]);

    let bramble = scenario
        .add_creature_from_oracle(P0, "Bramble Elemental", 4, 4, BRAMBLE_ELEMENTAL)
        .id();

    let plax = scenario
        .add_spell_to_hand(P0, "Shielding Plax", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::generic(0))
        .from_oracle_text_with_keywords(&["Enchant"], SHIELDING_PLAX)
        .id();

    let mut runner = scenario.build();

    // Cast Shielding Plax targeting Bramble Elemental
    let outcome = runner.cast(plax).target_object(bramble).resolve();

    // 1. Bramble Elemental creates two 1/1 green Saprolings
    assert_eq!(
        saproling_count(&runner, P0),
        2,
        "Bramble Elemental must create 2 Saproling tokens when an Aura becomes attached"
    );

    // 2. Shielding Plax ETB draws a card (hand delta since commit is +1)
    outcome.assert_hand_drawn(P0, 1);

    // 3. Shielding Plax carries CantBeTargeted { Opponents } affecting the enchanted creature,
    // and does NOT grant Keyword::Hexproof directly to the creature (CR 109.5).
    assert!(
        !has_keyword(&runner.state().objects[&bramble], &Keyword::Hexproof),
        "Shielding Plax must not grant Keyword::Hexproof directly to the creature"
    );
}

#[test]
fn shielding_plax_restricts_targeting_of_enchanted_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest"]);

    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    let plax = scenario
        .add_spell_to_hand(P0, "Shielding Plax", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::generic(0))
        .from_oracle_text_with_keywords(&["Enchant"], SHIELDING_PLAX)
        .id();

    let mut runner = scenario.build();

    // Initially Grizzly Bears does not have Hexproof
    assert!(
        !has_keyword(&runner.state().objects[&bear], &Keyword::Hexproof),
        "Grizzly Bears should not have Hexproof before being enchanted"
    );

    // Cast Shielding Plax targeting Grizzly Bears
    runner.cast(plax).target_object(bear).resolve();

    // After resolution, Grizzly Bears does not receive the Hexproof keyword directly (CR 109.5);
    // targeting is governed by the Aura's static CantBeTargeted ability.
    assert!(
        !has_keyword(&runner.state().objects[&bear], &Keyword::Hexproof),
        "Shielding Plax must not grant Hexproof keyword (governed by CantBeTargeted static)"
    );

    // The restriction is the Aura's typed static: opponent-scoped, affecting the
    // enchanted creature (and not a keyword grant).
    assert!(
        runner.state().objects[&plax]
            .static_definitions
            .iter_unchecked()
            .any(is_enchanted_creature_opponent_restriction),
        "Shielding Plax must carry CantBeTargeted {{ Opponents }} affecting the enchanted creature"
    );

    // Shielding Plax itself is NOT the creature and does not have Hexproof on itself
    assert!(
        !has_keyword(&runner.state().objects[&plax], &Keyword::Hexproof),
        "Shielding Plax itself must not have Hexproof"
    );
}

#[test]
fn compound_aura_grants_pt_and_cant_be_targeted_to_enchanted_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let sibling_1 = scenario.add_creature(P0, "Sibling Bear 1", 2, 2).id();
    let sibling_2 = scenario.add_creature(P0, "Sibling Bear 2", 2, 2).id();

    let probe = scenario
        .add_spell_to_hand(P1, "Targeting Probe", false)
        .with_mana_cost(ManaCost::generic(0))
        .from_oracle_text("Targeting Probe deals 1 damage to target creature.")
        .id();

    let aura = scenario
        .add_spell_to_hand(P0, "Compound Shield", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::generic(0))
        .from_oracle_text_with_keywords(&["Enchant"], COMPOUND_AURA)
        .id();

    let mut runner = scenario.build();

    runner.cast(aura).target_object(bear).resolve();

    // Grizzly Bears gets +1/+1
    let bear_obj = &runner.state().objects[&bear];
    assert_eq!(
        bear_obj.power,
        Some(3),
        "Grizzly Bears should get +1/+1 (power 3)"
    );
    assert_eq!(
        bear_obj.toughness,
        Some(3),
        "Grizzly Bears should get +1/+1 (toughness 3)"
    );
    assert!(
        !has_keyword(bear_obj, &Keyword::Hexproof),
        "Compound Aura must not grant Hexproof keyword (governed by CantBeTargeted static)"
    );

    // The parsed/cast compound fixture must also restrict targeting at runtime:
    // the opponent cannot select the enchanted host but can select either
    // unprotected sibling.
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    runner
        .act(GameAction::CastSpell {
            object_id: probe,
            card_id: runner.state().objects[&probe].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("opponent can begin casting the probe");
    let WaitingFor::TargetSelection { target_slots, .. } = runner.state().waiting_for.clone()
    else {
        panic!("expected production target selection");
    };
    let legal = &target_slots[0].legal_targets;
    assert!(
        !legal.contains(&TargetRef::Object(bear)),
        "the enchanted host must be excluded for the opponent"
    );
    assert!(legal.contains(&TargetRef::Object(sibling_1)));
    assert!(legal.contains(&TargetRef::Object(sibling_2)));
    assert!(
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(bear)),
            })
            .is_err(),
        "selecting the enchanted host must be rejected"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(sibling_1)),
        })
        .expect("a legal unprotected sibling is selectable");
}

#[test]
fn opponent_cannot_target_creature_enchanted_by_shielding_plax() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest"]);

    let protected_bear = scenario.add_creature(P0, "Protected Bear", 2, 2).id();
    let unprotected_bear_1 = scenario.add_creature(P0, "Unprotected Bear 1", 2, 2).id();
    let unprotected_bear_2 = scenario.add_creature(P0, "Unprotected Bear 2", 2, 2).id();

    let plax = scenario
        .add_spell_to_hand(P0, "Shielding Plax", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::generic(0))
        .from_oracle_text_with_keywords(&["Enchant"], SHIELDING_PLAX)
        .id();

    let p0_buff = scenario
        .add_spell_to_hand(P0, "Giant Growth", false)
        .with_mana_cost(ManaCost::generic(0))
        .from_oracle_text("Target creature gets +3/+3 until end of turn.")
        .id();

    let spitting_earth = scenario
        .add_spell_to_hand(P1, "Spitting Earth", false)
        .with_mana_cost(ManaCost::generic(0))
        .from_oracle_text("Spitting Earth deals damage to target creature equal to the number of Mountains you control.")
        .id();

    let mut runner = scenario.build();

    // P0 enchants Protected Bear with Shielding Plax
    runner.cast(plax).target_object(protected_bear).resolve();

    // Positive Control 1: P0 (the controller) CAN target Protected Bear with a spell
    let p0_result = runner
        .cast(p0_buff)
        .target_object(protected_bear)
        .try_resolve();
    assert!(
        p0_result.is_ok(),
        "Controller must be able to target their own Hexproof creature"
    );

    // Give active turn and priority to P1 (opponent)
    runner.state_mut().active_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    runner.state_mut().priority_player = P1;

    // Negative Test: P1 initiates casting Spitting Earth
    let cast_res = runner.act(GameAction::CastSpell {
        object_id: spitting_earth,
        card_id: runner.state().objects[&spitting_earth].card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    });
    assert!(
        cast_res.is_ok(),
        "Initiating cast must enter target selection"
    );

    // Verify engine excludes protected_bear from legal_targets for opponent
    let waiting = runner.state().waiting_for.clone();
    if let WaitingFor::TargetSelection {
        ref target_slots, ..
    } = waiting
    {
        assert!(
            !target_slots[0]
                .legal_targets
                .contains(&TargetRef::Object(protected_bear)),
            "Protected bear must not be in legal targets for opponent"
        );
        assert!(
            target_slots[0]
                .legal_targets
                .contains(&TargetRef::Object(unprotected_bear_1)),
            "Unprotected bear 1 must be in legal targets for opponent"
        );
        assert!(
            target_slots[0]
                .legal_targets
                .contains(&TargetRef::Object(unprotected_bear_2)),
            "Unprotected bear 2 must be in legal targets for opponent"
        );
    } else {
        panic!("Expected WaitingFor::TargetSelection, got: {waiting:?}");
    }

    // Attempting to select protected_bear must be rejected with "Illegal target selected"
    let choose_res = runner.act(GameAction::ChooseTarget {
        target: Some(TargetRef::Object(protected_bear)),
    });
    match choose_res {
        Err(EngineError::InvalidAction(ref msg)) => {
            assert!(
                msg.contains("Illegal target"),
                "Expected illegal target rejection message, got: {msg}"
            );
        }
        Err(other) => panic!("Expected InvalidAction(Illegal target), got: {other:?}"),
        Ok(_) => panic!("Opponent targeting creature with Hexproof must fail, but succeeded"),
    }

    // Positive Control 2: Selecting unprotected_bear_1 succeeds
    let choose_valid = runner.act(GameAction::ChooseTarget {
        target: Some(TargetRef::Object(unprotected_bear_1)),
    });
    assert!(
        choose_valid.is_ok(),
        "Opponent choosing unprotected creature must succeed"
    );
}

#[test]
fn shielding_plax_different_controller_scope_cr_109_5() {
    // CR 109.5: "For a static ability, this is the current controller of the object it's on."
    // Shielding Plax says: "Enchanted creature can't be the target of spells or abilities your opponents control."
    // Here, P0 controls Shielding Plax attached to P1's creature.
    // P0's opponents are prohibited from targeting the creature (so P1 cannot target their own creature).
    // P0 is NOT an opponent of P0, so P0 CAN target P1's enchanted creature.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest"]);

    let p1_bear = scenario.add_creature(P1, "P1 Bear", 2, 2).id();
    let p1_unprotected_bear_1 = scenario
        .add_creature(P1, "P1 Unprotected Bear 1", 2, 2)
        .id();
    let p1_unprotected_bear_2 = scenario
        .add_creature(P1, "P1 Unprotected Bear 2", 2, 2)
        .id();

    let plax = scenario
        .add_spell_to_hand(P0, "Shielding Plax", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::generic(0))
        .from_oracle_text_with_keywords(&["Enchant"], SHIELDING_PLAX)
        .id();

    let p0_buff = scenario
        .add_spell_to_hand(P0, "Giant Growth", false)
        .with_mana_cost(ManaCost::generic(0))
        .from_oracle_text("Target creature gets +3/+3 until end of turn.")
        .id();

    let spitting_earth = scenario
        .add_spell_to_hand(P1, "Spitting Earth", false)
        .with_mana_cost(ManaCost::generic(0))
        .from_oracle_text("Spitting Earth deals damage to target creature equal to the number of Mountains you control.")
        .id();

    let mut runner = scenario.build();

    // P0 enchants P1's creature with Shielding Plax
    runner.cast(plax).target_object(p1_bear).resolve();

    // Positive Control: P0 (Aura controller) CAN target P1's creature
    let p0_result = runner.cast(p0_buff).target_object(p1_bear).try_resolve();
    assert!(
        p0_result.is_ok(),
        "Aura controller (P0) must be able to target P1's creature enchanted by P0's Shielding Plax"
    );

    // Pass turn / priority to P1 (creature controller, but opponent of Aura controller)
    runner.state_mut().active_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    runner.state_mut().priority_player = P1;

    // Negative Test: P1 tries to target P1 Bear
    let cast_res = runner.act(GameAction::CastSpell {
        object_id: spitting_earth,
        card_id: runner.state().objects[&spitting_earth].card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    });
    assert!(
        cast_res.is_ok(),
        "Initiating cast must enter target selection"
    );

    // Verify p1_bear is excluded from legal_targets for P1
    let waiting = runner.state().waiting_for.clone();
    if let WaitingFor::TargetSelection {
        ref target_slots, ..
    } = waiting
    {
        assert!(
            !target_slots[0]
                .legal_targets
                .contains(&TargetRef::Object(p1_bear)),
            "P1's creature enchanted by P0's Shielding Plax must NOT be a legal target for P1 (P0's opponent)"
        );
        assert!(
            target_slots[0]
                .legal_targets
                .contains(&TargetRef::Object(p1_unprotected_bear_1)),
            "P1's unprotected creature 1 must be a legal target for P1"
        );
        assert!(
            target_slots[0]
                .legal_targets
                .contains(&TargetRef::Object(p1_unprotected_bear_2)),
            "P1's unprotected creature 2 must be a legal target for P1"
        );
    } else {
        panic!("Expected WaitingFor::TargetSelection, got: {waiting:?}");
    }

    // Attempting to select p1_bear must fail with Illegal target
    let choose_res = runner.act(GameAction::ChooseTarget {
        target: Some(TargetRef::Object(p1_bear)),
    });
    match choose_res {
        Err(EngineError::InvalidAction(ref msg)) => {
            assert!(
                msg.contains("Illegal target"),
                "Expected illegal target rejection message, got: {msg}"
            );
        }
        Err(other) => panic!("Expected InvalidAction(Illegal target), got: {other:?}"),
        Ok(_) => panic!("P1 targeting creature enchanted by P0's Shielding Plax must fail"),
    }

    // Positive Control 2: Selecting p1_unprotected_bear_1 succeeds
    let choose_valid = runner.act(GameAction::ChooseTarget {
        target: Some(TargetRef::Object(p1_unprotected_bear_1)),
    });
    assert!(
        choose_valid.is_ok(),
        "P1 selecting their own unprotected creature must succeed"
    );
}

#[test]
fn equipment_attachment_single_trigger_and_same_host_noop() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let bear1 = scenario.add_creature(P0, "Grizzly Bears 1", 2, 2).id();
    let bear2 = scenario.add_creature(P0, "Grizzly Bears 2", 2, 2).id();

    let blade = scenario
        .add_artifact_from_oracle(P0, "Enormous Energy Blade", ENORMOUS_ENERGY_BLADE)
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text_with_keywords(&["Equip"], ENORMOUS_ENERGY_BLADE)
        .id();

    let mut runner = scenario.build();

    // Bear 1 and Bear 2 start untapped
    assert!(!runner.state().objects[&bear1].tapped);
    assert!(!runner.state().objects[&bear2].tapped);

    let equip_idx = runner.state().objects[&blade]
        .abilities
        .iter()
        .position(|a| {
            a.description
                .as_deref()
                .is_some_and(|d| d.contains("Equip"))
        })
        .expect("Blade must carry an Equip activated ability");

    // 1. First Equip to Bear 1:
    // Resolves Equip, attaches to Bear 1, fires exactly 1 attached trigger which taps Bear 1.
    runner
        .activate(blade, equip_idx)
        .target_object(bear1)
        .resolve();

    assert_eq!(
        runner.state().objects[&blade].attached_to,
        Some(AttachTarget::Object(bear1)),
        "Blade must be attached to Bear 1"
    );
    assert_eq!(
        runner.state().objects[&bear1].power,
        Some(6),
        "Bear 1 must have +4/+0 (power 6)"
    );
    assert!(
        runner.state().objects[&bear1].tapped,
        "Bear 1 must be tapped by the attached trigger"
    );

    // Untap Bear 1 manually to test same-host re-equip
    runner.state_mut().objects.get_mut(&bear1).unwrap().tapped = false;
    assert!(!runner.state().objects[&bear1].tapped);

    // 2. Same-host Equip to Bear 1 (CR 701.3b):
    // Re-equipping to the same host is a no-op that does NOT emit GameEvent::Attached.
    // 0 triggers must fire, so Bear 1 must REMAIN untapped.
    runner
        .activate(blade, equip_idx)
        .target_object(bear1)
        .resolve();

    assert_eq!(
        runner.state().objects[&blade].attached_to,
        Some(AttachTarget::Object(bear1))
    );
    assert!(
        !runner.state().objects[&bear1].tapped,
        "CR 701.3b: Re-equipping to same host is a no-op and must not trigger attached ability"
    );

    // 3. Move Equip to Bear 2:
    // Attaches to Bear 2, Bear 1 loses P/T bonus, Bear 2 gets +4/+0 and becomes tapped by the 1 attached trigger.
    runner
        .activate(blade, equip_idx)
        .target_object(bear2)
        .resolve();

    assert_eq!(
        runner.state().objects[&blade].attached_to,
        Some(AttachTarget::Object(bear2))
    );
    assert_eq!(
        runner.state().objects[&bear1].power,
        Some(2),
        "Bear 1 loses P/T bonus after equipment is moved"
    );
    assert_eq!(
        runner.state().objects[&bear2].power,
        Some(6),
        "Bear 2 gets +4/+0"
    );
    assert!(
        runner.state().objects[&bear2].tapped,
        "Bear 2 must be tapped by the attached trigger upon moving equipment"
    );
}

#[test]
fn opponent_can_target_shielding_plax_with_naturalize() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest"]);

    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    let plax = scenario
        .add_spell_to_hand(P0, "Shielding Plax", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::generic(0))
        .from_oracle_text_with_keywords(&["Enchant"], SHIELDING_PLAX)
        .id();

    let naturalize = scenario
        .add_spell_to_hand(P1, "Naturalize", false)
        .with_mana_cost(ManaCost::generic(0))
        .from_oracle_text("Destroy target artifact or enchantment.")
        .id();

    let mut runner = scenario.build();

    // P0 enchants Bear with Shielding Plax
    runner.cast(plax).target_object(bear).resolve();

    // Give priority to P1 (opponent)
    runner.state_mut().active_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    runner.state_mut().priority_player = P1;

    // Shielding Plax protects the enchanted creature, NOT the Aura itself.
    // P1 (opponent) casts Naturalize targeting Shielding Plax.
    runner.cast(naturalize).target_object(plax).resolve();

    // Shielding Plax is destroyed (put into graveyard)
    assert!(
        runner.state().players[0].graveyard.contains(&plax),
        "Shielding Plax should be destroyed by Naturalize and put into P0's graveyard"
    );
    assert!(
        !runner.state().battlefield.contains(&plax),
        "Shielding Plax should no longer be on the battlefield"
    );
}

/// CR 608.2k + CR 701.3d + CR 701.21a: When Grafted Wargear becomes unattached from a permanent,
/// "sacrifice that permanent" refers to the permanent it became unattached from (the former host),
/// NOT the Equipment itself.
#[test]
fn grafted_wargear_unattached_sacrifices_former_host_not_equipment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let bear1 = scenario.add_creature(P0, "Grizzly Bears 1", 2, 2).id();
    let bear2 = scenario.add_creature(P0, "Grizzly Bears 2", 2, 2).id();

    let wargear = scenario
        .add_artifact_from_oracle(P0, "Grafted Wargear", GRAFTED_WARGEAR)
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text_with_keywords(&["Equip"], GRAFTED_WARGEAR)
        .id();

    let mut runner = scenario.build();

    let equip_idx = runner.state().objects[&wargear]
        .abilities
        .iter()
        .position(|a| {
            a.description
                .as_deref()
                .is_some_and(|d| d.contains("Equip"))
        })
        .expect("Grafted Wargear must carry an Equip activated ability");

    // 1. Equip to Bear 1:
    runner
        .activate(wargear, equip_idx)
        .target_object(bear1)
        .resolve();

    assert_eq!(
        runner.state().objects[&wargear].attached_to,
        Some(AttachTarget::Object(bear1)),
        "Grafted Wargear must be attached to Bear 1"
    );
    assert_eq!(
        runner.state().objects[&bear1].power,
        Some(5),
        "Bear 1 gets +3/+2 (power 5)"
    );
    assert_eq!(
        runner.state().objects[&bear1].toughness,
        Some(4),
        "Bear 1 gets +3/+2 (toughness 4)"
    );

    // 2. Move Equip to Bear 2:
    // Wargear becomes unattached from Bear 1 and attached to Bear 2.
    // The unattached trigger fires: "sacrifice that permanent" (CR 701.3d, CR 608.2k).
    // The former host (Bear 1) must be sacrificed (CR 701.21a), while Grafted Wargear
    // remains on the battlefield attached to Bear 2!
    runner
        .activate(wargear, equip_idx)
        .target_object(bear2)
        .resolve();

    // Bear 1 was sacrificed: in graveyard, not on battlefield
    assert!(
        runner.state().players[0].graveyard.contains(&bear1),
        "Bear 1 (former host) must be sacrificed and in graveyard"
    );
    assert!(
        !runner.state().battlefield.contains(&bear1),
        "Bear 1 must no longer be on battlefield"
    );

    // Grafted Wargear remains on battlefield and is attached to Bear 2
    assert!(
        runner.state().battlefield.contains(&wargear),
        "Grafted Wargear must remain on the battlefield, NOT be sacrificed"
    );
    assert_eq!(
        runner.state().objects[&wargear].attached_to,
        Some(AttachTarget::Object(bear2)),
        "Grafted Wargear must be attached to Bear 2"
    );

    // Bear 2 is alive on battlefield and gets +3/+2
    assert!(
        runner.state().battlefield.contains(&bear2),
        "Bear 2 must remain on the battlefield"
    );
    assert_eq!(
        runner.state().objects[&bear2].power,
        Some(5),
        "Bear 2 gets +3/+2 (power 5)"
    );
    assert_eq!(
        runner.state().objects[&bear2].toughness,
        Some(4),
        "Bear 2 gets +3/+2 (toughness 4)"
    );
}

/// CR 603.2e + CR 201.5: An Aura attaching to a player must not trigger creatures
/// with "Whenever an Aura becomes attached to this creature" (Bramble Elemental, Brood Keeper).
/// An object-source SelfRef requires an object host and never matches a player host.
#[test]
fn curse_attaching_to_player_does_not_trigger_bramble_elemental() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest"]);

    let bramble = scenario
        .add_creature_from_oracle(P0, "Bramble Elemental", 4, 4, BRAMBLE_ELEMENTAL)
        .id();

    let curse = scenario
        .add_spell_to_hand(P0, "Curse of Opulence", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura", "Curse"])
        .with_mana_cost(ManaCost::generic(0))
        .from_oracle_text_with_keywords(
            &["Enchant"],
            "Enchant player\nWhenever enchanted player is attacked, create a Gold token. Each opponent attacking that player does the same.",
        )
        .id();

    let plax = scenario
        .add_spell_to_hand(P0, "Shielding Plax", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::generic(0))
        .from_oracle_text_with_keywords(&["Enchant"], SHIELDING_PLAX)
        .id();

    let mut runner = scenario.build();

    // Cast Curse of Opulence targeting P1 (enchant player)
    runner.cast(curse).target_player(P1).resolve();

    // Verify Curse is attached to P1
    assert_eq!(
        runner.state().objects[&curse].attached_to,
        Some(AttachTarget::Player(P1)),
        "Curse of Opulence must be attached to P1"
    );

    // CR 603.2e + CR 201.5: Bramble Elemental must NOT trigger when an Aura attaches to a player
    assert_eq!(
        saproling_count(&runner, P0),
        0,
        "Curse attaching to a player must not trigger Bramble Elemental"
    );

    // Positive control: Cast Shielding Plax targeting Bramble Elemental
    runner.cast(plax).target_object(bramble).resolve();

    // Shielding Plax attaching to Bramble Elemental triggers it and creates exactly 2 Saprolings
    assert_eq!(
        saproling_count(&runner, P0),
        2,
        "Shielding Plax attaching to Bramble Elemental must trigger and create 2 Saprolings"
    );
}

/// CR 303.4f + CR 701.3a + CR 603.2e: When Copy Enchantment enters copying an Aura
/// with a sole legal host, it must auto-attach to that host, emit GameEvent::Attached,
/// and trigger "Whenever an Aura becomes attached to this creature".
#[test]
fn copy_enchantment_aura_sole_host_emits_attached_and_triggers_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let bramble = scenario
        .add_creature_from_oracle(P0, "Bramble Elemental", 4, 4, BRAMBLE_ELEMENTAL)
        .id();

    // Opponent controls a creature with an "Enchant creature you control" Aura
    let opp_bear = scenario.add_creature(P1, "Opponent Bear", 2, 2).id();
    let opp_aura = {
        let mut b = scenario.add_enchantment_from_oracle(
            P1,
            "Friendly Aura",
            "Enchant creature you control\nEnchanted creature gets +1/+1.",
        );
        b.with_subtypes(vec!["Aura"]);
        b.from_oracle_text_with_keywords(
            &["Enchant"],
            "Enchant creature you control\nEnchanted creature gets +1/+1.",
        );
        b.id()
    };

    let copy_ench = scenario
        .add_spell_to_hand(P0, "Copy Enchantment", false)
        .as_enchantment()
        .with_mana_cost(ManaCost::generic(0))
        .from_oracle_text(
            "You may have this enchantment enter as a copy of any enchantment on the battlefield.",
        )
        .id();

    let mut runner = scenario.build();
    // Attach opp_aura to opp_bear
    runner
        .state_mut()
        .objects
        .get_mut(&opp_aura)
        .unwrap()
        .attached_to = Some(AttachTarget::Object(opp_bear));

    // P0 casts Copy Enchantment, choosing to copy opp_aura.
    // For P0, "Enchant creature you control" only matches Bramble Elemental (the sole creature P0 controls).
    // Copy Enchantment auto-attaches to Bramble Elemental and emits GameEvent::Attached.
    let outcome = runner
        .cast(copy_ench)
        .replacement_choice(0)
        .copy_target(opp_aura)
        .resolve();

    assert_eq!(
        runner.state().objects[&copy_ench].attached_to,
        Some(AttachTarget::Object(bramble)),
        "Copy Enchantment must auto-attach to Bramble Elemental as the sole legal host"
    );

    assert!(
        outcome.events().iter().any(|e| matches!(
            e,
            engine::types::events::GameEvent::Attached {
                attachment_id,
                target: TargetRef::Object(target_id)
            } if attachment_id == &copy_ench && target_id == &bramble
        )),
        "GameEvent::Attached must be emitted when Copy Enchantment auto-attaches"
    );

    assert_eq!(
        saproling_count(&runner, P0),
        2,
        "Bramble Elemental must create 2 Saprolings when copied Aura becomes attached"
    );
}

/// CR 303.4f + CR 701.3a + CR 603.2e: When Copy Enchantment enters copying an Aura
/// with multiple legal hosts, the game must prompt for a host choice (WaitingFor::ReturnAsAuraTarget),
/// and upon submission, emit GameEvent::Attached and trigger the creature.
#[test]
fn copy_enchantment_aura_multi_host_prompt_and_choice_emits_attached() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let bramble = scenario
        .add_creature_from_oracle(P0, "Bramble Elemental", 4, 4, BRAMBLE_ELEMENTAL)
        .id();
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    let opp_bear = scenario.add_creature(P1, "Opponent Bear", 2, 2).id();
    let opp_aura = {
        let mut b = scenario.add_enchantment_from_oracle(
            P1,
            "General Aura",
            "Enchant creature\nEnchanted creature gets +1/+1.",
        );
        b.with_subtypes(vec!["Aura"]);
        b.from_oracle_text_with_keywords(
            &["Enchant"],
            "Enchant creature\nEnchanted creature gets +1/+1.",
        );
        b.id()
    };

    let copy_ench = scenario
        .add_spell_to_hand(P0, "Copy Enchantment", false)
        .as_enchantment()
        .with_mana_cost(ManaCost::generic(0))
        .from_oracle_text(
            "You may have this enchantment enter as a copy of any enchantment on the battlefield.",
        )
        .id();

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&opp_aura)
        .unwrap()
        .attached_to = Some(AttachTarget::Object(opp_bear));

    // P0 casts Copy Enchantment copying opp_aura.
    // Both Bramble Elemental and Grizzly Bears (and opp_bear) are legal hosts for "Enchant creature".
    let commit = runner
        .cast(copy_ench)
        .replacement_choice(0)
        .copy_target(opp_aura);
    let _ = commit.try_resolve();

    let WaitingFor::ReturnAsAuraTarget {
        returned_id,
        legal_targets,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "Expected WaitingFor::ReturnAsAuraTarget, got {:?}",
            runner.state().waiting_for
        );
    };

    assert!(legal_targets.contains(&TargetRef::Object(bramble)));
    assert!(legal_targets.contains(&TargetRef::Object(bear)));

    // Choose Bramble Elemental as the host
    let res = runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(bramble)),
        })
        .expect("submit aura host choice");

    assert!(
        res.events.iter().any(|e| matches!(
            e,
            engine::types::events::GameEvent::Attached {
                attachment_id,
                target: TargetRef::Object(target_id)
            } if attachment_id == &returned_id && target_id == &bramble
        )),
        "GameEvent::Attached must be emitted when host choice is submitted"
    );

    // Drive priority until stack is empty so Bramble Elemental's triggered ability resolves
    while !runner.state().stack.is_empty() {
        runner.act(GameAction::PassPriority).expect("pass priority");
    }

    assert_eq!(
        runner.state().objects[&returned_id].attached_to,
        Some(AttachTarget::Object(bramble)),
        "Copy Enchantment must be attached to Bramble Elemental"
    );

    assert_eq!(
        saproling_count(&runner, P0),
        2,
        "Bramble Elemental must create 2 Saprolings when host is chosen and Attached is emitted"
    );
}

/// CR 701.3b: "Attaching an Aura, Equipment, or Fortification in the battlefield to the
/// object or player it's already attached to does nothing." No events are emitted and returns false.
#[test]
fn entering_aura_same_host_does_nothing_cr_701_3b() {
    let mut scenario = GameScenario::new();
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let aura = scenario
        .add_enchantment_from_oracle(
            P0,
            "Aura",
            "Enchant creature\nEnchanted creature gets +1/+1.",
        )
        .with_subtypes(vec!["Aura"])
        .id();
    let curse = scenario
        .add_enchantment_from_oracle(P0, "Curse", "Enchant player")
        .with_subtypes(vec!["Aura", "Curse"])
        .id();

    let mut runner = scenario.build();
    let state = runner.state_mut();
    state.objects.get_mut(&aura).unwrap().attached_to = Some(AttachTarget::Object(bear));
    state.objects.get_mut(&curse).unwrap().attached_to = Some(AttachTarget::Player(P1));

    // 1. Same object host:
    let mut events = Vec::new();
    let attached =
        engine::game::effects::attach::attach_to_with_events(state, aura, bear, &mut events);
    assert!(
        !attached,
        "Attaching to same object host must return false (does nothing)"
    );
    assert!(
        events.is_empty(),
        "CR 701.3b: Attaching to same object host must emit zero events"
    );

    // 2. Same player host:
    let mut player_events = Vec::new();
    let player_attached = engine::game::effects::attach::attach_to_player_with_events(
        state,
        curse,
        P1,
        &mut player_events,
    );
    assert!(
        !player_attached,
        "Attaching to same player host must return false (does nothing)"
    );
    assert!(
        player_events.is_empty(),
        "CR 701.3b: Attaching to same player host must emit zero events"
    );
}

/// CR 701.3a + CR 701.3b: Attaching an Aura to an illegal host does nothing.
/// No events are emitted and returns false.
#[test]
fn entering_aura_rejected_host_control() {
    let mut scenario = GameScenario::new();
    let artifact = scenario
        .add_artifact_from_oracle(P0, "Sol Ring", "{T}: Add {C}{C}.")
        .id();
    let aura = {
        let mut b = scenario.add_enchantment_from_oracle(
            P0,
            "Aura",
            "Enchant creature\nEnchanted creature gets +1/+1.",
        );
        b.with_subtypes(vec!["Aura"]);
        b.from_oracle_text_with_keywords(
            &["Enchant"],
            "Enchant creature\nEnchanted creature gets +1/+1.",
        );
        b.id()
    };

    let mut runner = scenario.build();
    let state = runner.state_mut();

    // 1. "Enchant creature" attempted on non-creature artifact:
    let mut events = Vec::new();
    let attached =
        engine::game::effects::attach::attach_to_with_events(state, aura, artifact, &mut events);
    assert!(
        !attached,
        "Attaching 'Enchant creature' Aura to an artifact must return false"
    );
    assert!(
        events.is_empty(),
        "Illegal object attachment must emit zero events"
    );

    // 2. "Enchant creature" attempted on a player:
    let mut player_events = Vec::new();
    let player_attached = engine::game::effects::attach::attach_to_player_with_events(
        state,
        aura,
        P1,
        &mut player_events,
    );
    assert!(
        !player_attached,
        "Attaching 'Enchant creature' Aura to a player must return false"
    );
    assert!(
        player_events.is_empty(),
        "Illegal player attachment must emit zero events"
    );

    // 3. CR 303.4d: Attaching an Aura to itself must return false
    let mut self_events = Vec::new();
    let self_attached =
        engine::game::effects::attach::attach_to_with_events(state, aura, aura, &mut self_events);
    assert!(
        !self_attached,
        "CR 303.4d: Attaching an Aura to itself must return false"
    );
    assert!(
        self_events.is_empty(),
        "Self attachment must emit zero events"
    );
}

/// CR 303.4g + CR 701.3b: When Copy Enchantment enters copying an Aura but has no legal host,
/// it does not attach and emits zero Attached events.
#[test]
fn copy_enchantment_aura_no_legal_host_does_not_attach() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Opponent controls a creature with an "Enchant creature you control" Aura
    let opp_bear = scenario.add_creature(P1, "Opponent Bear", 2, 2).id();
    let opp_aura = {
        let mut b = scenario.add_enchantment_from_oracle(
            P1,
            "Friendly Aura",
            "Enchant creature you control\nEnchanted creature gets +1/+1.",
        );
        b.with_subtypes(vec!["Aura"]);
        b.from_oracle_text_with_keywords(
            &["Enchant"],
            "Enchant creature you control\nEnchanted creature gets +1/+1.",
        );
        b.id()
    };

    // P0 controls NO creatures at all.
    let copy_ench = scenario
        .add_spell_to_hand(P0, "Copy Enchantment", false)
        .as_enchantment()
        .with_mana_cost(ManaCost::generic(0))
        .from_oracle_text(
            "You may have this enchantment enter as a copy of any enchantment on the battlefield.",
        )
        .id();

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&opp_aura)
        .unwrap()
        .attached_to = Some(AttachTarget::Object(opp_bear));

    // P0 casts Copy Enchantment copying opp_aura ("Enchant creature you control").
    // P0 has NO creatures, so there are 0 legal hosts (CR 303.4g).
    let outcome = runner
        .cast(copy_ench)
        .replacement_choice(0)
        .copy_target(opp_aura)
        .resolve();

    // CR 303.4g: Aura does not attach to anything
    assert_eq!(
        runner.state().objects[&copy_ench].attached_to,
        None,
        "Copy Enchantment must not attach when there is no legal host"
    );

    // No Attached event is emitted
    assert!(
        !outcome
            .events()
            .iter()
            .any(|e| matches!(e, engine::types::events::GameEvent::Attached { .. })),
        "No Attached event must be emitted when no host is legal"
    );
}

/// CR 611.3a + CR 109.5: synthetic accepted-grammar discriminator, not a
/// printed-card claim. No printed card has a conditional ("as long as ...")
/// opponent-scoped targeting restriction; the nearest printed cards are the
/// unconditional Shielding Plax and Canopy Cover. An attached targeting
/// prohibition follows the Aura controller's live Island condition through
/// production targeting and bounce.
#[test]
fn gated_attached_targeting_allows_after_the_condition_ceases() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest"]);
    scenario.with_library_top(P1, &["Forest", "Forest", "Forest"]);
    let island = scenario.add_basic_land(P0, ManaColor::Blue);
    let protected = scenario.add_creature(P0, "Condition Host", 2, 2).id();
    // Two unprotected siblings: a lone legal assignment is auto-selected before
    // the interactive target prompt, so the exclusion assertions below would
    // never be reached.
    let bystander = scenario.add_creature(P0, "Target Reach Control", 2, 2).id();
    let second_bystander = scenario
        .add_creature(P0, "Second Target Reach Control", 2, 2)
        .id();
    let aura = scenario.add_spell_to_hand(P0, "Conditional Targeting Probe", false)
        .as_enchantment().with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::generic(0))
        .from_oracle_text_with_keywords(&["Enchant"], "Enchant creature\nEnchanted creature can't be the target of spells or abilities your opponents control as long as you control an Island.")
        .id();
    let tap = scenario
        .add_spell_to_hand(P1, "Targeting Probe", true)
        .with_mana_cost(ManaCost::generic(0))
        .from_oracle_text("Tap target creature.")
        .id();
    let bounce = scenario
        .add_spell_to_hand(P1, "Condition Removal Probe", true)
        .with_mana_cost(ManaCost::generic(0))
        .from_oracle_text("Return target land to its owner's hand.")
        .id();
    let mut runner = scenario.build();
    runner.cast(aura).target_object(protected).resolve();
    assert_eq!(
        runner.state().objects[&aura].attached_to,
        Some(AttachTarget::Object(protected))
    );
    assert!(
        runner.state().objects[&aura]
            .static_definitions
            .iter_unchecked()
            .any(|def| matches!(
                def.mode,
                StaticMode::CantBeTargeted {
                    who: ProhibitionScope::Opponents
                }
            ) && def.condition.is_some()),
        "the actual cast Aura must carry the conditional scoped restriction"
    );
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    runner
        .act(GameAction::CastSpell {
            object_id: tap,
            card_id: runner.state().objects[&tap].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    let WaitingFor::TargetSelection { target_slots, .. } = &runner.state().waiting_for else {
        panic!("must reach production target selection")
    };
    for sibling in [bystander, second_bystander] {
        assert!(
            target_slots[0]
                .legal_targets
                .contains(&TargetRef::Object(sibling)),
            "nonempty sibling targets guard the negative"
        );
    }
    assert!(!target_slots[0]
        .legal_targets
        .contains(&TargetRef::Object(protected)));
    assert!(runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(protected))
        })
        .is_err());
    runner.act(GameAction::CancelCast).unwrap();
    runner.cast(bounce).target_object(island).resolve();
    assert_eq!(runner.state().objects[&island].zone, Zone::Hand);
    assert!(!runner.state().objects[&protected].tapped);
    runner.cast(tap).target_object(protected).resolve();
    assert!(
        runner.state().objects[&protected].tapped,
        "the same opponent spell must resolve on the host after the gate ceases"
    );
}

/// Vines of Vastwood (verified Scryfall Oracle): "Target creature can't be the
/// target of spells or abilities your opponents control this turn."
///
/// CR 109.5: "your" refers to the controller of the spell that created the
/// effect. When P0 casts Vines on P1's creature, the restriction is granted onto
/// that creature, but it still prohibits P0's opponents (P1), not the creature
/// controller's opponents (P0): P1 can't target its own creature, while P0 can.
#[test]
fn vines_of_vastwood_scopes_opponents_from_its_caster() {
    const VINES_OF_VASTWOOD: &str = "Kicker {G}
Target creature can't be the target of spells or abilities your opponents control this turn. If this spell was kicked, that creature gets +4/+4 until end of turn.";

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let p1_creature = scenario.add_creature(P1, "Opposing Bear", 2, 2).id();
    // Two untouched bystanders keep P1's target prompt interactive (a lone legal
    // assignment is auto-selected before the prompt).
    let bystander = scenario.add_creature(P1, "Bystander Bear", 2, 2).id();
    let second_bystander = scenario.add_creature(P1, "Second Bystander", 2, 2).id();
    let vines = scenario
        .add_spell_to_hand(P0, "Vines of Vastwood", true)
        .with_mana_cost(ManaCost::generic(0))
        .from_oracle_text(VINES_OF_VASTWOOD)
        .id();
    let p0_tap = scenario
        .add_spell_to_hand(P0, "Caster Tap Probe", true)
        .with_mana_cost(ManaCost::generic(0))
        .from_oracle_text("Tap target creature.")
        .id();
    let p1_tap = scenario
        .add_spell_to_hand(P1, "Controller Tap Probe", true)
        .with_mana_cost(ManaCost::generic(0))
        .from_oracle_text("Tap target creature.")
        .id();
    let mut runner = scenario.build();

    runner.cast(vines).target_object(p1_creature).resolve();

    // CR 611.2c + CR 613.11: the prohibition is a resolution-created rule effect
    // read off its transient effect, anchored to the caster (CR 109.5). It is NOT
    // an ability on the creature.
    assert!(
        runner
            .state()
            .transient_continuous_effects
            .iter()
            .any(|tce| tce.controller == P0
                && tce.modifications.iter().any(|m| matches!(
                    m,
                    engine::types::ability::ContinuousModification::AddStaticMode {
                        mode: StaticMode::CantBeTargeted {
                            who: ProhibitionScope::Opponents
                        }
                    }
                ))),
        "Vines must install an opponent-scoped CantBeTargeted transient effect controlled by its caster"
    );
    assert!(
        !runner.state().objects[&p1_creature]
            .static_definitions
            .iter_unchecked()
            .any(|def| matches!(def.mode, StaticMode::CantBeTargeted { .. })),
        "the restriction must not be an ability on the creature"
    );

    // P1, the creature's controller and Vines' caster's opponent, can't target it.
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    runner
        .act(GameAction::CastSpell {
            object_id: p1_tap,
            card_id: runner.state().objects[&p1_tap].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    let WaitingFor::TargetSelection { target_slots, .. } = &runner.state().waiting_for else {
        panic!("P1's spell must reach production target selection")
    };
    for sibling in [bystander, second_bystander] {
        assert!(
            target_slots[0]
                .legal_targets
                .contains(&TargetRef::Object(sibling)),
            "nonempty sibling targets guard the negative"
        );
    }
    assert!(
        !target_slots[0]
            .legal_targets
            .contains(&TargetRef::Object(p1_creature)),
        "P1 is an opponent of Vines' caster and must not be able to target the creature"
    );
    runner.act(GameAction::CancelCast).unwrap();

    // P0, Vines' caster, is not prohibited and can target the opposing creature.
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    runner.cast(p0_tap).target_object(p1_creature).resolve();
    assert!(
        runner.state().objects[&p1_creature].tapped,
        "Vines' caster must be able to target the creature it protected"
    );
}

// ---------------------------------------------------------------------------
// Source-kind qualifiers fail closed; temporary restrictions are transient rules
// ---------------------------------------------------------------------------

const SPECTRAL_SHIELD: &str =
    "Enchant creature\nEnchanted creature gets +0/+2 and can't be the target of spells.";
const VINES: &str = "Kicker {G}
Target creature can't be the target of spells or abilities your opponents control this turn. If this spell was kicked, that creature gets +4/+4 until end of turn.";
const VEILSTONE_AMULET: &str = "Whenever you cast a spell, creatures you control can't be the targets of spells or abilities your opponents control this turn.";

/// Production target announcement: `player` begins `action`, the prompt's legal
/// targets are read, and the cast/activation is cancelled.
fn announced_legal_targets(
    runner: &mut GameRunner,
    player: PlayerId,
    action: GameAction,
) -> Vec<ObjectId> {
    runner.state_mut().active_player = player;
    runner.state_mut().priority_player = player;
    runner.state_mut().waiting_for = WaitingFor::Priority { player };
    runner.act(action).expect("announcement must start");
    let WaitingFor::TargetSelection { target_slots, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected production target selection, got {:?}",
            runner.state().waiting_for
        )
    };
    let legal = target_slots[0]
        .legal_targets
        .iter()
        .filter_map(|t| match t {
            TargetRef::Object(id) => Some(*id),
            TargetRef::Player(_) => None,
        })
        .collect();
    runner.act(GameAction::CancelCast).unwrap();
    legal
}

fn cast_action(runner: &GameRunner, object_id: ObjectId) -> GameAction {
    GameAction::CastSpell {
        object_id,
        card_id: runner.state().objects[&object_id].card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    }
}

fn tap_object(runner: &mut GameRunner, id: ObjectId) {
    runner.state_mut().objects.get_mut(&id).unwrap().tapped = true;
}

/// Spectral Shield (verified Scryfall Oracle): "Enchanted creature gets +0/+2 and
/// can't be the target of spells." Royal Assassin: "{T}: Destroy target tapped
/// creature." CR 115.1a / CR 115.1c: only SPELLS are prohibited, so an activated
/// ability may still target the enchanted creature. The unqualified Shielding Plax
/// ("spells or abilities your opponents control") control still blocks it.
#[test]
fn royal_assassin_can_target_creature_enchanted_by_spectral_shield() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Shielding Plax's ETB draw must not deck P0.
    scenario.with_library_top(P0, &["Forest"]);
    let shielded = scenario.add_creature(P0, "Shield Host", 2, 2).id();
    let plaxed = scenario.add_creature(P0, "Plax Host", 2, 2).id();
    let decoy = scenario.add_creature(P0, "Tapped Decoy", 2, 2).id();
    let assassin = scenario
        .add_creature(P1, "Royal Assassin", 1, 1)
        .from_oracle_text("{T}: Destroy target tapped creature.")
        .id();
    let shield = scenario
        .add_spell_to_hand(P0, "Spectral Shield", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::generic(0))
        .from_oracle_text_with_keywords(&["Enchant"], SPECTRAL_SHIELD)
        .id();
    let plax = scenario
        .add_spell_to_hand(P0, "Shielding Plax", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::generic(0))
        .from_oracle_text_with_keywords(&["Enchant"], SHIELDING_PLAX)
        .id();
    let mut runner = scenario.build();
    runner.cast(shield).target_object(shielded).resolve();
    runner.cast(plax).target_object(plaxed).resolve();
    for id in [shielded, plaxed, decoy] {
        tap_object(&mut runner, id);
    }

    let legal = announced_legal_targets(
        &mut runner,
        P1,
        GameAction::ActivateAbility {
            source_id: assassin,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        },
    );
    assert!(legal.contains(&decoy), "nonempty control: {legal:?}");
    assert!(
        legal.contains(&shielded),
        "a spells-only Aura must not stop an activated ability from targeting: {legal:?}"
    );
    assert!(
        !legal.contains(&plaxed),
        "the unqualified restriction must still block an opponent's targeted ability: {legal:?}"
    );
}

/// Veilstone Amulet (verified Scryfall Oracle): "Whenever you cast a spell,
/// creatures you control can't be the targets of spells or abilities your
/// opponents control this turn."
///
/// CR 611.2c + CR 613.11: the effect modifies the rules, so a creature that
/// enters AFTER the trigger resolves is protected too. It expires at end of turn;
/// P0's own spells are unaffected (CR 109.5).
#[test]
fn veilstone_amulet_protects_creatures_entering_later_and_expires() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let existing = scenario.add_creature(P0, "Existing Bear", 2, 2).id();
    let enemy_a = scenario.add_creature(P1, "Enemy A", 2, 2).id();
    let enemy_b = scenario.add_creature(P1, "Enemy B", 2, 2).id();
    scenario.add_artifact_from_oracle(P0, "Veilstone Amulet", VEILSTONE_AMULET);
    let maker = scenario
        .add_artifact_from_oracle(
            P0,
            "Token Maker",
            "{0}: Create a 2/2 green Bear creature token.",
        )
        .id();
    let trigger_spell = scenario
        .add_spell_to_hand(P0, "Trigger Probe", true)
        .with_mana_cost(ManaCost::generic(0))
        .from_oracle_text("You gain 1 life.")
        .id();
    let p0_tap = scenario
        .add_spell_to_hand(P0, "Own Tap Probe", true)
        .with_mana_cost(ManaCost::generic(0))
        .from_oracle_text("Tap target creature.")
        .id();
    let p1_tap = scenario
        .add_spell_to_hand(P1, "Opposing Tap Probe", true)
        .with_mana_cost(ManaCost::generic(0))
        .from_oracle_text("Tap target creature.")
        .id();
    let mut runner = scenario.build();

    runner.cast(trigger_spell).resolve();
    // The token enters AFTER the Amulet's trigger resolved, via an ability (no cast).
    let before: Vec<_> = runner.state().battlefield.iter().copied().collect();
    runner.activate(maker, 0).resolve();
    let token = *runner
        .state()
        .battlefield
        .iter()
        .find(|id| !before.contains(id))
        .expect("the token must have entered");

    let action = cast_action(&runner, p1_tap);
    let legal = announced_legal_targets(&mut runner, P1, action);
    assert!(
        legal.contains(&enemy_a) && legal.contains(&enemy_b),
        "{legal:?}"
    );
    assert!(
        !legal.contains(&existing),
        "creature already present: {legal:?}"
    );
    assert!(
        !legal.contains(&token),
        "a creature entering after the trigger resolved must be protected (CR 611.2c): {legal:?}"
    );

    // P0's own spell is unaffected.
    let action = cast_action(&runner, p0_tap);
    let own = announced_legal_targets(&mut runner, P0, action);
    assert!(own.contains(&existing) && own.contains(&token), "{own:?}");

    // CR 514.2: the effect ends with the turn. P0 controls creatures, so the walk
    // to the next upkeep declares no attackers when asked.
    for _ in 0..4 {
        runner.advance_to_upkeep();
        if runner.state().phase == Phase::Upkeep {
            break;
        }
        runner
            .declare_attackers(&[])
            .expect("declaring no attackers must be legal");
    }
    assert_eq!(
        runner.state().phase,
        Phase::Upkeep,
        "must reach next upkeep"
    );
    let action = cast_action(&runner, p1_tap);
    let legal = announced_legal_targets(&mut runner, P1, action);
    assert!(
        legal.contains(&existing) && legal.contains(&token),
        "the Amulet's effect must expire: {legal:?}"
    );
}

/// CR 109.2 + CR 109.2b: "creatures you control" in Veilstone Amulet's effect is
/// an unzoned card-type description, so it means creatures on the battlefield. A
/// creature SPELL on the stack is not one, even while the Amulet trigger that
/// cast it resolves: an opponent's "Counter target creature spell" may still
/// target it, and the spell is countered.
#[test]
fn veilstone_amulet_does_not_protect_the_triggering_creature_spell_on_the_stack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P1, ManaColor::Blue);
    scenario.add_basic_land(P1, ManaColor::Blue);
    scenario.add_artifact_from_oracle(P0, "Veilstone Amulet", VEILSTONE_AMULET);
    let bear = scenario
        .add_creature_to_hand(P0, "Grizzly Probe", 2, 2)
        .id();
    let essence_scatter = scenario
        .add_spell_to_hand_from_oracle(
            P1,
            "Essence Scatter",
            true,
            "Counter target creature spell.",
        )
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![engine::types::mana::ManaCostShard::Blue],
        })
        .id();
    let mut runner = scenario.build();

    // P0 casts the creature spell; the Amulet trigger goes on top of it.
    let bear_card = runner.state().objects[&bear].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: bear,
            card_id: bear_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast the creature spell");
    // Resolve the Amulet trigger only; the creature spell stays on the stack.
    for _ in 0..4 {
        if runner.state().stack.len() == 1 {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pass priority");
    }
    assert_eq!(
        runner.state().stack.len(),
        1,
        "only the creature spell remains"
    );
    assert_eq!(runner.state().objects[&bear].zone, Zone::Stack);

    // The Amulet's effect is live, but P1 may still target the creature spell.
    runner.act(GameAction::PassPriority).expect("P0 passes");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1),
        "P1 must hold priority, got {:?}",
        runner.state().waiting_for
    );
    // Production cast: the sole legal target is auto-selected.
    runner
        .act(cast_action(&runner, essence_scatter))
        .expect("Essence Scatter must be castable at the creature spell");
    assert_eq!(
        runner.state().stack.len(),
        2,
        "Essence Scatter is on the stack above the creature spell"
    );
    let scatter_targets = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.id == essence_scatter)
        .and_then(|entry| entry.ability())
        .map(|ability| ability.targets.clone())
        .unwrap_or_default();
    assert_eq!(
        scatter_targets,
        vec![TargetRef::Object(bear)],
        "a creature spell is not a battlefield creature (CR 109.2), so the Amulet's          effect does not make it an illegal target"
    );
    for _ in 0..8 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pass priority");
    }
    assert_eq!(runner.state().objects[&bear].zone, Zone::Graveyard);
    assert!(!runner.state().battlefield.contains(&bear));
}

/// Vines of Vastwood: the restriction is bound to the recipient's incarnation and
/// is not an ability. Turn to Frog ("loses all abilities", cast later) does not
/// clear it; a blink makes a new object (CR 400.7) and ends it; it expires at end
/// of turn.
#[test]
fn vines_survives_ability_removal_ends_on_blink_and_expires() {
    for scenario_kind in ["ability_removal", "blink", "expiry"] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let protected = scenario.add_creature(P1, "Opposing Bear", 2, 2).id();
        let bystander = scenario.add_creature(P1, "Bystander A", 2, 2).id();
        let second = scenario.add_creature(P1, "Bystander B", 2, 2).id();
        let vines = scenario
            .add_spell_to_hand(P0, "Vines of Vastwood", true)
            .with_mana_cost(ManaCost::generic(0))
            .from_oracle_text(VINES)
            .id();
        let frog = scenario
            .add_spell_to_hand(P0, "Turn to Frog", true)
            .with_mana_cost(ManaCost::generic(0))
            .from_oracle_text("Until end of turn, target creature loses all abilities and becomes a blue Frog with base power and toughness 1/1.")
            .id();
        let blink = scenario
            .add_spell_to_hand(P0, "Flicker Probe", true)
            .with_mana_cost(ManaCost::generic(0))
            .from_oracle_text("Exile target creature, then return it to the battlefield under its owner's control.")
            .id();
        let p1_tap = scenario
            .add_spell_to_hand(P1, "Opposing Tap Probe", true)
            .with_mana_cost(ManaCost::generic(0))
            .from_oracle_text("Tap target creature.")
            .id();
        let mut runner = scenario.build();

        runner.cast(vines).target_object(protected).resolve();
        let action = cast_action(&runner, p1_tap);
        let legal = announced_legal_targets(&mut runner, P1, action);
        assert!(
            legal.contains(&bystander) && legal.contains(&second),
            "{legal:?}"
        );
        assert!(
            !legal.contains(&protected),
            "{scenario_kind}: Vines protects: {legal:?}"
        );

        runner.state_mut().active_player = P0;
        runner.state_mut().priority_player = P0;
        runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
        match scenario_kind {
            "ability_removal" => {
                runner.cast(frog).target_object(protected).resolve();
                assert!(
                    runner.state().objects[&protected]
                        .card_types
                        .subtypes
                        .iter()
                        .any(|t| t == "Frog"),
                    "the ability-removal effect must have applied"
                );
                let action = cast_action(&runner, p1_tap);
                let legal = announced_legal_targets(&mut runner, P1, action);
                assert!(
                    !legal.contains(&protected),
                    "later ability removal must not clear Vines' rule effect: {legal:?}"
                );
            }
            "blink" => {
                runner.cast(blink).target_object(protected).resolve();
                let action = cast_action(&runner, p1_tap);
                let legal = announced_legal_targets(&mut runner, P1, action);
                assert!(
                    legal.contains(&protected),
                    "CR 400.7: the blinked creature is a new object, no longer protected: {legal:?}"
                );
            }
            _ => {
                runner.advance_to_upkeep();
                let action = cast_action(&runner, p1_tap);
                let legal = announced_legal_targets(&mut runner, P1, action);
                assert!(legal.contains(&protected), "Vines must expire: {legal:?}");
            }
        }
    }
}
