//! CR 612.2: which serialized color and basic-land-type strings are words of the Oracle text, and which are symbols or names that only share a word's spelling.

use std::str::FromStr;

use serde_json::Value;

use super::ability::TextWordDomain;

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
    carrier(Some("ColorWord"), "color", COLOR, W),
    carrier(Some("Cost"), "shards[]", COLOR, SYMBOL),
    carrier(Some("DevotionGE"), "colors[]", COLOR, W),
    carrier(Some("ExileWithAggregate"), "ManaSymbolCount", COLOR, SYMBOL),
    carrier(Some("Fixed"), "colors[]", COLOR, SYMBOL),
    carrier(Some("Fixed"), "value[]", COLOR, SYMBOL),
    carrier(Some("GenericEffect"), "filter", COLOR, W),
    carrier(Some("HasColor"), "color", COLOR, W),
    carrier(Some("ManaSymbol"), "color", COLOR, SYMBOL),
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
pub(crate) fn walk_strings(
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
    if crate::types::mana::ManaColor::from_str(leaf).is_ok() {
        Some(TextWordDomain::ColorWord)
    } else if crate::types::ability::BasicLandType::from_str(leaf).is_ok() {
        Some(TextWordDomain::BasicLandType)
    } else {
        None
    }
}
