//! CR 401.5 + CR 601.2a + CR 305.1 + CR 609.4b: Foreign library look, play and payment terms.

use engine::ai_support::legal_actions;
use engine::game::casting::spell_objects_available_to_cast;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::visibility::{filter_state_for_viewer, HIDDEN_CARD_NAME};
use engine::types::ability::{
    CardPlayMode, ContinuousModification, ControllerRef, Duration, ManaSpendPermission,
    StaticDefinition, TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
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
    install_foreign_library_grant(
        runner.state_mut(),
        source,
        P1,
        StaticMode::MayLookAtTopOfLibrary {
            library: ControllerRef::SpecificPlayer { id: P1 },
        },
    );
    let assert_private_top = |state: &GameState, top| {
        assert_eq!(state.library_of(P1).front(), Some(&top));
        assert_eq!(
            filter_state_for_viewer(state, P0).objects[&top].name,
            state.objects[&top].name
        );
        assert_eq!(
            filter_state_for_viewer(state, P1).objects[&top].name,
            HIDDEN_CARD_NAME
        );
    };
    let land_card_id = runner.state().objects[&land].card_id;
    assert_private_top(runner.state(), land);
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
    assert_private_top(runner.state(), blue);
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
    assert_private_top(runner.state(), colorless);
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

#[test]
fn library_cast_elects_only_spell_faces_admitted_by_its_grant() {
    let db = crate::support::shared_card_db().expect("integration card fixture");
    let planeswalkers = TargetFilter::Typed(TypedFilter::new(TypeFilter::Planeswalker));
    for filter in [None, Some(TargetFilter::Any), Some(planeswalkers)] {
        let has_permission = filter.is_some();
        let front_granted = filter == Some(TargetFilter::Any);
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        if front_granted {
            scenario.add_real_card(P0, "Future Sight", Zone::Battlefield, db);
        } else if let Some(filter) = filter {
            scenario
                .add_creature(P0, "Planeswalker cast grant", 1, 1)
                .with_static_definition(
                    StaticDefinition::new(StaticMode::TopOfLibraryCastPermission {
                        library: ControllerRef::You,
                        play_mode: CardPlayMode::Cast,
                        frequency: CastFrequency::Unlimited,
                        alt_cost: None,
                        mana_spend_permission: None,
                    })
                    .affected(filter),
                );
        }
        let valki = scenario.add_real_card(P0, "Valki, God of Lies", Zone::Library, db);
        let mut runner = scenario.build();
        add_mana(runner.state_mut(), P0, ManaType::Black, 1);
        add_mana(runner.state_mut(), P0, ManaType::Red, 6);
        let card_id = runner.state().objects[&valki].card_id;
        let announce = GameAction::CastSpell {
            object_id: valki,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        };
        assert_eq!(
            legal_actions(runner.state()).contains(&announce),
            has_permission
        );
        let result = runner.act(announce);
        if !has_permission {
            assert!(result.is_err());
            assert_eq!(runner.state().library_of(P0).front(), Some(&valki));
            continue;
        }
        assert!(matches!(
            result.expect("announce from library").waiting_for,
            WaitingFor::ModalFaceChoice { player: P0, object_id, .. } if object_id == valki
        ));
        let choose_front = GameAction::ChooseModalFace { back_face: false };
        let choose_back = GameAction::ChooseModalFace { back_face: true };
        let actions = legal_actions(runner.state());
        assert_eq!(actions.contains(&choose_front), front_granted);
        assert!(actions.contains(&choose_back));
        if !front_granted {
            assert!(runner.act(choose_front).is_err());
            assert_eq!(runner.state().objects[&valki].name, "Valki, God of Lies");
        }
        runner
            .act(choose_back)
            .expect("cast Tibalt from the library");
        let spell = runner.state().stack.back().expect("Tibalt on the stack");
        assert_eq!(
            runner.state().objects[&spell.id].name,
            "Tibalt, Cosmic Impostor"
        );
        assert!(runner.state().library_of(P0).is_empty());
    }
}

#[test]
fn library_cast_offers_an_affordable_back_face_when_the_front_cost_is_unpayable() {
    let db = crate::support::shared_card_db().expect("integration card fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Future Sight", Zone::Battlefield, db);
    let halvar = scenario.add_real_card(P0, "Halvar, God of Battle", Zone::Library, db);
    let mut runner = scenario.build();
    add_mana(runner.state_mut(), P0, ManaType::White, 1);
    add_mana(runner.state_mut(), P0, ManaType::Red, 1);
    let card_id = runner.state().objects[&halvar].card_id;
    let announce = GameAction::CastSpell {
        object_id: halvar,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    };
    assert!(legal_actions(runner.state()).contains(&announce));
    assert!(matches!(
        runner
            .act(announce)
            .expect("announce from library")
            .waiting_for,
        WaitingFor::ModalFaceChoice { .. }
    ));
    runner
        .act(GameAction::ChooseModalFace { back_face: true })
        .expect("cast Sword of the Realms for {1}{W}");
    let spell = runner
        .state()
        .stack
        .back()
        .expect("Sword of the Realms on the stack");
    assert_eq!(
        runner.state().objects[&spell.id].name,
        "Sword of the Realms"
    );
}
