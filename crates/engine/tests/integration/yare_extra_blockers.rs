//! Yare — "That creature can block up to two additional creatures this turn."
//!
//! Parser regression: the optional "up to" prefix must lower to
//! `ExtraBlockers { count: Some(2) }`, not `Effect::Unimplemented`.
//!
//! Runtime regression: a blocker granted that static can legally block three
//! attackers (default 1 + 2 additional).

use engine::game::combat::{validate_blockers, AttackerInfo, CombatState};
use engine::game::zones::create_object;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityKind, Effect, StaticDefinition};
use engine::types::card_type::CoreType;
use engine::types::format::FormatConfig;
use engine::types::game_state::GameState;
use engine::types::identifiers::CardId;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

const YARE_ORACLE: &str = "Target creature defending player controls gets +3/+0 until end of turn. That creature can block up to two additional creatures this turn.";

fn create_creature(
    state: &mut GameState,
    controller: PlayerId,
    name: &str,
    power: i32,
    toughness: i32,
) -> engine::types::identifiers::ObjectId {
    let id = create_object(
        state,
        CardId(state.next_object_id),
        controller,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types = vec![CoreType::Creature];
    obj.base_card_types = obj.card_types.clone();
    obj.power = Some(power);
    obj.toughness = Some(toughness);
    obj.base_power = Some(power);
    obj.base_toughness = Some(toughness);
    obj.summoning_sick = false;
    id
}

#[test]
fn yare_parses_up_to_two_additional_blockers_sub_ability() {
    let def = parse_effect_chain(YARE_ORACLE, AbilityKind::Spell);
    let sub = def
        .sub_ability
        .expect("Yare must parse a supported extra-block sub-ability");
    let Effect::GenericEffect {
        static_abilities,
        duration,
        ..
    } = &*sub.effect
    else {
        panic!("expected GenericEffect, got {:?}", sub.effect);
    };
    assert_eq!(
        duration,
        &Some(engine::types::ability::Duration::UntilEndOfTurn)
    );
    assert_eq!(
        static_abilities[0].mode,
        StaticMode::ExtraBlockers { count: Some(2) }
    );
}

#[test]
fn yare_extra_blockers_allow_three_attackers_blocked() {
    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    let attacker1 = create_creature(&mut state, PlayerId(0), "Attacker A", 2, 2);
    let attacker2 = create_creature(&mut state, PlayerId(0), "Attacker B", 2, 2);
    let attacker3 = create_creature(&mut state, PlayerId(0), "Attacker C", 2, 2);
    let blocker = create_creature(&mut state, PlayerId(1), "Defender", 2, 2);

    state
        .objects
        .get_mut(&blocker)
        .unwrap()
        .static_definitions
        .push(StaticDefinition::new(StaticMode::ExtraBlockers {
            count: Some(2),
        }));

    state.combat = Some(CombatState {
        attackers: vec![
            AttackerInfo::attacking_player(attacker1, PlayerId(1)),
            AttackerInfo::attacking_player(attacker2, PlayerId(1)),
            AttackerInfo::attacking_player(attacker3, PlayerId(1)),
        ],
        ..Default::default()
    });

    assert!(
        validate_blockers(
            &state,
            &[
                (blocker, attacker1),
                (blocker, attacker2),
                (blocker, attacker3),
            ],
        )
        .is_ok(),
        "ExtraBlockers(2) must allow blocking three attackers"
    );
}
