//! Migration binary that converts old-format ability JSON files to the new typed format.
//!
//! Handles:
//! - `remaining_params` on AbilityDefinition -> typed fields (description, target_prompt, sorcery_speed)
//! - `params` on TriggerDefinition -> typed fields (execute, valid_card, origin, destination, etc.)
//! - `params` on StaticDefinition -> typed fields (affected, modifications, description, etc.)
//! - `params` on ReplacementDefinition -> typed fields (description, valid_card)
//! - Old TargetFilter variants (All, Filtered) -> new typed TargetFilter
//! - Effect::Other { api_type, params } -> Effect::Unimplemented { name, description }
//! - Effect::Mana { produced: "G" } -> Effect::Mana { produced: ["Green"] }
//! - Effect params HashMap removal
//! - ManaCost string cleanup

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

/// Migration statistics.
#[derive(Default)]
struct Stats {
    total: usize,
    migrated: usize,
    already_new: usize,
    errors: Vec<(PathBuf, String)>,
    warnings: Vec<(PathBuf, String)>,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let abilities_dir = PathBuf::from(args.get(1).map(|s| s.as_str()).unwrap_or("data/abilities"));

    if !abilities_dir.is_dir() {
        eprintln!(
            "Error: Abilities directory not found: {}",
            abilities_dir.display()
        );
        std::process::exit(1);
    }

    let mut stats = Stats::default();
    let mut entries: Vec<_> = fs::read_dir(&abilities_dir)
        .expect("Failed to read abilities directory")
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path().is_file()
                && e.path().extension().and_then(|x| x.to_str()) == Some("json")
                && e.path().file_stem().and_then(|s| s.to_str()) != Some("schema")
        })
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in &entries {
        let path = entry.path();
        stats.total += 1;

        if stats.total % 5000 == 0 {
            eprintln!("  Progress: {}/{}", stats.total, entries.len());
        }

        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                stats
                    .errors
                    .push((path.clone(), format!("Read error: {e}")));
                continue;
            }
        };

        let mut root: Value = match serde_json::from_str(&content) {
            Ok(v) => v,
            Err(e) => {
                stats
                    .errors
                    .push((path.clone(), format!("Parse error: {e}")));
                continue;
            }
        };

        let changed = migrate_ability_file(&mut root, &path, &mut stats);

        if changed {
            let output = serde_json::to_string_pretty(&root).unwrap();
            if let Err(e) = fs::write(&path, format!("{output}\n")) {
                stats
                    .errors
                    .push((path.clone(), format!("Write error: {e}")));
            } else {
                stats.migrated += 1;
            }
        } else {
            stats.already_new += 1;
        }
    }

    println!("Migration complete:");
    println!("  Total files:    {}", stats.total);
    println!("  Migrated:       {}", stats.migrated);
    println!("  Already new:    {}", stats.already_new);
    println!("  Errors:         {}", stats.errors.len());
    println!("  Warnings:       {}", stats.warnings.len());

    if !stats.errors.is_empty() {
        println!("\nFirst 20 errors:");
        for (path, msg) in stats.errors.iter().take(20) {
            println!("  {}: {}", path.display(), msg);
        }
    }
    if !stats.warnings.is_empty() {
        println!("\nFirst 20 warnings:");
        for (path, msg) in stats.warnings.iter().take(20) {
            println!("  {}: {}", path.display(), msg);
        }
    }
}

/// Valid Zone enum values.
const VALID_ZONES: &[&str] = &[
    "Library",
    "Hand",
    "Battlefield",
    "Graveyard",
    "Stack",
    "Exile",
    "Command",
];

/// Zone-related field names that should contain valid Zone enum values.
const ZONE_FIELD_NAMES: &[&str] = &[
    "origin",
    "destination",
    "zone",
    "effect_zone",
    "affected_zone",
];

/// Recursively walk a JSON value and fix any zone strings in known zone fields.
/// Also fixes zone strings inside `trigger_zones` arrays.
/// Returns true if any changes were made.
fn fix_zones_in_value(value: &mut Value) -> bool {
    let mut changed = false;
    match value {
        Value::Object(map) => {
            // Fix known zone fields
            let keys: Vec<String> = map.keys().cloned().collect();
            for key in &keys {
                if ZONE_FIELD_NAMES.contains(&key.as_str()) {
                    if let Some(zone_val) = map.get(key) {
                        if let Some(z) = zone_val.as_str() {
                            if !VALID_ZONES.contains(&z) {
                                let fixed = map_zone_string(z);
                                map.insert(key.clone(), json!(fixed));
                                changed = true;
                            }
                        }
                    }
                }
            }

            // Fix trigger_zones arrays
            if let Some(tz) = map.get_mut("trigger_zones") {
                if let Some(arr) = tz.as_array_mut() {
                    for zone_val in arr.iter_mut() {
                        if let Some(z) = zone_val.as_str() {
                            if !VALID_ZONES.contains(&z) {
                                let fixed = map_zone_string(z);
                                *zone_val = json!(fixed);
                                changed = true;
                            }
                        }
                    }
                }
            }

            // Recurse into all values
            for val in map.values_mut() {
                changed |= fix_zones_in_value(val);
            }
        }
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                changed |= fix_zones_in_value(item);
            }
        }
        _ => {}
    }
    changed
}

/// Recursively walk a JSON value and fix any TargetFilter objects with invalid card_type.
/// This catches deeply nested filters that aren't reached by the targeted migration functions.
/// Returns true if any changes were made.
fn fix_target_filters_deep(value: &mut Value) -> bool {
    let mut changed = false;
    match value {
        Value::Object(map) => {
            // Check if this object is a Typed TargetFilter with invalid card_type
            let is_typed = map
                .get("type")
                .and_then(|t| t.as_str())
                .is_some_and(|t| t == "Typed");

            if is_typed {
                // Delegate to migrate_target_filter which handles all the fixing
                changed |= migrate_target_filter(value);
            }

            // Recurse into all values regardless
            if let Value::Object(map) = value {
                for val in map.values_mut() {
                    changed |= fix_target_filters_deep(val);
                }
            }
        }
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                changed |= fix_target_filters_deep(item);
            }
        }
        _ => {}
    }
    changed
}

/// Recursively walk a JSON value and fix invalid ManaColor values in known color fields
/// (`colors` arrays and `color` fields), as well as SetColor/AddColor modifications.
fn fix_colors_deep(value: &mut Value) -> bool {
    let mut changed = false;
    match value {
        Value::Object(map) => {
            // Fix "modifications" arrays (SetColor/AddColor with invalid colors)
            if let Some(mods_val) = map.get_mut("modifications") {
                if let Some(mods) = mods_val.as_array_mut() {
                    changed |= fix_type_modifications(mods);
                }
            }

            // Fix "colors" arrays that contain invalid ManaColor values (e.g. on Token effects)
            if let Some(colors_val) = map.get_mut("colors") {
                if let Some(colors) = colors_val.as_array_mut() {
                    let original_len = colors.len();
                    colors.retain(|c| c.as_str().is_none_or(is_valid_mana_color));
                    if colors.len() != original_len {
                        changed = true;
                    }
                }
            }

            // Fix "color" field if it's an invalid ManaColor in a modification context
            let should_remove_color = map
                .get("color")
                .and_then(|v| v.as_str())
                .is_some_and(|c| !is_valid_mana_color(c))
                && map
                    .get("type")
                    .and_then(|t| t.as_str())
                    .is_some_and(|t| t == "AddColor");
            if should_remove_color {
                map.remove("color");
                changed = true;
            }

            // Recurse into all values
            for val in map.values_mut() {
                changed |= fix_colors_deep(val);
            }
        }
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                changed |= fix_colors_deep(item);
            }
        }
        _ => {}
    }
    changed
}

/// Migrate an entire ability file. Returns true if any changes were made.
fn migrate_ability_file(root: &mut Value, path: &Path, stats: &mut Stats) -> bool {
    let mut changed = false;

    // Run deep fixers on the entire JSON tree first
    changed |= fix_zones_in_value(root);
    changed |= fix_target_filters_deep(root);
    changed |= fix_colors_deep(root);
    changed |= fix_split_keywords_deep(root);

    if let Some(abilities) = root.get_mut("abilities").and_then(|v| v.as_array_mut()) {
        for ability in abilities.iter_mut() {
            changed |= migrate_ability_definition(ability, path, stats);
        }
    }

    if let Some(triggers) = root.get_mut("triggers").and_then(|v| v.as_array_mut()) {
        for trigger in triggers.iter_mut() {
            changed |= migrate_trigger_definition(trigger, path, stats);
        }
    }

    if let Some(statics) = root.get_mut("statics").and_then(|v| v.as_array_mut()) {
        for static_def in statics.iter_mut() {
            changed |= migrate_static_definition(static_def, path, stats);
        }
    }

    if let Some(replacements) = root.get_mut("replacements").and_then(|v| v.as_array_mut()) {
        for repl in replacements.iter_mut() {
            changed |= migrate_replacement_definition(repl, path, stats);
        }
    }

    if let Some(faces) = root.get_mut("faces").and_then(|v| v.as_array_mut()) {
        for face in faces.iter_mut() {
            if let Some(abilities) = face.get_mut("abilities").and_then(|v| v.as_array_mut()) {
                for ability in abilities.iter_mut() {
                    changed |= migrate_ability_definition(ability, path, stats);
                }
            }
            if let Some(triggers) = face.get_mut("triggers").and_then(|v| v.as_array_mut()) {
                for trigger in triggers.iter_mut() {
                    changed |= migrate_trigger_definition(trigger, path, stats);
                }
            }
            if let Some(statics) = face.get_mut("statics").and_then(|v| v.as_array_mut()) {
                for static_def in statics.iter_mut() {
                    changed |= migrate_static_definition(static_def, path, stats);
                }
            }
            if let Some(replacements) = face.get_mut("replacements").and_then(|v| v.as_array_mut())
            {
                for repl in replacements.iter_mut() {
                    changed |= migrate_replacement_definition(repl, path, stats);
                }
            }
        }
    }

    changed
}

/// Migrate an AbilityDefinition: extract remaining_params into typed fields, fix effect, fix cost.
fn migrate_ability_definition(ability: &mut Value, path: &Path, stats: &mut Stats) -> bool {
    let mut changed = false;

    // Migrate the effect
    if let Some(effect) = ability.get_mut("effect") {
        changed |= migrate_effect(effect, path, stats);
    }

    // Migrate the cost
    if let Some(cost) = ability.get_mut("cost") {
        changed |= migrate_cost(cost, path, stats);
    }

    // Migrate sub_ability recursively
    if let Some(sub) = ability.get_mut("sub_ability") {
        if !sub.is_null() {
            changed |= migrate_ability_definition(sub, path, stats);
        }
    }

    // Extract remaining_params into typed fields
    if let Some(remaining) = ability.get("remaining_params").cloned() {
        if let Some(map) = remaining.as_object() {
            if !map.is_empty() {
                changed = true;

                // SpellDescription / StackDescription -> description
                if let Some(desc) = map
                    .get("SpellDescription")
                    .or_else(|| map.get("StackDescription"))
                {
                    if ability.get("description").is_none_or(|v| v.is_null()) {
                        ability["description"] = desc.clone();
                    }
                }

                // TgtPrompt -> target_prompt
                if let Some(prompt) = map.get("TgtPrompt") {
                    if ability.get("target_prompt").is_none_or(|v| v.is_null()) {
                        ability["target_prompt"] = prompt.clone();
                    }
                }

                // SorcerySpeed -> sorcery_speed
                if map.contains_key("SorcerySpeed") {
                    ability["sorcery_speed"] = json!(true);
                }

                // Duration -> duration
                if let Some(dur) = map.get("Duration") {
                    if let Some(dur_str) = dur.as_str() {
                        let mapped = match dur_str {
                            "UntilEndOfTurn" | "EOT" => Some("UntilEndOfTurn"),
                            "UntilYourNextTurn" => Some("UntilYourNextTurn"),
                            "UntilHostLeavesPlay" | "UntilLeavesTheBattlefield" => {
                                Some("UntilHostLeavesPlay")
                            }
                            "Permanent" => Some("Permanent"),
                            _ => None,
                        };
                        if let Some(dur_val) = mapped {
                            if ability.get("duration").is_none_or(|v| v.is_null()) {
                                ability["duration"] = json!(dur_val);
                            }
                        }
                    }
                }
            }
        }

        // Remove remaining_params
        if let Some(obj) = ability.as_object_mut() {
            obj.remove("remaining_params");
        }
    }

    changed
}

/// Migrate an Effect value: handle Effect::Other, old Mana format, old TargetFilter, params removal.
fn migrate_effect(effect: &mut Value, path: &Path, stats: &mut Stats) -> bool {
    let mut changed = false;

    let effect_type = match effect.get("type").and_then(|t| t.as_str()) {
        Some(t) => t.to_string(),
        None => return false,
    };

    // Effect::Other -> Effect::Unimplemented
    if effect_type == "Other" {
        let api_type = effect
            .get("api_type")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown")
            .to_string();
        let description = effect
            .get("params")
            .and_then(|p| p.get("SpellDescription"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let mut new_effect = Map::new();
        new_effect.insert("type".to_string(), json!("Unimplemented"));
        new_effect.insert("name".to_string(), json!(api_type));
        if let Some(desc) = description {
            new_effect.insert("description".to_string(), json!(desc));
        }
        *effect = Value::Object(new_effect);
        return true;
    }

    // Effect::Mana - fix produced field from string to array
    if effect_type == "Mana" {
        if let Some(produced) = effect.get("produced") {
            if produced.is_string() {
                changed = true;
                let produced_str = produced.as_str().unwrap();
                let colors: Vec<Value> = parse_mana_produced(produced_str);
                effect["produced"] = Value::Array(colors);
            }
        }
        // Remove params from Mana effect
        if effect.get("params").is_some() {
            effect.as_object_mut().unwrap().remove("params");
            changed = true;
        }
    }

    // Fix TargetFilter on any effect that has a target field
    if let Some(target) = effect.get_mut("target") {
        changed |= migrate_target_filter(target);
    }

    // Remove params from any effect (not just Other/Mana)
    if let Some(obj) = effect.as_object_mut() {
        if obj.contains_key("params") {
            obj.remove("params");
            changed = true;
        }
    }

    // Fix zone fields that are strings instead of enum values
    for zone_field in &["origin", "destination"] {
        if let Some(zone) = effect.get(zone_field) {
            if let Some(z) = zone.as_str() {
                let mapped = map_zone_string(z);
                if mapped != z {
                    effect[*zone_field] = json!(mapped);
                    changed = true;
                }
            }
        }
    }

    // Fix color fields
    if effect_type == "Token" {
        if let Some(colors) = effect.get_mut("colors") {
            if let Some(arr) = colors.as_array_mut() {
                for color in arr.iter_mut() {
                    if let Some(c) = color.as_str() {
                        let mapped = map_color_string(c);
                        if mapped != c {
                            *color = json!(mapped);
                            changed = true;
                        }
                    }
                }
            }
        }
    }

    // Fix GenericEffect - ensure static_abilities is properly typed
    if effect_type == "GenericEffect" || effect_type == "Effect" {
        if let Some(statics) = effect.get_mut("static_abilities") {
            if let Some(arr) = statics.as_array_mut() {
                for s in arr.iter_mut() {
                    changed |= migrate_static_definition(s, path, stats);
                }
            }
        }
    }

    changed
}

/// Migrate a cost value: convert Forge SVar cost strings to typed AbilityCost variants.
#[allow(clippy::only_used_in_recursion)]
fn migrate_cost(cost: &mut Value, path: &Path, stats: &mut Stats) -> bool {
    if cost.is_null() {
        return false;
    }
    let mut changed = false;

    let cost_type = match cost.get("type").and_then(|t| t.as_str()) {
        Some(t) => t.to_string(),
        None => return false,
    };

    if cost_type == "Composite" {
        if let Some(costs) = cost.get_mut("costs").and_then(|v| v.as_array_mut()) {
            for c in costs.iter_mut() {
                changed |= migrate_cost(c, path, stats);
            }
        }
        return changed;
    }

    // Fix Sacrifice target filter
    if cost_type == "Sacrifice" {
        if let Some(target) = cost.get_mut("target") {
            changed |= migrate_target_filter(target);
        }
        return changed;
    }

    // Fix Discard target filter
    if cost_type == "Discard" {
        if let Some(filter) = cost.get_mut("filter") {
            changed |= migrate_target_filter(filter);
        }
        return changed;
    }

    // Fix Exile target filter
    if cost_type == "Exile" {
        if let Some(filter) = cost.get_mut("filter") {
            changed |= migrate_target_filter(filter);
        }
        return changed;
    }

    // Convert Mana cost with string value to structured ManaCost or reclassify
    if cost_type == "Mana" {
        if let Some(cost_val) = cost.get("cost") {
            if let Some(cost_str) = cost_val.as_str() {
                changed = true;
                if let Some(reclassified) = reclassify_forge_cost(cost_str) {
                    // Non-mana cost: replace entire cost object
                    *cost = reclassified;
                } else {
                    // Pure mana cost: convert string to structured ManaCost
                    cost["cost"] = parse_mana_cost_string(cost_str);
                }
            }
        }
    }

    changed
}

/// Check if a Forge cost string is actually a non-mana cost and reclassify it.
/// Returns Some(new_cost_json) if reclassified, None if it's a pure mana cost.
fn reclassify_forge_cost(s: &str) -> Option<Value> {
    // Discard<N/Filter> or Discard<N/Filter/description>
    if let Some(inner) = strip_forge_angle("Discard", s) {
        let parts: Vec<&str> = inner.splitn(3, '/').collect();
        let count = parse_forge_count(parts.first().copied().unwrap_or("1"));
        let filter_str = parts.get(1).copied().unwrap_or("Card");
        let random = filter_str == "Random";
        let filter = forge_cost_filter(filter_str);
        let mut obj = json!({"type": "Discard", "count": count});
        if let Some(f) = filter {
            obj["filter"] = f;
        }
        if random {
            obj["random"] = json!(true);
        }
        return Some(obj);
    }

    // PayLife<N>
    if let Some(inner) = strip_forge_angle("PayLife", s) {
        let amount = parse_forge_count(inner);
        return Some(json!({"type": "PayLife", "amount": amount}));
    }

    // PayEnergy<N>
    if let Some(inner) = strip_forge_angle("PayEnergy", s) {
        let amount = parse_forge_count(inner);
        return Some(json!({"type": "PayEnergy", "amount": amount}));
    }

    // SubCounter<N/TYPE> or SubCounter<N/TYPE/Filter/description>
    if let Some(inner) = strip_forge_angle("SubCounter", s) {
        let parts: Vec<&str> = inner.splitn(4, '/').collect();
        let count = parse_forge_count(parts.first().copied().unwrap_or("1"));
        let counter_type = parts.get(1).copied().unwrap_or("P1P1").to_string();

        // Check if this is a loyalty cost
        if counter_type == "LOYALTY" {
            return Some(json!({"type": "Loyalty", "amount": -(count as i64)}));
        }

        let mut obj =
            json!({"type": "RemoveCounter", "count": count, "counter_type": counter_type});
        // If there's a filter (3rd part), parse it as target
        if let Some(filter_str) = parts.get(2) {
            if !filter_str.is_empty() {
                obj["target"] = parse_forge_filter_to_target_filter(filter_str);
            }
        }
        return Some(obj);
    }

    // RemoveAnyCounter<N/Any/Filter/description>
    if let Some(inner) = strip_forge_angle("RemoveAnyCounter", s) {
        let parts: Vec<&str> = inner.splitn(4, '/').collect();
        let count = parse_forge_count(parts.first().copied().unwrap_or("1"));
        let mut obj = json!({"type": "RemoveCounter", "count": count, "counter_type": "Any"});
        if let Some(filter_str) = parts.get(2) {
            if !filter_str.is_empty() {
                obj["target"] = parse_forge_filter_to_target_filter(filter_str);
            }
        }
        return Some(obj);
    }

    // ExileFromGrave<N/Filter> or ExileFromGrave<N/Filter/description>
    if let Some(inner) = strip_forge_angle("ExileFromGrave", s) {
        let parts: Vec<&str> = inner.splitn(3, '/').collect();
        let count = parse_forge_count(parts.first().copied().unwrap_or("1"));
        let filter_str = parts.get(1).copied().unwrap_or("Card");
        let filter = forge_cost_filter(filter_str);
        let mut obj = json!({"type": "Exile", "count": count, "zone": "Graveyard"});
        if let Some(f) = filter {
            obj["filter"] = f;
        }
        return Some(obj);
    }

    // ExileFromHand<N/Filter>
    if let Some(inner) = strip_forge_angle("ExileFromHand", s) {
        let parts: Vec<&str> = inner.splitn(3, '/').collect();
        let count = parse_forge_count(parts.first().copied().unwrap_or("1"));
        let filter_str = parts.get(1).copied().unwrap_or("Card");
        let filter = forge_cost_filter(filter_str);
        let mut obj = json!({"type": "Exile", "count": count, "zone": "Hand"});
        if let Some(f) = filter {
            obj["filter"] = f;
        }
        return Some(obj);
    }

    // ExileFromTop<N/Filter>
    if let Some(inner) = strip_forge_angle("ExileFromTop", s) {
        let parts: Vec<&str> = inner.splitn(3, '/').collect();
        let count = parse_forge_count(parts.first().copied().unwrap_or("1"));
        let filter_str = parts.get(1).copied().unwrap_or("Card");
        let filter = forge_cost_filter(filter_str);
        let mut obj = json!({"type": "Exile", "count": count, "zone": "Library"});
        if let Some(f) = filter {
            obj["filter"] = f;
        }
        return Some(obj);
    }

    // Exile<N/Filter> (from battlefield)
    if let Some(inner) = strip_forge_angle("Exile", s) {
        let parts: Vec<&str> = inner.splitn(3, '/').collect();
        let count = parse_forge_count(parts.first().copied().unwrap_or("1"));
        let filter_str = parts.get(1).copied().unwrap_or("Card");
        let filter = forge_cost_filter(filter_str);
        let mut obj = json!({"type": "Exile", "count": count});
        if let Some(f) = filter {
            obj["filter"] = f;
        }
        return Some(obj);
    }

    // tapXType<N/Type>
    if let Some(inner) = strip_forge_angle("tapXType", s) {
        let parts: Vec<&str> = inner.splitn(2, '/').collect();
        let count = parse_forge_count(parts.first().copied().unwrap_or("1"));
        let filter_str = parts.get(1).copied().unwrap_or("Creature");
        let filter = parse_forge_filter_to_target_filter(filter_str);
        return Some(json!({"type": "TapCreatures", "count": count, "filter": filter}));
    }

    // Return<N/Filter>
    if let Some(inner) = strip_forge_angle("Return", s) {
        let parts: Vec<&str> = inner.splitn(3, '/').collect();
        let count = parse_forge_count(parts.first().copied().unwrap_or("1"));
        let filter_str = parts.get(1).copied().unwrap_or("Card");
        let filter = forge_cost_filter(filter_str);
        let mut obj = json!({"type": "ReturnToHand", "count": count});
        if let Some(f) = filter {
            obj["filter"] = f;
        }
        return Some(obj);
    }

    // Mill<N>
    if let Some(inner) = strip_forge_angle("Mill", s) {
        let count = parse_forge_count(inner);
        return Some(json!({"type": "Mill", "count": count}));
    }

    // Exert<N/Filter>
    if s.starts_with("Exert") {
        return Some(json!({"type": "Exert"}));
    }

    // Reveal<N/Filter>
    if let Some(inner) = strip_forge_angle("Reveal", s) {
        let parts: Vec<&str> = inner.splitn(2, '/').collect();
        let count = parse_forge_count(parts.first().copied().unwrap_or("1"));
        return Some(json!({"type": "Reveal", "count": count}));
    }

    // AddCounter as a cost (rare)
    if s.starts_with("AddCounter<") {
        return Some(json!({"type": "Unimplemented", "description": s}));
    }

    // Waterbend and other exotic costs
    if s.contains('<') && !is_pure_mana_string(s) {
        return Some(json!({"type": "Unimplemented", "description": s}));
    }

    // It's a pure mana cost string
    None
}

/// Strip a Forge angle-bracket wrapper: "Discard<1/Card>" → Some("1/Card")
fn strip_forge_angle<'a>(prefix: &str, s: &'a str) -> Option<&'a str> {
    s.strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix('<'))
        .and_then(|rest| rest.strip_suffix('>'))
}

/// Parse a Forge count value: number or "X" (X becomes 0).
fn parse_forge_count(s: &str) -> u32 {
    if s == "X" || s == "x" {
        0
    } else {
        s.parse::<u32>().unwrap_or(1)
    }
}

/// Check if a string looks like a pure mana cost (only digits and WUBRGCXS letters).
fn is_pure_mana_string(s: &str) -> bool {
    s.split_whitespace().all(|part| {
        part.parse::<u32>().is_ok()
            || part
                .chars()
                .all(|c| matches!(c, 'W' | 'U' | 'B' | 'R' | 'G' | 'C' | 'X' | 'S' | '/'))
    })
}

/// Convert a Forge cost filter string to a TargetFilter JSON value.
/// Used for cost filter args like "Card", "CARDNAME", "Creature", "Land", etc.
fn forge_cost_filter(s: &str) -> Option<Value> {
    match s {
        "Card" | "card" | "Any" => None,
        "CARDNAME" | "Self" => Some(json!({"type": "SelfRef"})),
        "Random" => None,
        "Hand" => None,
        "Creature" | "creature" => Some(json!({"type": "Typed", "card_type": "Creature"})),
        "Land" | "land" => Some(json!({"type": "Typed", "card_type": "Land"})),
        "Artifact" | "artifact" => Some(json!({"type": "Typed", "card_type": "Artifact"})),
        "Enchantment" | "enchantment" => Some(json!({"type": "Typed", "card_type": "Enchantment"})),
        "Instant" | "instant" => Some(json!({"type": "Typed", "card_type": "Instant"})),
        "Sorcery" | "sorcery" => Some(json!({"type": "Typed", "card_type": "Sorcery"})),
        "Permanent" | "permanent" => Some(json!({"type": "Typed", "card_type": "Permanent"})),
        _ => Some(parse_forge_filter_to_target_filter(s)),
    }
}

/// Parse a Forge mana cost string like "1 W" into structured ManaCost JSON.
/// Returns either "NoCost" or {"Cost": {"shards": [...], "generic": N}}.
fn parse_mana_cost_string(s: &str) -> Value {
    let s = s.trim();
    if s.is_empty() || s == "0" {
        return json!({"Cost": {"shards": [], "generic": 0}});
    }

    let mut shards = Vec::new();
    let mut generic = 0u32;

    for part in s.split_whitespace() {
        if let Ok(n) = part.parse::<u32>() {
            generic += n;
        } else if part.contains('/') {
            // Hybrid mana: W/U, 2/W, W/U/P, etc.
            shards.push(json!(map_hybrid_shard(part)));
        } else {
            for ch in part.chars() {
                match ch {
                    'W' => shards.push(json!("White")),
                    'U' => shards.push(json!("Blue")),
                    'B' => shards.push(json!("Black")),
                    'R' => shards.push(json!("Red")),
                    'G' => shards.push(json!("Green")),
                    'C' => shards.push(json!("Colorless")),
                    'X' => shards.push(json!("X")),
                    'S' => shards.push(json!("Snow")),
                    _ => generic += 1,
                }
            }
        }
    }

    json!({"Cost": {"shards": shards, "generic": generic}})
}

/// Map a hybrid mana notation to a ManaCostShard name.
fn map_hybrid_shard(s: &str) -> &str {
    match s {
        "W/U" => "WhiteBlue",
        "W/B" => "WhiteBlack",
        "U/B" => "BlueBlack",
        "U/R" => "BlueRed",
        "B/R" => "BlackRed",
        "B/G" => "BlackGreen",
        "R/G" => "RedGreen",
        "R/W" => "RedWhite",
        "G/W" => "GreenWhite",
        "G/U" => "GreenBlue",
        // Generic hybrid (2/{color})
        "2/W" => "TwoWhite",
        "2/U" => "TwoBlue",
        "2/B" => "TwoBlack",
        "2/R" => "TwoRed",
        "2/G" => "TwoGreen",
        // Phyrexian
        "W/P" => "PhyrexianWhite",
        "U/P" => "PhyrexianBlue",
        "B/P" => "PhyrexianBlack",
        "R/P" => "PhyrexianRed",
        "G/P" => "PhyrexianGreen",
        // Phyrexian hybrid
        "W/U/P" => "PhyrexianWhiteBlue",
        "W/B/P" => "PhyrexianWhiteBlack",
        "U/B/P" => "PhyrexianBlueBlack",
        "U/R/P" => "PhyrexianBlueRed",
        "B/R/P" => "PhyrexianBlackRed",
        "B/G/P" => "PhyrexianBlackGreen",
        "R/G/P" => "PhyrexianRedGreen",
        "R/W/P" => "PhyrexianRedWhite",
        "G/W/P" => "PhyrexianGreenWhite",
        "G/U/P" => "PhyrexianGreenBlue",
        // Colorless hybrid
        "C/W" => "ColorlessWhite",
        "C/U" => "ColorlessBlue",
        "C/B" => "ColorlessBlack",
        "C/R" => "ColorlessRed",
        "C/G" => "ColorlessGreen",
        _ => "X", // Unknown hybrid: fallback
    }
}

/// Migrate a TriggerDefinition from old params format to typed fields.
fn migrate_trigger_definition(trigger: &mut Value, path: &Path, stats: &mut Stats) -> bool {
    let mut changed = false;

    // Check if already migrated (has no "params" key)
    let params = match trigger.get("params").cloned() {
        Some(Value::Object(map)) if !map.is_empty() => map,
        _ => {
            // Already migrated or no params — still fix any nested structures
            if let Some(exec) = trigger.get_mut("execute") {
                if !exec.is_null() {
                    changed |= migrate_ability_definition(exec, path, stats);
                }
            }
            if let Some(vc) = trigger.get_mut("valid_card") {
                changed |= migrate_target_filter(vc);
            }
            if let Some(vt) = trigger.get_mut("valid_target") {
                changed |= migrate_target_filter(vt);
            }
            if let Some(vs) = trigger.get_mut("valid_source") {
                changed |= migrate_target_filter(vs);
            }
            return changed;
        }
    };
    changed = true;

    // Extract typed fields from params
    if let Some(desc) = params
        .get("TriggerDescription")
        .or(params.get("Description"))
    {
        trigger["description"] = desc.clone();
    }

    if let Some(vc) = params.get("ValidCard") {
        trigger["valid_card"] = parse_forge_filter_to_target_filter(vc.as_str().unwrap_or(""));
    }

    if let Some(origin) = params.get("Origin") {
        if let Some(z) = origin.as_str() {
            trigger["origin"] = json!(map_zone_string(z));
        }
    }

    if let Some(dest) = params.get("Destination") {
        if let Some(z) = dest.as_str() {
            trigger["destination"] = json!(map_zone_string(z));
        }
    }

    if let Some(tz) = params.get("TriggerZones") {
        if let Some(s) = tz.as_str() {
            let zones: Vec<Value> = s
                .split(',')
                .map(|z| json!(map_zone_string(z.trim())))
                .collect();
            trigger["trigger_zones"] = Value::Array(zones);
        }
    }

    if let Some(phase) = params.get("Phase") {
        if let Some(s) = phase.as_str() {
            let mapped = map_phase_string(s);
            if let Some(p) = mapped {
                trigger["phase"] = json!(p);
            }
        }
    }

    if params.contains_key("OptionalDecider") {
        trigger["optional"] = json!(true);
    }

    if let Some(cd) = params.get("CombatDamage") {
        if cd.as_str() == Some("True") {
            trigger["combat_damage"] = json!(true);
        }
    }

    if let Some(sec) = params.get("Secondary") {
        if sec.as_str() == Some("True") {
            trigger["secondary"] = json!(true);
        }
    }

    if let Some(vt) = params.get("ValidTarget") {
        trigger["valid_target"] = parse_forge_filter_to_target_filter(vt.as_str().unwrap_or(""));
    }

    if let Some(vs) = params.get("ValidSource") {
        trigger["valid_source"] = parse_forge_filter_to_target_filter(vs.as_str().unwrap_or(""));
    }

    // The Execute SVar reference can't be resolved from JSON alone (no SVar table).
    // Set execute to null; card will use the existing resolved execute if present.
    if let Some(exec) = params.get("Execute") {
        if exec.as_str().is_some() {
            // Can't resolve SVar reference at this level -- leave execute as-is
            stats.warnings.push((
                path.to_path_buf(),
                format!(
                    "Trigger has unresolvable Execute SVar: {}",
                    exec.as_str().unwrap_or("?")
                ),
            ));
        }
    }

    // Remove the old params field
    if let Some(obj) = trigger.as_object_mut() {
        obj.remove("params");
    }

    changed
}

/// Migrate a StaticDefinition from old params format to typed fields.
fn migrate_static_definition(static_def: &mut Value, _path: &Path, _stats: &mut Stats) -> bool {
    let mut changed = false;

    // Check if already migrated
    let params = match static_def.get("params").cloned() {
        Some(Value::Object(map)) if !map.is_empty() => map,
        _ => {
            // Already migrated -- fix nested structures
            if let Some(affected) = static_def.get_mut("affected") {
                changed |= migrate_target_filter(affected);
            }
            // Fix AddType/RemoveType with non-CoreType values → AddSubtype/RemoveSubtype
            if let Some(mods) = static_def
                .get_mut("modifications")
                .and_then(|v| v.as_array_mut())
            {
                changed |= fix_type_modifications(mods);
            }
            return changed;
        }
    };
    changed = true;

    // Description
    if let Some(desc) = params.get("Description") {
        static_def["description"] = desc.clone();
    }

    // Affected -> typed TargetFilter
    if let Some(affected) = params.get("Affected") {
        static_def["affected"] =
            parse_forge_filter_to_target_filter(affected.as_str().unwrap_or(""));
    }

    // EffectZone
    if let Some(ez) = params.get("EffectZone") {
        if let Some(z) = ez.as_str() {
            static_def["effect_zone"] = json!(map_zone_string(z));
        }
    }

    // AffectedZone
    if let Some(az) = params.get("AffectedZone") {
        if let Some(z) = az.as_str() {
            static_def["affected_zone"] = json!(map_zone_string(z));
        }
    }

    // CharacteristicDefining
    if let Some(cd) = params.get("CharacteristicDefining") {
        if cd.as_str() == Some("True") {
            static_def["characteristic_defining"] = json!(true);
        }
    }

    // Build modifications array from param keys
    let mut modifications = Vec::new();

    if let Some(v) = params.get("AddPower") {
        if let Some(n) = parse_i32_value(v) {
            modifications.push(json!({"type": "AddPower", "value": n}));
        }
    }
    if let Some(v) = params.get("AddToughness") {
        if let Some(n) = parse_i32_value(v) {
            modifications.push(json!({"type": "AddToughness", "value": n}));
        }
    }
    if let Some(v) = params.get("SetPower") {
        if let Some(n) = parse_i32_value(v) {
            modifications.push(json!({"type": "SetPower", "value": n}));
        }
    }
    if let Some(v) = params.get("SetToughness") {
        if let Some(n) = parse_i32_value(v) {
            modifications.push(json!({"type": "SetToughness", "value": n}));
        }
    }
    if let Some(kw) = params.get("AddKeyword") {
        if let Some(s) = kw.as_str() {
            for k in split_keyword_list(s) {
                modifications.push(json!({"type": "AddKeyword", "keyword": k}));
            }
        }
    }
    if let Some(kw) = params.get("RemoveKeyword") {
        if let Some(s) = kw.as_str() {
            for k in split_keyword_list(s) {
                modifications.push(json!({"type": "RemoveKeyword", "keyword": k}));
            }
        }
    }
    if let Some(ab) = params.get("AddAbility") {
        if let Some(s) = ab.as_str() {
            modifications.push(json!({"type": "AddAbility", "ability": s}));
        }
    }
    if params.get("RemoveAllAbilities").and_then(|v| v.as_str()) == Some("True") {
        modifications.push(json!({"type": "RemoveAllAbilities"}));
    }
    if let Some(ct) = params.get("AddType") {
        if let Some(s) = ct.as_str() {
            for mod_val in classify_type_modification(s, true) {
                modifications.push(mod_val);
            }
        }
    }
    if let Some(ct) = params.get("RemoveType") {
        if let Some(s) = ct.as_str() {
            for mod_val in classify_type_modification(s, false) {
                modifications.push(mod_val);
            }
        }
    }
    if let Some(colors) = params.get("SetColor") {
        if let Some(s) = colors.as_str() {
            let color_list: Vec<Value> = s
                .split(',')
                .filter_map(|c| {
                    let mapped = map_color_string(c.trim());
                    if mapped != c.trim() || is_valid_color(c.trim()) {
                        Some(json!(mapped))
                    } else {
                        None
                    }
                })
                .collect();
            if !color_list.is_empty() {
                modifications.push(json!({"type": "SetColor", "colors": color_list}));
            }
        }
    }
    if let Some(color) = params.get("AddColor") {
        if let Some(s) = color.as_str() {
            let mapped = map_color_string(s);
            modifications.push(json!({"type": "AddColor", "color": mapped}));
        }
    }

    if !modifications.is_empty() {
        static_def["modifications"] = Value::Array(modifications);
    }

    // Condition from CheckSVar/IsPresent
    if let Some(svar) = params.get("CheckSVar") {
        let compare = params
            .get("SVarCompare")
            .or(params.get("ConditionSVarCompare"))
            .cloned()
            .unwrap_or(json!(""));
        static_def["condition"] = json!({
            "type": "CheckSVar",
            "var": svar,
            "compare": compare
        });
    } else if let Some(present) = params.get("IsPresent") {
        static_def["condition"] = json!({
            "type": "IsPresent",
            "filter": parse_forge_filter_to_target_filter(present.as_str().unwrap_or(""))
        });
    }

    // Remove old params field
    if let Some(obj) = static_def.as_object_mut() {
        obj.remove("params");
    }

    changed
}

/// Migrate a ReplacementDefinition from old params format to typed fields.
fn migrate_replacement_definition(repl: &mut Value, path: &Path, stats: &mut Stats) -> bool {
    let mut changed = false;

    // Check if already migrated
    let params = match repl.get("params").cloned() {
        Some(Value::Object(map)) if !map.is_empty() => map,
        _ => {
            // Already migrated -- fix nested structures
            if let Some(exec) = repl.get_mut("execute") {
                if !exec.is_null() {
                    changed |= migrate_ability_definition(exec, path, stats);
                }
            }
            if let Some(vc) = repl.get_mut("valid_card") {
                changed |= migrate_target_filter(vc);
            }
            return changed;
        }
    };
    changed = true;

    // Description
    if let Some(desc) = params.get("Description") {
        repl["description"] = desc.clone();
    }

    // ValidCard
    if let Some(vc) = params.get("ValidCard") {
        repl["valid_card"] = parse_forge_filter_to_target_filter(vc.as_str().unwrap_or(""));
    }

    // Execute SVar -- can't resolve
    if let Some(exec) = params.get("Execute") {
        if exec.as_str().is_some() {
            stats.warnings.push((
                path.to_path_buf(),
                format!(
                    "Replacement has unresolvable Execute SVar: {}",
                    exec.as_str().unwrap_or("?")
                ),
            ));
        }
    }

    // Remove old params
    if let Some(obj) = repl.as_object_mut() {
        obj.remove("params");
    }

    changed
}

/// Migrate an old-format TargetFilter (All/Filtered) to new typed format.
/// Returns true if changed.
fn migrate_target_filter(filter: &mut Value) -> bool {
    if filter.is_null() {
        return false;
    }

    let filter_type = match filter.get("type").and_then(|t| t.as_str()) {
        Some(t) => t.to_string(),
        None => return false,
    };

    match filter_type.as_str() {
        "All" => {
            // Old: { "type": "All", "filter": "" } -> { "type": "Any" } or parse filter
            let filter_str = filter.get("filter").and_then(|f| f.as_str()).unwrap_or("");
            if filter_str.is_empty() {
                *filter = json!({"type": "Any"});
            } else {
                *filter = parse_forge_filter_to_target_filter(filter_str);
            }
            true
        }
        "Filtered" => {
            // Old: { "type": "Filtered", "filter": "Creature" } -> typed
            let filter_str = filter.get("filter").and_then(|f| f.as_str()).unwrap_or("");
            *filter = parse_forge_filter_to_target_filter(filter_str);
            true
        }
        "Typed" => {
            // Fix card_type field: move non-TypeFilter values to subtype
            let mut changed = false;
            if let Some(ct_val) = filter.get("card_type").cloned() {
                if let Some(ct) = ct_val.as_str() {
                    // CARDNAME / NICKNAME → replace entire filter with SelfRef
                    if ct == "CARDNAME" || ct == "NICKNAME" {
                        *filter = json!({"type": "SelfRef"});
                        return true;
                    }

                    // Strip description suffix after "/" (e.g. "Creature/creature" → "Creature")
                    let ct_clean = ct.split('/').next().unwrap_or(ct);

                    let valid_type_filters = [
                        "Creature",
                        "Land",
                        "Artifact",
                        "Enchantment",
                        "Instant",
                        "Sorcery",
                        "Planeswalker",
                        "Permanent",
                        "Card",
                        "Any",
                    ];

                    // Handle semicolon-separated types: "Artifact;Creature;Land"
                    if ct_clean.contains(';') {
                        changed = true;
                        let parts: Vec<&str> = ct_clean.split(';').map(|p| p.trim()).collect();
                        let valid_parts: Vec<&str> = parts
                            .iter()
                            .copied()
                            .filter(|p| valid_type_filters.contains(p))
                            .collect();
                        if valid_parts.len() == 1 {
                            filter["card_type"] = json!(valid_parts[0]);
                        } else if valid_parts.len() > 1 {
                            // Build an Or filter from the valid types
                            let filters: Vec<Value> = valid_parts
                                .iter()
                                .map(|t| {
                                    let mut f = filter.as_object().unwrap().clone();
                                    f.insert("card_type".to_string(), json!(t));
                                    Value::Object(f)
                                })
                                .collect();
                            *filter = json!({"type": "Or", "filters": filters});
                        } else {
                            // No valid types found - try first part as subtype
                            filter.as_object_mut().unwrap().remove("card_type");
                            filter["subtype"] = json!(parts[0]);
                        }
                    } else if ct_clean != ct {
                        // Had a "/" suffix that was stripped
                        changed = true;
                        if valid_type_filters.contains(&ct_clean) {
                            filter["card_type"] = json!(ct_clean);
                        } else {
                            filter.as_object_mut().unwrap().remove("card_type");
                            filter["subtype"] = json!(ct_clean);
                        }
                    } else if !valid_type_filters.contains(&ct) {
                        changed = true;
                        // Handle compound like "Player,Planeswalker"
                        if ct.contains(',') {
                            let parts: Vec<&str> = ct.split(',').collect();
                            let mut found_type = false;
                            for p in &parts {
                                let p = p.trim();
                                if valid_type_filters.contains(&p) && !found_type {
                                    filter["card_type"] = json!(p);
                                    found_type = true;
                                }
                            }
                            if !found_type {
                                filter.as_object_mut().unwrap().remove("card_type");
                            }
                        } else {
                            // Move to subtype field
                            filter.as_object_mut().unwrap().remove("card_type");
                            filter["subtype"] = json!(ct);
                        }
                    }
                }
            }
            changed
        }
        "Not" => {
            let mut changed = false;
            if let Some(inner) = filter.get_mut("filter") {
                changed |= migrate_target_filter(inner);
            }
            changed
        }
        "Or" | "And" => {
            let mut changed = false;
            if let Some(filters) = filter.get_mut("filters") {
                if let Some(arr) = filters.as_array_mut() {
                    for f in arr.iter_mut() {
                        changed |= migrate_target_filter(f);
                    }
                }
            }
            changed
        }
        _ => false, // None, Any, Player, Controller, SelfRef -- already valid
    }
}

/// Parse a Forge filter string like "Creature.Other+YouCtrl" into a typed TargetFilter JSON value.
fn parse_forge_filter_to_target_filter(filter: &str) -> Value {
    let filter = filter.trim();

    if filter.is_empty() || filter == "Any" {
        return json!({"type": "Any"});
    }
    if filter == "Card.Self" || filter == "Self" {
        return json!({"type": "SelfRef"});
    }
    if filter == "Player" || filter == "Player.You" || filter == "Player.Opponent" {
        return json!({"type": "Player"});
    }
    if filter == "Opponent" {
        return json!({"type": "Player"});
    }

    // Split on "." to get type part and property parts
    let parts: Vec<&str> = filter.split('.').collect();
    let type_part = parts[0];
    let prop_string = if parts.len() > 1 {
        parts[1..].join(".")
    } else {
        String::new()
    };
    let prop_parts: Vec<&str> = if prop_string.is_empty() {
        vec![]
    } else {
        prop_string.split('+').collect()
    };

    let type_mapping = map_type_filter(type_part);
    let mut controller: Option<&str> = None;
    let mut properties = Vec::new();
    let mut has_other = false;

    for prop in &prop_parts {
        let p = prop.trim();
        match p {
            "YouCtrl" => controller = Some("You"),
            "OppCtrl" => controller = Some("Opponent"),
            "Other" => has_other = true,
            "nonLand" => properties.push(json!({"type": "NonType", "value": "Land"})),
            "nonCreature" => properties.push(json!({"type": "NonType", "value": "Creature"})),
            "nonArtifact" => properties.push(json!({"type": "NonType", "value": "Artifact"})),
            "nonToken" => properties.push(json!({"type": "NonType", "value": "Token"})),
            "Token" | "token" => properties.push(json!({"type": "Token"})),
            "tapped" | "Tapped" => properties.push(json!({"type": "Tapped"})),
            "attacking" | "Attacking" => properties.push(json!({"type": "Attacking"})),
            "EnchantedBy" | "enchantedBy" => properties.push(json!({"type": "EnchantedBy"})),
            "EquippedBy" | "equippedBy" => properties.push(json!({"type": "EquippedBy"})),
            _ => {
                if p.starts_with(char::is_uppercase) && !p.contains('<') {
                    properties.push(json!({"type": "Other", "value": p}));
                }
            }
        }
    }

    let mut typed_filter = Map::new();
    typed_filter.insert("type".to_string(), json!("Typed"));
    match type_mapping {
        TypeMapping::CardType(ct) => {
            typed_filter.insert("card_type".to_string(), json!(ct));
        }
        TypeMapping::Subtype(st) => {
            typed_filter.insert("subtype".to_string(), json!(st));
        }
        TypeMapping::None => {}
    }
    if let Some(ctrl) = controller {
        typed_filter.insert("controller".to_string(), json!(ctrl));
    }
    if !properties.is_empty() {
        typed_filter.insert("properties".to_string(), Value::Array(properties));
    }

    let base = Value::Object(typed_filter);

    if has_other {
        // Wrap in And with Not(SelfRef)
        json!({
            "type": "And",
            "filters": [
                base,
                {"type": "Not", "filter": {"type": "SelfRef"}}
            ]
        })
    } else {
        base
    }
}

/// Result of mapping a Forge type string.
enum TypeMapping<'a> {
    CardType(&'a str),
    Subtype(&'a str),
    None,
}

/// Map a type string to TypeFilter enum value or subtype.
fn map_type_filter(s: &str) -> TypeMapping<'_> {
    match s {
        "Creature" | "creature" => TypeMapping::CardType("Creature"),
        "Land" | "land" => TypeMapping::CardType("Land"),
        "Artifact" | "artifact" => TypeMapping::CardType("Artifact"),
        "Enchantment" | "enchantment" => TypeMapping::CardType("Enchantment"),
        "Instant" | "instant" => TypeMapping::CardType("Instant"),
        "Sorcery" | "sorcery" => TypeMapping::CardType("Sorcery"),
        "Planeswalker" | "planeswalker" => TypeMapping::CardType("Planeswalker"),
        "Permanent" | "permanent" => TypeMapping::CardType("Permanent"),
        "Card" | "card" => TypeMapping::CardType("Card"),
        "Any" | "any" => TypeMapping::None,
        _ => TypeMapping::Subtype(s),
    }
}

/// Map zone strings to Zone enum values.
/// Handles empty strings, comma-separated zones, and invalid zone names.
fn map_zone_string(s: &str) -> &'static str {
    let s = s.trim();

    // Empty string defaults to Battlefield
    if s.is_empty() {
        return "Battlefield";
    }

    // Comma-separated zones: take the first one
    if s.contains(',') {
        let first = s.split(',').next().unwrap_or("").trim();
        return map_zone_string_simple(first);
    }

    map_zone_string_simple(s)
}

/// Map a single zone string (no commas) to a Zone enum value.
fn map_zone_string_simple(s: &str) -> &'static str {
    match s {
        "Battlefield" | "battlefield" => "Battlefield",
        "Hand" | "hand" => "Hand",
        "Graveyard" | "graveyard" => "Graveyard",
        "Library" | "library" => "Library",
        "Exile" | "exile" => "Exile",
        "Stack" | "stack" => "Stack",
        "Command" | "command" | "CommandZone" => "Command",
        "Any" | "any" | "All" => "Battlefield",
        // Invalid zones mapped to closest equivalents
        "Sideboard" => "Exile",
        "Ante" => "Exile",
        "AttractionDeck" => "Exile",
        "" => "Battlefield",
        _ => "Battlefield",
    }
}

/// Map phase strings to Phase enum values.
fn map_phase_string(s: &str) -> Option<&str> {
    match s {
        "Upkeep" | "upkeep" => Some("Upkeep"),
        "Draw" | "draw" => Some("Draw"),
        "Main1" | "PreCombatMain" => Some("PreCombatMain"),
        "Main2" | "PostCombatMain" => Some("PostCombatMain"),
        "BeginCombat" | "BeginningOfCombat" => Some("BeginCombat"),
        "DeclareAttackers" | "Declare Attackers" => Some("DeclareAttackers"),
        "DeclareBlockers" | "Declare Blockers" => Some("DeclareBlockers"),
        "CombatDamage" | "FirstStrikeDamage" => Some("CombatDamage"),
        "EndCombat" | "EndOfCombat" => Some("EndCombat"),
        "End" | "EndOfTurn" | "EndStep" => Some("EndStep"),
        "Cleanup" => Some("Cleanup"),
        _ => None,
    }
}

/// Map a color string to ManaColor enum name.
fn map_color_string(s: &str) -> &str {
    match s {
        "W" | "White" | "white" => "White",
        "U" | "Blue" | "blue" => "Blue",
        "B" | "Black" | "black" => "Black",
        "R" | "Red" | "red" => "Red",
        "G" | "Green" | "green" => "Green",
        "ChosenColor" => "ChosenColor", // Handled specially by callers
        other => other,
    }
}

/// Check if a color string is a valid ManaColor enum value.
fn is_valid_mana_color(s: &str) -> bool {
    matches!(s, "White" | "Blue" | "Black" | "Red" | "Green")
}

fn is_valid_color(s: &str) -> bool {
    matches!(
        s,
        "White" | "Blue" | "Black" | "Red" | "Green" | "W" | "U" | "B" | "R" | "G"
    )
}

/// Parse a Forge mana produced string like "G" or "U" into ManaColor JSON values.
fn parse_mana_produced(s: &str) -> Vec<Value> {
    s.chars()
        .filter_map(|c| match c {
            'W' => Some(json!("White")),
            'U' => Some(json!("Blue")),
            'B' => Some(json!("Black")),
            'R' => Some(json!("Red")),
            'G' => Some(json!("Green")),
            _ => None,
        })
        .collect()
}

/// Parse an i32 value from a JSON value (string or number).
fn parse_i32_value(v: &Value) -> Option<i32> {
    if let Some(n) = v.as_i64() {
        Some(n as i32)
    } else if let Some(s) = v.as_str() {
        s.parse::<i32>().ok()
    } else {
        None
    }
}

const VALID_CORE_TYPES: &[&str] = &[
    "Artifact",
    "Creature",
    "Enchantment",
    "Instant",
    "Land",
    "Planeswalker",
    "Sorcery",
    "Tribal",
    "Battle",
    "Kindred",
    "Dungeon",
];

/// Classify a type string as CoreType (AddType/RemoveType) or subtype (AddSubtype/RemoveSubtype).
/// Handles compound types like "Artifact & Creature" by splitting into multiple modifications.
fn classify_type_modification(s: &str, is_add: bool) -> Vec<Value> {
    let parts: Vec<&str> = s.split(" & ").map(|p| p.trim()).collect();
    let mut mods = Vec::new();

    for part in parts {
        // Check supertypes that should be ignored or handled separately
        if part == "Legendary" || part == "Basic" || part == "Snow" || part == "World" {
            // Supertypes aren't handled by AddType/AddSubtype - skip
            continue;
        }

        if part == "ChosenType" || part == "AllBasicLandType" || part == "AllNonBasicLandType" {
            // Special Forge references - use AddSubtype with the string
            if is_add {
                mods.push(json!({"type": "AddSubtype", "subtype": part}));
            } else {
                mods.push(json!({"type": "RemoveSubtype", "subtype": part}));
            }
            continue;
        }

        if VALID_CORE_TYPES.contains(&part) {
            if is_add {
                mods.push(json!({"type": "AddType", "core_type": part}));
            } else {
                mods.push(json!({"type": "RemoveType", "core_type": part}));
            }
        } else {
            // It's a subtype (creature type, land type, etc.)
            if is_add {
                mods.push(json!({"type": "AddSubtype", "subtype": part}));
            } else {
                mods.push(json!({"type": "RemoveSubtype", "subtype": part}));
            }
        }
    }

    if mods.is_empty() {
        // Fallback: preserve as subtype
        if is_add {
            mods.push(json!({"type": "AddSubtype", "subtype": s}));
        } else {
            mods.push(json!({"type": "RemoveSubtype", "subtype": s}));
        }
    }

    mods
}

/// Fix existing AddType/RemoveType modifications that have invalid core_type values,
/// and SetColor/AddColor modifications with invalid ManaColor values (e.g. ChosenColor).
fn fix_type_modifications(mods: &mut Vec<Value>) -> bool {
    let mut changed = false;
    let mut new_mods = Vec::new();

    for m in mods.iter() {
        let mod_type = m.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match mod_type {
            "AddType" | "RemoveType" => {
                if let Some(ct) = m.get("core_type").and_then(|v| v.as_str()) {
                    if !VALID_CORE_TYPES.contains(&ct) {
                        changed = true;
                        let is_add = mod_type == "AddType";
                        new_mods.extend(classify_type_modification(ct, is_add));
                        continue;
                    }
                }
                new_mods.push(m.clone());
            }
            "SetColor" => {
                // Filter out invalid colors (e.g. ChosenColor) from the colors array
                if let Some(colors) = m.get("colors").and_then(|v| v.as_array()) {
                    let has_invalid = colors
                        .iter()
                        .any(|c| c.as_str().is_some_and(|s| !is_valid_mana_color(s)));
                    if has_invalid {
                        changed = true;
                        // Keep only valid colors
                        let valid: Vec<Value> = colors
                            .iter()
                            .filter(|c| c.as_str().is_some_and(is_valid_mana_color))
                            .cloned()
                            .collect();
                        if !valid.is_empty() {
                            new_mods.push(json!({"type": "SetColor", "colors": valid}));
                        }
                        // If all colors invalid, skip this modification entirely
                        continue;
                    }
                }
                new_mods.push(m.clone());
            }
            "AddColor" => {
                if let Some(color) = m.get("color").and_then(|v| v.as_str()) {
                    if !is_valid_mana_color(color) {
                        changed = true;
                        // Skip this modification - can't represent dynamic color
                        continue;
                    }
                }
                new_mods.push(m.clone());
            }
            _ => new_mods.push(m.clone()),
        }
    }

    if changed {
        *mods = new_mods;
    }
    changed
}

/// Split a Forge keyword list like "Flying & First Strike & Ward:4" into
/// individual keyword strings, preserving multi-word names.
fn split_keyword_list(s: &str) -> Vec<String> {
    s.split(" & ")
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
        .collect()
}

/// Fix incorrectly split keyword modifications in all `modifications` arrays.
///
/// A previous migration bug split `AddKeyword$ Flying & First Strike` on single
/// spaces instead of ` & `, producing fragment entries like `"First"`, `"Strike"`,
/// `"&"`, `"from"`, etc. This pass reconstructs the original string by joining
/// consecutive same-type keyword entries with spaces, then re-splits correctly
/// using `split_keyword_list` (which splits on ` & `).
fn fix_split_keywords_deep(value: &mut Value) -> bool {
    match value {
        Value::Array(arr) => {
            let mut changed = false;
            for item in arr.iter_mut() {
                changed |= fix_split_keywords_deep(item);
            }
            changed
        }
        Value::Object(map) => {
            let mut changed = false;
            if let Some(mods) = map.get_mut("modifications") {
                if let Some(arr) = mods.as_array_mut() {
                    changed |= fix_keyword_modifications(arr);
                }
            }
            for (_, v) in map.iter_mut() {
                changed |= fix_split_keywords_deep(v);
            }
            changed
        }
        _ => false,
    }
}

fn fix_keyword_modifications(mods: &mut Vec<Value>) -> bool {
    // Detect the bug signature: any keyword entry that is a bare lowercase word,
    // a single character, or "&" — these can only come from incorrect space-splitting.
    let has_fragments = mods.iter().any(|m| {
        let mod_type = m.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if mod_type != "AddKeyword" && mod_type != "RemoveKeyword" {
            return false;
        }
        m.get("keyword")
            .and_then(|v| v.as_str())
            .is_some_and(is_keyword_fragment)
    });

    if !has_fragments {
        return false;
    }

    // Reconstruct: collect runs of consecutive same-type keyword mods,
    // rejoin with spaces to reconstruct the original Forge value,
    // then re-split correctly using split_keyword_list.
    let mut new_mods = Vec::new();
    let entries: Vec<Value> = std::mem::take(mods);
    let mut i = 0;

    while i < entries.len() {
        let mod_type = entries[i]
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let is_kw_mod = mod_type == "AddKeyword" || mod_type == "RemoveKeyword";

        if !is_kw_mod {
            new_mods.push(entries[i].clone());
            i += 1;
            continue;
        }

        // Collect all consecutive keyword mods of the same type
        let mut kw_parts = Vec::new();
        let mut j = i;
        while j < entries.len() {
            let jtype = entries[j]
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if jtype != mod_type {
                break;
            }
            if let Some(kw) = entries[j].get("keyword").and_then(|v| v.as_str()) {
                kw_parts.push(kw.to_string());
            } else {
                break;
            }
            j += 1;
        }

        // Rejoin with spaces to reconstruct the original Forge value,
        // then re-split on " & " to get correct keyword boundaries
        let reconstructed = kw_parts.join(" ");
        for kw in split_keyword_list(&reconstructed) {
            new_mods.push(json!({"type": mod_type, "keyword": kw}));
        }
        i = j;
    }

    *mods = new_mods;
    true
}

/// Returns true if a keyword value is a fragment from incorrect space-splitting.
/// These can never be valid standalone keyword values.
fn is_keyword_fragment(kw: &str) -> bool {
    // "&" separator treated as keyword
    if kw == "&" {
        return true;
    }
    // Single character (e.g., "B", "R") — not a valid keyword
    if kw.len() == 1 {
        return true;
    }
    // All-lowercase words are fragments (valid keywords are PascalCase)
    if kw.chars().all(|c| c.is_ascii_lowercase()) {
        return true;
    }
    // Sentence-ending punctuation
    if kw.ends_with('.') {
        return true;
    }
    false
}
