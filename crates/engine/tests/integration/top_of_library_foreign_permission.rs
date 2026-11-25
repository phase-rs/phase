//! CR 401.5 + CR 601.2a + CR 305.1 + CR 609.4b: Foreign library look, play and payment terms.

use engine::ai_support::legal_actions;
use engine::game::casting::spell_objects_available_to_cast;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::visibility::{filter_state_for_viewer, HIDDEN_CARD_NAME};
use engine::types::ability::{
    CardPlayMode, ContinuousModification, ControllerRef, Duration, ManaSpendPermission,
    TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, GameState, ManaChoice, WaitingFor};
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

#[test]
fn casting_from_hand_defers_a_new_foreign_top_until_commit() {
    let third = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 41);
    scenario.at_phase(Phase::PreCombatMain);
    // CR 605.1a + CR 614.6: Grove qualifies as a mana ability. The external
    // replacement makes its opponent life gain draw a card during payment.
    let grove = scenario
        .add_land_from_oracle(
            P0,
            "Grove of the Burnwillows",
            "{T}: Add {C}.\n{T}: Add {R} or {G}. Each opponent gains 1 life.",
        )
        .id();
    scenario.add_enchantment_from_oracle(P1, "Nefarious Lich",
        "If damage would be dealt to you, exile that many cards from your graveyard instead. If you can't, you lose the game.\nIf you would gain life, draw that many cards instead.\nWhen this enchantment leaves the battlefield, you lose the game."
    );
    let hand = scenario
        .add_spell_to_hand(P0, "Hand spell", true)
        .with_mana_cost(mono_cost(ManaCostShard::Green))
        .id();
    let next = scenario.add_spell_to_library_top(P1, "New top", true).id();
    let before = scenario
        .add_spell_to_library_top(P1, "Original top", true)
        .id();
    let mut runner = scenario.build();
    install_foreign_library_grant(
        runner.state_mut(),
        grove,
        P1,
        StaticMode::MayLookAtTopOfLibrary {
            library: ControllerRef::SpecificPlayer { id: P1 },
        },
    );

    let seen = |state: &GameState, viewer, object| {
        filter_state_for_viewer(state, viewer).objects[&object].name != HIDDEN_CARD_NAME
    };
    assert!(seen(runner.state(), P0, before));
    assert!(!seen(runner.state(), P0, next));
    assert!(!seen(runner.state(), P1, before));
    assert!(!seen(runner.state(), third, before));

    let card_id = runner.state().objects[&hand].card_id;
    let casting = runner
        .act(GameAction::CastSpell {
            object_id: hand,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("start the cast");
    assert!(matches!(
        casting.waiting_for,
        WaitingFor::ManaPayment { .. }
    ));
    let choose_color = runner
        .act(GameAction::ActivateAbility {
            source_id: grove,
            ability_index: 1,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate Grove during payment");
    assert!(matches!(
        choose_color.waiting_for,
        WaitingFor::ChooseManaColor { .. }
    ));
    let activation = runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Green),
            count: 1,
        })
        .expect("add green mana with Grove");
    assert!(activation
        .events
        .iter()
        .any(|event| matches!(event, GameEvent::CardDrawn { player_id: P1, .. })));
    assert!(matches!(
        activation.waiting_for,
        WaitingFor::ManaPayment { .. }
    ));
    assert_eq!(runner.state().library_of(P1).front(), Some(&next));
    assert!(!seen(runner.state(), P0, next));

    let mut canceled = GameRunner::from_state(runner.state().clone());
    canceled
        .act(GameAction::CancelCast)
        .expect("cancel the cast");
    let canceled_top = *canceled.state().library_of(P1).front().unwrap();
    assert!(seen(canceled.state(), P0, canceled_top));

    let committed = runner
        .act(GameAction::PassPriority)
        .expect("commit the payment");
    assert!(committed
        .events
        .iter()
        .any(|event| matches!(event, GameEvent::SpellCast { controller: P0, .. })));
    assert!(seen(runner.state(), P0, next));
    assert!(!seen(runner.state(), P1, next));
    assert!(!seen(runner.state(), third, next));
}
