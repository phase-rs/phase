use engine::types::game_state::GameState;
use serde::Deserialize;
use serde_json::{Map, Value};

#[derive(Deserialize)]
struct Saved {
    #[serde(rename = "gameState")]
    game_state: GameState,
}

pub fn load_saved_game_state(raw: &str) -> Result<GameState, serde_json::Error> {
    let mut value = serde_json::from_str(raw)?;
    migrate_saved_state(&mut value);
    serde_json::from_value::<Saved>(value).map(|saved| saved.game_state)
}

fn migrate_saved_state(value: &mut Value) {
    match value {
        Value::Array(values) => {
            for value in values {
                migrate_saved_state(value);
            }
        }
        Value::Object(map) => {
            if let Some(effect) = map.get_mut("effect") {
                migrate_effect(effect);
            }
            if let Some(condition) = map.get_mut("condition") {
                migrate_condition(condition);
            }
            if let Some(modification) = map.get_mut("quantity_modification") {
                migrate_quantity_modification(modification);
            }
            for (key, value) in map {
                if key != "effect" && key != "condition" && key != "quantity_modification" {
                    migrate_saved_state(value);
                }
            }
        }
        _ => {}
    }
}

fn migrate_effect(effect: &mut Value) {
    if let Value::Object(map) = effect {
        if migrate_legacy_tap_effect(map) {
            return;
        }
    }
    migrate_saved_state(effect);
}

fn migrate_condition(condition: &mut Value) {
    if let Value::Object(map) = condition {
        if migrate_legacy_attackers_declared_min(map) {
            return;
        }
    }
    migrate_saved_state(condition);
}

fn migrate_quantity_modification(modification: &mut Value) {
    if let Value::Object(map) = modification {
        if migrate_legacy_double_quantity(map) {
            return;
        }
    }
    migrate_saved_state(modification);
}

fn migrate_legacy_tap_effect(map: &mut Map<String, Value>) -> bool {
    let Some(effect_type) = map.get("type").and_then(Value::as_str) else {
        return false;
    };
    let Some((scope, state)) = legacy_tap_effect(effect_type) else {
        return false;
    };

    map.insert("type".to_string(), Value::String("SetTapState".to_string()));
    map.insert("scope".to_string(), tagged(scope));
    map.insert("state".to_string(), tagged(state));
    true
}

fn migrate_legacy_attackers_declared_min(map: &mut Map<String, Value>) -> bool {
    let Some("AttackersDeclaredMin") = map.get("type").and_then(Value::as_str) else {
        return false;
    };
    let scope = map
        .remove("scope")
        .unwrap_or_else(|| Value::String("You".to_string()));
    let count = map.remove("minimum").unwrap_or_else(|| Value::from(1));

    let mut subject = Map::new();
    subject.insert("type".to_string(), Value::String("Controller".to_string()));
    subject.insert("scope".to_string(), scope);

    map.insert(
        "type".to_string(),
        Value::String("AttackersDeclaredCount".to_string()),
    );
    map.insert("subject".to_string(), Value::Object(subject));
    map.insert("comparator".to_string(), Value::String("GE".to_string()));
    map.insert("count".to_string(), count);
    true
}

/// `QuantityModification::Double` (unit) was parameterized into `Times { factor }`
/// (factor 2 = the former doubling; factor 3 = Ojer Taq, Deepest Foundation).
/// Saved states captured before that change carry `{"type":"Double"}`; rewrite
/// them to the equivalent `{"type":"Times","factor":2}` so old replacement
/// definitions keep loading.
fn migrate_legacy_double_quantity(map: &mut Map<String, Value>) -> bool {
    let Some("Double") = map.get("type").and_then(Value::as_str) else {
        return false;
    };
    map.insert("type".to_string(), Value::String("Times".to_string()));
    map.insert("factor".to_string(), Value::from(2));
    true
}

fn legacy_tap_effect(effect_type: &str) -> Option<(&'static str, &'static str)> {
    match effect_type {
        "Tap" => Some(("Single", "Tap")),
        "Untap" => Some(("Single", "Untap")),
        "TapAll" => Some(("All", "Tap")),
        "UntapAll" => Some(("All", "Untap")),
        _ => None,
    }
}

fn tagged(variant: &str) -> Value {
    let mut map = Map::new();
    map.insert("type".to_string(), Value::String(variant.to_string()));
    Value::Object(map)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::migrate_saved_state;

    #[test]
    fn migrates_legacy_tap_effects_without_touching_costs() {
        let mut value = json!({
            "gameState": {
                "stack": [
                    {
                        "effect": {
                            "type": "Tap",
                            "target": { "type": "Any" }
                        }
                    },
                    {
                        "cost": {
                            "type": "Tap"
                        }
                    }
                ]
            }
        });

        migrate_saved_state(&mut value);

        assert_eq!(
            value["gameState"]["stack"][0]["effect"],
            json!({
                "type": "SetTapState",
                "target": { "type": "Any" },
                "scope": { "type": "Single" },
                "state": { "type": "Tap" }
            })
        );
        assert_eq!(
            value["gameState"]["stack"][1]["cost"],
            json!({ "type": "Tap" })
        );
    }

    #[test]
    fn migrates_legacy_mass_untap_effects() {
        let mut value = json!({
            "effect": {
                "type": "UntapAll",
                "target": { "type": "Artifact" }
            }
        });

        migrate_saved_state(&mut value);

        assert_eq!(
            value["effect"],
            json!({
                "type": "SetTapState",
                "target": { "type": "Artifact" },
                "scope": { "type": "All" },
                "state": { "type": "Untap" }
            })
        );
    }

    #[test]
    fn migrates_legacy_double_quantity_modification_without_touching_effect_double() {
        let mut value = json!({
            "gameState": {
                "replacement_definitions": [
                    {
                        "quantity_modification": { "type": "Double" }
                    }
                ],
                "stack": [
                    {
                        "effect": { "type": "Double", "target_kind": { "type": "Tokens" } }
                    }
                ]
            }
        });

        migrate_saved_state(&mut value);

        // The renamed QuantityModification is rewritten to the parameterized form.
        assert_eq!(
            value["gameState"]["replacement_definitions"][0]["quantity_modification"],
            json!({ "type": "Times", "factor": 2 })
        );
        // Effect::Double is a different (unchanged) enum and must be left intact.
        assert_eq!(
            value["gameState"]["stack"][0]["effect"],
            json!({ "type": "Double", "target_kind": { "type": "Tokens" } })
        );
    }

    #[test]
    fn migrates_legacy_attackers_declared_min_conditions() {
        let mut value = json!({
            "condition": {
                "type": "AttackersDeclaredMin",
                "scope": "You",
                "minimum": 3
            }
        });

        migrate_saved_state(&mut value);

        assert_eq!(
            value["condition"],
            json!({
                "type": "AttackersDeclaredCount",
                "subject": {
                    "type": "Controller",
                    "scope": "You"
                },
                "comparator": "GE",
                "count": 3
            })
        );
    }
}
