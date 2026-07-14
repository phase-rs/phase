//! Torch the Tower exiles a permanent that dies after taking its damage.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::Effect;
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
