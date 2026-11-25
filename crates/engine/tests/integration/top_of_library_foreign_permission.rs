//! CR 401.5 + CR 601.2a + CR 305.1 + CR 609.4b: Foreign library look, play and payment terms.

use engine::ai_support::legal_actions;
use engine::game::casting::spell_objects_available_to_cast;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{
    CardPlayMode, ContinuousModification, ControllerRef, Duration, ManaSpendPermission,
    TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::{CastFrequency, StaticMode};
use engine::types::zones::Zone;

fn install_foreign_library_grant(
    state: &mut GameState,
    source: ObjectId,
    owner: PlayerId,
    mode: StaticMode,
) {
    assert_ne!(P0, owner);
    // Exercise the journaled TCE installer; Phase 2 supplies the Oracle
    // clause and resolves its parent target into this concrete owner.
    state
        .add_transient_continuous_effect(
            source,
            P0,
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificPlayer { id: P0 },
            vec![ContinuousModification::AddStaticMode { mode }],
            None,
        )
        .expect("player-scoped library grant installed");
}

fn add_mana(state: &mut GameState, player: PlayerId, mana: ManaType, count: usize) {
    for _ in 0..count {
        state
            .add_mana_to_pool(player, ManaUnit::new(mana, ObjectId(0), false, vec![]))
            .expect("mana seeded");
    }
}

fn mono_cost(shard: ManaCostShard) -> ManaCost {
    ManaCost::Cost {
        shards: vec![shard],
        generic: 0,
    }
}

#[test]
fn foreign_top_plays_land_and_repeatedly_casts_only_with_its_own_spend_rider() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Future Sight", 1, 1)
        .from_oracle_text("Play with the top card of your library revealed.\nYou may play lands and cast spells from the top of your library.")
        .id();
    let own = scenario
        .add_spell_to_library_top(P0, "Own blue spell", true)
        .with_mana_cost(mono_cost(ManaCostShard::Blue))
        .id();
    let colorless = scenario
        .add_spell_to_library_top(P1, "Foreign colorless spell", true)
        .with_mana_cost(mono_cost(ManaCostShard::Colorless))
        .id();
    let blue = scenario
        .add_spell_to_library_top(P1, "Foreign blue spell", true)
        .with_mana_cost(mono_cost(ManaCostShard::Blue))
        .id();
    let land = scenario.add_land_to_library_top(P1, "Foreign land").id();
    let mut runner = scenario.build();
    install_foreign_library_grant(
        runner.state_mut(),
        source,
        P1,
        StaticMode::TopOfLibraryCastPermission {
            library: ControllerRef::SpecificPlayer { id: P1 },
            play_mode: CardPlayMode::Play,
            frequency: CastFrequency::Unlimited,
            alt_cost: None,
            mana_spend_permission: Some(ManaSpendPermission::AnyColor),
        },
    );
    let land_card_id = runner.state().objects[&land].card_id;
    assert_eq!(runner.state().library_of(P1).front(), Some(&land));
    assert!(legal_actions(runner.state()).iter().any(|action| {
        matches!(action, GameAction::PlayLand { object_id, .. } if *object_id == land)
    }));
    let played = runner
        .act(GameAction::PlayLand {
            object_id: land,
            card_id: land_card_id,
        })
        .expect("foreign land is playable");
    assert!(played.events.iter().any(|event| matches!(
        event,
        GameEvent::LandPlayed {
            player_id: P0,
            from_zone: Zone::Library,
            ..
        }
    )));
    assert!(runner.state().battlefield.iter().any(|id| {
        let obj = &runner.state().objects[id];
        obj.card_id == land_card_id && obj.owner == P1 && obj.controller == P0
    }));

    assert_eq!(
        runner.state().players[P0.0 as usize].lands_played_this_turn,
        1
    );
    assert_eq!(
        runner.state().players[P1.0 as usize].lands_played_this_turn,
        0
    );

    let available = spell_objects_available_to_cast(runner.state(), P0);
    assert!(available.contains(&own) && available.contains(&blue));
    assert!(!available.contains(&colorless));
    assert_eq!(runner.state().library_of(P1).front(), Some(&blue));
    assert!(runner.state().exile.is_empty());

    add_mana(runner.state_mut(), P0, ManaType::Red, 2);
    assert!(
        runner.cast(own).try_resolve().is_err(),
        "Future Sight has no rider"
    );
    let cast = runner
        .cast(blue)
        .try_resolve()
        .expect("red mana pays the foreign blue spell");
    assert!(cast.events().iter().any(|event| matches!(
        event,
        GameEvent::ZoneChanged {
            from: Some(Zone::Library),
            to: Zone::Stack,
            ..
        }
    )));
    assert!(!cast.events().iter().any(|event| matches!(
        event,
        GameEvent::ZoneChanged {
            to: Zone::Exile,
            ..
        }
    )));
    assert_eq!(runner.state().library_of(P1).front(), Some(&colorless));
    assert!(
        runner.cast(colorless).try_resolve().is_err(),
        "colored mana cannot pay {{C}}"
    );
    add_mana(runner.state_mut(), P0, ManaType::Colorless, 1);
    runner
        .cast(colorless)
        .try_resolve()
        .expect("colorless mana pays {C}");
    assert!(runner.state().exile.is_empty());
    assert!(runner.state().library_of(P1).is_empty());

    add_mana(runner.state_mut(), P0, ManaType::Blue, 1);
    runner
        .cast(own)
        .try_resolve()
        .expect("Future Sight remains usable");
}
