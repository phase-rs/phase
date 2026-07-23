//! Regression for issue #5326: Avatar Aang must transform after performing all
//! four bend types in the same turn.
//!
//! https://github.com/phase-rs/phase/issues/5326

use engine::game::effects::register_bending::resolve as resolve_register_bending;
use engine::game::game_object::BackFaceData;
use engine::game::scenario::{GameScenario, P0};
use engine::game::triggers::process_triggers;
use engine::parser::parse_oracle_text;
use engine::types::ability::{AbilityKind, Effect, ResolvedAbility};
use engine::types::actions::GameAction;
use engine::types::card_type::{CardType, CoreType};
use engine::types::events::BendingType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::triggers::TriggerMode;

const AVATAR_AANG_ORACLE: &str = "Flying, firebending 2\nWhenever you waterbend, earthbend, \
     firebend, or airbend, draw a card. Then if you've done all four this turn, transform \
     Avatar Aang.";

fn drain_to_priority(runner: &mut engine::game::scenario::GameRunner) {
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(
            guard < 128,
            "drain exceeded bound: {:?}, stack={}",
            runner.state().waiting_for,
            runner.state().stack.len()
        );
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            _ => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
        }
    }
}

fn register_bend(
    runner: &mut engine::game::scenario::GameRunner,
    kind: BendingType,
    source: ObjectId,
) {
    let ability = ResolvedAbility::new(Effect::RegisterBending { kind }, vec![], source, P0)
        .kind(AbilityKind::Spell);
    let mut events = Vec::new();
    resolve_register_bending(runner.state_mut(), &ability, &mut events).expect("register bend");
    process_triggers(runner.state_mut(), &events);
    drain_to_priority(runner);
}

fn attach_aang_back_face(runner: &mut engine::game::scenario::GameRunner, aang: ObjectId) {
    let obj = runner.state_mut().objects.get_mut(&aang).unwrap();
    obj.back_face = Some(BackFaceData {
        name: "Avatar Aang, Master of Elements".to_string(),
        power: Some(6),
        toughness: Some(6),
        loyalty: None,
        defense: None,
        card_types: CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Avatar".to_string()],
        },
        mana_cost: ManaCost::default(),
        keywords: vec![],
        abilities: vec![],
        trigger_definitions: Default::default(),
        replacement_definitions: Default::default(),
        static_definitions: Default::default(),
        color: vec![],
        printed_ref: None,
        modal: None,
        additional_cost: None,
        strive_cost: None,
        casting_restrictions: vec![],
        casting_options: vec![],
        layout_kind: None,
    });
}

fn seed_aang(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature(P0, "Avatar Aang", 4, 4)
        .from_oracle_text_with_keywords(&["Flying", "firebending"], AVATAR_AANG_ORACLE)
        .id()
}

#[test]
fn avatar_aang_parses_bend_trigger_with_transform_gate() {
    let parsed = parse_oracle_text(
        AVATAR_AANG_ORACLE,
        "Avatar Aang",
        &["Flying".to_string(), "firebending".to_string()],
        &["Creature".to_string()],
        &["Avatar".to_string()],
    );
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::ElementalBend)
        .expect("ElementalBend trigger");
    let draw = trigger.execute.as_ref().expect("execute");
    let transform = draw.sub_ability.as_ref().expect("transform sub-ability");
    assert!(
        matches!(draw.effect.as_ref(), Effect::Draw { .. }),
        "first effect must draw"
    );
    assert!(
        matches!(transform.effect.as_ref(), Effect::Transform { .. }),
        "second effect must transform"
    );
    assert!(
        transform.condition.is_some(),
        "transform must be gated by intervening-if (all four bends)"
    );
}

#[test]
fn avatar_aang_transforms_after_fourth_bend_in_same_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    let aang = seed_aang(&mut scenario);
    // One library card per bend trigger draw.
    for i in 0..4 {
        scenario.add_card_to_library_top(P0, &format!("Lib {i}"));
    }
    let mut runner = scenario.build();
    attach_aang_back_face(&mut runner, aang);
    assert!(!runner.state().objects[&aang].transformed);

    for kind in [
        BendingType::Water,
        BendingType::Earth,
        BendingType::Fire,
        BendingType::Air,
    ] {
        register_bend(&mut runner, kind, aang);
    }

    assert!(
        runner.state().objects[&aang].transformed,
        "Avatar Aang must transform after all four bend types in one turn"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .bending_types_this_turn
            .len(),
        4
    );
}

#[test]
fn avatar_aang_does_not_transform_after_only_three_bends() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    let aang = seed_aang(&mut scenario);
    for i in 0..3 {
        scenario.add_card_to_library_top(P0, &format!("Lib {i}"));
    }
    let mut runner = scenario.build();
    attach_aang_back_face(&mut runner, aang);

    for kind in [BendingType::Water, BendingType::Earth, BendingType::Fire] {
        register_bend(&mut runner, kind, aang);
    }

    assert!(
        !runner.state().objects[&aang].transformed,
        "three bend types must not satisfy the all-four gate"
    );
}
