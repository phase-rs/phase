use engine::game::effects::resolve_ability_chain;
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityCondition, Effect, ResolvedAbility, TargetFilter, TargetRef, TypeFilter, TypedFilter,
};
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::{EtbTapState, Zone};

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

fn typed_object(
    state: &mut GameState,
    id: u64,
    owner: PlayerId,
    name: &str,
    zone: Zone,
    core_type: CoreType,
) -> ObjectId {
    let object_id = create_object(state, CardId(id), owner, name.to_string(), zone);
    let object = state.objects.get_mut(&object_id).expect("object exists");
    object.card_types.core_types.push(core_type);
    object.base_card_types = object.card_types.clone();
    object_id
}

fn library_top(
    state: &mut GameState,
    id: u64,
    owner: PlayerId,
    name: &str,
    core_type: CoreType,
) -> ObjectId {
    let object_id = typed_object(state, id, owner, name, Zone::Library, core_type);
    let player = state
        .players
        .iter_mut()
        .find(|player| player.id == owner)
        .expect("player exists");
    player.library.retain(|&id| id != object_id);
    player.library.insert(0, object_id);
    object_id
}

fn chaos_warp_reveal_chain(source: ObjectId, target: ObjectId) -> ResolvedAbility {
    let put_revealed_onto_battlefield = ResolvedAbility::new(
        Effect::ChangeZone {
            origin: Some(Zone::Library),
            destination: Zone::Battlefield,
            target: TargetFilter::ParentTarget,
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: EtbTapState::Unspecified,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: Vec::new(),
            conditional_enter_with_counters: Vec::new(),
            face_down_profile: None,
            enters_modified_if: None,
        },
        Vec::new(),
        source,
        P0,
    )
    .condition(AbilityCondition::TargetMatchesFilter {
        filter: TargetFilter::Typed(TypedFilter::new(TypeFilter::Permanent)),
        use_lki: false,
        subject_slot: None,
    });

    ResolvedAbility::new(
        Effect::RevealTop {
            count: 1,
            player: TargetFilter::ParentTargetOwner,
        },
        vec![TargetRef::Object(target)],
        source,
        P0,
    )
    .sub_ability(put_revealed_onto_battlefield)
}

#[test]
fn chaos_warp_reveals_target_owners_library_and_moves_revealed_permanent() {
    let mut state = GameState::new_two_player(42);
    let source = typed_object(
        &mut state,
        1,
        P0,
        "Chaos Warp Source",
        Zone::Battlefield,
        CoreType::Enchantment,
    );
    let target = typed_object(
        &mut state,
        2,
        P1,
        "Target Owner Permanent",
        Zone::Battlefield,
        CoreType::Artifact,
    );
    let caster_top = library_top(&mut state, 3, P0, "Caster Top Instant", CoreType::Instant);
    let owner_top = library_top(&mut state, 4, P1, "Owner Top Creature", CoreType::Creature);

    let ability = chaos_warp_reveal_chain(source, target);
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).expect("chain resolves");

    assert_eq!(state.objects[&owner_top].zone, Zone::Battlefield);
    assert_eq!(state.objects[&caster_top].zone, Zone::Library);
    assert_eq!(state.objects[&target].zone, Zone::Battlefield);
    assert_eq!(state.last_revealed_ids, vec![owner_top]);
}

#[test]
fn chaos_warp_revealed_nonpermanent_owner_top_does_not_enter() {
    let mut state = GameState::new_two_player(42);
    let source = typed_object(
        &mut state,
        11,
        P0,
        "Chaos Warp Source",
        Zone::Battlefield,
        CoreType::Enchantment,
    );
    let target = typed_object(
        &mut state,
        12,
        P1,
        "Target Owner Permanent",
        Zone::Battlefield,
        CoreType::Artifact,
    );
    let caster_top = library_top(
        &mut state,
        13,
        P0,
        "Caster Top Creature",
        CoreType::Creature,
    );
    let owner_top = library_top(&mut state, 14, P1, "Owner Top Instant", CoreType::Instant);

    let ability = chaos_warp_reveal_chain(source, target);
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).expect("chain resolves");

    assert_eq!(state.objects[&owner_top].zone, Zone::Library);
    assert_eq!(state.objects[&caster_top].zone, Zone::Library);
    assert_eq!(state.objects[&target].zone, Zone::Battlefield);
    assert_eq!(state.last_revealed_ids, vec![owner_top]);
}
