//! Torch the Tower exiles a permanent that dies after taking its damage.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::zones::move_to_zone;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const TORCH: &str = "Bargain (You may sacrifice an artifact, enchantment, or token as you cast \
this spell.)\nTorch the Tower deals 2 damage to target creature or planeswalker. If this spell \
was bargained, instead it deals 3 damage to that permanent and you scry 1.\nIf a permanent \
dealt damage by Torch the Tower would die this turn, exile it instead.";

#[test]
fn torch_the_tower_parses_a_target_bound_die_exile_rider() {
    let parsed = parse_oracle_text(
        TORCH,
        "Torch the Tower",
        &["Bargain".into()],
        &["Instant".into()],
        &[],
    );
    assert!(
        parsed.replacements.is_empty(),
        "Torch must not inherit a printed self-death replacement: {:?}",
        parsed.replacements
    );
    let mut cursor = Some(&parsed.abilities[0]);
    let mut found = false;
    while let Some(def) = cursor {
        found |= matches!(*def.effect, Effect::AddTargetReplacement { .. });
        cursor = def.sub_ability.as_deref();
    }
    assert!(
        found,
        "Torch must install a replacement on the damaged target"
    );
    let bargain_override = parsed.abilities[0]
        .sub_ability
        .as_ref()
        .expect("Torch must retain its bargain override");
    assert!(
        bargain_override
            .else_ability
            .as_ref()
            .is_some_and(|def| matches!(*def.effect, Effect::AddTargetReplacement { .. })),
        "the unbargained branch must also install the die-exile rider: {bargain_override:#?}"
    );
}

fn cast_torch(bargain: bool, toughness: i32) -> Zone {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::Red);
    scenario.with_library_top(P0, &["Scry Target"]);
    let target = scenario
        .add_creature(P1, "Target Creature", 2, toughness)
        .id();
    let artifact = scenario
        .add_creature(P0, "Artifact", 1, 1)
        .as_artifact()
        .id();
    let spell = scenario
        .add_spell_to_hand(P0, "Torch the Tower", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .from_oracle_text_with_keywords(&["bargain"], TORCH)
        .id();
    let mut runner = scenario.build();
    let cast = runner.cast(spell).target_object(target);
    let outcome = if bargain {
        cast.accept_optional().sacrifice_with(&[artifact]).resolve()
    } else {
        cast.decline_optional().resolve()
    };
    outcome.state().objects[&target].zone
}

#[test]
fn torch_the_tower_exiles_a_lethally_damaged_creature() {
    assert_eq!(cast_torch(false, 2), Zone::Exile);
}

#[test]
fn bargained_torch_the_tower_exiles_a_three_toughness_creature() {
    assert_eq!(cast_torch(true, 3), Zone::Exile);
}

#[test]
fn torch_the_tower_exiles_a_target_that_dies_to_combat_later_that_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::Red);
    let target = scenario.add_creature(P0, "Target Attacker", 2, 3).id();
    let blocker = scenario.add_creature(P1, "Blocking Creature", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand(P0, "Torch the Tower", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .from_oracle_text_with_keywords(&["bargain"], TORCH)
        .id();
    let mut runner = scenario.build();

    runner
        .cast(spell)
        .target_object(target)
        .decline_optional()
        .resolve();
    assert_eq!(runner.state().objects[&target].damage_marked, 2);
    assert_eq!(runner.state().objects[&target].zone, Zone::Battlefield);

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(target, AttackTarget::Player(P1))])
        .expect("the damaged target must be able to attack");
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner.pass_both_players();
    }
    runner
        .declare_blockers(&[(blocker, target)])
        .expect("the blocker must be able to block the damaged target");
    runner.combat_damage();

    assert_eq!(
        runner.state().objects[&target].zone,
        Zone::Exile,
        "Torch's replacement must exile a damaged permanent that dies later in combat"
    );
}

/// CR 514.2 + CR 700.4 + CR 701.8a: a generated-database Torch installs the
/// target-bound replacement for this turn, then that same rider expires at
/// cleanup.
#[test]
fn torch_the_tower_replacement_expires_at_cleanup_through_production_actions() {
    let db = crate::support::shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["P0 cleanup fixture"; 12]);
    scenario.with_library_top(P1, &["P1 cleanup fixture"; 12]);
    let hit_before_cleanup = scenario.add_creature(P1, "Torch Hit Before", 2, 4).id();
    let hit_after_cleanup = scenario.add_creature(P1, "Torch Hit After", 2, 4).id();
    let torch_before_cleanup = scenario.add_real_card(P0, "Torch the Tower", Zone::Hand, db);
    let torch_after_cleanup = scenario.add_real_card(P0, "Torch the Tower", Zone::Hand, db);
    let murder_before_cleanup = scenario.add_real_card(P0, "Murder", Zone::Hand, db);
    let murder_after = scenario.add_real_card(P0, "Murder", Zone::Hand, db);
    scenario.with_mana_pool(
        P0,
        [
            ManaType::Red,
            ManaType::Red,
            ManaType::Black,
            ManaType::Black,
            ManaType::Black,
            ManaType::Black,
            ManaType::Colorless,
            ManaType::Colorless,
        ]
        .into_iter()
        .map(|mana| ManaUnit::new(mana, ObjectId(0), false, vec![]))
        .collect(),
    );
    let mut runner = scenario.build();
    for (torch, host) in [
        (torch_before_cleanup, hit_before_cleanup),
        (torch_after_cleanup, hit_after_cleanup),
    ] {
        let outcome = runner
            .cast(torch)
            .target_object(host)
            .decline_optional()
            .resolve();
        assert!(
            outcome.events().iter().any(|event| matches!(
                event,
                GameEvent::DamageDealt {
                    source_id,
                    target: engine::types::ability::TargetRef::Object(target),
                    amount: 2,
                    is_combat: false,
                    ..
                } if *source_id == torch && *target == host
            )),
            "reach guard: generated Torch {torch:?} must deal 2 damage to host {host:?}"
        );
        assert_eq!(runner.state().objects[&host].zone, Zone::Battlefield);
        assert_eq!(runner.state().objects[&host].damage_marked, 2);
    }
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));

    let before_cleanup = runner
        .cast(murder_before_cleanup)
        .target_object(hit_before_cleanup)
        .resolve();
    assert!(before_cleanup.events().iter().any(|event| matches!(
        event,
        GameEvent::ZoneChanged {
            object_id,
            to: Zone::Exile,
            ..
        } if *object_id == hit_before_cleanup
    )));
    assert_eq!(
        runner.state().objects[&hit_before_cleanup].zone,
        Zone::Exile
    );
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));

    let ending_turn = runner.state().turn_number;
    runner.advance_to_combat();
    runner
        .declare_attackers(&[])
        .expect("P0 declares no attackers before cleanup");
    runner.advance_to_end_step();
    assert_eq!(runner.state().phase, Phase::End);
    runner
        .act(GameAction::PassPriority)
        .expect("the active player passes the End step priority window");
    runner
        .act(GameAction::PassPriority)
        .expect("both players pass and the real cleanup step executes");
    assert!(
        runner.state().turn_number > ending_turn,
        "the End-step passes must cross the real cleanup boundary"
    );
    assert_eq!(
        runner.state().objects[&hit_after_cleanup].zone,
        Zone::Battlefield
    );
    assert_eq!(runner.state().objects[&hit_after_cleanup].damage_marked, 0);

    // The next player's priority passes to P0 so the second real Murder tests
    // the other hit host after CR 514.2 ends its turn-bound replacement.
    runner.advance_to_phase(Phase::PreCombatMain);
    if !matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ) {
        runner
            .act(GameAction::PassPriority)
            .expect("the active player passes so P0 receives priority");
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    // CR 106.4: Mana pools empty at the end of every step and phase. Re-seed
    // the Murder cost after crossing cleanup before testing the expired rider.
    for mana in [ManaType::Black, ManaType::Black, ManaType::Colorless] {
        runner
            .state_mut()
            .players
            .iter_mut()
            .find(|player| player.id == P0)
            .unwrap()
            .mana_pool
            .add(ManaUnit::new(mana, ObjectId(0), false, vec![]));
    }
    let hit_outcome = runner
        .cast(murder_after)
        .target_object(hit_after_cleanup)
        .resolve();
    assert!(hit_outcome.events().iter().any(|event| matches!(
        event,
        GameEvent::ZoneChanged {
            object_id,
            to: Zone::Graveyard,
            ..
        } if *object_id == hit_after_cleanup
    )));
    assert_eq!(
        runner.state().objects[&hit_after_cleanup].zone,
        Zone::Graveyard
    );
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn torch_the_tower_replacement_is_not_inherited_by_a_copy() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::Red);
    let target = scenario.add_creature(P0, "Surviving Target", 2, 4).id();
    let torch = scenario
        .add_spell_to_hand(P0, "Torch the Tower", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .from_oracle_text_with_keywords(&["bargain"], TORCH)
        .id();
    let copy = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic Copier",
            true,
            "Create a token that's a copy of target creature you control.",
        )
        .id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Synthetic Destroy", true, "Destroy target creature.")
        .id();
    let mut runner = scenario.build();

    runner
        .cast(torch)
        .target_object(target)
        .decline_optional()
        .resolve();
    runner.cast(copy).target_object(target).resolve();
    let copy_id = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| {
            runner.state().objects[id].is_token
                && runner.state().objects[id].name == "Surviving Target"
        })
        .expect("the copy spell must create a copy of the damaged target");

    let outcome = runner.cast(destroy).target_object(copy_id).resolve();
    assert!(
        outcome.events().iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged { object_id, to: Zone::Graveyard, .. } if *object_id == copy_id
        )),
        "CR 707.2: a copy of the damaged creature was never dealt damage by Torch and must die normally"
    );
}

#[test]
fn torch_the_tower_replacement_does_not_follow_a_bounced_creature_back_onto_the_battlefield() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::Red);
    let target = scenario.add_creature(P0, "Surviving Target", 2, 4).id();
    let torch = scenario
        .add_spell_to_hand(P0, "Torch the Tower", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .from_oracle_text_with_keywords(&["bargain"], TORCH)
        .id();
    let bounce = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic Bounce",
            true,
            "Return target creature to its owner's hand.",
        )
        .id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Synthetic Destroy", true, "Destroy target creature.")
        .id();
    let mut runner = scenario.build();

    runner
        .cast(torch)
        .target_object(target)
        .decline_optional()
        .resolve();
    runner.cast(bounce).target_object(target).resolve();
    assert_eq!(runner.state().objects[&target].zone, Zone::Hand);
    move_to_zone(
        runner.state_mut(),
        target,
        Zone::Battlefield,
        &mut Vec::new(),
    );
    assert_eq!(runner.state().objects[&target].zone, Zone::Battlefield);

    runner.cast(destroy).target_object(target).resolve();
    assert_eq!(
        runner.state().objects[&target].zone,
        Zone::Graveyard,
        "CR 400.7: the re-entered creature is a new object and was never dealt damage by Torch"
    );
}
