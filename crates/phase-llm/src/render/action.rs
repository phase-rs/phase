//! Human-readable rendering of an engine [`GameAction`].
//!
//! Written as a generic walk over the action's own serialized payload rather
//! than a match over its ~100 variants. That is deliberate and is the only
//! approach that satisfies "build for the class, not the card": a new
//! `GameAction` variant is described correctly the day it is added, with no
//! second registration point to forget. The engine's `#[serde(tag = "type",
//! content = "data")]` representation and its `strum::IntoStaticStr` variant
//! name are the two facts this relies on, and both are structural.

use std::collections::BTreeMap;

use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use serde_json::Value;

use super::text::one_line;

/// Payload fields whose numeric values name a player rather than an object.
/// An explicit list, not a heuristic: guessing wrong here would print
/// "Player 7" for an object id.
const PLAYER_FIELDS: &[&str] = &[
    "player",
    "players",
    "controller",
    "owner",
    "opponent",
    "recipient",
    "chooser",
    "representative",
    "defender",
    "attacking_player",
    "target_player",
];

/// Fields carrying object ids that do not follow the `_id` suffix convention.
const OBJECT_FIELDS: &[&str] = &["targets", "attackers", "blockers", "cards", "permanents"];

/// A one-line description of what `action` does, with object ids resolved to
/// card names against the live state.
pub fn describe_action(state: &GameState, action: &GameAction) -> String {
    let verb = humanize_identifier(<&'static str>::from(action));
    let Ok(value) = serde_json::to_value(action) else {
        return verb;
    };
    let Some(data) = value.get("data").and_then(Value::as_object) else {
        // A unit variant (`PassPriority`) has no payload; the verb is the whole
        // description.
        return verb;
    };

    // `BTreeMap` for a stable field order: the same decision must render
    // identically on every call, or the fingerprint that guards against a stale
    // decision would flap.
    let ordered: BTreeMap<&String, &Value> = data.iter().collect();
    let details: Vec<String> = ordered
        .into_iter()
        .filter(|(_, value)| !is_empty_value(value))
        .map(|(field, value)| {
            format!(
                "{}: {}",
                humanize_identifier(field),
                render_value(state, field, value)
            )
        })
        .collect();

    // Folded at the exit, not per leaf: a payload string is humanized word by
    // word and can still carry a line break, and every leaf would otherwise need
    // to remember that.
    if details.is_empty() {
        verb
    } else {
        one_line(&format!("{verb} ({})", details.join(", ")))
    }
}

/// The primary object an action operates on, when it has one. Used to lead a
/// candidate line with the card name — the highest-signal token for a model
/// that already knows the card.
pub fn primary_object_name(state: &GameState, action: &GameAction) -> Option<String> {
    let id = match action {
        GameAction::CastSpell { object_id, .. }
        | GameAction::PlayLand { object_id, .. }
        | GameAction::Foretell { object_id, .. }
        | GameAction::PlayFaceDown { object_id, .. }
        | GameAction::TurnFaceUp { object_id, .. } => Some(*object_id),
        GameAction::ActivateAbility { source_id, .. } => Some(*source_id),
        _ => None,
    }?;
    object_name(state, id)
}

fn object_name(state: &GameState, id: ObjectId) -> Option<String> {
    state.objects.get(&id).map(|object| one_line(&object.name))
}

/// `snake_case` / `CamelCase` -> `Title Case`, with the repository's existing
/// `id -> ID` convention preserved.
pub fn humanize_identifier(raw: &str) -> String {
    let spaced = split_camel_case(raw);
    spaced
        .split(['_', ' '])
        .filter(|word| !word.is_empty())
        .map(|word| match word.to_ascii_lowercase().as_str() {
            "id" => "ID".to_string(),
            "ids" => "IDs".to_string(),
            _ => {
                let mut chars = word.chars();
                chars.next().map_or_else(String::new, |first| {
                    first.to_uppercase().collect::<String>() + chars.as_str()
                })
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn split_camel_case(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 4);
    for (index, c) in raw.char_indices() {
        if index > 0 && c.is_uppercase() {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

fn is_empty_value(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(items) => items.is_empty(),
        Value::Object(fields) => fields.is_empty(),
        Value::String(text) => text.is_empty(),
        _ => false,
    }
}

/// Render one payload field. Object-id fields become card names; player fields
/// become `Player N`; everything else falls through to a compact JSON-ish form.
fn render_value(state: &GameState, field: &str, value: &Value) -> String {
    if names_players(field) {
        return map_scalars(value, |number| format!("Player {number}"));
    }
    if names_objects(field) {
        return map_scalars(value, |number| {
            object_name(state, ObjectId(number)).unwrap_or_else(|| format!("object #{number}"))
        });
    }
    match value {
        Value::String(text) => humanize_identifier(text),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::Null => "none".to_string(),
        Value::Array(items) => items
            .iter()
            .map(|item| render_value(state, field, item))
            .collect::<Vec<_>>()
            .join(", "),
        Value::Object(fields) => fields
            .iter()
            .map(|(key, item)| {
                format!(
                    "{} {}",
                    humanize_identifier(key),
                    render_value(state, key, item)
                )
            })
            .collect::<Vec<_>>()
            .join(", "),
    }
}

/// `card_id` is deliberately excluded: it indexes the card database, not the
/// object table, so resolving it as an object would print an unrelated card.
fn names_objects(field: &str) -> bool {
    if field == "card_id" || field == "card_ids" {
        return false;
    }
    OBJECT_FIELDS.contains(&field) || field.ends_with("_id") || field.ends_with("_ids")
}

fn names_players(field: &str) -> bool {
    PLAYER_FIELDS.contains(&field)
}

/// A short description of the decision the engine is waiting on, from the
/// `WaitingFor` variant name and (when the prompt has one) the engine-latched
/// display name of the source that raised it.
pub fn describe_waiting_for(waiting_for: &engine::types::game_state::WaitingFor) -> String {
    let Ok(value) = serde_json::to_value(waiting_for) else {
        return "a decision".to_string();
    };
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .map_or_else(|| "a decision".to_string(), humanize_identifier);
    let source = value
        .get("data")
        .and_then(|data| data.get("source"))
        .and_then(|source| source.get("display_name"))
        .and_then(Value::as_str);
    match source {
        Some(name) => format!("{kind} (from {})", one_line(name)),
        None => kind,
    }
}

/// Apply `render` to every number in `value`, preserving array structure.
fn map_scalars(value: &Value, render: impl Fn(u64) -> String + Copy) -> String {
    match value {
        Value::Number(number) => number.as_u64().map_or_else(|| number.to_string(), render),
        Value::Array(items) => items
            .iter()
            .map(|item| map_scalars(item, render))
            .collect::<Vec<_>>()
            .join(", "),
        Value::Null => "none".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::types::identifiers::CardId;

    fn empty_state() -> GameState {
        GameState::default()
    }

    #[test]
    fn a_unit_variant_renders_as_its_verb_alone() {
        assert_eq!(
            describe_action(&empty_state(), &GameAction::PassPriority),
            "Pass Priority"
        );
    }

    #[test]
    fn a_payload_variant_lists_its_fields_in_a_stable_order() {
        let action = GameAction::PlayLand {
            object_id: ObjectId(7),
            card_id: CardId(3),
        };
        let state = empty_state();
        let rendered = describe_action(&state, &action);
        assert_eq!(rendered, describe_action(&state, &action));
        assert!(rendered.starts_with("Play Land ("), "{rendered}");
        // `card_id` must NOT be resolved through the object table.
        assert!(rendered.contains("Card ID: 3"), "{rendered}");
        assert!(rendered.contains("Object ID: object #7"), "{rendered}");
    }

    #[test]
    fn identifier_humanization_keeps_the_id_convention() {
        assert_eq!(humanize_identifier("source_id"), "Source ID");
        assert_eq!(humanize_identifier("CastSpell"), "Cast Spell");
        assert_eq!(
            humanize_identifier("card_instance_ids"),
            "Card Instance IDs"
        );
    }

    #[test]
    fn player_fields_render_as_players_and_object_fields_do_not() {
        assert!(names_players("controller"));
        assert!(!names_players("source_id"));
        assert!(names_objects("targets"));
        assert!(names_objects("source_id"));
        assert!(!names_objects("card_id"));
    }
}
