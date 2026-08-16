//! Torch the Tower exiles a permanent that dies after taking its damage.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::move_to_zone;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::Effect;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
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

#[test]
fn torch_the_tower_replacement_expires_from_the_layer_baseline_at_cleanup() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::Red);
    let target = scenario.add_creature(P1, "Surviving Target", 2, 4).id();
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

    assert!(
        !runner.state().objects[&target]
            .base_replacement_definitions
            .is_empty(),
        "the turn-bound replacement must survive layer recalculation"
    );
    let mut events = Vec::new();
    engine::game::turns::execute_cleanup(runner.state_mut(), &mut events);
    assert!(
        runner.state().objects[&target]
            .base_replacement_definitions
            .is_empty(),
        "the turn-bound replacement must not survive cleanup"
    );
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
