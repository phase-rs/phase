//! Issue #1970 — Eladamri, Korvecdal's hand mode must pause on RevealChoice so
//! the activator can pick a card from their hand.

use engine::game::ability_utils::build_resolved_from_def_with_targets;
use engine::game::effects::resolve_ability_chain;
use engine::game::zones::create_object;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityKind, Effect, TargetFilter, TargetRef};
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const ELADAMRI_HAND_MODE: &str = "\
Reveal a card from your hand. If it's a creature card, you may put it onto the battlefield.";

fn hand_creature(state: &mut GameState, card_id: u64, owner: PlayerId, name: &str) -> ObjectId {
    let oid = create_object(state, CardId(card_id), owner, name.to_string(), Zone::Hand);
    let obj = state.objects.get_mut(&oid).expect("just created");
    obj.card_types.core_types.push(CoreType::Creature);
    obj.base_card_types = obj.card_types.clone();
    oid
}

#[test]
fn eladamri_hand_mode_offers_reveal_choice_for_controller_hand() {
    let def = parse_effect_chain(ELADAMRI_HAND_MODE, AbilityKind::Activated);
    let Effect::RevealHand {
        target,
        card_filter,
        ..
    } = def.effect.as_ref()
    else {
        panic!("parsed shape: {:?}", def.effect);
    };
    assert_eq!(
        *target,
        TargetFilter::Controller,
        "hand mode should reveal from the controller's hand"
    );
    assert_eq!(
        *card_filter,
        TargetFilter::Any,
        "hand mode must expose every hand card for selection, got {card_filter:?}"
    );

    let mut state = GameState::new_two_player(1970);
    let source = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Eladamri, Korvecdal".to_string(),
        Zone::Battlefield,
    );
    let creature_in_hand = hand_creature(&mut state, 10, PlayerId(0), "Grizzly Bears");
    let _land_in_hand = create_object(
        &mut state,
        CardId(11),
        PlayerId(0),
        "Forest".to_string(),
        Zone::Hand,
    );

    let ability = build_resolved_from_def_with_targets(
        &def,
        source,
        PlayerId(0),
        vec![TargetRef::Player(PlayerId(0))],
    );
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0)
        .expect("Eladamri hand mode resolves through reveal");

    match &state.waiting_for {
        WaitingFor::RevealChoice {
            player,
            cards,
            filter,
            ..
        } => {
            assert_eq!(*player, PlayerId(0));
            assert_eq!(*filter, TargetFilter::Any);
            assert!(
                cards.contains(&creature_in_hand),
                "creature in hand must be eligible: {cards:?}"
            );
            assert_eq!(
                cards.len(),
                2,
                "both hand cards are eligible with Any filter"
            );
        }
        other => panic!("hand reveal must pause on RevealChoice, not stall or skip: {other:?}"),
    }
}
