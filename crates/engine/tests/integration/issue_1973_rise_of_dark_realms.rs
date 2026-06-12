//! Issue #1973 — Rise of the Dark Realms must reanimate every creature card from
//! every graveyard under the caster's control without hanging on zone choice.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::zones::create_object;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityKind, Effect};
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const RISE_ORACLE: &str =
    "Put all creature cards from all graveyards onto the battlefield under your control.";

fn graveyard_creature(
    state: &mut GameState,
    card_id: u64,
    owner: PlayerId,
    name: &str,
) -> ObjectId {
    let oid = create_object(
        state,
        CardId(card_id),
        owner,
        name.to_string(),
        Zone::Graveyard,
    );
    let obj = state.objects.get_mut(&oid).expect("just created");
    obj.card_types.core_types.push(CoreType::Creature);
    obj.base_card_types = obj.card_types.clone();
    oid
}

#[test]
fn rise_of_dark_realms_reanimates_all_graveyard_creatures_under_caster() {
    let def = parse_effect_chain(RISE_ORACLE, AbilityKind::Spell);
    assert!(
        matches!(
            def.effect.as_ref(),
            Effect::ChangeZoneAll {
                origin: Some(Zone::Graveyard),
                destination: Zone::Battlefield,
                ..
            }
        ),
        "parsed shape: {:?}",
        def.effect
    );

    let mut state = GameState::new_two_player(1973);
    let source = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Rise of the Dark Realms".to_string(),
        Zone::Stack,
    );

    let p0_creature = graveyard_creature(&mut state, 10, PlayerId(0), "P0 Zombie");
    let p1_creature = graveyard_creature(&mut state, 11, PlayerId(1), "P1 Zombie");
    let p1_sorcery = create_object(
        &mut state,
        CardId(12),
        PlayerId(1),
        "P1 Instant".to_string(),
        Zone::Graveyard,
    );

    let ability = build_resolved_from_def(&def, source, PlayerId(0));
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).expect("Rise resolves");

    assert_eq!(state.objects[&p0_creature].zone, Zone::Battlefield);
    assert_eq!(state.objects[&p1_creature].zone, Zone::Battlefield);
    assert_eq!(
        state.objects[&p0_creature].controller,
        PlayerId(0),
        "caster-owned creature enters under caster"
    );
    assert_eq!(
        state.objects[&p1_creature].controller,
        PlayerId(0),
        "opponent-owned creature enters under caster"
    );
    assert_eq!(
        state.objects[&p1_sorcery].zone,
        Zone::Graveyard,
        "non-creature cards stay in graveyard"
    );
    assert!(
        !matches!(state.waiting_for, WaitingFor::EffectZoneChoice { .. }),
        "mass reanimation must not stall on EffectZoneChoice: {:?}",
        state.waiting_for
    );
}
