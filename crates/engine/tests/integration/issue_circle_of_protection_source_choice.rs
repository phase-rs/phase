//! Circle of Protection / Rune of Protection cycles — "a <color/type> source of
//! your choice ... prevent that damage" must parse into a `PreventDamage` effect
//! whose `damage_source_filter` is a QUALIFIED `ChosenDamageSource` (retaining
//! the color/type qualifier), not a bare/unconstrained shield. Verifies the real
//! production parser path against verbatim Scryfall Oracle text.

use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityKind, Effect, FilterProp, TargetFilter, TypeFilter};
use engine::types::keywords::Keyword;
use engine::types::mana::ManaColor;

// Verbatim Scryfall Oracle text (fetched 2026-07).
const COP_RED: &str =
    "{1}: The next time a red source of your choice would deal damage to you this turn, prevent that damage.";
const RUNE_LANDS: &str = "{W}: The next time a land source of your choice would deal damage to you this turn, prevent that damage.\nCycling {2} ({2}, Discard this card: Draw a card.)";

/// Extract the single `PreventDamage` effect from a parsed card.
fn prevent_damage_source_filter(parsed: &engine::parser::oracle::ParsedAbilities) -> TargetFilter {
    parsed
        .abilities
        .iter()
        .find_map(|a| match a.effect.as_ref() {
            Effect::PreventDamage {
                damage_source_filter: Some(f),
                ..
            } => Some(f.clone()),
            _ => None,
        })
        .expect("card must have a PreventDamage ability with a source filter")
}

#[test]
fn circle_of_protection_red_parses_qualified_chosen_source() {
    let parsed = parse_oracle_text(
        COP_RED,
        "Circle of Protection: Red",
        &[],
        &["Enchantment".to_string()],
        &[],
    );

    // The prevention lives on an activated ability.
    assert!(
        parsed
            .abilities
            .iter()
            .any(|a| matches!(a.kind, AbilityKind::Activated)),
        "Circle of Protection: Red's prevention must be an activated ability"
    );

    match prevent_damage_source_filter(&parsed) {
        TargetFilter::ChosenDamageSource {
            filter: Some(inner),
        } => match *inner {
            TargetFilter::Typed(tf) => assert!(
                tf.properties.iter().any(|p| matches!(
                    p,
                    FilterProp::HasColor {
                        color: ManaColor::Red
                    }
                )),
                "qualifier must constrain to red sources, got {tf:?}"
            ),
            other => panic!("expected Typed red qualifier, got {other:?}"),
        },
        other => panic!("expected qualified ChosenDamageSource, got {other:?}"),
    }
}

#[test]
fn rune_of_protection_lands_parses_type_qualifier_and_keeps_cycling_separate() {
    let parsed = parse_oracle_text(
        RUNE_LANDS,
        "Rune of Protection: Lands",
        &["Cycling".to_string()],
        &["Enchantment".to_string()],
        &[],
    );

    match prevent_damage_source_filter(&parsed) {
        TargetFilter::ChosenDamageSource {
            filter: Some(inner),
        } => match *inner {
            TargetFilter::Typed(tf) => assert!(
                tf.type_filters.contains(&TypeFilter::Land),
                "qualifier must constrain to Land sources, got {tf:?}"
            ),
            other => panic!("expected Typed Land qualifier, got {other:?}"),
        },
        other => panic!("expected qualified ChosenDamageSource, got {other:?}"),
    }

    // Cycling is a keyword-only line: the parser decomposes it into
    // `extracted_keywords` (as `Keyword::Cycling`), NOT a second entry in
    // `abilities`. Assert it is present there, independent of the prevention
    // clause — the prevention line is not swallowed and Cycling is not lost.
    assert!(
        parsed
            .extracted_keywords
            .iter()
            .any(|kw| matches!(kw, Keyword::Cycling(_))),
        "Rune of Protection: Lands must extract Cycling as a keyword alongside its prevention ability, got keywords: {:?}",
        parsed.extracted_keywords,
    );
}
