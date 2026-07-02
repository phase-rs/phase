//! Issue #4786: Wrenn and Realmbreaker's -7 emblem must grant graveyard play/cast.

use engine::ai_support::legal_actions;
use engine::game::casting::{can_cast_object_now, graveyard_lands_playable_by_permission};
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::zones::create_object;
use engine::types::ability::{AbilityCost, Effect, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::StackEntryKind;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::statics::{CastFrequency, StaticMode};
use engine::types::zones::Zone;

const WRENN_ORACLE: &str = "Lands you control have \"{T}: Add one mana of any color.\"\n\
+1: Up to one target land you control becomes a 3/3 Elemental creature with vigilance, hexproof, and haste until your next turn. It's still a land.\n\
−2: Mill three cards. You may put a permanent card from among the milled cards into your hand.\n\
−7: You get an emblem with \"You may play lands and cast permanent spells from your graveyard.\"";

fn activate_wrenn_minus_seven_emblem(runner: &mut GameRunner, wrenn: ObjectId) -> ObjectId {
    {
        let wrenn_obj = runner.state_mut().objects.get_mut(&wrenn).unwrap();
        wrenn_obj.card_types.core_types = vec![CoreType::Planeswalker];
        wrenn_obj.loyalty = Some(10);
    }

    let minus_seven_index = runner.state().objects[&wrenn]
        .abilities
        .iter()
        .position(|ability| {
            matches!(
                ability.cost.as_ref(),
                Some(AbilityCost::Loyalty { amount: -7 })
            ) && matches!(ability.effect.as_ref(), Effect::CreateEmblem { .. })
        })
        .expect("Wrenn must expose a -7 CreateEmblem loyalty ability");

    runner.activate(wrenn, minus_seven_index).resolve();

    assert_eq!(
        runner.state().command_zone.len(),
        1,
        "activating -7 must create an emblem in the command zone"
    );
    let emblem_id = runner.state().command_zone[0];
    let emblem = &runner.state().objects[&emblem_id];
    assert!(emblem.is_emblem);

    let static_def = &emblem.static_definitions[0];
    assert!(
        matches!(
            static_def.mode,
            StaticMode::GraveyardCastPermission {
                frequency: CastFrequency::Unlimited,
                ..
            }
        ),
        "Wrenn emblem must install a graveyard play/cast permission static"
    );
    assert!(
        static_def.active_zones.contains(&Zone::Command),
        "emblem permission static must function from the command zone"
    );
    match &static_def.affected {
        Some(TargetFilter::Or { filters }) => assert_eq!(filters.len(), 2),
        other => panic!("expected combined land + permanent filter, got {other:?}"),
    }

    emblem_id
}

#[test]
fn wrenn_minus_seven_emblem_grants_graveyard_land_play() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wrenn = scenario
        .add_creature(P0, "Wrenn and Realmbreaker", 0, 0)
        .from_oracle_text(WRENN_ORACLE)
        .id();

    let mut runner = scenario.build();
    activate_wrenn_minus_seven_emblem(&mut runner, wrenn);

    let forest = create_object(
        runner.state_mut(),
        CardId(9001),
        P0,
        "Forest".to_string(),
        Zone::Graveyard,
    );
    {
        let obj = runner.state_mut().objects.get_mut(&forest).unwrap();
        obj.card_types.core_types = vec![CoreType::Land];
    }

    let playable = graveyard_lands_playable_by_permission(runner.state(), P0);
    assert!(
        playable.iter().any(|(id, _)| *id == forest),
        "Forest in graveyard must be playable with Wrenn emblem active"
    );
}

#[test]
fn wrenn_minus_seven_emblem_grants_graveyard_permanent_spell_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wrenn = scenario
        .add_creature(P0, "Wrenn and Realmbreaker", 0, 0)
        .from_oracle_text(WRENN_ORACLE)
        .id();
    let graveyard_creature = scenario
        .add_creature_to_graveyard(P0, "Graveyard Bear", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    activate_wrenn_minus_seven_emblem(&mut runner, wrenn);

    assert!(
        can_cast_object_now(runner.state(), P0, graveyard_creature),
        "nonland permanent in graveyard must be castable with Wrenn emblem active"
    );
    assert!(
        legal_actions(runner.state()).iter().any(|action| matches!(
            action,
            GameAction::CastSpell { object_id, .. } if *object_id == graveyard_creature
        )),
        "legal action generation must offer casting the graveyard permanent"
    );

    runner.cast(graveyard_creature).commit();

    let entry = runner
        .state()
        .stack
        .last()
        .expect("creature spell on stack");
    match &entry.kind {
        StackEntryKind::Spell { .. } => {}
        other => panic!("expected Spell on stack, got {other:?}"),
    }
    assert_eq!(
        runner.state().objects[&graveyard_creature].zone,
        Zone::Stack,
        "graveyard creature must leave the graveyard when cast through emblem permission"
    );
}
