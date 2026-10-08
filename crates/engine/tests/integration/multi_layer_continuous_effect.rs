//! Runtime coverage for CR 613.6 affected-set retention across layers.

use engine::game::layers::{evaluate_layers, flush_layers, mark_layers_entered};
use engine::game::perf_counters;
use engine::game::zones::create_object;
use engine::types::ability::{
    ContinuousModification, Duration, ObjectScope, QuantityExpr, QuantityRef, StaticDefinition,
    TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaCost;
use engine::types::player::PlayerId;
use engine::types::stickers::{AppliedSticker, StickerLocator};
use engine::types::zones::Zone;

fn make_creature(
    state: &mut GameState,
    name: &str,
    power: i32,
    toughness: i32,
    player: PlayerId,
) -> ObjectId {
    let id = create_object(
        state,
        CardId(0),
        player,
        name.to_string(),
        Zone::Battlefield,
    );
    let timestamp = state.next_timestamp();
    let object = state.objects.get_mut(&id).expect("created creature exists");
    object.card_types.core_types.push(CoreType::Creature);
    object.base_card_types = object.card_types.clone();
    object.power = Some(power);
    object.toughness = Some(toughness);
    object.base_power = Some(power);
    object.base_toughness = Some(toughness);
    object.timestamp = timestamp;
    id
}

fn make_noncreature_permanent(
    state: &mut GameState,
    name: &str,
    core_type: CoreType,
    mana_value: u32,
    player: PlayerId,
) -> ObjectId {
    let id = create_object(
        state,
        CardId(0),
        player,
        name.to_string(),
        Zone::Battlefield,
    );
    let timestamp = state.next_timestamp();
    let object = state
        .objects
        .get_mut(&id)
        .expect("created permanent exists");
    object.card_types.core_types.push(core_type);
    object.base_card_types = object.card_types.clone();
    object.mana_cost = ManaCost::generic(mana_value);
    object.base_mana_cost = object.mana_cost.clone();
    object.timestamp = timestamp;
    id
}

fn noncreature_type_filter(core_type: TypeFilter) -> TargetFilter {
    TargetFilter::Typed(TypedFilter {
        type_filters: vec![core_type, TypeFilter::Non(Box::new(TypeFilter::Creature))],
        controller: None,
        properties: Vec::new(),
    })
}

fn add_mana_value_animation_static(
    state: &mut GameState,
    source: ObjectId,
    affected_type: TypeFilter,
) {
    let mana_value = QuantityExpr::Ref {
        qty: QuantityRef::ObjectManaValue {
            scope: ObjectScope::Recipient,
        },
    };
    let definition = StaticDefinition::continuous()
        .affected(noncreature_type_filter(affected_type))
        .modifications(vec![
            ContinuousModification::AddType {
                core_type: CoreType::Creature,
            },
            ContinuousModification::SetPowerDynamic {
                value: mana_value.clone(),
            },
            ContinuousModification::SetToughnessDynamic { value: mana_value },
        ]);
    state
        .objects
        .get_mut(&source)
        .expect("static source exists")
        .static_definitions
        .push(definition);
}

fn add_granted_set_pt_static(
    state: &mut GameState,
    grant_host: ObjectId,
    recipient: ObjectId,
    value: i32,
) {
    let granted = StaticDefinition::continuous()
        .affected(TargetFilter::SelfRef)
        .modifications(vec![
            ContinuousModification::SetPower { value },
            ContinuousModification::SetToughness { value },
        ]);
    let grant = StaticDefinition::continuous()
        .affected(TargetFilter::SpecificObject { id: recipient })
        .modifications(vec![ContinuousModification::GrantStaticAbility {
            definition: Box::new(granted),
        }]);
    state
        .objects
        .get_mut(&grant_host)
        .expect("grant host exists")
        .static_definitions
        .push(grant);
}

fn add_ability_suppressor(state: &mut GameState, target: ObjectId) {
    let suppressor = make_creature(state, "Suppressor", 1, 1, PlayerId(1));
    let suppression = StaticDefinition::continuous()
        .affected(TargetFilter::SpecificObject { id: target })
        .modifications(vec![ContinuousModification::RemoveAllAbilities]);
    state
        .objects
        .get_mut(&suppressor)
        .expect("suppression source exists")
        .static_definitions
        .push(suppression);
}

/// CR 613.6: A multi-layer effect retains the object set it began applying to.
/// Type-changing an object into a creature cannot make that effect's later
/// base-P/T parts lose the object.
#[test]
fn multi_layer_effect_retains_affected_set_across_type_and_set_pt_layers() {
    let mut state = GameState::new_two_player(42);
    let artifact_animator = make_creature(&mut state, "Artifact Animator", 1, 1, PlayerId(0));
    add_mana_value_animation_static(&mut state, artifact_animator, TypeFilter::Artifact);

    // A second source also uses definition index 0. Its independent recipient
    // set is a hostile fixture for effect-group provenance.
    let enchantment_animator = make_creature(&mut state, "Enchantment Animator", 1, 1, PlayerId(0));
    add_mana_value_animation_static(&mut state, enchantment_animator, TypeFilter::Enchantment);

    let artifact = make_noncreature_permanent(
        &mut state,
        "Three-Mana Artifact",
        CoreType::Artifact,
        3,
        PlayerId(0),
    );
    let enchantment = make_noncreature_permanent(
        &mut state,
        "Five-Mana Enchantment",
        CoreType::Enchantment,
        5,
        PlayerId(0),
    );
    let artifact_creature = make_creature(&mut state, "Already a Creature", 4, 4, PlayerId(0));
    {
        let object = state
            .objects
            .get_mut(&artifact_creature)
            .expect("artifact creature exists");
        object.card_types.core_types.push(CoreType::Artifact);
        object.base_card_types = object.card_types.clone();
        object.mana_cost = ManaCost::generic(9);
        object.base_mana_cost = object.mana_cost.clone();
    }

    evaluate_layers(&mut state);

    let artifact = &state.objects[&artifact];
    assert!(artifact.card_types.core_types.contains(&CoreType::Creature));
    assert_eq!(artifact.power, Some(3));
    assert_eq!(artifact.toughness, Some(3));

    let enchantment = &state.objects[&enchantment];
    assert!(enchantment
        .card_types
        .core_types
        .contains(&CoreType::Creature));
    assert_eq!(enchantment.power, Some(5));
    assert_eq!(enchantment.toughness, Some(5));

    assert_eq!(state.objects[&artifact_creature].power, Some(4));
    assert_eq!(state.objects[&artifact_creature].toughness, Some(4));
}

/// CR 613.6: A resolving spell or ability can create a continuous effect with
/// parts in several layers. Its stable transient-effect identity retains one
/// affected set just like a static-ability effect.
#[test]
fn transient_multi_layer_effect_retains_affected_set() {
    let mut state = GameState::new_two_player(42);
    let source = make_creature(&mut state, "Animation Source", 1, 1, PlayerId(0));
    let artifact = make_noncreature_permanent(
        &mut state,
        "Transiently Animated Artifact",
        CoreType::Artifact,
        3,
        PlayerId(0),
    );
    state.add_transient_continuous_effect(
        source,
        PlayerId(0),
        Duration::UntilEndOfTurn,
        noncreature_type_filter(TypeFilter::Artifact),
        vec![
            ContinuousModification::AddType {
                core_type: CoreType::Creature,
            },
            ContinuousModification::SetPower { value: 8 },
            ContinuousModification::SetToughness { value: 8 },
        ],
        None,
    );

    evaluate_layers(&mut state);

    let artifact = &state.objects[&artifact];
    assert!(artifact.card_types.core_types.contains(&CoreType::Creature));
    assert_eq!(artifact.power, Some(8));
    assert_eq!(artifact.toughness, Some(8));
}

/// CR 613.6: A static ability granted to an object generates its own
/// multi-layer effect. The host grant origin and recipient incarnation form an
/// independent identity whose later parts retain the initial object set.
#[test]
fn granted_static_multi_layer_effect_retains_affected_set() {
    let mut state = GameState::new_two_player(42);
    let grant_host = make_creature(&mut state, "Grant Host", 1, 1, PlayerId(0));
    let recipient = make_noncreature_permanent(
        &mut state,
        "Granted Animator",
        CoreType::Artifact,
        2,
        PlayerId(0),
    );
    let granted = StaticDefinition::continuous()
        .affected(noncreature_type_filter(TypeFilter::Artifact))
        .modifications(vec![
            ContinuousModification::AddType {
                core_type: CoreType::Creature,
            },
            ContinuousModification::SetPower { value: 7 },
            ContinuousModification::SetToughness { value: 7 },
        ]);
    let grant = StaticDefinition::continuous()
        .affected(TargetFilter::SpecificObject { id: recipient })
        .modifications(vec![ContinuousModification::GrantStaticAbility {
            definition: Box::new(granted),
        }]);
    state
        .objects
        .get_mut(&grant_host)
        .expect("grant host exists")
        .static_definitions
        .push(grant);

    evaluate_layers(&mut state);

    let recipient = &state.objects[&recipient];
    assert!(recipient
        .card_types
        .core_types
        .contains(&CoreType::Creature));
    assert_eq!(recipient.power, Some(7));
    assert_eq!(recipient.toughness, Some(7));
}

/// CR 613.1f + CR 613.6: A granted static whose first applicable part is in
/// layer 7 never starts if the recipient loses the granted ability in layer 6.
#[test]
fn unstarted_granted_static_is_suppressed_with_recipient_abilities() {
    let mut state = GameState::new_two_player(42);
    let grant_host = make_creature(&mut state, "Grant Host", 1, 1, PlayerId(0));
    let recipient = make_creature(&mut state, "Grant Recipient", 2, 2, PlayerId(0));
    add_granted_set_pt_static(&mut state, grant_host, recipient, 9);
    add_ability_suppressor(&mut state, recipient);

    evaluate_layers(&mut state);

    assert_eq!(state.objects[&recipient].power, Some(2));
    assert_eq!(state.objects[&recipient].toughness, Some(2));

    evaluate_layers(&mut state);
    assert_eq!(state.objects[&recipient].power, Some(2));
    assert_eq!(state.objects[&recipient].toughness, Some(2));
}

/// CR 613.1f + CR 613.6: A granted static whose first applicable part is in
/// layer 7 never starts if the ability generating the grant is removed in
/// layer 6.
#[test]
fn unstarted_granted_static_is_suppressed_with_grant_host_abilities() {
    let mut state = GameState::new_two_player(42);
    let grant_host = make_creature(&mut state, "Grant Host", 1, 1, PlayerId(0));
    let recipient = make_creature(&mut state, "Grant Recipient", 2, 2, PlayerId(0));
    add_granted_set_pt_static(&mut state, grant_host, recipient, 9);
    add_ability_suppressor(&mut state, grant_host);

    evaluate_layers(&mut state);

    assert_eq!(state.objects[&recipient].power, Some(2));
    assert_eq!(state.objects[&recipient].toughness, Some(2));

    evaluate_layers(&mut state);
    assert_eq!(state.objects[&recipient].power, Some(2));
    assert_eq!(state.objects[&recipient].toughness, Some(2));
}

/// CR 123.8: A P/T sticker sets a creature's power and toughness independently
/// of its abilities, so layer-6 ability removal cannot suppress the sticker's
/// layer-7b effect.
#[test]
fn pt_sticker_survives_remove_all_abilities() {
    let mut state = GameState::new_two_player(42);
    let creature = make_creature(&mut state, "Stickered Creature", 2, 2, PlayerId(0));
    let timestamp = state.next_timestamp();
    state
        .objects
        .get_mut(&creature)
        .expect("stickered creature exists")
        .stickers
        .push(AppliedSticker::PowerToughness {
            locator: StickerLocator {
                sheet: "Regression Sheet".to_string(),
                index: 0,
            },
            ticket_cost: 0,
            power: 8,
            toughness: 9,
            timestamp,
        });
    add_ability_suppressor(&mut state, creature);

    evaluate_layers(&mut state);

    assert_eq!(state.objects[&creature].power, Some(8));
    assert_eq!(state.objects[&creature].toughness, Some(9));
}

/// CR 613.6: The incremental entry path uses the same retained-set semantics
/// as a full layer pass for a newly entered affected object.
#[test]
fn incremental_entry_retains_multi_layer_effect_affected_set() {
    let mut state = GameState::new_two_player(42);
    let animator = make_creature(&mut state, "Artifact Animator", 1, 1, PlayerId(0));
    add_mana_value_animation_static(&mut state, animator, TypeFilter::Artifact);
    evaluate_layers(&mut state);

    let artifact = make_noncreature_permanent(
        &mut state,
        "Four-Mana Artifact",
        CoreType::Artifact,
        4,
        PlayerId(0),
    );
    perf_counters::reset();
    mark_layers_entered(&mut state, artifact);
    flush_layers(&mut state);

    let counters = perf_counters::snapshot();
    assert_eq!(counters.layers_incremental, 1);
    assert_eq!(counters.layers_escalated, 0);
    assert_eq!(counters.layers_full_eval, 0);
    assert!(state.objects[&artifact]
        .card_types
        .core_types
        .contains(&CoreType::Creature));
    assert_eq!(state.objects[&artifact].power, Some(4));
    assert_eq!(state.objects[&artifact].toughness, Some(4));
}

/// CR 613.6: If a multi-layer effect starts applying before its source loses
/// the generating ability, its later parts still apply.
#[test]
fn started_multi_layer_effect_survives_source_ability_removal() {
    let mut state = GameState::new_two_player(42);
    let source = make_noncreature_permanent(
        &mut state,
        "Self Animator",
        CoreType::Artifact,
        2,
        PlayerId(0),
    );
    let definition = StaticDefinition::continuous()
        .affected(TargetFilter::SelfRef)
        .modifications(vec![
            ContinuousModification::AddType {
                core_type: CoreType::Creature,
            },
            ContinuousModification::SetPower { value: 6 },
            ContinuousModification::SetToughness { value: 6 },
        ]);
    state
        .objects
        .get_mut(&source)
        .expect("self-animation source exists")
        .static_definitions
        .push(definition);

    let suppressor = make_creature(&mut state, "Suppressor", 1, 1, PlayerId(1));
    let suppression = StaticDefinition::continuous()
        .affected(TargetFilter::SpecificObject { id: source })
        .modifications(vec![ContinuousModification::RemoveAllAbilities]);
    state
        .objects
        .get_mut(&suppressor)
        .expect("suppression source exists")
        .static_definitions
        .push(suppression);

    evaluate_layers(&mut state);

    let source = &state.objects[&source];
    assert!(source.card_types.core_types.contains(&CoreType::Creature));
    assert!(source.static_definitions.is_empty());
    assert_eq!(source.power, Some(6));
    assert_eq!(source.toughness, Some(6));
}
