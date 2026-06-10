//! [#2373](https://github.com/phase-rs/phase/issues/2373): when an enchanted
//! permanent phases back in, the continuous effects from Auras (and Equipment)
//! still attached to it must be re-applied via the layer system.
//!
//! Scenario from the report: a land (Blinkmoth Nexus) is animated to a creature
//! until end of turn, a type/P-T-setting Aura (Darksteel Mutation / Kenrith's
//! Transformation) is attached, then the land is phased out. After the
//! until-end-of-turn animation expires (correct — end of turn passed) and the
//! land phases back in on the next untap step, the still-attached Aura's
//! continuous effect must STILL apply (CR 702.26e + CR 702.26f + CR 611.2 + CR 613).
//!
//! Root cause: `phase_in_object` flipped `phase_status` (changing which objects
//! contribute and receive continuous effects per CR 702.26e) without marking the
//! layer system dirty, so the next `flush_layers` was a no-op and the permanent
//! kept its reset-to-base characteristics with no Aura effect re-applied.

use engine::game::game_object::PhaseOutCause;
use engine::game::layers::{evaluate_layers, flush_layers, prune_end_of_turn_effects};
use engine::game::phasing::{phase_in_object, phase_out_object};
use engine::game::zones::create_object;
use engine::types::ability::{
    ContinuousModification, Duration, FilterProp, StaticDefinition, TargetFilter, TypedFilter,
};
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// A "Blinkmoth Nexus"-style land: base characteristics are land-only, no
/// power/toughness, no creature type.
fn setup_land(state: &mut GameState, name: &str, controller: PlayerId) -> ObjectId {
    let id = create_object(
        state,
        CardId(1),
        controller,
        name.to_string(),
        Zone::Battlefield,
    );
    if let Some(obj) = state.objects.get_mut(&id) {
        obj.card_types.core_types = vec![CoreType::Land];
        obj.base_card_types = obj.card_types.clone();
        obj.power = None;
        obj.toughness = None;
        obj.base_power = None;
        obj.base_toughness = None;
    }
    id
}

/// A "Darksteel Mutation"/"Kenrith's Transformation"-style Aura: a continuous
/// effect that sets the enchanted creature's base P/T and changes its type. We
/// model the load-bearing layer effects (set base toughness in 7b, add the
/// Insect creature type in layer 4) targeting the `EnchantedBy` recipient —
/// exactly the shape the static parser emits for "enchanted creature is a 0/1
/// Insect ...".
fn attach_type_setting_aura(
    state: &mut GameState,
    name: &str,
    controller: PlayerId,
    host: ObjectId,
) -> ObjectId {
    let id = create_object(
        state,
        CardId(2),
        controller,
        name.to_string(),
        Zone::Battlefield,
    );
    let ts = state.next_timestamp();
    if let Some(obj) = state.objects.get_mut(&id) {
        obj.card_types.core_types = vec![CoreType::Enchantment];
        obj.card_types.subtypes = vec!["Aura".to_string()];
        obj.base_card_types = obj.card_types.clone();
        obj.attached_to = Some(host.into());
        obj.timestamp = ts;

        let enchanted =
            TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::EnchantedBy]));
        // "Enchanted creature is a 0/1 Insect artifact creature" — set base P/T
        // (layer 7b) plus the Artifact/Creature core types and Insect subtype
        // (layer 4). This is the load-bearing shape Darksteel Mutation /
        // Kenrith's Transformation emit.
        let aura_static = StaticDefinition::continuous()
            .affected(enchanted)
            .modifications(vec![
                ContinuousModification::SetToughness { value: 1 },
                ContinuousModification::SetPower { value: 0 },
                ContinuousModification::AddType {
                    core_type: CoreType::Creature,
                },
                ContinuousModification::AddType {
                    core_type: CoreType::Artifact,
                },
                ContinuousModification::AddSubtype {
                    subtype: "Insect".to_string(),
                },
            ]);
        obj.static_definitions = vec![aura_static.clone()].into();
        obj.base_static_definitions = std::sync::Arc::new(vec![aura_static]);
    }
    if let Some(host_obj) = state.objects.get_mut(&host) {
        host_obj.attachments.push(id);
    }
    id
}

/// Apply an "animate until end of turn" transient continuous effect, mirroring
/// `effects::animate::resolve`: become a 1/1 creature with flying until EOT.
fn animate_until_eot(state: &mut GameState, id: ObjectId, controller: PlayerId) {
    state.add_transient_continuous_effect(
        id,
        controller,
        Duration::UntilEndOfTurn,
        TargetFilter::SpecificObject { id },
        vec![
            ContinuousModification::SetPower { value: 1 },
            ContinuousModification::SetToughness { value: 1 },
            ContinuousModification::AddType {
                core_type: CoreType::Creature,
            },
            ContinuousModification::AddKeyword {
                keyword: Keyword::Flying,
            },
        ],
        None,
    );
}

fn is_creature(state: &GameState, id: ObjectId) -> bool {
    state.objects[&id]
        .card_types
        .core_types
        .contains(&CoreType::Creature)
}

fn has_insect(state: &GameState, id: ObjectId) -> bool {
    state.objects[&id]
        .card_types
        .subtypes
        .iter()
        .any(|s| s == "Insect")
}

/// The full report scenario end-to-end: animate-until-EOT + attached Aura, phase
/// out, expire the animation at end of turn, phase back in, and confirm the
/// Aura's continuous effect STILL applies after phase-in.
#[test]
fn aura_continuous_effect_reapplies_after_phase_in() {
    let mut state = GameState::new_two_player(42);
    state.active_player = PlayerId(0);
    let land = setup_land(&mut state, "Blinkmoth Nexus", PlayerId(0));

    // Animate the land to a creature until end of turn, then attach the Aura.
    animate_until_eot(&mut state, land, PlayerId(0));
    let aura = attach_type_setting_aura(&mut state, "Darksteel Mutation", PlayerId(0), land);

    evaluate_layers(&mut state);

    // Baseline: it's a creature, the Aura's type/P-T effect applies.
    assert!(
        is_creature(&state, land),
        "animated + enchanted land is a creature"
    );
    assert!(
        has_insect(&state, land),
        "Aura adds the Insect creature type"
    );
    assert_eq!(
        state.objects[&land].toughness,
        Some(1),
        "Aura sets base toughness to 1"
    );

    // Phase the land out. The Aura cascades indirectly (CR 702.26g).
    let mut events = Vec::new();
    phase_out_object(&mut state, land, PhaseOutCause::Directly, &mut events);
    assert!(state.objects[&land].is_phased_out());
    assert!(state.objects[&aura].is_phased_out());

    // End of turn passes while phased out: the until-EOT animation expires.
    prune_end_of_turn_effects(&mut state);
    flush_layers(&mut state);

    // Next untap step: the land phases back in (Aura rides along, CR 702.26g).
    events.clear();
    phase_in_object(&mut state, land, &mut events);
    assert!(state.objects[&land].is_phased_in());
    assert!(state.objects[&aura].is_phased_in());

    // Resolve the layer system at the next opportunity (priority/SBA boundary).
    flush_layers(&mut state);

    // The animation is gone (end of turn passed) — but the still-attached Aura's
    // continuous effect MUST apply. Pre-fix this failed: the land behaved like a
    // plain land because phase-in never marked the layer system dirty.
    assert!(
        is_creature(&state, land),
        "CR 702.26f + CR 611.2: re-phased-in permanent is a creature again via its still-attached Aura"
    );
    assert!(
        has_insect(&state, land),
        "CR 613: Aura's Insect type-change re-applies after phase-in"
    );
    assert_eq!(
        state.objects[&land].toughness,
        Some(1),
        "CR 613.4: Aura's base-toughness=1 re-applies after phase-in"
    );
    // The discriminating assertion: toughness alone is 1 under BOTH the Aura
    // (0/1) and the expired animation (1/1), so it can't prove which effect is
    // live. Power distinguishes them — the Aura sets power 0, the animation set
    // power 1. Asserting power==0 proves the Aura re-applied and the stale
    // until-EOT animation did NOT linger (CR 702.26f).
    assert_eq!(
        state.objects[&land].power,
        Some(0),
        "CR 613.4 + CR 702.26f: Aura's base-power=0 re-applies, not the expired animation's 1"
    );
    // The animation also granted Flying; once it expired while phased out, the
    // re-phased-in permanent must not have it (the Aura grants no keywords).
    assert!(
        !engine::game::keywords::has_flying(&state.objects[&land]),
        "CR 702.26f: the until-EOT animation's Flying is gone after expiry + phase-in"
    );
}

/// Build-for-class sibling: an Equipment buff (a +N/+N "anthem on the equipped
/// creature") must likewise re-apply after the equipped creature phases out and
/// back in. Same seam (phase-in → layer recompute), different attachment kind.
#[test]
fn equipment_buff_reapplies_after_phase_in() {
    let mut state = GameState::new_two_player(42);
    state.active_player = PlayerId(0);

    // A vanilla 2/2 creature.
    let creature = create_object(
        &mut state,
        CardId(3),
        PlayerId(0),
        "Bear".to_string(),
        Zone::Battlefield,
    );
    if let Some(obj) = state.objects.get_mut(&creature) {
        obj.card_types.core_types = vec![CoreType::Creature];
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
    }

    // A +2/+2 Equipment attached to it.
    let equip = create_object(
        &mut state,
        CardId(4),
        PlayerId(0),
        "Bonesplitter".to_string(),
        Zone::Battlefield,
    );
    let ts = state.next_timestamp();
    if let Some(obj) = state.objects.get_mut(&equip) {
        obj.card_types.core_types = vec![CoreType::Artifact];
        obj.card_types.subtypes = vec!["Equipment".to_string()];
        obj.base_card_types = obj.card_types.clone();
        obj.attached_to = Some(creature.into());
        obj.timestamp = ts;
        let equipped =
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EquippedBy]));
        let st = StaticDefinition::continuous()
            .affected(equipped)
            .modifications(vec![
                ContinuousModification::AddPower { value: 2 },
                ContinuousModification::AddToughness { value: 2 },
            ]);
        obj.static_definitions = vec![st.clone()].into();
        obj.base_static_definitions = std::sync::Arc::new(vec![st]);
    }
    state
        .objects
        .get_mut(&creature)
        .unwrap()
        .attachments
        .push(equip);

    evaluate_layers(&mut state);
    assert_eq!(
        state.objects[&creature].power,
        Some(4),
        "equipped: 2 base + 2"
    );

    // Phase out, then back in — the Equipment buff must re-apply.
    let mut events = Vec::new();
    phase_out_object(&mut state, creature, PhaseOutCause::Directly, &mut events);
    flush_layers(&mut state);
    events.clear();
    phase_in_object(&mut state, creature, &mut events);
    flush_layers(&mut state);

    assert_eq!(
        state.objects[&creature].power,
        Some(4),
        "CR 611.2 + CR 613: Equipment +2/+2 re-applies after the equipped creature phases in"
    );
}

/// Regression guard: a permanent with NO attachments that phases out and back in
/// is unaffected (its base characteristics are restored, no spurious effects).
#[test]
fn plain_permanent_phasing_unaffected() {
    let mut state = GameState::new_two_player(42);
    let creature = create_object(
        &mut state,
        CardId(5),
        PlayerId(0),
        "Bear".to_string(),
        Zone::Battlefield,
    );
    if let Some(obj) = state.objects.get_mut(&creature) {
        obj.card_types.core_types = vec![CoreType::Creature];
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
    }
    evaluate_layers(&mut state);

    let mut events = Vec::new();
    phase_out_object(&mut state, creature, PhaseOutCause::Directly, &mut events);
    flush_layers(&mut state);
    events.clear();
    phase_in_object(&mut state, creature, &mut events);
    flush_layers(&mut state);

    assert!(is_creature(&state, creature));
    assert_eq!(state.objects[&creature].power, Some(2));
    assert_eq!(state.objects[&creature].toughness, Some(2));
}
