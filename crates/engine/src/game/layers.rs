use std::collections::HashMap;
use std::str::FromStr;

use petgraph::algo::toposort;
use petgraph::graph::DiGraph;

use crate::game::filter::object_matches_filter;
use crate::game::game_object::CounterType;
use crate::types::card_type::CoreType;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::layers::{ActiveContinuousEffect, Layer};
use crate::types::mana::ManaColor;

/// Evaluate all continuous effects through the seven-layer system.
///
/// 1. Reset computed characteristics to base values.
/// 2. Gather all active continuous effects from battlefield permanents.
/// 3. For each layer, filter/order effects and apply them.
/// 4. Apply counter-based P/T modifications (layer 7e).
/// 5. Clear the layers_dirty flag.
pub fn evaluate_layers(state: &mut GameState) {
    // Step 1: Reset computed characteristics to base values.
    // Only reset fields where base values were explicitly set; objects without
    // base values (e.g., from older test helpers) retain their current values.
    let bf_ids: Vec<ObjectId> = state.battlefield.clone();
    for &id in &bf_ids {
        if let Some(obj) = state.objects.get_mut(&id) {
            if obj.base_power.is_some() {
                obj.power = obj.base_power;
            }
            if obj.base_toughness.is_some() {
                obj.toughness = obj.base_toughness;
            }
            if !obj.base_keywords.is_empty() {
                obj.keywords = obj.base_keywords.clone();
            }
            if !obj.base_color.is_empty() {
                obj.color = obj.base_color.clone();
            }
        }
    }

    // Step 2: Gather active continuous effects
    let effects = gather_active_continuous_effects(state);

    // Step 3: Process each layer in order
    for &layer in Layer::all() {
        if layer == Layer::CounterPT {
            // Step 4: Counter-based P/T handled separately
            continue;
        }

        let layer_effects: Vec<&ActiveContinuousEffect> =
            effects.iter().filter(|e| e.layer == layer).collect();

        if layer_effects.is_empty() {
            continue;
        }

        let ordered = if layer.has_dependency_ordering() {
            order_with_dependencies(&layer_effects, state)
        } else {
            order_by_timestamp(&layer_effects)
        };

        for effect in &ordered {
            apply_continuous_effect(state, effect);
        }
    }

    // Step 4: Apply counter-based P/T (layer 7e)
    for &id in &bf_ids {
        if let Some(obj) = state.objects.get_mut(&id) {
            let plus = *obj.counters.get(&CounterType::Plus1Plus1).unwrap_or(&0) as i32;
            let minus = *obj.counters.get(&CounterType::Minus1Minus1).unwrap_or(&0) as i32;
            let delta = plus - minus;
            if delta != 0 {
                if let Some(ref mut p) = obj.power {
                    *p += delta;
                }
                if let Some(ref mut t) = obj.toughness {
                    *t += delta;
                }
            }
        }
    }

    // Step 5: Clear dirty flag
    state.layers_dirty = false;
}

/// Collect all active continuous effects from permanents on the battlefield.
fn gather_active_continuous_effects(state: &GameState) -> Vec<ActiveContinuousEffect> {
    let mut effects = Vec::new();

    for &id in &state.battlefield {
        let obj = match state.objects.get(&id) {
            Some(o) => o,
            None => continue,
        };

        for (def_idx, def) in obj.static_definitions.iter().enumerate() {
            if def.mode != "Continuous" {
                continue;
            }

            let affected = def.params.get("Affected").cloned().unwrap_or_default();

            // Determine which layer(s) this effect targets based on params
            let layers = determine_layers_from_params(&def.params);

            for layer in layers {
                effects.push(ActiveContinuousEffect {
                    source_id: id,
                    def_index: def_idx,
                    layer,
                    timestamp: obj.timestamp,
                    params: def.params.clone(),
                    affected_filter: affected.clone(),
                    mode: def.mode.clone(),
                });
            }
        }
    }

    effects
}

/// Determine which layer(s) a continuous effect targets based on its params.
fn determine_layers_from_params(params: &HashMap<String, String>) -> Vec<Layer> {
    let mut layers = Vec::new();

    if params.contains_key("AddPower") || params.contains_key("AddToughness") {
        layers.push(Layer::ModifyPT);
    }
    if params.contains_key("SetPower") || params.contains_key("SetToughness") {
        layers.push(Layer::SetPT);
    }
    if params.contains_key("AddKeyword") || params.contains_key("RemoveKeyword") {
        layers.push(Layer::Ability);
    }
    if params.contains_key("AddType") || params.contains_key("RemoveType") {
        layers.push(Layer::Type);
    }
    if params.contains_key("SetColor") || params.contains_key("AddColor") {
        layers.push(Layer::Color);
    }
    if params.contains_key("AddAbility") || params.contains_key("RemoveAllAbilities") {
        layers.push(Layer::Ability);
    }

    // Deduplicate
    layers.sort();
    layers.dedup();

    layers
}

/// Order effects using dependency-aware topological sort.
fn order_with_dependencies(
    effects: &[&ActiveContinuousEffect],
    state: &GameState,
) -> Vec<ActiveContinuousEffect> {
    if effects.len() <= 1 {
        return effects.iter().map(|e| (*e).clone()).collect();
    }

    // Start with timestamp ordering as fallback
    let mut sorted: Vec<&ActiveContinuousEffect> = effects.iter().copied().collect();
    sorted.sort_by_key(|e| (e.timestamp, e.source_id.0, e.def_index));

    let mut graph = DiGraph::<usize, ()>::new();
    let nodes: Vec<_> = (0..sorted.len()).map(|i| graph.add_node(i)).collect();

    // Check dependencies between each pair
    for i in 0..sorted.len() {
        for j in 0..sorted.len() {
            if i == j {
                continue;
            }
            if depends_on(sorted[i], sorted[j], state) {
                // i depends on j, so j must come first: edge j -> i
                graph.add_edge(nodes[j], nodes[i], ());
            }
        }
    }

    match toposort(&graph, None) {
        Ok(order) => order
            .into_iter()
            .map(|node_idx| sorted[graph[node_idx]].clone())
            .collect(),
        Err(_) => {
            // Cycle detected -- fall back to timestamp ordering per CR 613.8
            sorted.iter().map(|e| (*e).clone()).collect()
        }
    }
}

/// Check if effect `a` depends on effect `b`.
/// If `b` changes types and `a`'s filter is type-based, `a` depends on `b`.
fn depends_on(a: &ActiveContinuousEffect, b: &ActiveContinuousEffect, _state: &GameState) -> bool {
    // If b changes types (layer Type) and a's filter references a type
    if b.params.contains_key("AddType") || b.params.contains_key("RemoveType") {
        let type_keywords = [
            "Creature",
            "Artifact",
            "Enchantment",
            "Land",
            "Planeswalker",
            "Instant",
            "Sorcery",
            "Tribal",
        ];
        for kw in &type_keywords {
            if a.affected_filter.contains(kw) {
                return true;
            }
        }
    }

    // If b adds/removes abilities and a checks for abilities
    if b.params.contains_key("AddAbility") || b.params.contains_key("RemoveAllAbilities") {
        if a.affected_filter.contains("withAbility") {
            return true;
        }
    }

    false
}

/// Order effects by timestamp (deterministic fallback).
fn order_by_timestamp(effects: &[&ActiveContinuousEffect]) -> Vec<ActiveContinuousEffect> {
    let mut sorted: Vec<ActiveContinuousEffect> = effects.iter().map(|e| (*e).clone()).collect();
    sorted.sort_by_key(|e| (e.timestamp, e.source_id.0, e.def_index));
    sorted
}

/// Apply a single continuous effect to all affected objects.
fn apply_continuous_effect(state: &mut GameState, effect: &ActiveContinuousEffect) {
    // Find affected objects
    let bf_ids: Vec<ObjectId> = state.battlefield.clone();
    let affected_ids: Vec<ObjectId> = bf_ids
        .iter()
        .filter(|&&id| object_matches_filter(state, id, &effect.affected_filter, effect.source_id))
        .copied()
        .collect();

    for id in affected_ids {
        let obj = match state.objects.get_mut(&id) {
            Some(o) => o,
            None => continue,
        };

        // Apply modifications based on params
        if let Some(val) = effect.params.get("AddPower") {
            if let Ok(n) = val.parse::<i32>() {
                if let Some(ref mut p) = obj.power {
                    *p += n;
                }
            }
        }
        if let Some(val) = effect.params.get("AddToughness") {
            if let Ok(n) = val.parse::<i32>() {
                if let Some(ref mut t) = obj.toughness {
                    *t += n;
                }
            }
        }
        if let Some(val) = effect.params.get("SetPower") {
            if let Ok(n) = val.parse::<i32>() {
                obj.power = Some(n);
            }
        }
        if let Some(val) = effect.params.get("SetToughness") {
            if let Ok(n) = val.parse::<i32>() {
                obj.toughness = Some(n);
            }
        }
        if let Some(val) = effect.params.get("AddKeyword") {
            let kw: Keyword = val.parse().unwrap();
            if !obj.has_keyword(&kw) {
                obj.keywords.push(kw);
            }
        }
        if let Some(val) = effect.params.get("RemoveKeyword") {
            let kw: Keyword = val.parse().unwrap();
            obj.keywords
                .retain(|k| std::mem::discriminant(k) != std::mem::discriminant(&kw));
        }
        if let Some(val) = effect.params.get("AddType") {
            if let Ok(ct) = val.parse::<CoreType>() {
                if !obj.card_types.core_types.contains(&ct) {
                    obj.card_types.core_types.push(ct);
                }
            }
        }
        if let Some(val) = effect.params.get("RemoveType") {
            if let Ok(ct) = val.parse::<CoreType>() {
                obj.card_types.core_types.retain(|t| t != &ct);
            }
        }
        if let Some(val) = effect.params.get("SetColor") {
            let colors: Vec<ManaColor> = val
                .split(',')
                .filter_map(|s| match s.trim() {
                    "White" => Some(ManaColor::White),
                    "Blue" => Some(ManaColor::Blue),
                    "Black" => Some(ManaColor::Black),
                    "Red" => Some(ManaColor::Red),
                    "Green" => Some(ManaColor::Green),
                    _ => None,
                })
                .collect();
            obj.color = colors;
        }
        if let Some(val) = effect.params.get("AddColor") {
            let color = match val.as_str() {
                "White" => Some(ManaColor::White),
                "Blue" => Some(ManaColor::Blue),
                "Black" => Some(ManaColor::Black),
                "Red" => Some(ManaColor::Red),
                "Green" => Some(ManaColor::Green),
                _ => None,
            };
            if let Some(c) = color {
                if !obj.color.contains(&c) {
                    obj.color.push(c);
                }
            }
        }
        if effect.params.contains_key("RemoveAllAbilities") {
            obj.abilities.clear();
            obj.keywords.clear();
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::StaticDefinition;
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn setup() -> GameState {
        GameState::new_two_player(42)
    }

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
        let ts = state.next_timestamp();
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(power);
        obj.toughness = Some(toughness);
        obj.base_power = Some(power);
        obj.base_toughness = Some(toughness);
        obj.timestamp = ts;
        id
    }

    fn add_lord_static(
        state: &mut GameState,
        lord_id: ObjectId,
        filter: &str,
        add_power: i32,
        add_toughness: i32,
    ) {
        let mut params = HashMap::new();
        params.insert("Affected".to_string(), filter.to_string());
        params.insert("AddPower".to_string(), add_power.to_string());
        params.insert("AddToughness".to_string(), add_toughness.to_string());
        let def = StaticDefinition {
            mode: "Continuous".to_string(),
            params,
        };
        state
            .objects
            .get_mut(&lord_id)
            .unwrap()
            .static_definitions
            .push(def);
    }

    #[test]
    fn test_lord_buff_modifies_computed_not_base() {
        let mut state = setup();
        let lord = make_creature(&mut state, "Lord", 2, 2, PlayerId(0));
        let bear = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));

        add_lord_static(&mut state, lord, "Creature.YouCtrl", 1, 1);

        evaluate_layers(&mut state);

        let bear_obj = state.objects.get(&bear).unwrap();
        assert_eq!(
            bear_obj.power,
            Some(3),
            "Bear computed power should be 2+1=3"
        );
        assert_eq!(
            bear_obj.toughness,
            Some(3),
            "Bear computed toughness should be 2+1=3"
        );
        assert_eq!(bear_obj.base_power, Some(2), "Bear base power unchanged");
        assert_eq!(
            bear_obj.base_toughness,
            Some(2),
            "Bear base toughness unchanged"
        );
    }

    #[test]
    fn test_layer_order_type_before_pt() {
        let mut state = setup();

        // A non-creature artifact
        let artifact = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Artifact".to_string(),
            Zone::Battlefield,
        );
        let art_ts = state.next_timestamp();
        {
            let obj = state.objects.get_mut(&artifact).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.power = Some(0);
            obj.toughness = Some(0);
            obj.base_power = Some(0);
            obj.base_toughness = Some(0);
            obj.timestamp = art_ts;
        }

        // Effect that makes artifacts into creatures (layer 4 - Type)
        let animator = make_creature(&mut state, "Animator", 1, 1, PlayerId(0));
        {
            let mut params = HashMap::new();
            params.insert("Affected".to_string(), "Artifact.YouCtrl".to_string());
            params.insert("AddType".to_string(), "Creature".to_string());
            let def = StaticDefinition {
                mode: "Continuous".to_string(),
                params,
            };
            state
                .objects
                .get_mut(&animator)
                .unwrap()
                .static_definitions
                .push(def);
        }

        // Effect that buffs creatures (layer 7c - ModifyPT)
        let lord = make_creature(&mut state, "Lord", 2, 2, PlayerId(0));
        add_lord_static(&mut state, lord, "Creature.YouCtrl", 1, 1);

        evaluate_layers(&mut state);

        let art_obj = state.objects.get(&artifact).unwrap();
        // The artifact should now be a creature (type change layer 4) and get the buff (layer 7c)
        assert!(art_obj.card_types.core_types.contains(&CoreType::Creature));
        assert_eq!(art_obj.power, Some(1), "Artifact+creature gets +1/+1");
        assert_eq!(art_obj.toughness, Some(1), "Artifact+creature gets +1/+1");
    }

    #[test]
    fn test_timestamp_ordering_within_layer() {
        let mut state = setup();
        let bear = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));

        // Two lords with different timestamps, both +1/+1
        let lord1 = make_creature(&mut state, "Lord1", 2, 2, PlayerId(0));
        add_lord_static(&mut state, lord1, "Creature.YouCtrl", 1, 1);

        let lord2 = make_creature(&mut state, "Lord2", 2, 2, PlayerId(0));
        add_lord_static(&mut state, lord2, "Creature.YouCtrl", 1, 1);

        evaluate_layers(&mut state);

        let bear_obj = state.objects.get(&bear).unwrap();
        // Both lords apply: 2 + 1 + 1 = 4
        assert_eq!(bear_obj.power, Some(4));
        assert_eq!(bear_obj.toughness, Some(4));
    }

    #[test]
    fn test_dependency_ordering_overrides_timestamp() {
        let mut state = setup();

        // A non-creature artifact (will gain creature type from effect B)
        let artifact = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Artifact".to_string(),
            Zone::Battlefield,
        );
        let art_ts = state.next_timestamp();
        {
            let obj = state.objects.get_mut(&artifact).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.power = Some(0);
            obj.toughness = Some(0);
            obj.base_power = Some(0);
            obj.base_toughness = Some(0);
            obj.timestamp = art_ts;
        }

        // Effect A: Buffs creatures, timestamp 5 (created first, older)
        let lord = make_creature(&mut state, "Lord", 2, 2, PlayerId(0));
        {
            let obj = state.objects.get_mut(&lord).unwrap();
            obj.timestamp = 5;
        }
        add_lord_static(&mut state, lord, "Creature.YouCtrl", 2, 2);

        // Effect B: Adds creature type to artifacts, timestamp 10 (created later, newer)
        let animator = make_creature(&mut state, "Animator", 1, 1, PlayerId(0));
        {
            let obj = state.objects.get_mut(&animator).unwrap();
            obj.timestamp = 10;
        }
        {
            let mut params = HashMap::new();
            params.insert("Affected".to_string(), "Artifact.YouCtrl".to_string());
            params.insert("AddType".to_string(), "Creature".to_string());
            let def = StaticDefinition {
                mode: "Continuous".to_string(),
                params,
            };
            state
                .objects
                .get_mut(&animator)
                .unwrap()
                .static_definitions
                .push(def);
        }

        evaluate_layers(&mut state);

        let art_obj = state.objects.get(&artifact).unwrap();
        // Type change (layer 4) makes artifact a creature
        assert!(art_obj.card_types.core_types.contains(&CoreType::Creature));
        // ModifyPT (layer 7c) gives it +2/+2
        assert_eq!(art_obj.power, Some(2));
        assert_eq!(art_obj.toughness, Some(2));
    }

    #[test]
    fn test_counter_pt_layer_7e() {
        let mut state = setup();
        let creature = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));

        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 2);

        evaluate_layers(&mut state);

        let obj = state.objects.get(&creature).unwrap();
        assert_eq!(obj.power, Some(4), "2 base + 2 counters = 4");
        assert_eq!(obj.toughness, Some(4), "2 base + 2 counters = 4");
    }

    #[test]
    fn test_layers_dirty_flag_cleared() {
        let mut state = setup();
        assert!(state.layers_dirty);

        evaluate_layers(&mut state);

        assert!(!state.layers_dirty);
    }

    #[test]
    fn test_source_leaves_battlefield_effect_stops() {
        let mut state = setup();
        let lord = make_creature(&mut state, "Lord", 2, 2, PlayerId(0));
        let bear = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));

        add_lord_static(&mut state, lord, "Creature.YouCtrl", 1, 1);

        evaluate_layers(&mut state);
        assert_eq!(state.objects.get(&bear).unwrap().power, Some(3));

        // Remove lord from battlefield
        state.battlefield.retain(|&id| id != lord);

        // Re-evaluate
        state.layers_dirty = true;
        evaluate_layers(&mut state);

        let bear_obj = state.objects.get(&bear).unwrap();
        assert_eq!(
            bear_obj.power,
            Some(2),
            "Bear returns to base P/T after lord leaves"
        );
        assert_eq!(bear_obj.toughness, Some(2));
    }
}
