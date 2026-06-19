//! Regression for GitHub issue #3310 — Condemn requires two targets instead of one.
//!
//! Oracle: "Put target attacking or blocking creature on the bottom of its owner's
//! library. Its controller gains 7 life."

use engine::game::ability_utils::{build_resolved_from_def, build_target_slots};
use engine::game::combat::{AttackTarget, AttackerInfo};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityKind, Effect, TargetFilter, TargetRef};
use engine::types::player::PlayerId;

const CONDEMN_ORACLE: &str =
    "Put target attacking or blocking creature on the bottom of its owner's library. Its controller gains 7 life.";

#[test]
fn condemn_parses_put_bottom_then_controller_gains_life() {
    let def = parse_effect_chain(CONDEMN_ORACLE, AbilityKind::Spell);
    if let Effect::PutAtLibraryPosition { target, .. } = &*def.effect {
        let TargetFilter::Or { filters } = target else {
            panic!("expected Or(attacking|blocking) target filter, got {target:?}");
        };
        assert_eq!(filters.len(), 2);
    } else {
        panic!(
            "primary effect should be PutAtLibraryPosition, got {:?}",
            def.effect
        );
    }
    let sub = def
        .sub_ability
        .as_ref()
        .expect("should chain controller life gain as sub_ability");
    assert!(
        matches!(
            *sub.effect,
            Effect::GainLife {
                player: TargetFilter::ParentTargetController,
                ..
            }
        ),
        "sub_ability should be ParentTargetController GainLife, got {:?}",
        sub.effect
    );
    assert!(
        def.multi_target.is_none(),
        "Condemn must not declare multi-target; got {:?}",
        def.multi_target
    );
}

#[test]
fn condemn_spell_ability_matches_parsed_chain() {
    let mut scenario = GameScenario::new();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Condemn", true, CONDEMN_ORACLE)
        .id();
    let runner = scenario.build();
    let obj = &runner.state().objects[&spell];
    assert_eq!(
        obj.abilities.len(),
        1,
        "Condemn must parse as one spell ability, not split across sentences"
    );
    let ability = &obj.abilities[0];
    assert!(
        matches!(*ability.effect, Effect::PutAtLibraryPosition { .. }),
        "spell ability should be PutAtLibraryPosition"
    );
}

#[test]
fn condemn_multiline_oracle_merges_into_one_ability() {
    let multiline = "Put target attacking or blocking creature on the bottom of its owner's library.\nIts controller gains 7 life.";
    let mut scenario = GameScenario::new();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Condemn", true, multiline)
        .id();
    let runner = scenario.build();
    let obj = &runner.state().objects[&spell];
    assert_eq!(
        obj.abilities.len(),
        1,
        "newline-separated rider must merge into one spell ability (issue #3310)"
    );
    let ability = &obj.abilities[0];
    assert!(
        matches!(*ability.effect, Effect::PutAtLibraryPosition { .. }),
        "merged ability head must be PutAtLibraryPosition"
    );
    let sub = ability
        .sub_ability
        .as_ref()
        .expect("life-gain rider must chain as sub_ability");
    assert!(
        matches!(
            *sub.effect,
            Effect::GainLife {
                player: TargetFilter::ParentTargetController,
                ..
            }
        ),
        "life-gain rider must use ParentTargetController, got {:?}",
        sub.effect
    );
}

#[test]
fn condemn_builds_one_target_slot_for_attacking_creature() {
    let mut scenario = GameScenario::new();
    let attacker = scenario.add_creature(P1, "Attacker", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Condemn", true, CONDEMN_ORACLE)
        .id();

    let mut runner = scenario.build();
    let combat = runner
        .state_mut()
        .combat
        .get_or_insert_with(Default::default);
    combat.attackers.push(AttackerInfo::new(
        attacker,
        AttackTarget::Player(PlayerId(0)),
        PlayerId(0),
    ));

    let ability = runner.state().objects[&spell].abilities[0].clone();
    let resolved = build_resolved_from_def(&ability, spell, P0);
    let slots = build_target_slots(runner.state(), &resolved).expect("target slots");
    assert_eq!(
        slots.len(),
        1,
        "Condemn must build exactly one target slot (issue #3310), got {}",
        slots.len()
    );
    assert!(
        slots[0]
            .legal_targets
            .contains(&TargetRef::Object(attacker)),
        "the attacking creature must be legal"
    );
}
