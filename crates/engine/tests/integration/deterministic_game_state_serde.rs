use std::collections::{BTreeMap, BTreeSet, HashMap};

use engine::game::combat::{
    AttackTarget, BlockRequirement, CombatRequirement, CombatState, DamageAssignment, DamageTarget,
};
use engine::game::dungeon::DungeonProgress;
use engine::game::game_object::{BackFaceData, GameObject, ProtectionStartSnapshot};
use engine::game::printed_cards::intrinsic_copiable_values;
use engine::types::ability::ThisWayCause;
use engine::types::attribution::ObjectAttribution;
use engine::types::card_type::CardType;
use engine::types::definitions::Definitions;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::{
    AutoPassMode, LandPlayRecord, LiminalEntry, LinkedExileSnapshot, PendingConniveReentry,
    PersistedGameState, PriorityPassingMode, SpellCastRecord, StackPaidSnapshot, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId, TrackedSetId};
use engine::types::keywords::ProtectionTarget;
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::{PhaseStop, PhaseStopScope};
use engine::types::proposed_event::AppliedReplacementKey;
use engine::types::zones::EtbTapState;
use engine::types::{FormatConfig, GameState, Phase, PlayerId, Zone};
use syn::{Fields, GenericArgument, Item, PathArguments, Type};

const HASH_SET: &str = "serialize_with=\"crate::types::deterministic_serde::hash_set\"";
const OPTION_HASH_SET: &str =
    "serialize_with=\"crate::types::deterministic_serde::option_hash_set\"";
const HASH_MAP: &str = "serialize_with=\"crate::types::deterministic_serde::hash_map\"";
const OPTION_HASH_MAP: &str =
    "serialize_with=\"crate::types::deterministic_serde::option_hash_map\"";
const VEC_HASH_MAP: &str = "serialize_with=\"crate::types::deterministic_serde::vec_hash_map\"";
const HASH_MAP_OF_HASH_SET: &str =
    "serialize_with=\"crate::types::deterministic_serde::hash_map_of_hash_set\"";
const HASH_MAP_OF_HASH_MAP: &str =
    "serialize_with=\"crate::types::deterministic_serde::hash_map_of_hash_map\"";
const IM_HASH_SET: &str = "serialize_with=\"crate::types::deterministic_serde::im_hash_set\"";
const IM_HASH_MAP: &str = "serialize_with=\"crate::types::deterministic_serde::im_hash_map\"";
const IM_HASH_MAP_OF_IM_HASH_MAP: &str =
    "serialize_with=\"crate::types::deterministic_serde::im_hash_map_of_im_hash_map\"";
const NUMERIC_HASH_MAP_DESERIALIZER: &str =
    "deserialize_with=\"crate::types::deterministic_serde::deserialize_numeric_hash_map\"";
const OPTION_NUMERIC_HASH_MAP_DESERIALIZER: &str =
    "deserialize_with=\"crate::types::deterministic_serde::deserialize_option_numeric_hash_map\"";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Classification {
    Canonical(&'static str),
    SerdeSkip,
    DeserializeOnlyOrRuntime,
    NonStringJsonMapKey,
}

#[derive(Debug, Clone, Copy)]
struct OwnerSpec {
    shape: &'static str,
    classification: Classification,
    map_key_types: &'static [&'static str],
    numeric_deserializer: Option<&'static str>,
}

#[derive(Debug)]
struct DiscoveredOwner {
    shape: String,
    serde: String,
    map_key_types: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoundTripGroup {
    DirectGameState,
    CombatState,
    DeclareAttackers,
    DeclareBlockers,
}

#[derive(Debug, Clone, Copy)]
struct NumericRoundTripOwner {
    id: &'static str,
    map_key_types: &'static [&'static str],
    group: RoundTripGroup,
    numeric_deserializer: Option<&'static str>,
}

const NUMERIC_MAP_ROUND_TRIP_OWNERS: &[NumericRoundTripOwner] = &[
    NumericRoundTripOwner { id: "src/types/game_state.rs::GameState::objects", map_key_types: &["ObjectId"], group: RoundTripGroup::DirectGameState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/types/game_state.rs::GameState::stack_paid_facts", map_key_types: &["ObjectId"], group: RoundTripGroup::DirectGameState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/types/game_state.rs::GameState::liminal_entries", map_key_types: &["ObjectId"], group: RoundTripGroup::DirectGameState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/types/game_state.rs::GameState::attribution", map_key_types: &["ObjectId"], group: RoundTripGroup::DirectGameState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/types/game_state.rs::GameState::tracked_object_sets", map_key_types: &["TrackedSetId"], group: RoundTripGroup::DirectGameState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/types/game_state.rs::GameState::tracked_set_member_causes", map_key_types: &["TrackedSetId", "ObjectId"], group: RoundTripGroup::DirectGameState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/types/game_state.rs::GameState::commander_cast_count", map_key_types: &["ObjectId"], group: RoundTripGroup::DirectGameState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/types/game_state.rs::GameState::commander_cast_owners", map_key_types: &["ObjectId"], group: RoundTripGroup::DirectGameState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/types/game_state.rs::GameState::auto_pass", map_key_types: &["PlayerId"], group: RoundTripGroup::DirectGameState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/types/game_state.rs::GameState::phase_stops", map_key_types: &["PlayerId"], group: RoundTripGroup::DirectGameState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/types/game_state.rs::GameState::priority_passing_modes", map_key_types: &["PlayerId"], group: RoundTripGroup::DirectGameState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/types/game_state.rs::GameState::lands_tapped_for_mana", map_key_types: &["PlayerId"], group: RoundTripGroup::DirectGameState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/types/game_state.rs::GameState::prepaid_mulligan_bottoms", map_key_types: &["PlayerId"], group: RoundTripGroup::DirectGameState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/types/game_state.rs::GameState::loyalty_abilities_activated_this_turn", map_key_types: &["PlayerId"], group: RoundTripGroup::DirectGameState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/types/game_state.rs::GameState::extra_loyalty_activations_this_turn", map_key_types: &["PlayerId"], group: RoundTripGroup::DirectGameState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/types/game_state.rs::GameState::object_tap_count_this_turn", map_key_types: &["ObjectId"], group: RoundTripGroup::DirectGameState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/types/game_state.rs::GameState::object_counter_placement_count_this_turn", map_key_types: &["ObjectId"], group: RoundTripGroup::DirectGameState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/types/game_state.rs::GameState::cards_exiled_with_source_this_turn", map_key_types: &["ObjectId"], group: RoundTripGroup::DirectGameState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/types/game_state.rs::GameState::first_card_drawn_this_turn", map_key_types: &["PlayerId"], group: RoundTripGroup::DirectGameState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/types/game_state.rs::GameState::cards_drawn_this_turn", map_key_types: &["PlayerId"], group: RoundTripGroup::DirectGameState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/types/game_state.rs::GameState::spells_cast_this_game", map_key_types: &["PlayerId"], group: RoundTripGroup::DirectGameState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/types/game_state.rs::GameState::spells_cast_this_game_by_player", map_key_types: &["PlayerId"], group: RoundTripGroup::DirectGameState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/types/game_state.rs::GameState::spells_cast_this_turn_by_player", map_key_types: &["PlayerId"], group: RoundTripGroup::DirectGameState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/types/game_state.rs::GameState::lands_played_this_turn_by_player", map_key_types: &["PlayerId"], group: RoundTripGroup::DirectGameState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/types/game_state.rs::GameState::attacking_creatures_this_turn", map_key_types: &["PlayerId"], group: RoundTripGroup::DirectGameState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/types/game_state.rs::GameState::attacked_defenders_this_turn", map_key_types: &["PlayerId"], group: RoundTripGroup::DirectGameState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/types/game_state.rs::GameState::creature_attacked_defenders_this_turn", map_key_types: &["ObjectId"], group: RoundTripGroup::DirectGameState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/types/game_state.rs::GameState::cards_discarded_this_turn_by_player", map_key_types: &["PlayerId"], group: RoundTripGroup::DirectGameState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/types/game_state.rs::GameState::mana_spent_on_spells_this_turn", map_key_types: &["PlayerId"], group: RoundTripGroup::DirectGameState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/types/game_state.rs::GameState::last_effect_counts_by_player", map_key_types: &["PlayerId"], group: RoundTripGroup::DirectGameState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/types/game_state.rs::GameState::stack_trigger_event_batches", map_key_types: &["ObjectId"], group: RoundTripGroup::DirectGameState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/types/game_state.rs::GameState::stack_trigger_firings", map_key_types: &["ObjectId"], group: RoundTripGroup::DirectGameState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/types/game_state.rs::GameState::lki_cache", map_key_types: &["ObjectId"], group: RoundTripGroup::DirectGameState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/types/game_state.rs::GameState::lki_copiable_values", map_key_types: &["ObjectId"], group: RoundTripGroup::DirectGameState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/types/game_state.rs::GameState::lki_by_incarnation", map_key_types: &["ObjectId", "u64"], group: RoundTripGroup::DirectGameState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/types/game_state.rs::GameState::linked_exile_lki", map_key_types: &["ObjectId"], group: RoundTripGroup::DirectGameState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/types/game_state.rs::GameState::ring_level", map_key_types: &["PlayerId"], group: RoundTripGroup::DirectGameState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/types/game_state.rs::GameState::ring_bearer", map_key_types: &["PlayerId"], group: RoundTripGroup::DirectGameState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/types/game_state.rs::GameState::dungeon_progress", map_key_types: &["PlayerId"], group: RoundTripGroup::DirectGameState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/types/game_state.rs::GameState::planar_die_actions_this_turn", map_key_types: &["PlayerId"], group: RoundTripGroup::DirectGameState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/game/combat.rs::CombatState::blocker_assignments", map_key_types: &["ObjectId"], group: RoundTripGroup::CombatState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/game/combat.rs::CombatState::blocker_to_attacker", map_key_types: &["ObjectId"], group: RoundTripGroup::CombatState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/game/combat.rs::CombatState::attacked_defenders_this_combat", map_key_types: &["PlayerId"], group: RoundTripGroup::CombatState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/game/combat.rs::CombatState::creature_attacked_defenders_this_combat", map_key_types: &["ObjectId"], group: RoundTripGroup::CombatState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/game/combat.rs::CombatState::damage_assignments", map_key_types: &["ObjectId"], group: RoundTripGroup::CombatState, numeric_deserializer: None },
    NumericRoundTripOwner { id: "src/types/game_state.rs::WaitingFor::DeclareAttackers::valid_attack_targets_by_attacker", map_key_types: &["ObjectId"], group: RoundTripGroup::DeclareAttackers, numeric_deserializer: Some(OPTION_NUMERIC_HASH_MAP_DESERIALIZER) },
    NumericRoundTripOwner { id: "src/types/game_state.rs::WaitingFor::DeclareAttackers::attacker_constraints", map_key_types: &["ObjectId"], group: RoundTripGroup::DeclareAttackers, numeric_deserializer: Some(NUMERIC_HASH_MAP_DESERIALIZER) },
    NumericRoundTripOwner { id: "src/types/game_state.rs::WaitingFor::DeclareBlockers::valid_block_targets", map_key_types: &["ObjectId"], group: RoundTripGroup::DeclareBlockers, numeric_deserializer: Some(NUMERIC_HASH_MAP_DESERIALIZER) },
    NumericRoundTripOwner { id: "src/types/game_state.rs::WaitingFor::DeclareBlockers::block_requirements", map_key_types: &["ObjectId"], group: RoundTripGroup::DeclareBlockers, numeric_deserializer: Some(NUMERIC_HASH_MAP_DESERIALIZER) },
    NumericRoundTripOwner { id: "src/types/game_state.rs::WaitingFor::DeclareBlockers::blocker_constraints", map_key_types: &["ObjectId"], group: RoundTripGroup::DeclareBlockers, numeric_deserializer: Some(NUMERIC_HASH_MAP_DESERIALIZER) },
];

fn owner_id(file: &str, owner: &str, variant: Option<&str>, field: &str) -> String {
    match variant {
        Some(variant) => format!("{file}::{owner}::{variant}::{field}"),
        None => format!("{file}::{owner}::{field}"),
    }
}

fn add_spec(
    specs: &mut BTreeMap<String, OwnerSpec>,
    file: &str,
    owner: &str,
    variant: Option<&str>,
    field: &'static str,
    shape: &'static str,
    classification: Classification,
) {
    let id = owner_id(file, owner, variant, field);
    assert!(
        specs
            .insert(
                id.clone(),
                OwnerSpec {
                    shape,
                    classification,
                    map_key_types: &[],
                    numeric_deserializer: None,
                },
            )
            .is_none(),
        "duplicate owner spec: {id}"
    );
}

fn expected_manifest() -> BTreeMap<String, OwnerSpec> {
    let mut specs = BTreeMap::new();
    let game_state = "src/types/game_state.rs";

    for field in [
        "commander_declined_zone_return",
        "objects_that_dealt_damage",
        "triggers_fired_this_turn",
        "triggers_fired_this_turn_per_opponent",
        "triggers_fired_this_game",
        "crew_activated_this_turn",
        "exerted_this_turn",
        "graveyard_cast_permissions_used",
        "graveyard_cast_permissions_used_per_type",
        "hand_cast_free_permissions_used",
        "alt_cost_grant_permissions_used",
        "exile_play_permissions_used",
        "exile_play_single_use_consumed",
        "exile_cast_permissions_used",
        "top_of_library_cast_permissions_used",
        "players_who_searched_library_this_turn",
        "players_attacked_this_step",
        "players_attacked_this_turn",
        "creatures_attacked_this_turn",
        "creatures_blocked_this_turn",
        "players_who_created_token_this_turn",
        "players_who_discarded_card_this_turn",
        "players_who_sacrificed_artifact_this_turn",
        "batched_zone_change_trigger_fired",
        "assassin_or_commander_dealt_combat_damage_this_turn",
        "modal_modes_chosen_this_turn",
        "modal_modes_chosen_this_game",
        "revealed_cards",
        "player_actions_this_way",
        "city_blessing",
    ] {
        add_spec(
            &mut specs,
            game_state,
            "GameState",
            None,
            field,
            "HashSet",
            Classification::Canonical(HASH_SET),
        );
    }
    add_spec(
        &mut specs,
        game_state,
        "GameState",
        None,
        "public_revealed_cards",
        "Box<HashSet>",
        Classification::Canonical(HASH_SET),
    );
    add_spec(
        &mut specs,
        game_state,
        "GameState",
        None,
        "enduring_story",
        "Box<HashSet>",
        Classification::Canonical(HASH_SET),
    );
    add_spec(
        &mut specs,
        game_state,
        "GameState",
        None,
        "post_replacement_token_choice_applied",
        "Option<HashSet>",
        Classification::Canonical(OPTION_HASH_SET),
    );
    add_spec(
        &mut specs,
        game_state,
        "GameState",
        None,
        "creature_types_dealt_combat_damage_this_turn",
        "im::HashSet",
        Classification::Canonical(IM_HASH_SET),
    );

    for field in [
        "stack_paid_facts",
        "liminal_entries",
        "tracked_object_sets",
        "commander_cast_count",
        "commander_cast_owners",
        "auto_pass",
        "phase_stops",
        "priority_passing_modes",
        "lands_tapped_for_mana",
        "prepaid_mulligan_bottoms",
        "loyalty_abilities_activated_this_turn",
        "extra_loyalty_activations_this_turn",
        "object_tap_count_this_turn",
        "object_counter_placement_count_this_turn",
        "cards_exiled_with_source_this_turn",
        "first_card_drawn_this_turn",
        "cards_drawn_this_turn",
        "spells_cast_this_game",
        "spells_cast_this_game_by_player",
        "spells_cast_this_turn_by_player",
        "lands_played_this_turn_by_player",
        "attacking_creatures_this_turn",
        "cards_discarded_this_turn_by_player",
        "mana_spent_on_spells_this_turn",
        "last_effect_counts_by_player",
        "stack_trigger_event_batches",
        "stack_trigger_firings",
        "lki_copiable_values",
        "linked_exile_lki",
        "ring_level",
        "ring_bearer",
        "dungeon_progress",
        "planar_die_actions_this_turn",
    ] {
        add_spec(
            &mut specs,
            game_state,
            "GameState",
            None,
            field,
            "HashMap",
            Classification::Canonical(HASH_MAP),
        );
    }
    add_spec(
        &mut specs,
        game_state,
        "GameState",
        None,
        "tracked_set_member_causes",
        "HashMap<HashMap>",
        Classification::Canonical(HASH_MAP_OF_HASH_MAP),
    );
    add_spec(
        &mut specs,
        game_state,
        "GameState",
        None,
        "steps_to_skip",
        "Vec<HashMap>",
        Classification::Canonical(VEC_HASH_MAP),
    );
    for field in [
        "attacked_defenders_this_turn",
        "creature_attacked_defenders_this_turn",
    ] {
        add_spec(
            &mut specs,
            game_state,
            "GameState",
            None,
            field,
            "HashMap<HashSet>",
            Classification::Canonical(HASH_MAP_OF_HASH_SET),
        );
    }
    for field in ["objects", "attribution", "lki_cache"] {
        add_spec(
            &mut specs,
            game_state,
            "GameState",
            None,
            field,
            "im::HashMap",
            Classification::Canonical(IM_HASH_MAP),
        );
    }
    add_spec(
        &mut specs,
        game_state,
        "GameState",
        None,
        "lki_by_incarnation",
        "im::HashMap<im::HashMap>",
        Classification::Canonical(IM_HASH_MAP_OF_IM_HASH_MAP),
    );
    for (field, adapter) in [
        (
            "trigger_fire_counts_this_turn",
            "with=\"trigger_definition_ref_map\"",
        ),
        ("activated_abilities_this_turn", "with=\"tuple_key_map\""),
        ("activated_abilities_this_game", "with=\"tuple_key_map\""),
        ("ability_resolutions_this_turn", "with=\"tuple_key_map\""),
    ] {
        add_spec(
            &mut specs,
            game_state,
            "GameState",
            None,
            field,
            "HashMap",
            Classification::Canonical(adapter),
        );
    }

    for field in [
        "static_gate_truth",
        "remote_type_layer_recipients",
        "card_face_registry",
        "meld_pair_registry",
        "momir_pool_faces",
        "pending_taps_for_mana_overrides",
        "combat_prevention_tally",
    ] {
        let shape = match field {
            "remote_type_layer_recipients" => "im::HashSet",
            "card_face_registry" | "meld_pair_registry" | "momir_pool_faces" => "Arc<HashMap>",
            "combat_prevention_tally" => "Option<HashMap>",
            "static_gate_truth" => "im::HashMap",
            _ => "HashMap",
        };
        add_spec(
            &mut specs,
            game_state,
            "GameState",
            None,
            field,
            shape,
            Classification::SerdeSkip,
        );
    }

    for (owner, field, shape) in [
        ("PublicStateDirty", "dirty_objects", "HashSet"),
        ("PublicStateDirty", "dirty_players", "HashSet"),
        ("TriggerIndex", "by_key", "im::HashMap"),
        ("ReplacementIndex", "by_event", "im::HashMap"),
    ] {
        add_spec(
            &mut specs,
            game_state,
            owner,
            None,
            field,
            shape,
            Classification::DeserializeOnlyOrRuntime,
        );
    }
    for owner in ["LKISnapshot", "CounterAddedRecord"] {
        add_spec(
            &mut specs,
            game_state,
            owner,
            None,
            "counters",
            "HashMap",
            Classification::Canonical("with=\"counter_map_serde\""),
        );
    }

    for (owner, variant, field, shape, adapter) in [
        (
            "WaitingFor",
            Some("DeclareAttackers"),
            "valid_attack_targets_by_attacker",
            "Option<HashMap>",
            OPTION_HASH_MAP,
        ),
        (
            "WaitingFor",
            Some("DeclareAttackers"),
            "attacker_constraints",
            "HashMap",
            HASH_MAP,
        ),
        (
            "WaitingFor",
            Some("DeclareBlockers"),
            "valid_block_targets",
            "HashMap",
            HASH_MAP,
        ),
        (
            "WaitingFor",
            Some("DeclareBlockers"),
            "block_requirements",
            "HashMap",
            HASH_MAP,
        ),
        (
            "WaitingFor",
            Some("DeclareBlockers"),
            "blocker_constraints",
            "HashMap",
            HASH_MAP,
        ),
        (
            "WaitingFor",
            Some("ChooseOneOfBranch"),
            "replacement_applied",
            "HashSet",
            HASH_SET,
        ),
        (
            "PendingChooseOneOf",
            None,
            "replacement_applied",
            "HashSet",
            HASH_SET,
        ),
        (
            "PendingBatchDeliveries",
            None,
            "replacement_applied",
            "HashSet",
            HASH_SET,
        ),
        (
            "PendingBatchZoneChangeCause",
            Some("Draw"),
            "seed_applied",
            "HashSet",
            HASH_SET,
        ),
        (
            "PendingBatchZoneMoveRequest",
            None,
            "replacement_applied",
            "HashSet",
            HASH_SET,
        ),
        (
            "LiminalEntry",
            None,
            "replacement_applied",
            "HashSet",
            HASH_SET,
        ),
        (
            "PendingConniveReentry",
            None,
            "applied",
            "HashSet",
            HASH_SET,
        ),
        ("PostReplacementDrain", None, "applied", "HashSet", HASH_SET),
        ("DrawSequenceFrame", None, "applied", "HashSet", HASH_SET),
    ] {
        add_spec(
            &mut specs,
            game_state,
            owner,
            variant,
            field,
            shape,
            Classification::Canonical(adapter),
        );
    }

    for (file, owner, variant, field, shape, classification) in [
        (
            "src/types/player.rs",
            "Player",
            None,
            "bending_types_this_turn",
            "HashSet",
            Classification::Canonical(HASH_SET),
        ),
        (
            "src/types/player.rs",
            "Player",
            None,
            "player_counters",
            "HashMap",
            Classification::Canonical(HASH_MAP),
        ),
        (
            "src/types/events.rs",
            "EventObjectSnapshot",
            None,
            "counters",
            "HashMap",
            Classification::Canonical("with=\"crate::types::counter::counter_map_serde\""),
        ),
        (
            "src/types/ability.rs",
            "ResolvedAbility",
            None,
            "replacement_applied",
            "HashSet",
            Classification::Canonical(HASH_SET),
        ),
        (
            "src/types/resolution.rs",
            "ChangeZoneFrame",
            None,
            "devour_eligible_snapshot",
            "Option<HashSet>",
            Classification::Canonical(OPTION_HASH_SET),
        ),
        (
            "src/types/resolution.rs",
            "LegacyChangeZoneWire",
            None,
            "devour_eligible_snapshot",
            "Option<HashSet>",
            Classification::DeserializeOnlyOrRuntime,
        ),
        (
            "src/types/resolution.rs",
            "LegacyReplacementTailsWire",
            None,
            "post_replacement_applied",
            "HashSet",
            Classification::DeserializeOnlyOrRuntime,
        ),
        (
            "src/game/game_object.rs",
            "GameObject",
            None,
            "protection_start_exempt_attachments",
            "HashMap",
            Classification::NonStringJsonMapKey,
        ),
        (
            "src/game/game_object.rs",
            "GameObject",
            None,
            "counters",
            "HashMap",
            Classification::Canonical("with=\"counter_map_serde\""),
        ),
        (
            "src/game/game_object.rs",
            "GameObject",
            None,
            "specialize_faces",
            "Option<HashMap>",
            Classification::Canonical(OPTION_HASH_MAP),
        ),
        (
            "src/game/game_object.rs",
            "GameObject",
            None,
            "goaded_by",
            "HashSet",
            Classification::Canonical(HASH_SET),
        ),
        (
            "src/game/game_object.rs",
            "GameObject",
            None,
            "detained_by",
            "HashSet",
            Classification::Canonical(HASH_SET),
        ),
        (
            "src/game/combat.rs",
            "CombatState",
            None,
            "blocker_assignments",
            "HashMap",
            Classification::Canonical(HASH_MAP),
        ),
        (
            "src/game/combat.rs",
            "CombatState",
            None,
            "blocker_to_attacker",
            "HashMap",
            Classification::Canonical(HASH_MAP),
        ),
        (
            "src/game/combat.rs",
            "CombatState",
            None,
            "attacked_defenders_this_combat",
            "HashMap<HashSet>",
            Classification::Canonical(HASH_MAP_OF_HASH_SET),
        ),
        (
            "src/game/combat.rs",
            "CombatState",
            None,
            "creature_attacked_defenders_this_combat",
            "HashMap<HashSet>",
            Classification::Canonical(HASH_MAP_OF_HASH_SET),
        ),
        (
            "src/game/combat.rs",
            "CombatState",
            None,
            "attacking_incarnations_this_combat",
            "HashSet",
            Classification::Canonical(HASH_SET),
        ),
        (
            "src/game/combat.rs",
            "CombatState",
            None,
            "damage_assignments",
            "HashMap",
            Classification::Canonical(HASH_MAP),
        ),
        (
            "src/game/combat.rs",
            "CombatState",
            None,
            "first_strike_participants",
            "Option<HashSet>",
            Classification::Canonical(OPTION_HASH_SET),
        ),
        (
            "src/game/combat.rs",
            "AttackDeclarationConstraints",
            None,
            "legal_targets",
            "HashMap",
            Classification::DeserializeOnlyOrRuntime,
        ),
        (
            "src/game/combat.rs",
            "AttackDeclarationConstraints",
            None,
            "needs_companion",
            "HashSet",
            Classification::DeserializeOnlyOrRuntime,
        ),
        (
            "src/game/combat.rs",
            "AttackDeclarationConstraints",
            None,
            "must_be_sole",
            "HashSet",
            Classification::DeserializeOnlyOrRuntime,
        ),
        (
            "src/game/zone_pipeline.rs",
            "ZoneChangeCause",
            Some("Draw"),
            "seed_applied",
            "HashSet",
            Classification::DeserializeOnlyOrRuntime,
        ),
        (
            "src/game/zone_pipeline.rs",
            "ZoneMoveRequest",
            None,
            "replacement_applied",
            "HashSet",
            Classification::DeserializeOnlyOrRuntime,
        ),
        (
            "src/game/effects/choose_one_of.rs",
            "PromptRequest",
            None,
            "replacement_applied",
            "HashSet",
            Classification::DeserializeOnlyOrRuntime,
        ),
        (
            "src/game/effects/choose_one_of.rs",
            "BranchSelection",
            None,
            "replacement_applied",
            "HashSet",
            Classification::DeserializeOnlyOrRuntime,
        ),
    ] {
        add_spec(
            &mut specs,
            file,
            owner,
            variant,
            field,
            shape,
            classification,
        );
    }

    for variant in [
        "ZoneChange",
        "Damage",
        "Draw",
        "SearchFound",
        "Scry",
        "Mill",
        "CoinFlip",
        "Explore",
        "Connive",
        "Proliferate",
        "LifeGain",
        "LifeLoss",
        "AddCounter",
        "RemoveCounter",
        "MoveCounter",
        "CreateToken",
        "TokenEntry",
        "Discard",
        "Tap",
        "Untap",
        "TurnFaceUp",
        "Destroy",
        "Sacrifice",
        "BeginTurn",
        "BeginPhase",
        "ProduceMana",
        "EmptyManaPool",
        "Planeswalk",
        "Attach",
    ] {
        add_spec(
            &mut specs,
            "src/types/proposed_event.rs",
            "ProposedEvent",
            Some(variant),
            "applied",
            "HashSet",
            Classification::Canonical(HASH_SET),
        );
    }

    for owner in NUMERIC_MAP_ROUND_TRIP_OWNERS {
        let spec = specs.get_mut(owner.id).unwrap_or_else(|| {
            panic!(
                "numeric round-trip owner missing from manifest: {}",
                owner.id
            )
        });
        spec.map_key_types = owner.map_key_types;
        spec.numeric_deserializer = owner.numeric_deserializer;
    }

    specs
}

fn first_type_argument(segment: &syn::PathSegment) -> Option<&Type> {
    let PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return None;
    };
    arguments.args.iter().find_map(|argument| match argument {
        GenericArgument::Type(ty) => Some(ty),
        _ => None,
    })
}

fn nth_type_argument(segment: &syn::PathSegment, index: usize) -> Option<&Type> {
    let PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return None;
    };
    arguments
        .args
        .iter()
        .filter_map(|argument| match argument {
            GenericArgument::Type(ty) => Some(ty),
            _ => None,
        })
        .nth(index)
}

fn type_label(ty: &Type) -> String {
    match ty {
        Type::Path(path) => path.path.segments.last().map_or_else(
            || "<empty-path>".to_string(),
            |segment| segment.ident.to_string(),
        ),
        Type::Tuple(tuple) => format!(
            "({})",
            tuple
                .elems
                .iter()
                .map(type_label)
                .collect::<Vec<_>>()
                .join(",")
        ),
        _ => "<complex-type>".to_string(),
    }
}

fn unordered_map_key_types(ty: &Type) -> Vec<String> {
    let Type::Path(path) = ty else {
        return Vec::new();
    };
    let Some(segment) = path.path.segments.last() else {
        return Vec::new();
    };
    match segment.ident.to_string().as_str() {
        "Option" | "Vec" | "Arc" | "Box" => first_type_argument(segment)
            .map(unordered_map_key_types)
            .unwrap_or_default(),
        "HashMap" => {
            let mut keys = nth_type_argument(segment, 0)
                .map(type_label)
                .into_iter()
                .collect::<Vec<_>>();
            if let Some(value) = nth_type_argument(segment, 1) {
                keys.extend(unordered_map_key_types(value));
            }
            keys
        }
        "SpecializeFaceMap" => vec!["ManaColor".to_string()],
        _ => Vec::new(),
    }
}

fn hash_shape(ty: &Type) -> Option<String> {
    let Type::Path(path) = ty else {
        return None;
    };
    let segment = path.path.segments.last()?;
    let name = segment.ident.to_string();
    match name.as_str() {
        "Option" | "Vec" | "Arc" | "Box" => {
            hash_shape(first_type_argument(segment)?).map(|inner| format!("{name}<{inner}>"))
        }
        "HashSet" => Some(
            if path.path.segments.iter().any(|part| part.ident == "im") {
                "im::HashSet".to_string()
            } else {
                "HashSet".to_string()
            },
        ),
        "HashMap" => {
            let prefix = if path.path.segments.iter().any(|part| part.ident == "im") {
                "im::HashMap"
            } else {
                "HashMap"
            };
            Some(match nth_type_argument(segment, 1).and_then(hash_shape) {
                Some(inner) => format!("{prefix}<{inner}>"),
                None => prefix.to_string(),
            })
        }
        "SpecializeFaceMap" => Some("HashMap".to_string()),
        _ => None,
    }
}

fn serde_metadata(attributes: &[syn::Attribute]) -> String {
    attributes
        .iter()
        .filter(|attribute| attribute.path().is_ident("serde"))
        .filter_map(|attribute| match &attribute.meta {
            syn::Meta::List(list) => Some(list.tokens.to_string().replace(' ', "")),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(";")
}

fn discover_fields(
    discovered: &mut BTreeMap<String, DiscoveredOwner>,
    file: &str,
    owner: &str,
    variant: Option<&str>,
    fields: &Fields,
) {
    let Fields::Named(fields) = fields else {
        return;
    };
    for field in &fields.named {
        let Some(shape) = hash_shape(&field.ty) else {
            continue;
        };
        let field_name = field.ident.as_ref().expect("named field").to_string();
        let id = owner_id(file, owner, variant, &field_name);
        let serde = serde_metadata(&field.attrs);
        let map_key_types = unordered_map_key_types(&field.ty);
        assert!(
            discovered
                .insert(
                    id.clone(),
                    DiscoveredOwner {
                        shape,
                        serde,
                        map_key_types,
                    },
                )
                .is_none(),
            "duplicate discovered owner: {id}"
        );
    }
}

fn discover_file(discovered: &mut BTreeMap<String, DiscoveredOwner>, file: &str, source: &str) {
    let syntax = syn::parse_file(source).unwrap_or_else(|error| panic!("parse {file}: {error}"));
    for item in syntax.items {
        match item {
            Item::Struct(item) => discover_fields(
                discovered,
                file,
                &item.ident.to_string(),
                None,
                &item.fields,
            ),
            Item::Enum(item) => {
                let owner = item.ident.to_string();
                for variant in item.variants {
                    discover_fields(
                        discovered,
                        file,
                        &owner,
                        Some(&variant.ident.to_string()),
                        &variant.fields,
                    );
                }
            }
            Item::Macro(item)
                if item
                    .mac
                    .path
                    .segments
                    .last()
                    .is_some_and(|segment| segment.ident == "declare_game_state") =>
            {
                let synthetic = format!("struct GameState {{ {} }}", item.mac.tokens);
                let game_state: syn::ItemStruct = syn::parse_str(&synthetic)
                    .unwrap_or_else(|error| panic!("parse declare_game_state fields: {error}"));
                discover_fields(discovered, file, "GameState", None, &game_state.fields);
            }
            _ => {}
        }
    }
}

fn discovered_manifest() -> BTreeMap<String, DiscoveredOwner> {
    let mut discovered = BTreeMap::new();
    for (file, source) in [
        (
            "src/types/game_state.rs",
            include_str!("../../src/types/game_state.rs"),
        ),
        (
            "src/types/player.rs",
            include_str!("../../src/types/player.rs"),
        ),
        (
            "src/types/events.rs",
            include_str!("../../src/types/events.rs"),
        ),
        (
            "src/types/ability.rs",
            include_str!("../../src/types/ability.rs"),
        ),
        (
            "src/types/proposed_event.rs",
            include_str!("../../src/types/proposed_event.rs"),
        ),
        (
            "src/types/resolution.rs",
            include_str!("../../src/types/resolution.rs"),
        ),
        (
            "src/game/game_object.rs",
            include_str!("../../src/game/game_object.rs"),
        ),
        (
            "src/game/combat.rs",
            include_str!("../../src/game/combat.rs"),
        ),
        (
            "src/game/zone_pipeline.rs",
            include_str!("../../src/game/zone_pipeline.rs"),
        ),
        (
            "src/game/effects/choose_one_of.rs",
            include_str!("../../src/game/effects/choose_one_of.rs"),
        ),
    ] {
        discover_file(&mut discovered, file, source);
    }
    discovered
}

#[test]
fn serde_hash_owner_census_is_exhaustive_and_every_canonical_owner_names_its_adapter() {
    let expected = expected_manifest();
    let discovered = discovered_manifest();
    let expected_ids: BTreeSet<_> = expected.keys().cloned().collect();
    let discovered_ids: BTreeSet<_> = discovered.keys().cloned().collect();

    let missing = expected_ids.difference(&discovered_ids).collect::<Vec<_>>();
    let unexpected = discovered_ids
        .difference(&expected_ids)
        .map(|id| {
            let owner = &discovered[id];
            format!("{id} shape={} serde={}", owner.shape, owner.serde)
        })
        .collect::<Vec<_>>();
    assert!(
        missing.is_empty() && unexpected.is_empty(),
        "serde hash owner census mismatch\nmissing expected owners: {missing:#?}\nunclassified discovered owners: {unexpected:#?}"
    );

    let numeric_key_types = ["ObjectId", "PlayerId", "TrackedSetId", "u64"];
    let discovered_numeric_ids: BTreeSet<_> = discovered
        .iter()
        .filter(|(_, owner)| {
            !owner.map_key_types.is_empty()
                && owner
                    .map_key_types
                    .iter()
                    .all(|key| numeric_key_types.contains(&key.as_str()))
        })
        .filter(|(id, _)| {
            matches!(
                expected[id.as_str()].classification,
                Classification::Canonical(_)
            )
        })
        .map(|(id, _)| id.clone())
        .collect();
    let expected_numeric_ids: BTreeSet<_> = NUMERIC_MAP_ROUND_TRIP_OWNERS
        .iter()
        .map(|owner| owner.id.to_string())
        .collect();
    assert_eq!(
        discovered_numeric_ids, expected_numeric_ids,
        "numeric map owners and explicit populated round-trip assignments diverged"
    );

    for (id, spec) in expected {
        let actual = &discovered[&id];
        assert_eq!(
            actual.shape, spec.shape,
            "{id}: discovered hash shape changed; classification={:?}, serde={}",
            spec.classification, actual.serde
        );
        if !spec.map_key_types.is_empty() {
            assert_eq!(
                actual.map_key_types, spec.map_key_types,
                "{id}: numeric map key type path changed"
            );
            match spec.numeric_deserializer {
                Some(adapter) => assert!(
                    actual.serde.contains(adapter),
                    "{id}: missing numeric map-key deserializer {adapter}; actual serde={}",
                    actual.serde
                ),
                None => assert!(
                    !actual.serde.contains(NUMERIC_HASH_MAP_DESERIALIZER)
                        && !actual.serde.contains(OPTION_NUMERIC_HASH_MAP_DESERIALIZER),
                    "{id}: field-local numeric deserializer is not justified here; actual serde={}",
                    actual.serde
                ),
            }
        }
        match spec.classification {
            Classification::Canonical(adapter) => assert!(
                actual.serde.contains(adapter),
                "{id}: shape={} classification={:?}; missing serde adapter {adapter}; actual serde={}",
                actual.shape,
                spec.classification,
                actual.serde
            ),
            Classification::SerdeSkip => assert!(
                actual.serde.split(';').any(|attribute| {
                    attribute == "skip"
                        || attribute.split(',').any(|part| part == "skip")
                }),
                "{id}: shape={} classification={:?}; expected serde(skip); actual serde={}",
                actual.shape,
                spec.classification,
                actual.serde
            ),
            Classification::DeserializeOnlyOrRuntime => {}
            Classification::NonStringJsonMapKey => assert!(
                !actual.serde.contains("deterministic_serde"),
                "{id}: the non-string JSON key owner must not invent a generic wire adapter; actual serde={}",
                actual.serde
            ),
        }
    }

    assert_eq!(
        NUMERIC_MAP_ROUND_TRIP_OWNERS.len(),
        50,
        "the reviewed numeric-map owner matrix must remain exact"
    );
    for group in [
        RoundTripGroup::DirectGameState,
        RoundTripGroup::CombatState,
        RoundTripGroup::DeclareAttackers,
        RoundTripGroup::DeclareBlockers,
    ] {
        assert!(
            NUMERIC_MAP_ROUND_TRIP_OWNERS
                .iter()
                .any(|owner| owner.group == group),
            "numeric round-trip group {group:?} has no assigned owner"
        );
    }
}

fn back_face(name: &str) -> BackFaceData {
    BackFaceData {
        name: name.to_string(),
        power: None,
        toughness: None,
        loyalty: None,
        printed_loyalty: None,
        defense: None,
        card_types: CardType::default(),
        mana_cost: ManaCost::default(),
        keywords: Vec::new(),
        abilities: Vec::new(),
        trigger_definitions: Definitions::default(),
        replacement_definitions: Definitions::default(),
        static_definitions: Definitions::default(),
        color: Vec::new(),
        printed_ref: None,
        modal: None,
        additional_cost: None,
        strive_cost: None,
        casting_restrictions: Vec::new(),
        casting_options: Vec::new(),
        layout_kind: None,
    }
}

fn field_fragment<'a>(serialized: &'a str, field: &str, next_field: &str) -> &'a str {
    let start_marker = format!("\"{field}\":");
    let next_marker = format!(",\"{next_field}\":");
    let start = serialized
        .find(&start_marker)
        .unwrap_or_else(|| panic!("serialized state lacks {field}"));
    let remainder = &serialized[start..];
    let end = remainder
        .find(&next_marker)
        .unwrap_or_else(|| panic!("serialized state lacks field after {field}: {next_field}"));
    &remainder[..end]
}

fn build_populated_state(reverse: bool) -> GameState {
    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    let player_order = if reverse {
        [PlayerId(1), PlayerId(0)]
    } else {
        [PlayerId(0), PlayerId(1)]
    };
    let mut token_creator_order = (0_u8..32).map(PlayerId).collect::<Vec<_>>();
    if reverse {
        token_creator_order.reverse();
    }
    state.players_who_created_token_this_turn = token_creator_order.into_iter().collect();
    state.enduring_story = Box::new(player_order.iter().copied().collect());
    state.ring_level = player_order
        .into_iter()
        .map(|player| (player, player.0 + 1))
        .collect();

    let inner_order = if reverse {
        [PlayerId(1), PlayerId(0)]
    } else {
        [PlayerId(0), PlayerId(1)]
    };
    let outer_order = if reverse {
        [PlayerId(1), PlayerId(0)]
    } else {
        [PlayerId(0), PlayerId(1)]
    };
    state.attacked_defenders_this_turn = outer_order
        .into_iter()
        .map(|player| (player, inner_order.into_iter().collect()))
        .collect();
    state.steps_to_skip = vec![
        [
            (engine::types::Phase::PostCombatMain, 2),
            (engine::types::Phase::PreCombatMain, 1),
        ]
        .into_iter()
        .collect(),
        [(engine::types::Phase::End, 3)].into_iter().collect(),
    ];
    state.player_actions_this_turn = vec![
        (PlayerId(1), PlayerActionKind::Scry),
        (PlayerId(0), PlayerActionKind::SearchedLibrary),
    ];

    let mut first = GameObject::new(
        ObjectId(1),
        CardId(1),
        PlayerId(0),
        "First Deterministic Object".to_string(),
        Zone::Battlefield,
    );
    first.goaded_by = inner_order.into_iter().collect();
    let mut specialize_faces = HashMap::new();
    if reverse {
        specialize_faces.insert(ManaColor::Blue, back_face("Blue Face"));
        specialize_faces.insert(ManaColor::White, back_face("White Face"));
    } else {
        specialize_faces.insert(ManaColor::White, back_face("White Face"));
        specialize_faces.insert(ManaColor::Blue, back_face("Blue Face"));
    }
    first.specialize_faces = Some(specialize_faces);
    let second = GameObject::new(
        ObjectId(2),
        CardId(2),
        PlayerId(1),
        "Second Deterministic Object".to_string(),
        Zone::Battlefield,
    );
    if reverse {
        state.objects.insert(ObjectId(2), second);
        state.objects.insert(ObjectId(1), first);
    } else {
        state.objects.insert(ObjectId(1), first);
        state.objects.insert(ObjectId(2), second);
    }

    let mut combat = CombatState::default();
    let assignments = [
        (
            ObjectId(1),
            vec![DamageAssignment {
                target: DamageTarget::Player(PlayerId(1)),
                amount: 1,
            }],
        ),
        (
            ObjectId(2),
            vec![DamageAssignment {
                target: DamageTarget::Player(PlayerId(0)),
                amount: 2,
            }],
        ),
    ];
    for (key, value) in if reverse {
        assignments.into_iter().rev().collect::<Vec<_>>()
    } else {
        assignments.into_iter().collect()
    } {
        combat.damage_assignments.insert(key, value);
    }
    state.combat = Some(combat);

    let constraints = [
        (
            ObjectId(1),
            CombatRequirement::CantAttack {
                sources: Vec::new(),
            },
        ),
        (
            ObjectId(2),
            CombatRequirement::CantAttack {
                sources: Vec::new(),
            },
        ),
    ];
    state.waiting_for = WaitingFor::DeclareAttackers {
        player: PlayerId(0),
        valid_attacker_ids: vec![ObjectId(2), ObjectId(1)],
        valid_attack_targets: Vec::new(),
        valid_attack_targets_by_attacker: None,
        attacker_constraints: if reverse {
            constraints.into_iter().rev().collect()
        } else {
            constraints.into_iter().collect()
        },
    };

    let conniver = state
        .capture_connive_subject(ObjectId(1))
        .expect("fixture object should produce a connive snapshot");
    let applied_order = if reverse { [2, 1] } else { [1, 2] };
    state.push_connive_reentry(PendingConniveReentry {
        conniver,
        count: 2,
        applied: applied_order
            .into_iter()
            .map(AppliedReplacementKey::floating)
            .collect(),
    });

    state
}

fn assert_representative_membership(state: &GameState) {
    assert_eq!(state.players_who_created_token_this_turn.len(), 32);
    for player in (0_u8..32).map(PlayerId) {
        assert!(state.players_who_created_token_this_turn.contains(&player));
    }
    assert_eq!(state.ring_level.len(), 2);
    assert_eq!(state.enduring_story.len(), 2);
    assert_eq!(state.attacked_defenders_this_turn.len(), 2);
    assert_eq!(state.steps_to_skip.len(), 2);
    assert_eq!(state.objects.len(), 2);
    assert_eq!(
        state
            .objects
            .get(&ObjectId(1))
            .and_then(|object| object.specialize_faces.as_ref())
            .map(HashMap::len),
        Some(2)
    );
    assert_eq!(
        state
            .combat
            .as_ref()
            .map(|combat| combat.damage_assignments.len()),
        Some(2)
    );
    assert_eq!(
        state
            .active_connive_reentry()
            .map(|pending| pending.applied.len()),
        Some(2)
    );
}

fn build_all_direct_numeric_maps_state() -> GameState {
    let mut state = build_populated_state(false);
    let first = state.objects[&ObjectId(1)].clone();
    let second = state.objects[&ObjectId(2)].clone();

    state.stack_paid_facts = HashMap::from([
        (
            ObjectId(1),
            StackPaidSnapshot {
                actual_mana_spent: 1,
                ..Default::default()
            },
        ),
        (
            ObjectId(2),
            StackPaidSnapshot {
                actual_mana_spent: 2,
                ..Default::default()
            },
        ),
    ]);
    state.liminal_entries = HashMap::from([
        (
            ObjectId(1),
            LiminalEntry {
                object: first.clone(),
                name: "First liminal".to_string(),
                source_id: ObjectId(11),
                controller: PlayerId(0),
                enters_attacking: false,
                attach_to: None,
                sacrifice_at: None,
                remaining_count: 1,
                created_ids: vec![ObjectId(101)],
                copy_resume: None,
                spec_resume: None,
                enter_tapped: EtbTapState::Unspecified,
                enter_with_counters: Vec::new(),
                kind: Default::default(),
                replacement_applied: Default::default(),
            },
        ),
        (
            ObjectId(2),
            LiminalEntry {
                object: second.clone(),
                name: "Second liminal".to_string(),
                source_id: ObjectId(22),
                controller: PlayerId(1),
                enters_attacking: false,
                attach_to: None,
                sacrifice_at: None,
                remaining_count: 2,
                created_ids: vec![ObjectId(202)],
                copy_resume: None,
                spec_resume: None,
                enter_tapped: EtbTapState::Tapped,
                enter_with_counters: Vec::new(),
                kind: Default::default(),
                replacement_applied: Default::default(),
            },
        ),
    ]);
    state.attribution = im::HashMap::from_iter([
        (ObjectId(1), ObjectAttribution::default()),
        (ObjectId(2), ObjectAttribution::default()),
    ]);
    state.tracked_object_sets = HashMap::from([
        (TrackedSetId(1), vec![ObjectId(11)]),
        (TrackedSetId(2), vec![ObjectId(22), ObjectId(23)]),
    ]);
    state.tracked_set_member_causes = HashMap::from([
        (
            TrackedSetId(1),
            HashMap::from([
                (ObjectId(11), ThisWayCause::Exiled),
                (ObjectId(12), ThisWayCause::Sacrificed),
            ]),
        ),
        (
            TrackedSetId(2),
            HashMap::from([
                (ObjectId(21), ThisWayCause::Destroyed),
                (ObjectId(22), ThisWayCause::Milled),
            ]),
        ),
    ]);
    state.commander_cast_count = HashMap::from([(ObjectId(1), 1), (ObjectId(2), 2)]);
    state.commander_cast_owners =
        HashMap::from([(ObjectId(1), PlayerId(0)), (ObjectId(2), PlayerId(1))]);
    state.auto_pass = HashMap::from([
        (
            PlayerId(0),
            AutoPassMode::UntilStackEmpty {
                initial_stack_len: 1,
            },
        ),
        (
            PlayerId(1),
            AutoPassMode::UntilStackEmpty {
                initial_stack_len: 2,
            },
        ),
    ]);
    state.phase_stops = HashMap::from([
        (
            PlayerId(0),
            vec![PhaseStop {
                phase: Phase::Upkeep,
                scope: PhaseStopScope::OwnTurn,
            }],
        ),
        (
            PlayerId(1),
            vec![PhaseStop {
                phase: Phase::End,
                scope: PhaseStopScope::OpponentsTurns,
            }],
        ),
    ]);
    state.priority_passing_modes = HashMap::from([
        (PlayerId(0), PriorityPassingMode::Standard),
        (PlayerId(1), PriorityPassingMode::SkipLowUseWindows),
    ]);
    state.lands_tapped_for_mana = HashMap::from([
        (PlayerId(0), vec![ObjectId(11)]),
        (PlayerId(1), vec![ObjectId(21), ObjectId(22)]),
    ]);
    state.prepaid_mulligan_bottoms = HashMap::from([(PlayerId(0), 1), (PlayerId(1), 2)]);
    state.loyalty_abilities_activated_this_turn =
        HashMap::from([(PlayerId(0), 1), (PlayerId(1), 2)]);
    state.extra_loyalty_activations_this_turn = HashMap::from([(PlayerId(0), 2), (PlayerId(1), 3)]);
    state.object_tap_count_this_turn = HashMap::from([(ObjectId(1), 1), (ObjectId(2), 2)]);
    state.object_counter_placement_count_this_turn =
        HashMap::from([(ObjectId(1), 3), (ObjectId(2), 4)]);
    state.cards_exiled_with_source_this_turn = HashMap::from([
        (ObjectId(1), vec![ObjectId(11)]),
        (ObjectId(2), vec![ObjectId(21), ObjectId(22)]),
    ]);
    state.first_card_drawn_this_turn =
        HashMap::from([(PlayerId(0), ObjectId(11)), (PlayerId(1), ObjectId(22))]);
    state.cards_drawn_this_turn = HashMap::from([
        (PlayerId(0), vec![ObjectId(11)]),
        (PlayerId(1), vec![ObjectId(21), ObjectId(22)]),
    ]);
    state.spells_cast_this_game = HashMap::from([(PlayerId(0), 1), (PlayerId(1), 2)]);
    let first_spell = SpellCastRecord {
        name: "First spell".to_string(),
        mana_value: 1,
        ..Default::default()
    };
    let second_spell = SpellCastRecord {
        name: "Second spell".to_string(),
        mana_value: 2,
        ..Default::default()
    };
    state.spells_cast_this_game_by_player = HashMap::from([
        (PlayerId(0), im::Vector::from(vec![first_spell.clone()])),
        (
            PlayerId(1),
            im::Vector::from(vec![first_spell.clone(), second_spell.clone()]),
        ),
    ]);
    state.spells_cast_this_turn_by_player = HashMap::from([
        (PlayerId(0), im::Vector::from(vec![first_spell])),
        (PlayerId(1), im::Vector::from(vec![second_spell])),
    ]);
    state.lands_played_this_turn_by_player = HashMap::from([
        (
            PlayerId(0),
            im::Vector::from(vec![LandPlayRecord {
                from_zone: Zone::Hand,
            }]),
        ),
        (
            PlayerId(1),
            im::Vector::from(vec![LandPlayRecord {
                from_zone: Zone::Exile,
            }]),
        ),
    ]);
    state.attacking_creatures_this_turn = HashMap::from([(PlayerId(0), 1), (PlayerId(1), 2)]);
    state.attacked_defenders_this_turn = HashMap::from([
        (
            PlayerId(0),
            [PlayerId(1), PlayerId(0)].into_iter().collect(),
        ),
        (
            PlayerId(1),
            [PlayerId(0), PlayerId(1)].into_iter().collect(),
        ),
    ]);
    state.creature_attacked_defenders_this_turn = HashMap::from([
        (
            ObjectId(1),
            [PlayerId(1), PlayerId(0)].into_iter().collect(),
        ),
        (
            ObjectId(2),
            [PlayerId(0), PlayerId(1)].into_iter().collect(),
        ),
    ]);
    state.cards_discarded_this_turn_by_player = HashMap::from([(PlayerId(0), 1), (PlayerId(1), 2)]);
    state.mana_spent_on_spells_this_turn = HashMap::from([(PlayerId(0), 3), (PlayerId(1), 4)]);
    state.last_effect_counts_by_player = HashMap::from([(PlayerId(0), -1), (PlayerId(1), 2)]);
    state.stack_trigger_event_batches = HashMap::from([
        (
            ObjectId(1),
            vec![GameEvent::PriorityPassed {
                player_id: PlayerId(0),
            }],
        ),
        (
            ObjectId(2),
            vec![
                GameEvent::PriorityPassed {
                    player_id: PlayerId(1),
                },
                GameEvent::PriorityPassed {
                    player_id: PlayerId(0),
                },
            ],
        ),
    ]);
    let first_lki = first.snapshot_public_characteristics();
    let second_lki = second.snapshot_public_characteristics();
    state.lki_cache = im::HashMap::from_iter([
        (ObjectId(1), first_lki.clone()),
        (ObjectId(2), second_lki.clone()),
    ]);
    state.lki_copiable_values = HashMap::from([
        (ObjectId(1), intrinsic_copiable_values(&first)),
        (ObjectId(2), intrinsic_copiable_values(&second)),
    ]);
    state.lki_by_incarnation = im::HashMap::from_iter([
        (
            ObjectId(1),
            im::HashMap::from_iter([(1, first_lki.clone()), (2, second_lki.clone())]),
        ),
        (
            ObjectId(2),
            im::HashMap::from_iter([(1, second_lki.clone()), (2, first_lki.clone())]),
        ),
    ]);
    state.linked_exile_lki = HashMap::from([
        (
            ObjectId(1),
            vec![LinkedExileSnapshot {
                exiled_id: ObjectId(11),
                owner: PlayerId(0),
                mana_value: 1,
            }],
        ),
        (
            ObjectId(2),
            vec![LinkedExileSnapshot {
                exiled_id: ObjectId(22),
                owner: PlayerId(1),
                mana_value: 2,
            }],
        ),
    ]);
    state.ring_level = HashMap::from([(PlayerId(0), 1), (PlayerId(1), 2)]);
    state.ring_bearer = HashMap::from([
        (PlayerId(0), Some(ObjectId(1))),
        (PlayerId(1), Some(ObjectId(2))),
    ]);
    state.dungeon_progress = HashMap::from([
        (
            PlayerId(0),
            DungeonProgress {
                current_room: 1,
                ..Default::default()
            },
        ),
        (
            PlayerId(1),
            DungeonProgress {
                current_room: 2,
                ..Default::default()
            },
        ),
    ]);
    state.planar_die_actions_this_turn = HashMap::from([(PlayerId(0), 1), (PlayerId(1), 2)]);

    state
}

#[test]
fn every_direct_numeric_key_game_state_map_round_trips_populated() {
    let state = build_all_direct_numeric_maps_state();
    let serialized = serde_json::to_string(&state).expect("populated state should serialize");
    let before: serde_json::Value = serde_json::from_str(&serialized).unwrap();
    let restored: GameState =
        serde_json::from_str(&serialized).expect("every direct numeric map should restore");
    let reserialized = serde_json::to_string(&restored).expect("restored state should serialize");
    let after: serde_json::Value = serde_json::from_str(&reserialized).unwrap();

    let direct_fields = [
        "objects",
        "stack_paid_facts",
        "liminal_entries",
        "attribution",
        "tracked_object_sets",
        "tracked_set_member_causes",
        "commander_cast_count",
        "commander_cast_owners",
        "auto_pass",
        "phase_stops",
        "priority_passing_modes",
        "lands_tapped_for_mana",
        "prepaid_mulligan_bottoms",
        "loyalty_abilities_activated_this_turn",
        "extra_loyalty_activations_this_turn",
        "object_tap_count_this_turn",
        "object_counter_placement_count_this_turn",
        "cards_exiled_with_source_this_turn",
        "first_card_drawn_this_turn",
        "cards_drawn_this_turn",
        "spells_cast_this_game",
        "spells_cast_this_game_by_player",
        "spells_cast_this_turn_by_player",
        "lands_played_this_turn_by_player",
        "attacking_creatures_this_turn",
        "attacked_defenders_this_turn",
        "creature_attacked_defenders_this_turn",
        "cards_discarded_this_turn_by_player",
        "mana_spent_on_spells_this_turn",
        "last_effect_counts_by_player",
        "stack_trigger_event_batches",
        "lki_cache",
        "lki_copiable_values",
        "lki_by_incarnation",
        "linked_exile_lki",
        "ring_level",
        "ring_bearer",
        "dungeon_progress",
        "planar_die_actions_this_turn",
    ];
    assert_eq!(
        direct_fields.len(),
        39,
        "private stack_trigger_firings is covered by its unit test"
    );
    for field in direct_fields {
        let value = &before[field];
        assert_eq!(
            value.as_object().map(serde_json::Map::len),
            Some(2),
            "{field}: pre-serialization reach guard must contain two keys"
        );
        assert_eq!(
            after[field], *value,
            "{field}: exact key/value membership changed"
        );
    }
    for field in ["tracked_set_member_causes", "lki_by_incarnation"] {
        for (outer_key, inner) in before[field].as_object().unwrap() {
            assert_eq!(
                inner.as_object().map(serde_json::Map::len),
                Some(2),
                "{field}[{outer_key}]: nested reach guard must contain two keys"
            );
        }
    }
    assert_eq!(
        reserialized, serialized,
        "canonical bytes must survive restore"
    );
    assert_eq!(
        before["player_actions_this_turn"],
        serde_json::json!([[1, "Scry"], [0, "SearchedLibrary"]])
    );
}

#[test]
fn every_numeric_key_combat_map_round_trips_populated() {
    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    let combat = CombatState {
        blocker_assignments: HashMap::from([
            (ObjectId(1), vec![ObjectId(11)]),
            (ObjectId(2), vec![ObjectId(21), ObjectId(22)]),
        ]),
        blocker_to_attacker: HashMap::from([
            (ObjectId(11), vec![ObjectId(1)]),
            (ObjectId(22), vec![ObjectId(2), ObjectId(3)]),
        ]),
        attacked_defenders_this_combat: HashMap::from([
            (
                PlayerId(0),
                [PlayerId(0), PlayerId(1)].into_iter().collect(),
            ),
            (
                PlayerId(1),
                [PlayerId(1), PlayerId(0)].into_iter().collect(),
            ),
        ]),
        creature_attacked_defenders_this_combat: HashMap::from([
            (
                ObjectId(1),
                [PlayerId(0), PlayerId(1)].into_iter().collect(),
            ),
            (
                ObjectId(2),
                [PlayerId(1), PlayerId(0)].into_iter().collect(),
            ),
        ]),
        damage_assignments: HashMap::from([
            (
                ObjectId(1),
                vec![DamageAssignment {
                    target: DamageTarget::Player(PlayerId(1)),
                    amount: 1,
                }],
            ),
            (
                ObjectId(2),
                vec![DamageAssignment {
                    target: DamageTarget::Player(PlayerId(0)),
                    amount: 2,
                }],
            ),
        ]),
        ..Default::default()
    };
    state.combat = Some(combat);

    let serialized = serde_json::to_string(&state).unwrap();
    let before: serde_json::Value = serde_json::from_str(&serialized).unwrap();
    let restored: GameState = serde_json::from_str(&serialized).unwrap();
    let reserialized = serde_json::to_string(&restored).unwrap();
    let after: serde_json::Value = serde_json::from_str(&reserialized).unwrap();
    for field in [
        "blocker_assignments",
        "blocker_to_attacker",
        "attacked_defenders_this_combat",
        "creature_attacked_defenders_this_combat",
        "damage_assignments",
    ] {
        assert_eq!(
            before["combat"][field]
                .as_object()
                .map(serde_json::Map::len),
            Some(2),
            "CombatState.{field}: pre-serialization reach guard"
        );
        assert_eq!(
            after["combat"][field], before["combat"][field],
            "CombatState.{field}: exact membership changed"
        );
    }
    assert_eq!(reserialized, serialized);
}

fn waiting_value(waiting_for: &WaitingFor) -> serde_json::Value {
    serde_json::to_value(waiting_for).expect("waiting payload should serialize")
}

fn assert_waiting_round_trip_across_persistence_forms(state: GameState) {
    let expected = waiting_value(&state.waiting_for);
    let serialized = serde_json::to_string(&state).expect("state should serialize");
    let bare: GameState = serde_json::from_str(&serialized).expect("bare state should restore");
    assert_eq!(waiting_value(&bare.waiting_for), expected);

    for persisted in [
        PersistedGameState::Raw(Box::new(state.clone())),
        PersistedGameState::capture(state.clone()),
    ] {
        let serialized = serde_json::to_string(&persisted).expect("persistence should serialize");
        let restored: PersistedGameState =
            serde_json::from_str(&serialized).expect("persistence should restore");
        assert_eq!(
            waiting_value(&restored.into_game_state().waiting_for),
            expected
        );
    }
}

#[test]
fn declare_attackers_numeric_maps_round_trip_through_value_bare_raw_and_trusted() {
    let waiting = WaitingFor::DeclareAttackers {
        player: PlayerId(0),
        valid_attacker_ids: vec![ObjectId(3), ObjectId(2), ObjectId(1)],
        valid_attack_targets: vec![
            AttackTarget::Player(PlayerId(1)),
            AttackTarget::Planeswalker(ObjectId(9)),
        ],
        valid_attack_targets_by_attacker: Some(HashMap::from([
            (ObjectId(1), vec![AttackTarget::Player(PlayerId(1))]),
            (
                ObjectId(2),
                vec![
                    AttackTarget::Player(PlayerId(1)),
                    AttackTarget::Planeswalker(ObjectId(9)),
                ],
            ),
        ])),
        attacker_constraints: HashMap::from([
            (
                ObjectId(1),
                CombatRequirement::CantAttack {
                    sources: vec![ObjectId(11)],
                },
            ),
            (
                ObjectId(2),
                CombatRequirement::MustAttack {
                    players: vec![PlayerId(1)],
                    sources: vec![ObjectId(22)],
                },
            ),
        ]),
    };
    let value = waiting_value(&waiting);
    assert_eq!(
        value["data"]["valid_attack_targets_by_attacker"]
            .as_object()
            .map(serde_json::Map::len),
        Some(2)
    );
    assert_eq!(
        value["data"]["attacker_constraints"]
            .as_object()
            .map(serde_json::Map::len),
        Some(2)
    );
    assert_eq!(
        value["data"]["valid_attacker_ids"],
        serde_json::json!([3, 2, 1])
    );
    let restored: WaitingFor = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(waiting_value(&restored), value);

    for optional in [Some(HashMap::new()), None] {
        let boundary = WaitingFor::DeclareAttackers {
            player: PlayerId(0),
            valid_attacker_ids: vec![ObjectId(2), ObjectId(1)],
            valid_attack_targets: Vec::new(),
            valid_attack_targets_by_attacker: optional,
            attacker_constraints: HashMap::new(),
        };
        let boundary_value = waiting_value(&boundary);
        assert_eq!(
            waiting_value(&serde_json::from_value::<WaitingFor>(boundary_value.clone()).unwrap()),
            boundary_value
        );
    }

    let mut malformed = value.clone();
    malformed["data"]["attacker_constraints"] = serde_json::json!({"01": {"kind": "CantAttack"}});
    assert!(serde_json::from_value::<WaitingFor>(malformed).is_err());

    let valid_attack_targets = value["data"]["valid_attack_targets"].clone();

    let mut malformed_optional = value.clone();
    malformed_optional["data"]["valid_attack_targets_by_attacker"] =
        serde_json::json!({"01": valid_attack_targets.clone()});
    let error = serde_json::from_value::<WaitingFor>(malformed_optional).unwrap_err();
    assert_eq!(
        error.to_string(),
        r#"invalid value: string "01", expected a canonical unsigned decimal map key"#,
        "the optional numeric-map deserializer must reject at the noncanonical key path"
    );

    let mut canonical_optional = value.clone();
    canonical_optional["data"]["valid_attack_targets_by_attacker"] =
        serde_json::json!({"1": valid_attack_targets});
    let restored = serde_json::from_value::<WaitingFor>(canonical_optional.clone())
        .expect("the same valid payload must deserialize with a canonical numeric key");
    assert_eq!(waiting_value(&restored), canonical_optional);

    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    state.waiting_for = waiting;
    assert_waiting_round_trip_across_persistence_forms(state);
}

#[test]
fn declare_blockers_numeric_maps_round_trip_through_value_bare_raw_and_trusted() {
    let waiting = WaitingFor::DeclareBlockers {
        player: PlayerId(1),
        valid_blocker_ids: vec![ObjectId(4), ObjectId(3)],
        valid_block_targets: HashMap::from([
            (ObjectId(3), vec![ObjectId(1)]),
            (ObjectId(4), vec![ObjectId(1), ObjectId(2)]),
        ]),
        block_requirements: HashMap::from([
            (
                ObjectId(1),
                BlockRequirement {
                    count: 2,
                    sources: vec![ObjectId(11)],
                },
            ),
            (
                ObjectId(2),
                BlockRequirement {
                    count: 3,
                    sources: vec![ObjectId(22)],
                },
            ),
        ]),
        blocker_constraints: HashMap::from([
            (
                ObjectId(3),
                CombatRequirement::CantBlock {
                    sources: vec![ObjectId(33)],
                },
            ),
            (
                ObjectId(4),
                CombatRequirement::MustBlock {
                    sources: vec![ObjectId(44)],
                    attackers: vec![ObjectId(2)],
                },
            ),
        ]),
    };
    let value = waiting_value(&waiting);
    for field in [
        "valid_block_targets",
        "block_requirements",
        "blocker_constraints",
    ] {
        assert_eq!(
            value["data"][field].as_object().map(serde_json::Map::len),
            Some(2),
            "DeclareBlockers.{field}: pre-serialization reach guard"
        );
    }
    assert_eq!(
        value["data"]["valid_blocker_ids"],
        serde_json::json!([4, 3])
    );
    let restored: WaitingFor = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(waiting_value(&restored), value);

    let empty = WaitingFor::DeclareBlockers {
        player: PlayerId(1),
        valid_blocker_ids: vec![ObjectId(4), ObjectId(3)],
        valid_block_targets: HashMap::new(),
        block_requirements: HashMap::new(),
        blocker_constraints: HashMap::new(),
    };
    let empty_value = waiting_value(&empty);
    assert_eq!(
        waiting_value(&serde_json::from_value::<WaitingFor>(empty_value.clone()).unwrap()),
        empty_value
    );

    let mut malformed = value.clone();
    malformed["data"]["blocker_constraints"] = serde_json::json!({"-1": {"kind": "CantBlock"}});
    assert!(serde_json::from_value::<WaitingFor>(malformed).is_err());

    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    state.waiting_for = waiting;
    assert_waiting_round_trip_across_persistence_forms(state);
}

#[test]
fn real_game_state_hash_owners_are_canonical_and_round_trip_across_all_persistence_forms() {
    let forward = build_populated_state(false);
    let reverse = build_populated_state(true);
    assert_representative_membership(&forward);
    assert_representative_membership(&reverse);

    let canonical_token_creators = (0_u8..32).map(PlayerId).collect::<Vec<_>>();
    let native_token_creator_order = forward
        .players_who_created_token_this_turn
        .iter()
        .copied()
        .collect::<Vec<_>>();
    assert_ne!(
        native_token_creator_order, canonical_token_creators,
        "fixture must expose a noncanonical native HashSet iteration order"
    );

    let forward_json = serde_json::to_string(&forward).expect("forward state should serialize");
    let reverse_json = serde_json::to_string(&reverse).expect("reverse state should serialize");
    assert_eq!(
        forward_json, reverse_json,
        "logical insertion history must not affect bytes"
    );
    let forward_value: serde_json::Value =
        serde_json::from_str(&forward_json).expect("serialized state should be valid JSON");
    assert_eq!(
        forward_value["players_who_created_token_this_turn"],
        serde_json::to_value(&canonical_token_creators)
            .expect("canonical token-creator IDs should serialize")
    );
    assert!(forward_json.contains("\"ring_level\":{\"0\":1,\"1\":2}"));
    assert!(forward_json.contains("\"attacked_defenders_this_turn\":{\"0\":[0,1],\"1\":[0,1]}"));
    assert!(forward_json
        .contains("\"steps_to_skip\":[{\"PreCombatMain\":1,\"PostCombatMain\":2},{\"End\":3}]"));
    assert!(forward_json.contains("\"objects\":{\"1\":"));
    let first_name = forward_json
        .find("First Deterministic Object")
        .expect("first object payload should serialize");
    let second_name = forward_json
        .find("Second Deterministic Object")
        .expect("second object payload should serialize");
    assert!(
        first_name < second_name,
        "objects must emit by typed numeric key order"
    );
    let specialize = field_fragment(&forward_json, "specialize_faces", "foretold");
    assert!(
        specialize.find("\"White\"").expect("white face")
            < specialize.find("\"Blue\"").expect("blue face"),
        "specialize faces must follow ManaColor::Ord"
    );
    assert!(forward_json.contains("\"goaded_by\":[0,1]"));
    assert!(forward_json.contains(
        "\"damage_assignments\":{\"1\":[{\"target\":{\"Player\":1},\"amount\":1}],\"2\":[{\"target\":{\"Player\":0},\"amount\":2}]}"
    ));
    assert!(forward_json.contains(
        "\"attacker_constraints\":{\"1\":{\"kind\":\"CantAttack\"},\"2\":{\"kind\":\"CantAttack\"}}"
    ));
    assert!(forward_json.contains(
        "\"applied\":[{\"type\":\"Floating\",\"index\":1},{\"type\":\"Floating\",\"index\":2}]"
    ));
    assert!(forward_json.contains("\"valid_attacker_ids\":[2,1]",));
    assert!(forward_json
        .contains("\"player_actions_this_turn\":[[1,\"Scry\"],[0,\"SearchedLibrary\"]]"));

    let empty = GameState::new(FormatConfig::standard(), 2, 42);
    let empty_json = serde_json::to_value(&empty).expect("empty state should serialize");
    assert_eq!(
        empty_json["players_who_created_token_this_turn"],
        serde_json::json!([])
    );
    let mut singleton = empty;
    singleton
        .players_who_created_token_this_turn
        .insert(PlayerId(0));
    let singleton_json = serde_json::to_value(&singleton).expect("singleton should serialize");
    assert_eq!(
        singleton_json["players_who_created_token_this_turn"],
        serde_json::json!([0])
    );

    let bare: GameState = serde_json::from_str(&forward_json).expect("bare state should restore");
    assert_representative_membership(&bare);
    assert_eq!(
        serde_json::to_string(&bare).expect("bare state should reserialize"),
        forward_json
    );

    for persisted in [
        PersistedGameState::Raw(Box::new(forward.clone())),
        PersistedGameState::capture(forward.clone()),
    ] {
        let serialized =
            serde_json::to_string(&persisted).expect("persisted state should serialize");
        let restored: PersistedGameState =
            serde_json::from_str(&serialized).expect("persisted state should restore");
        let restored = restored.into_game_state();
        assert_representative_membership(&restored);
        assert_eq!(
            serde_json::to_string(&restored).expect("restored state should reserialize"),
            forward_json
        );
    }
}

#[test]
fn populated_protection_tuple_map_keeps_its_existing_non_string_json_key_failure() {
    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    let mut object = GameObject::new(
        ObjectId(1),
        CardId(1),
        PlayerId(0),
        "Protection Fixture".to_string(),
        Zone::Battlefield,
    );
    object.protection_start_exempt_attachments.insert(
        (0, 0, ObjectId(2)),
        ProtectionStartSnapshot {
            resolved_quality: ProtectionTarget::Color(ManaColor::White),
            attachment_ids: vec![ObjectId(3)],
        },
    );
    assert_eq!(object.protection_start_exempt_attachments.len(), 1);
    state.objects.insert(ObjectId(1), object);

    let error = serde_json::to_value(&state)
        .expect_err("the existing tuple-key map has no nonempty JSON representation");
    assert_eq!(error.to_string(), "key must be a string");
}
