//! Schema-guarded report-only omission of typed definition descriptions.

use serde::Serialize;
use serde_json::{Map, Value};

const ABILITY_KEYS: &[&str] = &[
    "kind",
    "effect",
    "cost",
    "sub_ability",
    "else_ability",
    "duration",
    "description",
    "target_prompt",
    "activation_restrictions",
    "activation_mana_payment_restriction",
    "activator_filter",
    "activation_zone",
    "ability_tag",
    "condition",
    "optional_targeting",
    "optional",
    "optional_player",
    "optional_for",
    "multi_target",
    "target_constraints",
    "target_choice_timing",
    "distribute",
    "unless_pay",
    "modal",
    "mode_abilities",
    "repeat_for",
    "min_x_value",
    "announced_x",
    "cant_be_copied",
    "cost_reduction",
    "forward_result",
    "player_scope",
    "starting_with",
    "target_selection_mode",
    "target_chooser",
    "repeat_until",
    "sub_link",
    "iteration_kind_binding",
    "sibling_condition",
    "consumes_source",
    "is_mana_ability",
];
const TRIGGER_KEYS: &[&str] = &[
    "mode",
    "execute",
    "valid_card",
    "origin",
    "origin_zones",
    "zone_change_clauses",
    "destination",
    "destination_constraint",
    "trigger_zones",
    "phase",
    "optional",
    "damage_kind",
    "secondary",
    "valid_target",
    "valid_subject_player",
    "valid_source",
    "spell_cast_origin",
    "description",
    "constraint",
    "condition",
    "counter_filter",
    "saga_chapter",
    "unless_pay",
    "batched",
    "die_sides",
    "expend_threshold",
    "attack_target_filter",
    "player_actions",
    "scry_bottom_count",
    "damage_amount",
    "life_amount",
    "coin_flip_result",
    "die_result",
    "taps_for_mana_produced",
    "mana_ability_produced",
    "clash_result",
    "room_door",
];
const STATIC_KEYS: &[&str] = &[
    "mode",
    "affected",
    "modifications",
    "condition",
    "per_player_condition",
    "affected_zone",
    "effect_zone",
    "active_zones",
    "characteristic_defining",
    "description",
    "attack_defended",
    "source_controller",
    "source_object",
    "bypass_beneficiary",
    "protection_does_not_remove",
    "room_door",
];
const REPLACEMENT_KEYS: &[&str] = &[
    "event",
    "execute",
    "runtime_execute",
    "mode",
    "valid_card",
    "description",
    "condition",
    "destination_zone",
    "damage_modification",
    "damage_source_filter",
    "damage_target_filter",
    "combat_scope",
    "draw_scope",
    "die_ignore_rule",
    "planeswalk_scope",
    "shield_kind",
    "quantity_modification",
    "token_owner_scope",
    "token_owner_redirect",
    "valid_player",
    "consume_on_apply",
    "is_consumed",
    "expiry",
    "redirect_target",
    "mana_modification",
    "mana_replacement_scope",
    "additional_token_spec",
    "ensure_token_specs",
    "counter_match",
    "enters_under",
    "source_controller",
    "source_object",
    "origin",
    "counter_replacement_subject",
];

#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct OmittedDefinitionDescription {
    pub json_pointer: String,
    pub value: String,
    pub carrier: &'static str,
}

fn carrier(map: &Map<String, Value>) -> Option<&'static str> {
    let schemas: &[(&str, &[&str], &[&str])] = &[
        (
            "AbilityDefinition",
            &["kind", "effect", "sub_ability", "duration", "description"],
            ABILITY_KEYS,
        ),
        (
            "TriggerDefinition",
            &[
                "mode",
                "execute",
                "valid_card",
                "trigger_zones",
                "description",
            ],
            TRIGGER_KEYS,
        ),
        (
            "StaticDefinition",
            &[
                "mode",
                "affected",
                "modifications",
                "active_zones",
                "description",
            ],
            STATIC_KEYS,
        ),
        (
            "ReplacementDefinition",
            &["event", "execute", "mode", "valid_card", "description"],
            REPLACEMENT_KEYS,
        ),
    ];
    schemas.iter().find_map(|(name, required, allowed)| {
        (required.iter().all(|key| map.contains_key(*key))
            && map.keys().all(|key| allowed.contains(&key.as_str())))
        .then_some(*name)
    })
}

fn escape_pointer_segment(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

pub fn omit_definition_descriptions(value: &mut Value) -> Vec<OmittedDefinitionDescription> {
    omit_definition_descriptions_at(value, "")
}

pub fn omit_definition_descriptions_at(
    value: &mut Value,
    pointer: &str,
) -> Vec<OmittedDefinitionDescription> {
    fn walk(value: &mut Value, pointer: &str, omitted: &mut Vec<OmittedDefinitionDescription>) {
        match value {
            Value::Array(values) => {
                for (index, child) in values.iter_mut().enumerate() {
                    walk(child, &format!("{pointer}/{index}"), omitted);
                }
            }
            Value::Object(map) => {
                if let Some(name) = carrier(map) {
                    if let Some(Value::String(description)) = map.remove("description") {
                        omitted.push(OmittedDefinitionDescription {
                            json_pointer: format!("{pointer}/description"),
                            value: description,
                            carrier: name,
                        });
                    }
                }
                for (key, child) in map.iter_mut() {
                    walk(
                        child,
                        &format!("{pointer}/{}", escape_pointer_segment(key)),
                        omitted,
                    );
                }
            }
            _ => {}
        }
    }
    let mut omitted = Vec::new();
    walk(value, pointer, &mut omitted);
    omitted.sort();
    omitted
}
