//! Issue #3249 — Faith's Fetters must prevent the enchanted permanent from attacking
//! and blocking.
//!
//! Root cause: the compound static splitter for "can't attack or block, and …
//! activated abilities can't be activated" bound both prohibitions to
//! `TargetFilter::SelfRef` (the Aura) instead of the enchanted host filter.

use engine::game::combat::{declare_attackers, declare_blockers, AttackTarget};
use engine::game::layers::evaluate_layers;
use engine::game::zones::create_object;
use engine::parser::oracle_static::parse_static_line_multi;
use engine::types::card_type::CoreType;
use engine::types::format::FormatConfig;
use engine::types::game_state::GameState;
use engine::types::identifiers::CardId;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const FAITHS_FETTERS_STATIC_LINE: &str = "Enchanted permanent can't attack or block, and its activated abilities can't be activated unless they're mana abilities.";

const P0: PlayerId = PlayerId(0); // Aura controller and enchanted creature's controller
const P1: PlayerId = PlayerId(1); // attacking opponent

#[test]
fn faiths_fetters_prevents_enchanted_creature_from_attacking() {
    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    state.active_player = P0;
    state.turn_number = 2;

    let bear = CardId(state.next_object_id);
    let bear_obj = create_object(
        &mut state,
        bear,
        P0,
        "Grizzly Bears".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&bear_obj).unwrap();
        obj.card_types.core_types = vec![CoreType::Creature];
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
    }

    let parsed_defs = parse_static_line_multi(FAITHS_FETTERS_STATIC_LINE);
    let fetters = CardId(state.next_object_id);
    let fetters_obj = create_object(
        &mut state,
        fetters,
        P0,
        "Faith's Fetters".to_string(),
        Zone::Battlefield,
    );
    let ts = state.next_timestamp();
    {
        let aura_obj = state.objects.get_mut(&fetters_obj).unwrap();
        aura_obj.card_types.core_types = vec![CoreType::Enchantment];
        aura_obj.card_types.subtypes = vec!["Aura".to_string()];
        aura_obj.base_card_types = aura_obj.card_types.clone();
        aura_obj.attached_to = Some(bear_obj.into());
        aura_obj.timestamp = ts;
        aura_obj.static_definitions = parsed_defs.clone().into();
        aura_obj.base_static_definitions = std::sync::Arc::new(parsed_defs);
    }
    state
        .objects
        .get_mut(&bear_obj)
        .unwrap()
        .attachments
        .push(fetters_obj);

    evaluate_layers(&mut state);

    let mut events = Vec::new();
    assert!(
        declare_attackers(
            &mut state,
            &[(bear_obj, AttackTarget::Player(P1))],
            &mut events,
        )
        .is_err(),
        "enchanted creature must be unable to attack under Faith's Fetters"
    );
}

#[test]
fn faiths_fetters_prevents_enchanted_creature_from_blocking() {
    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    state.active_player = P1;
    state.turn_number = 2;

    let bear = CardId(state.next_object_id);
    let bear_obj = create_object(
        &mut state,
        bear,
        P0,
        "Grizzly Bears".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&bear_obj).unwrap();
        obj.card_types.core_types = vec![CoreType::Creature];
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
    }

    let attacker = CardId(state.next_object_id);
    let attacker_obj = create_object(
        &mut state,
        attacker,
        P1,
        "Attacker".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&attacker_obj).unwrap();
        obj.card_types.core_types = vec![CoreType::Creature];
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(3);
        obj.toughness = Some(3);
        obj.base_power = Some(3);
        obj.base_toughness = Some(3);
    }

    let parsed_defs = parse_static_line_multi(FAITHS_FETTERS_STATIC_LINE);
    let fetters = CardId(state.next_object_id);
    let fetters_obj = create_object(
        &mut state,
        fetters,
        P0,
        "Faith's Fetters".to_string(),
        Zone::Battlefield,
    );
    let ts = state.next_timestamp();
    {
        let aura_obj = state.objects.get_mut(&fetters_obj).unwrap();
        aura_obj.card_types.core_types = vec![CoreType::Enchantment];
        aura_obj.card_types.subtypes = vec!["Aura".to_string()];
        aura_obj.base_card_types = aura_obj.card_types.clone();
        aura_obj.attached_to = Some(bear_obj.into());
        aura_obj.timestamp = ts;
        aura_obj.static_definitions = parsed_defs.clone().into();
        aura_obj.base_static_definitions = std::sync::Arc::new(parsed_defs);
    }
    state
        .objects
        .get_mut(&bear_obj)
        .unwrap()
        .attachments
        .push(fetters_obj);

    evaluate_layers(&mut state);

    let mut events = Vec::new();
    assert!(
        declare_attackers(
            &mut state,
            &[(attacker_obj, AttackTarget::Player(P0))],
            &mut events,
        )
        .is_ok(),
        "attacker must be able to attack P0"
    );

    events.clear();
    assert!(
        declare_blockers(&mut state, &[(bear_obj, attacker_obj)], &mut events).is_err(),
        "enchanted creature must be unable to block under Faith's Fetters"
    );
}
