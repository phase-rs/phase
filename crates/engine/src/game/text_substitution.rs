//! CR 612: text-changing effects that replace one word with another, read by the Layer 3 pre-pass for battlefield permanents and by the resolution seam for spells on the stack.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use crate::game::game_object::GameObject;
use crate::game::layers::{
    gather_transient_continuous_effects, is_intrinsic_basic_land_mana_ability,
    order_active_continuous_effects,
};
use crate::types::ability::{
    ContinuousModification, ResolvedAbility, TargetFilter, TextSubstitution, TextSubstitutionSpec,
    TextWordDomain, TriggerEntry,
};
use crate::types::definitions::Definitions;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::layers::{ActiveContinuousEffect, Layer};

/// Is a serialized color/land-type string a word of the Oracle text, or a symbol
/// or identity that merely shares its spelling?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WordClass {
    /// CR 612.2: the Oracle phrase that produced this value renders it as a word.
    Word,
    /// A mana symbol, card/token name or other non-word; the reason names why.
    NotAWord(&'static str),
}

/// One classified carrier position: the nearest enclosing serialized `type` tag
/// (`None` for externally tagged or untagged positions), the field or enum-variant
/// key holding the string (`[]` suffix for list elements), and the word domain of
/// the string that can sit there.
#[derive(Debug, Clone, Copy)]
pub struct WordCarrier {
    pub tag: Option<&'static str>,
    pub key: &'static str,
    pub domain: TextWordDomain,
    pub class: WordClass,
}

const fn carrier(
    tag: Option<&'static str>,
    key: &'static str,
    domain: TextWordDomain,
    class: WordClass,
) -> WordCarrier {
    WordCarrier {
        tag,
        key,
        domain,
        class,
    }
}

const SYMBOL: WordClass = WordClass::NotAWord("mana symbol or mana-production identity");
const NAME: WordClass = WordClass::NotAWord("card or token name (CR 612.2)");
const COLOR: TextWordDomain = TextWordDomain::ColorWord;
const LAND: TextWordDomain = TextWordDomain::BasicLandType;
const W: WordClass = WordClass::Word;

/// The single authority for CR 612.2 "used in the correct way", so a color or basic-land-type string at a position not listed here is left unchanged.
pub const WORD_CARRIERS: &[WordCarrier] = &[
    // ---- color words ----
    carrier(Some("AddColor"), "color", COLOR, W),
    carrier(Some("AddKeyword"), "Color", COLOR, W),
    carrier(Some("AddStaticMode"), "Transform", COLOR, W),
    carrier(Some("AddStaticMode"), "filter", COLOR, W),
    carrier(Some("AnyCombination"), "color_options[]", COLOR, SYMBOL),
    carrier(Some("AnyOneColor"), "color_options[]", COLOR, SYMBOL),
    carrier(Some("ChoiceAmongCombinations"), "options[]", COLOR, SYMBOL),
    carrier(Some("Choose"), "excluded[]", COLOR, W),
    carrier(Some("Choose"), "options[]", COLOR, W),
    carrier(Some("ChosenColor"), "fixed_alternative", COLOR, SYMBOL),
    carrier(Some("Color"), "data", COLOR, W),
    carrier(Some("Cost"), "shards[]", COLOR, SYMBOL),
    carrier(Some("DevotionGE"), "colors[]", COLOR, W),
    carrier(Some("ExileWithAggregate"), "ManaSymbolCount", COLOR, SYMBOL),
    carrier(Some("Fixed"), "colors[]", COLOR, SYMBOL),
    carrier(Some("Fixed"), "value[]", COLOR, SYMBOL),
    carrier(Some("GenericEffect"), "filter", COLOR, W),
    carrier(Some("HasColor"), "color", COLOR, W),
    carrier(Some("ManaColorSpent"), "color", COLOR, W),
    carrier(Some("ManaSymbolCount"), "color", COLOR, SYMBOL),
    carrier(Some("ManaSymbolsInManaCost"), "color", COLOR, SYMBOL),
    carrier(Some("Mixed"), "colors[]", COLOR, SYMBOL),
    carrier(None, "Color", COLOR, W),
    carrier(None, "Transform", COLOR, W),
    carrier(None, "color", COLOR, W),
    carrier(None, "colors[]", COLOR, W),
    carrier(None, "filter", COLOR, W),
    carrier(Some("NotColor"), "color", COLOR, W),
    carrier(Some("OfColor"), "color", COLOR, W),
    carrier(Some("PropertyAggregate"), "ManaSymbolCount", COLOR, SYMBOL),
    carrier(Some("RemoveKeyword"), "Color", COLOR, W),
    carrier(Some("ReplaceWith"), "mana_type", COLOR, SYMBOL),
    carrier(Some("SetColor"), "colors[]", COLOR, W),
    carrier(Some("SourceIsColor"), "color", COLOR, W),
    carrier(Some("Token"), "Color", COLOR, W),
    carrier(Some("Token"), "colors[]", COLOR, W),
    // The parser reads a color word as a subtype or creature type in these two
    // positions; the Oracle word is still a color word, so it is rewritten.
    carrier(Some("Token"), "types[]", COLOR, W),
    carrier(Some("Typed"), "Subtype", COLOR, W),
    carrier(Some("UnspentMana"), "color", COLOR, W),
    // ---- basic land types ----
    carrier(Some("AddKeyword"), "Landwalk", LAND, W),
    carrier(Some("AddSubtype"), "subtype", LAND, W),
    carrier(Some("ChangeZone"), "subtypes[]", LAND, W),
    carrier(Some("Choose"), "options[]", LAND, W),
    carrier(Some("Conjure"), "name", LAND, NAME),
    carrier(None, "Landwalk", LAND, W),
    carrier(None, "Subtype", LAND, W),
    carrier(None, "qualifier", LAND, W),
    carrier(None, "subtype", LAND, W),
    carrier(Some("RemoveKeyword"), "Landwalk", LAND, W),
    carrier(Some("SetBasicLandType"), "land_type", LAND, W),
    carrier(Some("Token"), "Landwalk", LAND, W),
    carrier(Some("Token"), "name", LAND, NAME),
    carrier(Some("Token"), "types[]", LAND, W),
    carrier(Some("Typed"), "Subtype", LAND, W),
    carrier(Some("UnlessControlsSubtype"), "subtypes[]", LAND, W),
    carrier(Some("WithKeyword"), "Landwalk", LAND, W),
    carrier(Some("WithoutKeyword"), "Landwalk", LAND, W),
];

/// Classification of the string at `(tag, key)` for words of `domain`, if listed.
pub fn carrier_class(tag: Option<&str>, key: &str, domain: TextWordDomain) -> Option<WordClass> {
    WORD_CARRIERS
        .iter()
        .find(|c| c.tag == tag && c.key == key && c.domain == domain)
        .map(|c| c.class)
}

/// Visits every string leaf of a serialized value with its nearest enclosing
/// `type` tag and the key (list elements carry a `[]` suffix) that holds it.
fn walk_strings(
    value: &mut Value,
    tag: Option<&str>,
    key: &str,
    visit: &mut dyn FnMut(Option<&str>, &str, &mut String),
) {
    match value {
        Value::Object(map) => {
            let own_tag = map.get("type").and_then(Value::as_str).map(str::to_owned);
            let tag = own_tag.as_deref().or(tag);
            for (field, child) in map.iter_mut() {
                walk_strings(child, tag, field, visit);
            }
        }
        Value::Array(items) => {
            let list_key = if key.ends_with("[]") {
                key.to_owned()
            } else {
                format!("{key}[]")
            };
            for item in items {
                walk_strings(item, tag, &list_key, visit);
            }
        }
        Value::String(leaf) => visit(tag, key, leaf),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

/// Every `(tag, key, word)` in `value` where a color word or basic land type string
/// sits at a position [`WORD_CARRIERS`] does not classify.
pub fn unclassified_word_positions(value: &Value) -> Vec<(Option<String>, String, String)> {
    let mut missing = Vec::new();
    let mut owned = value.clone();
    walk_strings(&mut owned, None, "", &mut |tag, key, leaf| {
        let Some(domain) = word_domain(leaf) else {
            return;
        };
        if carrier_class(tag, key, domain).is_none() {
            missing.push((tag.map(str::to_owned), key.to_owned(), leaf.clone()));
        }
    });
    missing
}

/// The `(tag, key)` positions of every classified `Word` occurrence in `value`
/// (used to prove a census visits real carriers, not an empty corpus).
pub fn classified_word_occurrences(value: &Value) -> usize {
    let mut count = 0;
    let mut owned = value.clone();
    walk_strings(&mut owned, None, "", &mut |tag, key, leaf| {
        if word_domain(leaf).is_some_and(|d| carrier_class(tag, key, d) == Some(WordClass::Word)) {
            count += 1;
        }
    });
    count
}

/// Which closed word set a bare string belongs to, if either.
fn word_domain(leaf: &str) -> Option<TextWordDomain> {
    use std::str::FromStr;
    if crate::types::mana::ManaColor::from_str(leaf).is_ok() {
        Some(TextWordDomain::ColorWord)
    } else if crate::types::ability::BasicLandType::from_str(leaf).is_ok() {
        Some(TextWordDomain::BasicLandType)
    } else {
        None
    }
}

impl TextSubstitution {
    /// CR 612.1 + CR 612.2: `value` with every replaced word rewritten at classified carrier positions only, or `None` when nothing changed or the value does not survive its serialized form.
    pub fn rewrite<T: Serialize + DeserializeOwned>(&self, value: &T) -> Option<T> {
        let mut json = serde_json::to_value(value).ok()?;
        let (from, to) = self.words();
        let domain = self.domain();
        let mut changed = false;
        walk_strings(&mut json, None, "", &mut |tag, key, leaf| {
            if leaf == from && carrier_class(tag, key, domain) == Some(WordClass::Word) {
                *leaf = to.to_owned();
                changed = true;
            }
        });
        if !changed {
            return None;
        }
        match serde_json::from_value(json) {
            Ok(rewritten) => Some(rewritten),
            Err(error) => {
                debug_assert!(false, "text substitution broke a serde round trip: {error}");
                None
            }
        }
    }

    /// Rewrites `value` in place; returns whether it changed.
    fn rewrite_in_place<T: Serialize + DeserializeOwned>(&self, value: &mut T) -> bool {
        match self.rewrite(value) {
            Some(rewritten) => {
                *value = rewritten;
                true
            }
            None => false,
        }
    }
}

fn rewrite_each<T: Clone>(definitions: &mut Definitions<T>, rewrite: impl Fn(&T) -> Option<T>) {
    let updates: Vec<(usize, T)> = (0..definitions.len())
        .filter_map(|i| rewrite(&definitions[i]).map(|rewritten| (i, rewritten)))
        .collect();
    for (i, rewritten) in updates {
        definitions[i] = rewritten;
    }
}

/// CR 612.1 + CR 612.2: applies one substitution to a permanent's rules text and type line, never touching the name, mana cost, color indicator or P/T because mana symbols and names are not words.
fn apply_to_permanent_text(obj: &mut GameObject, substitution: &TextSubstitution) {
    let updates: Vec<(usize, _)> = obj
        .abilities
        .iter()
        .enumerate()
        .filter_map(|(i, ability)| substitution.rewrite(ability).map(|a| (i, a)))
        .collect();
    if !updates.is_empty() {
        let abilities = Arc::make_mut(&mut obj.abilities);
        for (i, ability) in updates {
            abilities[i] = ability;
        }
    }

    rewrite_each(&mut obj.trigger_definitions, |entry: &TriggerEntry| {
        substitution.rewrite(&entry.definition).map(|definition| {
            let mut rewritten = entry.clone();
            rewritten.definition = definition;
            rewritten
        })
    });
    rewrite_each(&mut obj.static_definitions, |definition| {
        substitution.rewrite(definition)
    });
    rewrite_each(&mut obj.replacement_definitions, |definition| {
        (!definition.is_resolution_installed())
            .then(|| substitution.rewrite(definition))
            .flatten()
    });

    for keyword in obj.keywords.iter_mut() {
        substitution.rewrite_in_place(keyword);
    }

    // CR 205.3i + CR 612.1: a land-type word on the type line is text too; a subtype
    // set has no repeats, so a rewrite onto an existing subtype collapses.
    if let TextSubstitution::BasicLandType { from, .. } = substitution {
        let (from_word, to_word) = substitution.words();
        let mut changed = false;
        for subtype in obj.card_types.subtypes.iter_mut() {
            if subtype == from_word {
                *subtype = to_word.to_owned();
                changed = true;
            }
        }
        if changed {
            let mut seen = HashSet::new();
            obj.card_types.subtypes.retain(|s| seen.insert(s.clone()));
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
}

/// CR 612.1 + CR 613.7b + CR 613.8: the `Fixed` substitutions in effect on each `SpecificObject` recipient in application order, grouped per recipient so an effect on one object never changes what an effect on another does to its own words.
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
            let ordered = order_active_continuous_effects(Layer::Text, &effects, state);
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
    let substitutions = active_text_substitutions(state);
    if substitutions.is_empty() {
        return;
    }
    for id in bf_ids {
        let Some(list) = substitutions.get(id) else {
            continue;
        };
        let Some(obj) = state.objects.get_mut(id) else {
            continue;
        };
        for substitution in list {
            apply_to_permanent_text(obj, substitution);
        }
    }
}

/// CR 612.1 + CR 608.2b: rewrites the ability chain in a resolving spell's stack entry, because the spell reads that copy rather than the object's printed abilities and the layer pass never resets stack objects.
pub fn restamp_resolving_spell_text(
    state: &GameState,
    object_id: ObjectId,
    ability: &mut ResolvedAbility,
) {
    let substitutions = active_text_substitutions(state);
    let Some(list) = substitutions.get(&object_id) else {
        return;
    };
    for substitution in list {
        rewrite_resolved_ability(substitution, ability);
    }
}

/// Field-exhaustive on purpose: a new `ResolvedAbility` field forces a decision
/// between rules text (rewritten) and runtime state (never text).
fn rewrite_resolved_ability(substitution: &TextSubstitution, ability: &mut ResolvedAbility) {
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
        trigger_source: _,
        trigger_definition_ref: _,
        force_block_attacker: _,
        target_incarnations: _,
        selected_target_incarnations: _,
        activation_record: _,
        illegal_target_slots: _,
        illegal_local_target_slots: _,
        target_reads: _,
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

    substitution.rewrite_in_place(effect);
    substitution.rewrite_in_place(condition);
    substitution.rewrite_in_place(duration);
    substitution.rewrite_in_place(optional_player);
    substitution.rewrite_in_place(multi_target);
    substitution.rewrite_in_place(target_constraints);
    substitution.rewrite_in_place(repeat_for);
    substitution.rewrite_in_place(unless_pay);
    substitution.rewrite_in_place(player_scope);
    substitution.rewrite_in_place(target_chooser);
    substitution.rewrite_in_place(activation_cost_reduction);
    substitution.rewrite_in_place(repeat_until);
    substitution.rewrite_in_place(mode_abilities);
    for next in [sub_ability, else_ability].into_iter().flatten() {
        rewrite_resolved_ability(substitution, next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::BasicLandType;
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
