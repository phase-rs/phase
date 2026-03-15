use petgraph::algo::toposort;
use petgraph::graph::DiGraph;

use crate::game::devotion::count_devotion;
use crate::game::filter::matches_target_filter;
use crate::game::game_object::CounterType;
use crate::types::ability::{
    ContinuousModification, DynamicPTValue, StaticCondition, TargetFilter, TypedFilter,
};
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::layers::{ActiveContinuousEffect, Layer};
use crate::types::statics::StaticMode;

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
            if !obj.base_card_types.supertypes.is_empty()
                || !obj.base_card_types.core_types.is_empty()
                || !obj.base_card_types.subtypes.is_empty()
            {
                obj.card_types = obj.base_card_types.clone();
            }
            obj.keywords = obj.base_keywords.clone();
            if !obj.base_abilities.is_empty() {
                obj.abilities = obj.base_abilities.clone();
            }
            if !obj.base_trigger_definitions.is_empty() {
                obj.trigger_definitions = obj.base_trigger_definitions.clone();
            }
            if !obj.base_replacement_definitions.is_empty() {
                obj.replacement_definitions = obj.base_replacement_definitions.clone();
            }
            if !obj.base_static_definitions.is_empty() || !obj.granted_static_definitions.is_empty()
            {
                obj.static_definitions = obj.base_static_definitions.clone();
                obj.static_definitions
                    .extend(obj.granted_static_definitions.clone());
            }
            obj.color = obj.base_color.clone();
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

    // Step 3b: Changeling post-fixup — if Changeling was granted via AddKeyword
    // in Layer 6 (Ability), the CDA in Layer 4 (Type) was already processed.
    // Expand creature types for any object that now has Changeling but wasn't
    // covered by its own CDA static definition.
    if !state.all_creature_types.is_empty() {
        for &id in &bf_ids {
            let has_changeling = state
                .objects
                .get(&id)
                .is_some_and(|o| o.has_keyword(&Keyword::Changeling));
            if has_changeling {
                let all_types = state.all_creature_types.clone();
                if let Some(obj) = state.objects.get_mut(&id) {
                    for subtype in &all_types {
                        if !obj.card_types.subtypes.iter().any(|s| s == subtype) {
                            obj.card_types.subtypes.push(subtype.clone());
                        }
                    }
                }
            }
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
            if def.mode != StaticMode::Continuous {
                continue;
            }

            // Evaluate condition if present
            if let Some(ref condition) = def.condition {
                match condition {
                    StaticCondition::DevotionGE { colors, threshold } => {
                        let devotion = count_devotion(state, obj.controller, colors);
                        if devotion < *threshold {
                            continue;
                        }
                    }
                    StaticCondition::LifeMoreThanStartingBy { amount } => {
                        let Some(player) = state.players.iter().find(|p| p.id == obj.controller)
                        else {
                            continue;
                        };
                        let threshold = state.format_config.starting_life + *amount;
                        if player.life < threshold {
                            continue;
                        }
                    }
                    StaticCondition::IsPresent { .. } => {
                        // TODO: evaluate presence check
                    }
                    StaticCondition::CheckSVar { compare, .. } => {
                        // Legacy path: evaluate compare string against devotion if colors available
                        if !obj.base_color.is_empty() {
                            let devotion = count_devotion(state, obj.controller, &obj.base_color);
                            if !evaluate_compare(compare, devotion) {
                                continue;
                            }
                        }
                    }
                    StaticCondition::DuringYourTurn => {
                        if state.active_player != obj.controller {
                            continue;
                        }
                    }
                    StaticCondition::None => {}
                }
            }

            let affected_filter = def.affected.clone().unwrap_or(TargetFilter::Any);

            // Each modification becomes its own ActiveContinuousEffect with the correct layer
            for modification in &def.modifications {
                effects.push(ActiveContinuousEffect {
                    source_id: id,
                    def_index: def_idx,
                    layer: modification.layer(),
                    timestamp: obj.timestamp,
                    modification: modification.clone(),
                    affected_filter: affected_filter.clone(),
                    mode: def.mode.clone(),
                    characteristic_defining: def.characteristic_defining,
                });
            }
        }
    }

    effects
}

/// Evaluate a comparison string like "LT5", "GE7" against a value.
fn evaluate_compare(compare_str: &str, count: u32) -> bool {
    if compare_str.len() < 3 {
        return true;
    }
    let (op, num_str) = (&compare_str[..2], &compare_str[2..]);
    let threshold: u32 = match num_str.parse() {
        Ok(n) => n,
        Err(_) => return true,
    };
    match op {
        "GE" => count >= threshold,
        "LE" => count <= threshold,
        "EQ" => count == threshold,
        "NE" => count != threshold,
        "GT" => count > threshold,
        "LT" => count < threshold,
        _ => true,
    }
}

/// Order effects using dependency-aware topological sort.
fn order_with_dependencies(
    effects: &[&ActiveContinuousEffect],
    state: &GameState,
) -> Vec<ActiveContinuousEffect> {
    if effects.len() <= 1 {
        return effects.iter().map(|e| (*e).clone()).collect();
    }

    // Start with timestamp ordering as fallback; CDAs sort first per CR 604.3
    let mut sorted: Vec<&ActiveContinuousEffect> = effects.to_vec();
    sorted.sort_by_key(|e| {
        (
            !e.characteristic_defining,
            e.timestamp,
            e.source_id.0,
            e.def_index,
        )
    });

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
    // If b changes types (AddType/RemoveType) and a's filter references a type
    let b_changes_types = matches!(
        &b.modification,
        ContinuousModification::AddType { .. }
            | ContinuousModification::RemoveType { .. }
            | ContinuousModification::AddSubtype { .. }
            | ContinuousModification::RemoveSubtype { .. }
            | ContinuousModification::AddAllCreatureTypes
            | ContinuousModification::AddChosenSubtype { .. }
    );

    if b_changes_types && filter_references_type(&a.affected_filter) {
        return true;
    }

    // If b adds/removes abilities and a's filter checks for abilities
    let b_changes_abilities = matches!(
        &b.modification,
        ContinuousModification::AddAbility { .. } | ContinuousModification::RemoveAllAbilities
    );

    if b_changes_abilities && filter_references_ability(&a.affected_filter) {
        return true;
    }

    false
}

/// Check if a TargetFilter references a card type (used for dependency ordering).
fn filter_references_type(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(TypedFilter { card_type, .. }) => card_type.is_some(),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(filter_references_type)
        }
        TargetFilter::Not { filter } => filter_references_type(filter),
        _ => false,
    }
}

/// Check if a TargetFilter references abilities/keywords (used for dependency ordering).
fn filter_references_ability(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(TypedFilter { properties, .. }) => properties
            .iter()
            .any(|p| matches!(p, crate::types::ability::FilterProp::WithKeyword { .. })),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(filter_references_ability)
        }
        TargetFilter::Not { filter } => filter_references_ability(filter),
        _ => false,
    }
}

/// Order effects by timestamp (deterministic fallback). CDAs sort first per CR 604.3.
fn order_by_timestamp(effects: &[&ActiveContinuousEffect]) -> Vec<ActiveContinuousEffect> {
    let mut sorted: Vec<ActiveContinuousEffect> = effects.iter().map(|e| (*e).clone()).collect();
    sorted.sort_by_key(|e| {
        (
            !e.characteristic_defining,
            e.timestamp,
            e.source_id.0,
            e.def_index,
        )
    });
    sorted
}

/// Evaluate a dynamic P/T value by inspecting current game state.
fn evaluate_dynamic_pt(state: &GameState, value: &DynamicPTValue) -> i32 {
    match value {
        DynamicPTValue::CardTypesInAllGraveyards { offset } => {
            let mut seen = std::collections::HashSet::new();
            for player in &state.players {
                for &obj_id in &player.graveyard {
                    if let Some(obj) = state.objects.get(&obj_id) {
                        for ct in &obj.card_types.core_types {
                            seen.insert(*ct);
                        }
                    }
                }
            }
            seen.len() as i32 + offset
        }
    }
}

/// Apply a single continuous effect to all affected objects.
fn apply_continuous_effect(state: &mut GameState, effect: &ActiveContinuousEffect) {
    // Find affected objects
    let bf_ids: Vec<ObjectId> = state.battlefield.clone();
    let affected_ids: Vec<ObjectId> = bf_ids
        .iter()
        .filter(|&&id| matches_target_filter(state, id, &effect.affected_filter, effect.source_id))
        .copied()
        .collect();

    // Pre-read chosen subtype from source (avoids borrow conflict in the loop)
    let chosen_subtype =
        if let ContinuousModification::AddChosenSubtype { ref kind } = effect.modification {
            state
                .objects
                .get(&effect.source_id)
                .and_then(|src| src.chosen_subtype_str(kind))
        } else {
            None
        };

    // Pre-compute dynamic P/T values (avoids borrow conflict in the loop)
    let dynamic_pt = match &effect.modification {
        ContinuousModification::SetDynamicPower { value }
        | ContinuousModification::SetDynamicToughness { value } => {
            Some(evaluate_dynamic_pt(state, value))
        }
        _ => None,
    };

    for id in affected_ids {
        let obj = match state.objects.get_mut(&id) {
            Some(o) => o,
            None => continue,
        };

        match &effect.modification {
            ContinuousModification::AddPower { value } => {
                if let Some(ref mut p) = obj.power {
                    *p += value;
                }
            }
            ContinuousModification::AddToughness { value } => {
                if let Some(ref mut t) = obj.toughness {
                    *t += value;
                }
            }
            ContinuousModification::SetPower { value } => {
                obj.power = Some(*value);
            }
            ContinuousModification::SetToughness { value } => {
                obj.toughness = Some(*value);
            }
            ContinuousModification::AddKeyword { keyword } => {
                if !obj.has_keyword(keyword) {
                    obj.keywords.push(keyword.clone());
                }
            }
            ContinuousModification::RemoveKeyword { keyword } => {
                obj.keywords
                    .retain(|k| std::mem::discriminant(k) != std::mem::discriminant(keyword));
            }
            ContinuousModification::RemoveAllAbilities => {
                obj.abilities.clear();
                obj.trigger_definitions.clear();
                obj.replacement_definitions.clear();
                obj.static_definitions.clear();
                obj.keywords.clear();
            }
            ContinuousModification::AddType { core_type } => {
                if !obj.card_types.core_types.contains(core_type) {
                    obj.card_types.core_types.push(*core_type);
                }
            }
            ContinuousModification::RemoveType { core_type } => {
                obj.card_types.core_types.retain(|t| t != core_type);
            }
            ContinuousModification::SetColor { colors } => {
                obj.color = colors.clone();
            }
            ContinuousModification::AddColor { color } => {
                if !obj.color.contains(color) {
                    obj.color.push(*color);
                }
            }
            ContinuousModification::AddSubtype { ref subtype } => {
                if !obj.card_types.subtypes.iter().any(|s| s == subtype) {
                    obj.card_types.subtypes.push(subtype.clone());
                }
            }
            ContinuousModification::RemoveSubtype { ref subtype } => {
                obj.card_types.subtypes.retain(|s| s != subtype);
            }
            ContinuousModification::AddAllCreatureTypes => {
                for subtype in &state.all_creature_types {
                    if !obj.card_types.subtypes.iter().any(|s| s == subtype) {
                        obj.card_types.subtypes.push(subtype.clone());
                    }
                }
            }
            ContinuousModification::AddChosenSubtype { .. } => {
                if let Some(ref subtype) = chosen_subtype {
                    if !obj.card_types.subtypes.iter().any(|s| s == subtype) {
                        obj.card_types.subtypes.push(subtype.clone());
                    }
                }
            }
            ContinuousModification::SetDynamicPower { .. } => {
                if let Some(val) = dynamic_pt {
                    obj.power = Some(val);
                }
            }
            ContinuousModification::SetDynamicToughness { .. } => {
                if let Some(val) = dynamic_pt {
                    obj.toughness = Some(val);
                }
            }
            ContinuousModification::AddAbility { .. } => { /* TODO: future */ }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::scenario::GameScenario;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, ChosenSubtypeKind, ContinuousModification, ControllerRef,
        Effect, FilterProp, GainLifePlayer, LifeAmount, StaticDefinition, TargetFilter,
        TypeFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;
    use crate::types::keywords::Keyword;
    use crate::types::mana::ManaColor;
    use crate::types::player::PlayerId;
    use crate::types::statics::StaticMode;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::triggers::TriggerMode;
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
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(power);
        obj.toughness = Some(toughness);
        obj.base_power = Some(power);
        obj.base_toughness = Some(toughness);
        obj.timestamp = ts;
        id
    }

    /// Helper: creatures you control filter
    fn creature_you_ctrl() -> TargetFilter {
        TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You))
    }

    fn add_lord_static(
        state: &mut GameState,
        lord_id: ObjectId,
        filter: TargetFilter,
        add_power: i32,
        add_toughness: i32,
    ) {
        let def = StaticDefinition::continuous()
            .affected(filter)
            .modifications(vec![
                ContinuousModification::AddPower { value: add_power },
                ContinuousModification::AddToughness {
                    value: add_toughness,
                },
            ]);
        state
            .objects
            .get_mut(&lord_id)
            .unwrap()
            .static_definitions
            .push(def);
    }

    #[test]
    fn conditional_life_more_than_starting_applies_only_above_threshold() {
        let mut state = setup();
        state.format_config.starting_life = 20;
        state.players[0].life = 26;

        let leyline = make_creature(&mut state, "Leyline Source", 0, 0, PlayerId(0));
        let bear = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));

        let def = StaticDefinition::continuous()
            .affected(creature_you_ctrl())
            .modifications(vec![
                ContinuousModification::AddPower { value: 2 },
                ContinuousModification::AddToughness { value: 2 },
            ])
            .condition(StaticCondition::LifeMoreThanStartingBy { amount: 7 });
        state
            .objects
            .get_mut(&leyline)
            .unwrap()
            .static_definitions
            .push(def);

        evaluate_layers(&mut state);
        assert_eq!(state.objects[&bear].power, Some(2));
        assert_eq!(state.objects[&bear].toughness, Some(2));

        state.players[0].life = 27;
        state.layers_dirty = true;
        evaluate_layers(&mut state);
        assert_eq!(state.objects[&bear].power, Some(4));
        assert_eq!(state.objects[&bear].toughness, Some(4));
    }

    #[test]
    fn test_lord_buff_modifies_computed_not_base() {
        let mut state = setup();
        let lord = make_creature(&mut state, "Lord", 2, 2, PlayerId(0));
        let bear = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));

        add_lord_static(&mut state, lord, creature_you_ctrl(), 1, 1);

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
            obj.base_card_types = obj.card_types.clone();
            obj.power = Some(0);
            obj.toughness = Some(0);
            obj.base_power = Some(0);
            obj.base_toughness = Some(0);
            obj.timestamp = art_ts;
        }

        // Effect that makes artifacts into creatures (layer 4 - Type)
        let animator = make_creature(&mut state, "Animator", 1, 1, PlayerId(0));
        {
            let artifact_you_ctrl = TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Artifact).controller(ControllerRef::You),
            );
            let def = StaticDefinition::continuous()
                .affected(artifact_you_ctrl)
                .modifications(vec![ContinuousModification::AddType {
                    core_type: CoreType::Creature,
                }]);
            state
                .objects
                .get_mut(&animator)
                .unwrap()
                .static_definitions
                .push(def);
        }

        // Effect that buffs creatures (layer 7c - ModifyPT)
        let lord = make_creature(&mut state, "Lord", 2, 2, PlayerId(0));
        add_lord_static(&mut state, lord, creature_you_ctrl(), 1, 1);

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
        add_lord_static(&mut state, lord1, creature_you_ctrl(), 1, 1);

        let lord2 = make_creature(&mut state, "Lord2", 2, 2, PlayerId(0));
        add_lord_static(&mut state, lord2, creature_you_ctrl(), 1, 1);

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
            obj.base_card_types = obj.card_types.clone();
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
        add_lord_static(&mut state, lord, creature_you_ctrl(), 2, 2);

        // Effect B: Adds creature type to artifacts, timestamp 10 (created later, newer)
        let animator = make_creature(&mut state, "Animator", 1, 1, PlayerId(0));
        {
            let obj = state.objects.get_mut(&animator).unwrap();
            obj.timestamp = 10;
        }
        {
            let artifact_you_ctrl = TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Artifact).controller(ControllerRef::You),
            );
            let def = StaticDefinition::continuous()
                .affected(artifact_you_ctrl)
                .modifications(vec![ContinuousModification::AddType {
                    core_type: CoreType::Creature,
                }]);
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
    fn test_aura_static_only_affects_enchanted_creature() {
        let mut state = setup();
        let bear_a = make_creature(&mut state, "Bear A", 2, 2, PlayerId(0));
        let bear_b = make_creature(&mut state, "Bear B", 2, 2, PlayerId(0));

        // Create an aura with Rancor-like static: +2/+0 and trample to EnchantedBy
        let aura = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Rancor".to_string(),
            Zone::Battlefield,
        );
        {
            let ts = state.next_timestamp();
            let obj = state.objects.get_mut(&aura).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Enchantment);
            obj.attached_to = Some(bear_a);
            obj.timestamp = ts;

            let enchanted_creature = TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
            );
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(enchanted_creature)
                    .modifications(vec![
                        ContinuousModification::AddPower { value: 2 },
                        ContinuousModification::AddKeyword {
                            keyword: Keyword::Trample,
                        },
                    ]),
            );
        }
        state
            .objects
            .get_mut(&bear_a)
            .unwrap()
            .attachments
            .push(aura);

        evaluate_layers(&mut state);

        let a = state.objects.get(&bear_a).unwrap();
        assert_eq!(a.power, Some(4), "Enchanted bear: 2 base + 2 from aura");
        assert_eq!(a.toughness, Some(2), "Aura adds no toughness");
        assert!(
            a.has_keyword(&Keyword::Trample),
            "Enchanted bear gets trample"
        );

        let b = state.objects.get(&bear_b).unwrap();
        assert_eq!(b.power, Some(2), "Non-enchanted bear unchanged");
        assert_eq!(b.toughness, Some(2), "Non-enchanted bear unchanged");
        assert!(
            !b.has_keyword(&Keyword::Trample),
            "Non-enchanted bear has no trample"
        );
    }

    #[test]
    fn test_keyword_filtered_lord_uses_source_controller() {
        let mut state = setup();
        let winds = create_object(
            &mut state,
            CardId(0),
            PlayerId(1),
            "Favorable Winds".to_string(),
            Zone::Battlefield,
        );
        let winds_ts = state.next_timestamp();
        {
            let obj = state.objects.get_mut(&winds).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.timestamp = winds_ts;
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::Typed(
                        TypedFilter::creature()
                            .controller(ControllerRef::You)
                            .properties(vec![FilterProp::WithKeyword {
                                value: "flying".to_string(),
                            }]),
                    ))
                    .modifications(vec![
                        ContinuousModification::AddPower { value: 1 },
                        ContinuousModification::AddToughness { value: 1 },
                    ]),
            );
        }

        let opponent_flyer = make_creature(&mut state, "Opponent Flyer", 2, 2, PlayerId(1));
        state
            .objects
            .get_mut(&opponent_flyer)
            .unwrap()
            .base_keywords
            .push(Keyword::Flying);
        state.objects.get_mut(&opponent_flyer).unwrap().keywords = vec![Keyword::Flying];

        let my_flyer = make_creature(&mut state, "My Flyer", 2, 2, PlayerId(0));
        state
            .objects
            .get_mut(&my_flyer)
            .unwrap()
            .base_keywords
            .push(Keyword::Flying);
        state.objects.get_mut(&my_flyer).unwrap().keywords = vec![Keyword::Flying];

        let opponent_ground = make_creature(&mut state, "Opponent Ground", 2, 2, PlayerId(1));

        evaluate_layers(&mut state);

        let opponent_flyer_obj = state.objects.get(&opponent_flyer).unwrap();
        assert_eq!(opponent_flyer_obj.power, Some(3));
        assert_eq!(opponent_flyer_obj.toughness, Some(3));

        let my_flyer_obj = state.objects.get(&my_flyer).unwrap();
        assert_eq!(my_flyer_obj.power, Some(2));
        assert_eq!(my_flyer_obj.toughness, Some(2));

        let opponent_ground_obj = state.objects.get(&opponent_ground).unwrap();
        assert_eq!(opponent_ground_obj.power, Some(2));
        assert_eq!(opponent_ground_obj.toughness, Some(2));
    }

    #[test]
    fn test_multi_layer_effect_does_not_double_apply() {
        // Regression: an effect with AddPower + AddKeyword spans two layers
        // (ModifyPT and Ability). AddPower must only be applied once.
        let mut state = setup();
        let bear = make_creature(&mut state, "Bear", 3, 3, PlayerId(0));

        // Create a static with both AddPower and AddKeyword
        let source = make_creature(&mut state, "Source", 1, 1, PlayerId(0));
        {
            let def = StaticDefinition::continuous()
                .affected(creature_you_ctrl())
                .modifications(vec![
                    ContinuousModification::AddPower { value: 2 },
                    ContinuousModification::AddKeyword {
                        keyword: Keyword::Trample,
                    },
                ]);
            state
                .objects
                .get_mut(&source)
                .unwrap()
                .static_definitions
                .push(def);
        }

        evaluate_layers(&mut state);

        let obj = state.objects.get(&bear).unwrap();
        assert_eq!(
            obj.power,
            Some(5),
            "3 base + 2 from effect = 5, NOT 7 (double-applied)"
        );
        assert!(obj.has_keyword(&Keyword::Trample));
    }

    #[test]
    fn test_source_leaves_battlefield_effect_stops() {
        let mut state = setup();
        let lord = make_creature(&mut state, "Lord", 2, 2, PlayerId(0));
        let bear = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));

        add_lord_static(&mut state, lord, creature_you_ctrl(), 1, 1);

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

    #[test]
    fn test_remove_all_abilities_clears_all_computed_ability_buckets() {
        let mut scenario = GameScenario::new();
        let target = {
            let mut card = scenario.add_creature(PlayerId(0), "Target", 2, 2);
            card.flying()
                .with_ability_definition(AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::GainLife {
                        amount: LifeAmount::Fixed(1),
                        player: GainLifePlayer::Controller,
                    },
                ))
                .with_trigger(TriggerMode::Attacks)
                .with_replacement(ReplacementEvent::GainLife)
                .with_static(StaticMode::CantAttack);
            card.id()
        };
        {
            let mut card = scenario.add_creature(PlayerId(0), "Suppressor", 1, 1);
            card.with_static_definition(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject(target))
                    .modifications(vec![ContinuousModification::RemoveAllAbilities]),
            );
        }
        let mut state = scenario.build().state().clone();

        evaluate_layers(&mut state);

        let obj = state.objects.get(&target).unwrap();
        assert!(obj.keywords.is_empty());
        assert!(obj.abilities.is_empty());
        assert!(obj.trigger_definitions.is_empty());
        assert!(obj.replacement_definitions.is_empty());
        assert!(obj.static_definitions.is_empty());
    }

    #[test]
    fn test_remove_all_abilities_reverts_to_base_when_source_leaves() {
        let mut scenario = GameScenario::new();
        let target = {
            let mut card = scenario.add_creature(PlayerId(0), "Target", 2, 2);
            card.flying()
                .with_ability_definition(AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::GainLife {
                        amount: LifeAmount::Fixed(1),
                        player: GainLifePlayer::Controller,
                    },
                ))
                .with_trigger(TriggerMode::Attacks)
                .with_replacement(ReplacementEvent::GainLife)
                .with_static(StaticMode::CantAttack);
            card.id()
        };
        let suppressor = {
            let mut card = scenario.add_creature(PlayerId(0), "Suppressor", 1, 1);
            card.with_static_definition(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject(target))
                    .modifications(vec![ContinuousModification::RemoveAllAbilities]),
            );
            card.id()
        };
        let mut state = scenario.build().state().clone();

        evaluate_layers(&mut state);
        state.battlefield.retain(|&id| id != suppressor);
        state.layers_dirty = true;
        evaluate_layers(&mut state);

        let obj = state.objects.get(&target).unwrap();
        assert_eq!(obj.keywords, vec![Keyword::Flying]);
        assert_eq!(obj.abilities.len(), 1);
        assert_eq!(obj.trigger_definitions.len(), 1);
        assert_eq!(obj.replacement_definitions.len(), 1);
        assert_eq!(obj.static_definitions.len(), 1);
    }

    #[test]
    fn test_type_change_reverts_to_base_when_source_leaves() {
        let mut state = setup();

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
            obj.base_card_types = obj.card_types.clone();
            obj.timestamp = art_ts;
        }

        let animator = make_creature(&mut state, "Animator", 1, 1, PlayerId(0));
        let artifact_you_ctrl =
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact).controller(ControllerRef::You));
        state
            .objects
            .get_mut(&animator)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::continuous()
                    .affected(artifact_you_ctrl)
                    .modifications(vec![ContinuousModification::AddType {
                        core_type: CoreType::Creature,
                    }]),
            );

        evaluate_layers(&mut state);
        assert!(state.objects[&artifact]
            .card_types
            .core_types
            .contains(&CoreType::Creature));

        state.battlefield.retain(|&id| id != animator);
        state.layers_dirty = true;
        evaluate_layers(&mut state);

        let obj = state.objects.get(&artifact).unwrap();
        assert_eq!(obj.card_types.core_types, vec![CoreType::Artifact]);
    }

    #[test]
    fn test_color_change_reverts_to_base_when_source_leaves() {
        let mut state = setup();

        let bear = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));
        let painter = make_creature(&mut state, "Painter", 1, 1, PlayerId(0));

        state
            .objects
            .get_mut(&painter)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject(bear))
                    .modifications(vec![ContinuousModification::SetColor {
                        colors: vec![ManaColor::Blue],
                    }]),
            );

        evaluate_layers(&mut state);
        assert_eq!(state.objects[&bear].color, vec![ManaColor::Blue]);

        state.battlefield.retain(|&id| id != painter);
        state.layers_dirty = true;
        evaluate_layers(&mut state);

        assert!(
            state.objects[&bear].color.is_empty(),
            "Color should revert to printed/base color when the source leaves"
        );
    }

    #[test]
    fn test_changeling_cda_grants_all_creature_types() {
        let mut state = setup();
        state.all_creature_types = vec![
            "Dragon".to_string(),
            "Elf".to_string(),
            "Human".to_string(),
            "Wizard".to_string(),
        ];

        let shapeshifter = make_creature(&mut state, "Shapeshifter", 2, 2, PlayerId(0));
        // Give it the Changeling keyword (printed)
        state
            .objects
            .get_mut(&shapeshifter)
            .unwrap()
            .base_keywords
            .push(Keyword::Changeling);
        state
            .objects
            .get_mut(&shapeshifter)
            .unwrap()
            .keywords
            .push(Keyword::Changeling);

        // Add the CDA static definition (as the parser/loader would)
        let cda = StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(vec![ContinuousModification::AddAllCreatureTypes])
            .cda();
        state
            .objects
            .get_mut(&shapeshifter)
            .unwrap()
            .static_definitions
            .push(cda);

        state.layers_dirty = true;
        evaluate_layers(&mut state);

        let obj = state.objects.get(&shapeshifter).unwrap();
        assert!(obj.card_types.subtypes.contains(&"Dragon".to_string()));
        assert!(obj.card_types.subtypes.contains(&"Elf".to_string()));
        assert!(obj.card_types.subtypes.contains(&"Human".to_string()));
        assert!(obj.card_types.subtypes.contains(&"Wizard".to_string()));
    }

    #[test]
    fn test_granted_changeling_gets_all_creature_types_via_postfixup() {
        let mut state = setup();
        state.all_creature_types = vec!["Beast".to_string(), "Goblin".to_string()];

        let creature = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));
        let lord = make_creature(&mut state, "Changeling Lord", 1, 1, PlayerId(0));

        // Lord grants Changeling to all your creatures via AddKeyword (Layer 6)
        let def = StaticDefinition::continuous()
            .affected(creature_you_ctrl())
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Changeling,
            }]);
        state
            .objects
            .get_mut(&lord)
            .unwrap()
            .static_definitions
            .push(def);

        state.layers_dirty = true;
        evaluate_layers(&mut state);

        // The bear should have all creature types via the post-fixup
        let obj = state.objects.get(&creature).unwrap();
        assert!(obj.has_keyword(&Keyword::Changeling));
        assert!(
            obj.card_types.subtypes.contains(&"Beast".to_string()),
            "Granted Changeling should add Beast via post-fixup"
        );
        assert!(
            obj.card_types.subtypes.contains(&"Goblin".to_string()),
            "Granted Changeling should add Goblin via post-fixup"
        );
    }

    #[test]
    fn test_changeling_cda_sorts_before_non_cda_in_same_layer() {
        let mut state = setup();
        state.all_creature_types = vec!["Elf".to_string(), "Sliver".to_string()];

        let shapeshifter = make_creature(&mut state, "Shapeshifter", 1, 1, PlayerId(0));
        state
            .objects
            .get_mut(&shapeshifter)
            .unwrap()
            .base_keywords
            .push(Keyword::Changeling);
        state
            .objects
            .get_mut(&shapeshifter)
            .unwrap()
            .keywords
            .push(Keyword::Changeling);

        // CDA: add all creature types (characteristic_defining = true)
        let cda = StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(vec![ContinuousModification::AddAllCreatureTypes])
            .cda();

        // Non-CDA: also adds a subtype (later timestamp, but same layer)
        let non_cda = StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(vec![ContinuousModification::AddSubtype {
                subtype: "Shapeshifter".to_string(),
            }]);

        let obj = state.objects.get_mut(&shapeshifter).unwrap();
        obj.static_definitions.push(cda);
        obj.static_definitions.push(non_cda);

        state.layers_dirty = true;
        evaluate_layers(&mut state);

        let obj = state.objects.get(&shapeshifter).unwrap();
        // All types from CDA + the explicit Shapeshifter subtype should be present
        assert!(obj.card_types.subtypes.contains(&"Elf".to_string()));
        assert!(obj.card_types.subtypes.contains(&"Sliver".to_string()));
        assert!(obj
            .card_types
            .subtypes
            .contains(&"Shapeshifter".to_string()));
    }

    #[test]
    fn test_chosen_basic_land_type_adds_subtype() {
        use crate::types::ability::{BasicLandType, ChosenAttribute};

        let mut state = setup();

        // Create a land with a chosen basic land type (simulating Multiversal Passage)
        let land = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Multiversal Passage".to_string(),
            Zone::Battlefield,
        );
        let ts = state.next_timestamp();
        {
            let obj = state.objects.get_mut(&land).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Land);
            obj.timestamp = ts;
            // Simulate the ETB choice: chose Forest
            obj.chosen_attributes
                .push(ChosenAttribute::BasicLandType(BasicLandType::Forest));
            // Add the static definition that reads the chosen type
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SelfRef)
                    .modifications(vec![ContinuousModification::AddChosenSubtype {
                        kind: ChosenSubtypeKind::BasicLandType,
                    }]),
            );
        }

        state.layers_dirty = true;
        evaluate_layers(&mut state);

        let obj = state.objects.get(&land).unwrap();
        assert!(
            obj.card_types.subtypes.contains(&"Forest".to_string()),
            "Land should gain Forest subtype from chosen basic land type"
        );
    }

    #[test]
    fn test_chosen_basic_land_type_no_choice_is_noop() {
        let mut state = setup();

        // Land with AddChosenSubtype(BasicLandType) but no choice stored
        let land = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Unchosen Land".to_string(),
            Zone::Battlefield,
        );
        let ts = state.next_timestamp();
        {
            let obj = state.objects.get_mut(&land).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Land);
            obj.timestamp = ts;
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SelfRef)
                    .modifications(vec![ContinuousModification::AddChosenSubtype {
                        kind: ChosenSubtypeKind::BasicLandType,
                    }]),
            );
        }

        state.layers_dirty = true;
        evaluate_layers(&mut state);

        let obj = state.objects.get(&land).unwrap();
        assert!(
            obj.card_types.subtypes.is_empty(),
            "No subtypes should be added when no choice was made"
        );
    }

    #[test]
    fn test_chosen_creature_type_adds_subtype() {
        use crate::types::ability::ChosenAttribute;

        let mut state = setup();

        let mimic = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Metallic Mimic".to_string(),
            Zone::Battlefield,
        );
        let ts = state.next_timestamp();
        {
            let obj = state.objects.get_mut(&mimic).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Creature);
            obj.card_types.subtypes.push("Shapeshifter".to_string());
            obj.timestamp = ts;
            obj.chosen_attributes
                .push(ChosenAttribute::CreatureType("Elf".to_string()));
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SelfRef)
                    .modifications(vec![ContinuousModification::AddChosenSubtype {
                        kind: ChosenSubtypeKind::CreatureType,
                    }]),
            );
        }

        state.layers_dirty = true;
        evaluate_layers(&mut state);

        let obj = state.objects.get(&mimic).unwrap();
        assert!(
            obj.card_types.subtypes.contains(&"Elf".to_string()),
            "Creature should gain Elf subtype from chosen creature type"
        );
        assert!(
            obj.card_types
                .subtypes
                .contains(&"Shapeshifter".to_string()),
            "Should retain original subtypes"
        );
    }

    #[test]
    fn test_tarmogoyf_cda_counts_card_types_in_graveyards() {
        use crate::types::ability::DynamicPTValue;

        let mut state = setup();

        // Create Tarmogoyf with */1+* base P/T and CDA static definition
        let goyf = make_creature(&mut state, "Tarmogoyf", 0, 1, PlayerId(0));
        {
            let obj = state.objects.get_mut(&goyf).unwrap();
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SelfRef)
                    .modifications(vec![
                        ContinuousModification::SetDynamicPower {
                            value: DynamicPTValue::CardTypesInAllGraveyards { offset: 0 },
                        },
                        ContinuousModification::SetDynamicToughness {
                            value: DynamicPTValue::CardTypesInAllGraveyards { offset: 1 },
                        },
                    ])
                    .cda(),
            );
        }

        // Empty graveyards: 0 card types → P/T = 0/1
        state.layers_dirty = true;
        evaluate_layers(&mut state);
        let obj = state.objects.get(&goyf).unwrap();
        assert_eq!(obj.power, Some(0), "No card types in graveyards → power 0");
        assert_eq!(obj.toughness, Some(1), "No card types → toughness 0+1=1");

        // Add a creature to graveyard: 1 card type → P/T = 1/2
        let gy_creature = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Dead Bear".to_string(),
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&gy_creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state.players[0].graveyard.push(gy_creature);

        state.layers_dirty = true;
        evaluate_layers(&mut state);
        let obj = state.objects.get(&goyf).unwrap();
        assert_eq!(obj.power, Some(1), "Creature in graveyard → power 1");
        assert_eq!(
            obj.toughness,
            Some(2),
            "Creature in graveyard → toughness 2"
        );

        // Add an instant to opponent's graveyard: 2 card types → P/T = 2/3
        let gy_instant = create_object(
            &mut state,
            CardId(0),
            PlayerId(1),
            "Spent Bolt".to_string(),
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&gy_instant)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);
        state.players[1].graveyard.push(gy_instant);

        state.layers_dirty = true;
        evaluate_layers(&mut state);
        let obj = state.objects.get(&goyf).unwrap();
        assert_eq!(obj.power, Some(2), "Creature + Instant → power 2");
        assert_eq!(obj.toughness, Some(3), "Creature + Instant → toughness 3");

        // Add an artifact creature to graveyard: still 2 types (creature already counted), + artifact = 3
        let gy_artcreature = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Dead Robot".to_string(),
            Zone::Graveyard,
        );
        {
            let obj = state.objects.get_mut(&gy_artcreature).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.card_types.core_types.push(CoreType::Creature);
        }
        state.players[0].graveyard.push(gy_artcreature);

        state.layers_dirty = true;
        evaluate_layers(&mut state);
        let obj = state.objects.get(&goyf).unwrap();
        assert_eq!(
            obj.power,
            Some(3),
            "Creature + Instant + Artifact → power 3"
        );
        assert_eq!(
            obj.toughness,
            Some(4),
            "Creature + Instant + Artifact → toughness 4"
        );
    }
}
