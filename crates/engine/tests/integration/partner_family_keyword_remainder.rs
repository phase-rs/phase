//! phase-rs/phase#9998: the Partner-family arm of the shared keyword-line core
//! (`parse_keyword_line_core`) must report its REAL nom remainder.
//!
//! CR 702.124i: "Partner—[text]" (Character select, Friends forever).
//! CR 702.124k: "Choose a Background". CR 702.124m: "Doctor's companion".
//!
//! The arm used to bind `Ok((_, result))` and return `(keyword, "")`, so the strict
//! keyword router (`parse_router_keyword_list`, permanent priority 1b of the Oracle
//! pipeline) believed the whole line was the keyword declaration and consumed it,
//! silently dropping any trailing prose with no keyword diagnostic. After the fix the
//! router declines a line with trailing prose, and the line falls through to ordinary
//! parsing and becomes an honest `Effect::Unimplemented` carrying the line text.
//!
//! These are SHAPE tests: CR 702.124a says Partner abilities "function before the
//! game begins" (deck construction), and the typed keyword reaches the card from
//! MTGJSON regardless of the line parse, so there is no in-game delta to resolve.
//! They assert through the parse output (`parse_oracle_text`), each hostile case
//! paired with a verbatim-card control proving the input reached the keyword router.

use engine::parser::oracle::{parse_oracle_text, ParsedAbilities};
use engine::types::ability::Effect;
use engine::types::ability_visit::{
    visit_ability_def, visit_replacement, visit_static, visit_trigger,
};
use engine::types::keywords::{Keyword, PartnerType};
use std::ops::ControlFlow;

/// Alora, Merry Thief (MTGJSON `text`, verbatim).
const ALORA_FIRST_LINE: &str = "Whenever you attack, up to one target attacking creature \
     can't be blocked this turn. Return that creature to its owner's hand at the beginning \
     of the next end step.";
const ALORA_CHOOSE_A_BACKGROUND: &str =
    "Choose a Background (You can have a Background as a second commander.)";

/// Dan Lewis (MTGJSON `text`, verbatim).
const DAN_LEWIS_FIRST_LINE: &str = "Noncreature, non-Equipment artifacts you control are \
     Equipment in addition to their other types and have \"Equipped creature gets +1/+0\" \
     and equip {1}.";
const DAN_LEWIS_DOCTORS_COMPANION: &str =
    "Doctor's companion (You can have two commanders if the other is the Doctor.)";

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

/// Every `Effect::Unimplemented` description anywhere in the parse output.
fn unimplemented_descriptions(parsed: &ParsedAbilities) -> Vec<String> {
    let mut descriptions = Vec::new();
    let mut visitor = |effect: &Effect| {
        if let Some(description) = effect.unimplemented_description() {
            descriptions.push(description.to_string());
        }
        ControlFlow::<()>::Continue(())
    };
    for def in &parsed.abilities {
        let _ = visit_ability_def(def, &mut visitor);
    }
    for trigger in &parsed.triggers {
        let _ = visit_trigger(trigger, &mut visitor);
    }
    for static_def in &parsed.statics {
        let _ = visit_static(static_def, &mut visitor);
    }
    for replacement in &parsed.replacements {
        let _ = visit_replacement(replacement, &mut visitor);
    }
    descriptions
}

fn mentions(descriptions: &[String], needle: &str) -> bool {
    descriptions
        .iter()
        .any(|description| description.to_lowercase().contains(needle))
}

/// Asserts the control/hostile pair for one Partner-family declaration.
///
/// `declaration_needle` is the lowercase phrase that would appear in an
/// `Unimplemented` description if the declaration itself were left unparsed.
fn assert_trailing_prose_is_not_swallowed(
    first_line: &str,
    declaration: &str,
    hostile_declaration: &str,
    declaration_needle: &str,
    mtgjson_keywords: &[&str],
    subtypes: &[&str],
    variant: PartnerType,
) {
    let keywords = strings(mtgjson_keywords);
    let types = strings(&["Creature"]);
    let subtypes = strings(subtypes);

    // Control (reach guard): the verbatim card's declaration is a complete keyword
    // line, so nothing about it is left Unimplemented.
    let control = parse_oracle_text(
        &format!("{first_line}\n{declaration}"),
        "Control",
        &keywords,
        &types,
        &subtypes,
    );
    assert!(
        !mentions(&unimplemented_descriptions(&control), declaration_needle),
        "control: the verbatim declaration must not be left Unimplemented: {control:#?}"
    );

    // Hostile: the same card with trailing semantic prose after the keyword.
    let hostile = parse_oracle_text(
        &format!("{first_line}\n{hostile_declaration}"),
        "Hostile",
        &keywords,
        &types,
        &subtypes,
    );
    let descriptions = unimplemented_descriptions(&hostile);
    assert!(
        mentions(&descriptions, "xyzzy"),
        "the trailing prose must surface as an Unimplemented gap, got {descriptions:?} in \
         {hostile:#?}"
    );
    // Negative, paired with the positive reach guard above: the router no longer
    // claims the line, so it emits no keyword for it.
    assert!(
        !hostile
            .extracted_keywords
            .contains(&Keyword::Partner(variant)),
        "a declaration with trailing prose must not be consumed as the keyword: {hostile:#?}"
    );
}

#[test]
fn choose_a_background_trailing_prose_is_not_swallowed() {
    assert_trailing_prose_is_not_swallowed(
        ALORA_FIRST_LINE,
        ALORA_CHOOSE_A_BACKGROUND,
        "Choose a Background xyzzy",
        "background",
        &["choose a background"],
        &["Halfling", "Rogue"],
        PartnerType::ChooseABackground,
    );
}

#[test]
fn doctors_companion_trailing_prose_is_not_swallowed() {
    assert_trailing_prose_is_not_swallowed(
        DAN_LEWIS_FIRST_LINE,
        DAN_LEWIS_DOCTORS_COMPANION,
        "Doctor's companion xyzzy",
        "companion",
        &["doctor's companion"],
        &["Human"],
        PartnerType::DoctorsCompanion,
    );
}
