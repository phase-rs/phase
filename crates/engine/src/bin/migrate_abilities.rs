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

/// Migrate an entire ability file. Returns true if any changes were made.
fn migrate_ability_file(root: &mut Value, path: &Path, stats: &mut Stats) -> bool {
    let mut changed = false;

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

/// Migrate a cost value: handle old mana cost string format.
fn migrate_cost(cost: &mut Value, _path: &Path, _stats: &Stats) -> bool {
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
                changed |= migrate_cost(c, _path, _stats);
            }
        }
    }

    // Fix Sacrifice target filter
    if cost_type == "Sacrifice" {
        if let Some(target) = cost.get_mut("target") {
            changed |= migrate_target_filter(target);
        }
    }

    changed
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
            for k in s.split(' ') {
                let k = k.trim();
                if !k.is_empty() {
                    modifications.push(json!({"type": "AddKeyword", "keyword": k}));
                }
            }
        }
    }
    if let Some(kw) = params.get("RemoveKeyword") {
        if let Some(s) = kw.as_str() {
            for k in s.split(' ') {
                let k = k.trim();
                if !k.is_empty() {
                    modifications.push(json!({"type": "RemoveKeyword", "keyword": k}));
                }
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
            modifications.push(json!({"type": "AddType", "core_type": s}));
        }
    }
    if let Some(ct) = params.get("RemoveType") {
        if let Some(s) = ct.as_str() {
            modifications.push(json!({"type": "RemoveType", "core_type": s}));
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
            // Already new format -- no nested migration needed for properties
            false
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

    let card_type = map_type_filter(type_part);
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
                // Check if it looks like a subtype
                if p.starts_with(char::is_uppercase) && !p.contains('<') {
                    // Could be a subtype or a keyword reference
                    properties.push(json!({"type": "Other", "value": p}));
                }
            }
        }
    }

    let mut typed_filter = Map::new();
    typed_filter.insert("type".to_string(), json!("Typed"));
    if let Some(ct) = card_type {
        typed_filter.insert("card_type".to_string(), json!(ct));
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

/// Map a type string to TypeFilter enum value.
fn map_type_filter(s: &str) -> Option<&str> {
    match s {
        "Creature" | "creature" => Some("Creature"),
        "Land" | "land" => Some("Land"),
        "Artifact" | "artifact" => Some("Artifact"),
        "Enchantment" | "enchantment" => Some("Enchantment"),
        "Instant" | "instant" => Some("Instant"),
        "Sorcery" | "sorcery" => Some("Sorcery"),
        "Planeswalker" | "planeswalker" => Some("Planeswalker"),
        "Permanent" | "permanent" => Some("Permanent"),
        "Card" | "card" => Some("Card"),
        "Any" | "any" => None, // No type filter for "Any"
        _ => Some(s),          // Preserve unknown types
    }
}

/// Map zone strings to Zone enum values.
fn map_zone_string(s: &str) -> &str {
    match s {
        "Battlefield" | "battlefield" => "Battlefield",
        "Hand" | "hand" => "Hand",
        "Graveyard" | "graveyard" => "Graveyard",
        "Library" | "library" => "Library",
        "Exile" | "exile" => "Exile",
        "Stack" | "stack" => "Stack",
        "Command" | "command" | "CommandZone" => "Command",
        "Any" | "any" | "All" => "Battlefield", // Default fallback
        other => other,
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
        other => other,
    }
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
