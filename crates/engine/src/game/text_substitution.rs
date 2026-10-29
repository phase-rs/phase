//! CR 612: text-changing effects that replace one word with another, read by the Layer 3 pre-pass for battlefield permanents and by the resolution seam for spells on the stack.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use crate::game::game_object::GameObject;
use crate::game::layers::{
    gather_transient_continuous_effects, is_intrinsic_basic_land_mana_ability, order_by_timestamp,
    select_next_effect,
};
use crate::types::ability::{
    ContinuousModification, ResolvedAbility, TargetFilter, TextSubstitution, TextSubstitutionSpec,
    TriggerEntry,
};
use crate::types::definitions::Definitions;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::layers::{ActiveContinuousEffect, Layer};

pub use crate::types::text_word_carriers::{
    carrier_class, classified_word_occurrences, unclassified_word_positions, WordCarrier,
    WordClass, WORD_CARRIERS,
};

/// Rewrites each definition `rewrite` changes; returns the word instances replaced.
fn rewrite_each<T: Clone>(
    definitions: &mut Definitions<T>,
    rewrite: impl Fn(&T) -> Option<(T, usize)>,
) -> usize {
    let updates: Vec<(usize, T, usize)> = (0..definitions.len())
        .filter_map(|i| rewrite(&definitions[i]).map(|(rewritten, n)| (i, rewritten, n)))
        .collect();
    let mut occurrences = 0;
    for (i, rewritten, n) in updates {
        definitions[i] = rewritten;
        occurrences += n;
    }
    occurrences
}

/// CR 612.1 + CR 612.2: applies one substitution to a permanent's rules text and type line, never touching the name, mana cost, color indicator or P/T because mana symbols and names are not words; returns the word instances replaced, with the type line's repeats kept until [`collapse_subtype_repeats`].
fn apply_to_permanent_text(obj: &mut GameObject, substitution: &TextSubstitution) -> usize {
    let mut occurrences = 0;
    let updates: Vec<(usize, _, usize)> = obj
        .abilities
        .iter()
        .enumerate()
        .filter_map(|(i, ability)| {
            substitution
                .rewrite_counted(ability)
                .map(|(a, n)| (i, a, n))
        })
        .collect();
    if !updates.is_empty() {
        let abilities = Arc::make_mut(&mut obj.abilities);
        for (i, ability, n) in updates {
            abilities[i] = ability;
            occurrences += n;
        }
    }

    occurrences += rewrite_each(&mut obj.trigger_definitions, |entry: &TriggerEntry| {
        substitution
            .rewrite_counted(&entry.definition)
            .map(|(definition, n)| {
                let mut rewritten = entry.clone();
                rewritten.definition = definition;
                (rewritten, n)
            })
    });
    occurrences += rewrite_each(&mut obj.static_definitions, |definition| {
        substitution.rewrite_counted(definition)
    });
    occurrences += rewrite_each(&mut obj.replacement_definitions, |definition| {
        (!definition.is_resolution_installed())
            .then(|| substitution.rewrite_counted(definition))
            .flatten()
    });

    for keyword in obj.keywords.iter_mut() {
        occurrences += substitution.rewrite_in_place(keyword);
    }

    // CR 205.3i + CR 612.1: a land-type word on the type line is text too.
    if let TextSubstitution::BasicLandType { from, .. } = substitution {
        let (from_word, to_word) = substitution.words();
        let mut changed = false;
        for subtype in obj.card_types.subtypes.iter_mut() {
            if subtype == from_word {
                *subtype = to_word.to_owned();
                occurrences += 1;
                changed = true;
            }
        }
        if changed {
            // CR 305.6: the replaced type's intrinsic mana ability goes with its word, and the new type's ability is derived after the Type layer.
            let color = from.mana_color();
            if obj
                .abilities
                .iter()
                .any(|a| is_intrinsic_basic_land_mana_ability(a, color))
            {
                Arc::make_mut(&mut obj.abilities)
                    .retain(|a| !is_intrinsic_basic_land_mana_ability(a, color));
            }
        }
    }
    occurrences
}

/// The engine's subtype list holds no repeats, so rewrites onto an existing subtype collapse once the last change has applied.
fn collapse_subtype_repeats(obj: &mut GameObject) {
    let mut seen = HashSet::new();
    obj.card_types.subtypes.retain(|s| seen.insert(s.clone()));
}

/// CR 613.8a: effect X depends on effect Y iff applying Y to the recipient's current text changes how many words X rewrites there, so the edge `i -> j` is measured by running the rewrite itself on clones of the recipient.
fn dependency_edges<R: Clone>(
    recipient: &R,
    pending: &[TextSubstitution],
    apply: fn(&mut R, &TextSubstitution) -> usize,
) -> Vec<Vec<usize>> {
    let now: Vec<usize> = pending
        .iter()
        .map(|substitution| apply(&mut recipient.clone(), substitution))
        .collect();
    let mut edges = vec![Vec::new(); pending.len()];
    for (j, provider) in pending.iter().enumerate() {
        let mut after = recipient.clone();
        apply(&mut after, provider);
        for (i, substitution) in pending.iter().enumerate() {
            if i != j && apply(&mut after.clone(), substitution) != now[i] {
                edges[i].push(j);
            }
        }
    }
    edges
}

/// CR 613.8a-c: applies `pending` (in timestamp order) to `recipient` one effect at a time, reevaluating the dependency relation against the changed text after each; returns the word instances replaced.
fn apply_in_dependency_order<R: Clone>(
    recipient: &mut R,
    mut pending: Vec<TextSubstitution>,
    apply: fn(&mut R, &TextSubstitution) -> usize,
) -> usize {
    let mut occurrences = 0;
    while !pending.is_empty() {
        let next = if pending.len() == 1 {
            0
        } else {
            select_next_effect(&dependency_edges(recipient, &pending, apply))
        };
        let selected = pending.remove(next);
        occurrences += apply(recipient, &selected);
    }
    occurrences
}

/// CR 612.1 + CR 613.7: the `Fixed` substitutions in effect on each `SpecificObject` recipient in timestamp order, grouped per recipient so an effect on one object never changes what an effect on another does to its own words; the application order is [`apply_in_dependency_order`]'s.
pub fn active_text_substitutions(state: &GameState) -> BTreeMap<ObjectId, Vec<TextSubstitution>> {
    let mut gathered = Vec::new();
    gather_transient_continuous_effects(state, &mut gathered);

    let mut by_recipient: BTreeMap<ObjectId, Vec<ActiveContinuousEffect>> = BTreeMap::new();
    for effect in gathered {
        if effect.layer != Layer::Text {
            continue;
        }
        let ContinuousModification::SubstituteTextWord {
            substitution: TextSubstitutionSpec::Fixed(_),
        } = &effect.modification
        else {
            continue;
        };
        let TargetFilter::SpecificObject { id } = &effect.affected_filter else {
            continue;
        };
        by_recipient.entry(*id).or_default().push(effect);
    }

    by_recipient
        .into_iter()
        .map(|(id, effects)| {
            let ordered = order_by_timestamp(&effects.iter().collect::<Vec<_>>());
            let substitutions = ordered
                .into_iter()
                .filter_map(|effect| match effect.modification {
                    ContinuousModification::SubstituteTextWord {
                        substitution: TextSubstitutionSpec::Fixed(substitution),
                    } => Some(substitution),
                    _ => None,
                })
                .collect();
            (id, substitutions)
        })
        .collect()
}

/// CR 612.1 + CR 613.1c: the Layer 3 pre-pass runs before the statics of layers 4-7 are gathered so a changed static generates its effects from the changed text, and spells on the stack are read through [`restamp_resolving_spell_text`] instead.
pub(crate) fn apply_battlefield_text_substitutions(state: &mut GameState, bf_ids: &[ObjectId]) {
    let mut substitutions = active_text_substitutions(state);
    if substitutions.is_empty() {
        return;
    }
    for id in bf_ids {
        let Some(list) = substitutions.remove(id) else {
            continue;
        };
        let Some(obj) = state.objects.get_mut(id) else {
            continue;
        };
        if apply_in_dependency_order(obj, list, apply_to_permanent_text) > 0 {
            collapse_subtype_repeats(obj);
        }
    }
}

/// CR 612.1 + CR 608.2b: rewrites the ability chain in a resolving spell's stack entry, because the spell reads that copy rather than the object's printed abilities and the layer pass never resets stack objects.
pub fn restamp_resolving_spell_text(
    state: &GameState,
    object_id: ObjectId,
    ability: &mut ResolvedAbility,
) {
    let mut substitutions = active_text_substitutions(state);
    let Some(list) = substitutions.remove(&object_id) else {
        return;
    };
    apply_in_dependency_order(ability, list, rewrite_resolved_ability);
}

/// Field-exhaustive on purpose: a new `ResolvedAbility` field forces a decision
/// between rules text (rewritten) and runtime state (never text); returns the word instances replaced.
fn rewrite_resolved_ability(
    ability: &mut ResolvedAbility,
    substitution: &TextSubstitution,
) -> usize {
    let ResolvedAbility {
        // Rules text: filters, conditions and quantities that carry words.
        effect,
        condition,
        duration,
        optional_player,
        multi_target,
        target_constraints,
        repeat_for,
        unless_pay,
        player_scope,
        target_chooser,
        activation_cost_reduction,
        repeat_until,
        mode_abilities,
        // The rest of the chain is rewritten recursively.
        sub_ability,
        else_ability,
        // Runtime state, never text: choices, identities, paid costs and display strings are fixed at announcement (CR 601.2b) and keep the old words.
        targets: _,
        declares_chosen_group: _,
        reads_chosen_group: _,
        declares_return_result: _,
        reads_return_result: _,
        source_id: _,
        cast_occurrence: _,
        source_incarnation: _,
        announced_source_incarnation: _,
        trigger_source: _,
        trigger_definition_ref: _,
        delayed_origin: _,
        force_block_attacker: _,
        target_incarnations: _,
        target_pins: _,
        legacy_selected_target_incarnations: _,
        chosen_target_slots: _,
        activation_record: _,
        illegal_target_slots: _,
        unjudged_target_slots: _,
        illegal_local_target_slots: _,
        target_reads: _,
        entry_onset: _,
        controller: _,
        original_controller: _,
        scoped_player: _,
        kind: _,
        context: _,
        optional_targeting: _,
        optional: _,
        optional_for: _,
        target_choice_timing: _,
        description: _,
        selected_mode_labels: _,
        modal_instruction_ordinal: _,
        detached_remainder: _,
        min_x_value: _,
        announced_x: _,
        cant_be_copied: _,
        copy_count_status: _,
        forward_result: _,
        distribution: _,
        distribute: _,
        starting_with: _,
        chosen_x: _,
        cost_paid_object: _,
        noted_mana_payment: _,
        cost_paid_objects: _,
        effect_context_object: _,
        amassed_army_object: _,
        ability_index: _,
        may_trigger_origin: _,
        target_selection_mode: _,
        chosen_players: _,
        replacement_applied: _,
        sub_link: _,
        sibling_condition: _,
        modal: _,
        parent_target_missing_reason: _,
        illegal_targets_disposition: _,
    } = ability;

    let mut occurrences = substitution.rewrite_in_place(effect)
        + substitution.rewrite_in_place(condition)
        + substitution.rewrite_in_place(duration)
        + substitution.rewrite_in_place(optional_player)
        + substitution.rewrite_in_place(multi_target)
        + substitution.rewrite_in_place(target_constraints)
        + substitution.rewrite_in_place(repeat_for)
        + substitution.rewrite_in_place(unless_pay)
        + substitution.rewrite_in_place(player_scope)
        + substitution.rewrite_in_place(target_chooser)
        + substitution.rewrite_in_place(activation_cost_reduction)
        + substitution.rewrite_in_place(repeat_until)
        + substitution.rewrite_in_place(mode_abilities);
    for next in [sub_ability, else_ability].into_iter().flatten() {
        occurrences += rewrite_resolved_ability(next, substitution);
    }
    occurrences
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{AbilityCondition, BasicLandType, SpentColor, TriggerCondition};
    use crate::types::mana::ManaColor;
    use serde_json::json;

    #[test]
    fn land_word_rewrites_type_positions_but_not_names() {
        let substitution =
            TextSubstitution::basic_land_type(BasicLandType::Forest, BasicLandType::Island)
                .expect("from != to");
        let token = json!({
            "type": "Token",
            "name": "Forest Dryad",
            "types": ["Land", "Creature", "Forest", "Dryad"],
            "keywords": [{"Landwalk": "Forest"}],
        });
        let rewritten = substitution.rewrite(&token).expect("land words present");
        assert_eq!(
            rewritten["name"], "Forest Dryad",
            "CR 612.2: a name is not a word"
        );
        assert_eq!(
            rewritten["types"],
            json!(["Land", "Creature", "Island", "Dryad"])
        );
        assert_eq!(rewritten["keywords"], json!([{"Landwalk": "Island"}]));
    }

    #[test]
    fn color_word_rewrites_words_but_not_mana_symbols() {
        let substitution =
            TextSubstitution::color(ManaColor::Green, ManaColor::Blue).expect("from != to");
        let ability = json!({
            "type": "Mana",
            "produced": {"type": "Fixed", "colors": ["Green"]},
            "filter": {
                "type": "Typed",
                "properties": [{"type": "HasColor", "color": "Green"}],
            },
        });
        let rewritten = substitution
            .rewrite(&ability)
            .expect("a color word is present");
        assert_eq!(
            rewritten["produced"]["colors"],
            json!(["Green"]),
            "{{G}} is a symbol"
        );
        assert_eq!(
            rewritten["filter"]["properties"][0]["color"], "Blue",
            "the color word changes"
        );
    }

    #[test]
    fn spent_color_rewrites_the_word_form_and_spares_the_symbol_form() {
        let substitution =
            TextSubstitution::color(ManaColor::Red, ManaColor::Blue).expect("from != to");
        let word = AbilityCondition::ManaColorSpent {
            color: SpentColor::ColorWord {
                color: ManaColor::Red,
            },
            minimum: 3,
        };
        let symbol = AbilityCondition::ManaColorSpent {
            color: SpentColor::ManaSymbol {
                color: ManaColor::Red,
            },
            minimum: 1,
        };
        assert_eq!(
            substitution.rewrite(&word),
            Some(AbilityCondition::ManaColorSpent {
                color: SpentColor::ColorWord {
                    color: ManaColor::Blue,
                },
                minimum: 3,
            }),
            "CR 612.2: a color word changes"
        );
        assert_eq!(
            substitution.rewrite(&symbol),
            None,
            "CR 107.4: {{R}} is a symbol"
        );

        let both = AbilityCondition::And {
            conditions: vec![word, symbol.clone()],
        };
        let Some(AbilityCondition::And { conditions }) = substitution.rewrite(&both) else {
            panic!("the word leaf of the conjunction changes");
        };
        assert_eq!(
            conditions,
            [
                AbilityCondition::ManaColorSpent {
                    color: SpentColor::ColorWord {
                        color: ManaColor::Blue,
                    },
                    minimum: 3,
                },
                symbol
            ]
        );

        let trigger = |color| TriggerCondition::ManaColorSpent { color, minimum: 3 };
        assert_eq!(
            substitution.rewrite(&trigger(SpentColor::ColorWord {
                color: ManaColor::Red,
            })),
            Some(trigger(SpentColor::ColorWord {
                color: ManaColor::Blue,
            }))
        );
        assert_eq!(
            substitution.rewrite(&TriggerCondition::Not {
                condition: Box::new(trigger(SpentColor::ManaSymbol {
                    color: ManaColor::Red,
                })),
            }),
            None
        );
    }

    #[test]
    fn nothing_to_change_and_unclassified_positions_are_left_alone() {
        let substitution =
            TextSubstitution::color(ManaColor::Green, ManaColor::Blue).expect("from != to");
        assert_eq!(
            substitution.rewrite(&json!({"type": "Draw", "count": 1})),
            None
        );

        let mystery = json!({"type": "Mystery", "unlisted": "Green"});
        assert_eq!(
            substitution.rewrite(&mystery),
            None,
            "fail-closed on an unlisted position"
        );
        assert_eq!(
            unclassified_word_positions(&mystery),
            [(
                Some("Mystery".to_string()),
                "unlisted".to_string(),
                "Green".to_string()
            )]
        );
    }
}
