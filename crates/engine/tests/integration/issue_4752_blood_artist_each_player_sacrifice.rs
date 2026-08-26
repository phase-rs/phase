//! Regression for issue #4752: each-player sacrifice choices are announced
//! first, then the selected permanents are sacrificed simultaneously.
//!
//! CR 101.4: If players make choices and then take actions, APNAP choices are
//! announced first and then the actions happen simultaneously.
//! CR 603.10a: Leaves-the-battlefield triggers use the game state before the
//! event to determine whether they trigger.
//! CR 701.21a: To sacrifice a permanent, its controller moves it from the
//! battlefield to its owner's graveyard.

use engine::game::effects::resolve_ability_chain;
use engine::game::engine::apply_as_current;
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, PlayerFilter, QuantityExpr, ResolvedAbility,
    TargetFilter, TriggerDefinition, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

fn make_creature(state: &mut GameState, card: u64, controller: PlayerId, name: &str) -> ObjectId {
    let id = create_object(
        state,
        CardId(card),
        controller,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.base_power = Some(1);
    obj.base_toughness = Some(1);
    obj.power = Some(1);
    obj.toughness = Some(1);
    id
}

fn install_blood_artist_like_trigger(state: &mut GameState, source: ObjectId) {
    let trigger = TriggerDefinition::new(TriggerMode::ChangesZone)
        .origin(Zone::Battlefield)
        .destination(Zone::Graveyard)
        .valid_card(TargetFilter::Typed(TypedFilter::creature()))
        .execute(AbilityDefinition::new(
            AbilityKind::Database,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
        ));

    let obj = state.objects.get_mut(&source).unwrap();
    obj.trigger_definitions.push(trigger.clone());
    std::sync::Arc::make_mut(&mut obj.base_trigger_definitions).push(trigger);
}

#[test]
fn blood_artist_observes_each_creature_in_simultaneous_each_player_sacrifice() {
    let mut state = GameState::new_two_player(42);
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.players[0].life = 20;

    let blood_artist = make_creature(&mut state, 10, PlayerId(0), "Blood Artist");
    let decoy = make_creature(&mut state, 11, PlayerId(0), "Decoy");
    let opp_a = make_creature(&mut state, 20, PlayerId(1), "Opponent Creature A");
    let opp_b = make_creature(&mut state, 21, PlayerId(1), "Opponent Creature B");
    install_blood_artist_like_trigger(&mut state, blood_artist);

    let mut ability = ResolvedAbility::new(
        Effect::Sacrifice {
            target: TargetFilter::Typed(TypedFilter::creature()),
            count: QuantityExpr::Fixed { value: 1 },
            min_count: 0,
        },
        vec![],
        ObjectId(100),
        PlayerId(0),
    );
    ability.player_scope = Some(PlayerFilter::All);

    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    match &state.waiting_for {
        WaitingFor::EffectZoneChoice { player, cards, .. } => {
            assert_eq!(*player, PlayerId(0));
            assert!(cards.contains(&blood_artist));
            assert!(cards.contains(&decoy));
        }
        other => panic!("expected P0 sacrifice choice, got {other:?}"),
    }

    apply_as_current(
        &mut state,
        GameAction::SelectCards {
            cards: vec![blood_artist],
        },
    )
    .expect("P0 chooses Blood Artist");

    assert_eq!(
        state.objects[&blood_artist].zone,
        Zone::Battlefield,
        "the first player's choice must be announced before the simultaneous \
         sacrifice action happens"
    );

    match &state.waiting_for {
        WaitingFor::EffectZoneChoice { player, cards, .. } => {
            assert_eq!(*player, PlayerId(1));
            assert!(cards.contains(&opp_a));
            assert!(cards.contains(&opp_b));
        }
        other => panic!("expected P1 sacrifice choice, got {other:?}"),
    }

    apply_as_current(&mut state, GameAction::SelectCards { cards: vec![opp_a] })
        .expect("P1 chooses a creature");

    for _ in 0..20 {
        match &state.waiting_for {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(&mut state);
            }
            WaitingFor::Priority { .. } if state.stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                apply_as_current(&mut state, GameAction::PassPriority)
                    .expect("resolve Blood Artist trigger");
            }
            other => panic!("unexpected waiting_for while resolving triggers: {other:?}"),
        }
    }

    assert_eq!(state.objects[&blood_artist].zone, Zone::Graveyard);
    assert_eq!(state.objects[&opp_a].zone, Zone::Graveyard);
    assert_eq!(
        state.players[0].life, 22,
        "Blood Artist must observe both creatures dying in the simultaneous \
         each-player sacrifice"
    );
}
