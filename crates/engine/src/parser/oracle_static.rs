use std::str::FromStr;

use super::oracle_cost::parse_oracle_cost;
use super::oracle_effect::parse_effect_chain;
use super::oracle_target::parse_type_phrase;
use super::oracle_util::{parse_number, strip_reminder_text};
use crate::types::ability::{
    AbilityDefinition, AbilityKind, ChosenSubtypeKind, ContinuousModification, ControllerRef,
    DynamicPTValue, FilterProp, StaticCondition, StaticDefinition, TargetFilter, TypedFilter,
};
use crate::types::keywords::Keyword;
use crate::types::statics::StaticMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuleStaticPredicate {
    CantUntap,
    MustAttack,
    BlockOnlyCreaturesWithFlying,
    Shroud,
    MayLookAtTopOfLibrary,
    LoseAllAbilities,
    NoMaximumHandSize,
    MayPlayAdditionalLand,
}

/// Parse a static/continuous ability line into a StaticDefinition.
/// Handles: "Enchanted creature gets +N/+M", "has {keyword}",
/// "Creatures you control get +N/+M", etc.
pub fn parse_static_line(text: &str) -> Option<StaticDefinition> {
    let text = strip_reminder_text(text);
    let lower = text.to_lowercase();

    if lower.starts_with("you may choose not to untap ")
        && lower.contains(" during your untap step")
    {
        return Some(
            StaticDefinition::new(StaticMode::Other("MayChooseNotToUntap".to_string()))
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }

    // --- "Enchanted creature gets +N/+M" or "has {keyword}" ---
    if lower.starts_with("enchanted creature ") {
        if let Some(def) = parse_continuous_gets_has(
            &text[19..],
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EnchantedBy])),
        ) {
            return Some(def);
        }
    }

    // --- "Enchanted permanent gets/has ..." ---
    if lower.starts_with("enchanted permanent ") {
        if let Some(def) = parse_continuous_gets_has(
            &text[20..],
            TargetFilter::Typed(TypedFilter::permanent().properties(vec![FilterProp::EnchantedBy])),
        ) {
            return Some(def);
        }
    }

    if lower.starts_with("enchanted land ") {
        if let Some(def) = parse_continuous_gets_has(
            &text[15..],
            TargetFilter::Typed(TypedFilter::land().properties(vec![FilterProp::EnchantedBy])),
        ) {
            return Some(def);
        }
    }

    // --- "Equipped creature gets +N/+M" ---
    if lower.starts_with("equipped creature ") {
        if let Some(def) = parse_continuous_gets_has(
            &text[18..],
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EquippedBy])),
        ) {
            return Some(def);
        }
    }

    // --- "All creatures get/have ..." ---
    if lower.starts_with("all creatures ") {
        if let Some(def) =
            parse_continuous_gets_has(&text[14..], TargetFilter::Typed(TypedFilter::creature()))
        {
            return Some(def);
        }
    }

    // --- "Other [Subtype] creatures you control get/have..." ---
    // e.g. "Other Zombies you control get +1/+1"
    if let Some(rest) = lower.strip_prefix("other ") {
        if let Some(result) = parse_typed_you_control(&text[6..], rest, true) {
            return Some(result);
        }
    }

    // --- "[Subtype] creatures you control get/have..." ---
    // e.g. "Elf creatures you control get +1/+1"
    if let Some(result) = parse_typed_you_control(&text, &lower, false) {
        return Some(result);
    }

    // --- "Creatures you control get +N/+M" ---
    if lower.starts_with("creatures you control ") {
        if let Some(def) = parse_continuous_gets_has(
            &text[22..],
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
        ) {
            return Some(def);
        }
    }

    // --- "Other creatures you control get +N/+M" ---
    if lower.starts_with("other creatures you control ") {
        if let Some(def) = parse_continuous_gets_has(
            &text[28..],
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
        ) {
            return Some(def);
        }
    }

    if let Some(def) = parse_subject_continuous_static(&text) {
        return Some(def);
    }

    // --- "Lands you control have '[type]'" ---
    if lower.starts_with("lands you control have ") {
        let rest = text[23..]
            .trim()
            .trim_end_matches('.')
            .trim_matches(|c: char| c == '\'' || c == '"');
        return Some(
            StaticDefinition::continuous()
                .affected(TargetFilter::Typed(
                    TypedFilter::land().controller(ControllerRef::You),
                ))
                .modifications(vec![ContinuousModification::AddSubtype {
                    subtype: rest.to_string(),
                }])
                .description(text.to_string()),
        );
    }

    // --- "During your turn, [subject] has/gets ..." ---
    if let Some(rest) = lower.strip_prefix("during your turn, ") {
        let original_rest = &text["during your turn, ".len()..];
        if let Some(subject_end) = find_continuous_predicate_start(rest) {
            let subject = original_rest[..subject_end].trim();
            let predicate = original_rest[subject_end + 1..].trim();
            if let Some(affected) = parse_continuous_subject_filter(subject) {
                let modifications = parse_continuous_modifications(predicate);
                if !modifications.is_empty() {
                    return Some(
                        StaticDefinition::continuous()
                            .affected(affected)
                            .modifications(modifications)
                            .condition(StaticCondition::DuringYourTurn)
                            .description(text.to_string()),
                    );
                }
            }
        }
    }

    if let Some(def) = parse_subject_rule_static(&text) {
        return Some(def);
    }

    // --- "~ is the chosen type in addition to its other types" ---
    // Distinguish creature type (Metallic Mimic) vs basic land type (Multiversal Passage)
    if lower.contains("is the chosen type") {
        let kind = if lower.starts_with("this creature") || lower.contains("creature is the chosen")
        {
            ChosenSubtypeKind::CreatureType
        } else {
            ChosenSubtypeKind::BasicLandType
        };
        let modification = ContinuousModification::AddChosenSubtype { kind };
        return Some(
            StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .modifications(vec![modification])
                .description(text.to_string()),
        );
    }

    // --- CDA: "~'s power is equal to the number of card types among cards in all graveyards
    //     and its toughness is equal to that number plus 1" (Tarmogoyf) ---
    if let Some(def) = parse_cda_pt_equality(&lower, &text) {
        return Some(def);
    }

    if let Some(def) = parse_conditional_static(&text) {
        return Some(def);
    }

    // --- "~ has [keyword] as long as ..." (must be before generic self-ref "has") ---
    if let Some(has_pos) = lower.find(" has ") {
        if let Some(cond_pos) = lower.find(" as long as ") {
            if has_pos < cond_pos {
                let keyword_text = lower[has_pos + 5..cond_pos].trim();
                let condition_text = text[cond_pos + 12..].trim().trim_end_matches('.');
                let mut modifications = Vec::new();
                if let Some(kw) = map_keyword(keyword_text) {
                    modifications.push(ContinuousModification::AddKeyword { keyword: kw });
                }
                return Some(
                    StaticDefinition::continuous()
                        .affected(TargetFilter::SelfRef)
                        .modifications(modifications)
                        .condition(StaticCondition::CheckSVar {
                            var: "condition".to_string(),
                            compare: condition_text.to_string(),
                        })
                        .description(text.to_string()),
                );
            }
        }
    }

    // --- "~ has/gets ..." (self-referential) ---
    // Match lines like "CARDNAME has deathtouch" or "CARDNAME gets +1/+1"
    if let Some(pos) = lower
        .find(" has ")
        .or_else(|| lower.find(" gets "))
        .or_else(|| lower.find(" get "))
    {
        let verb_len = if lower[pos..].starts_with(" has ") {
            5
        } else if lower[pos..].starts_with(" gets ") {
            6
        } else {
            5 // " get "
        };
        let subject = &lower[..pos];
        // Only match if the subject doesn't look like a known prefix we handle elsewhere
        if !subject.contains("creature")
            && !subject.contains("permanent")
            && !subject.contains("land")
            && !subject.starts_with("all ")
            && !subject.starts_with("other ")
        {
            let after = &text[pos + verb_len..];
            return parse_continuous_gets_has(
                &format!(
                    "{}{}",
                    if lower[pos..].starts_with(" has ") {
                        "has "
                    } else {
                        "gets "
                    },
                    after
                ),
                TargetFilter::SelfRef,
            );
        }
    }

    // --- "~ can't be blocked" ---
    if lower.contains("can't be blocked") {
        return Some(
            StaticDefinition::new(StaticMode::Other("CantBeBlocked".to_string()))
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }

    // --- "~ can't block" ---
    if lower.contains("can't block") && !lower.contains("can't be blocked") {
        return Some(
            StaticDefinition::new(StaticMode::CantBlock)
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }

    // --- "~ can't attack" ---
    if lower.contains("can't attack") {
        return Some(
            StaticDefinition::new(StaticMode::CantAttack)
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }

    // --- "~ can't be countered" ---
    if lower.contains("can't be countered") {
        return Some(
            StaticDefinition::new(StaticMode::CantBeCast)
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }

    // --- "~ can't be the target" or "~ can't be targeted" ---
    if lower.contains("can't be the target") || lower.contains("can't be targeted") {
        return Some(
            StaticDefinition::new(StaticMode::CantBeTargeted)
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }

    // --- "~ can't be sacrificed" ---
    if lower.contains("can't be sacrificed") {
        return Some(
            StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }

    // --- "~ doesn't untap during your untap step" ---
    if lower.contains("doesn't untap during") || lower.contains("doesn\u{2019}t untap during") {
        return Some(
            StaticDefinition::new(StaticMode::Other("CantUntap".to_string()))
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }

    // --- "You may look at the top card of your library any time." ---
    if lower.starts_with("you may look at the top card of your library") {
        return Some(
            StaticDefinition::new(StaticMode::Other("MayLookAtTopOfLibrary".to_string()))
                .affected(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::You),
                ))
                .description(text.to_string()),
        );
    }

    // NOTE: "enters with N counters" patterns are now handled by oracle_replacement.rs
    // as proper Moved replacement effects (paralleling the "enters tapped" pattern).

    // --- "Spells you cast cost {N} less" ---
    if lower.contains("cost") && lower.contains("less") && lower.contains("spell") {
        return Some(
            StaticDefinition::new(StaticMode::ReduceCost)
                .affected(TargetFilter::Typed(
                    TypedFilter::card().controller(ControllerRef::You),
                ))
                .description(text.to_string()),
        );
    }

    // --- "Spells your opponents cast cost {N} more" ---
    if lower.contains("cost")
        && lower.contains("more")
        && lower.contains("spell")
        && lower.contains("opponent")
    {
        return Some(
            StaticDefinition::new(StaticMode::RaiseCost)
                .affected(TargetFilter::Typed(
                    TypedFilter::card().controller(ControllerRef::Opponent),
                ))
                .description(text.to_string()),
        );
    }

    // --- "As long as ..." (generic conditional static) ---
    if lower.starts_with("as long as ") {
        return Some(
            StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .condition(StaticCondition::CheckSVar {
                    var: "condition".to_string(),
                    compare: text.trim_end_matches('.').to_string(),
                })
                .description(text.to_string()),
        );
    }

    None
}

/// Try to parse "[Subtype] creatures you control get/have ..." patterns.
/// `text` is the original-case text starting at the subtype word.
/// `lower` is the lowercased version of `text`.
/// `is_other` indicates whether this was preceded by "Other ".
fn parse_typed_you_control(text: &str, lower: &str, is_other: bool) -> Option<StaticDefinition> {
    // Try "X creatures you control get/have" first
    if let Some(creatures_pos) = lower.find(" creatures you control ") {
        let descriptor = text[..creatures_pos].trim();
        if !descriptor.is_empty() {
            let after_prefix = &text[creatures_pos + 23..];
            let full_subject = text[..creatures_pos + " creatures you control".len()].trim();
            let typed_filter =
                if let Some(filter) = parse_modified_creature_subject_filter(full_subject) {
                    filter
                } else if let Some(color) = parse_named_color(descriptor) {
                    TargetFilter::Typed(
                        TypedFilter::creature()
                            .controller(ControllerRef::You)
                            .properties(vec![FilterProp::HasColor {
                                color: color.to_string(),
                            }]),
                    )
                } else if is_capitalized_words(descriptor) {
                    TargetFilter::Typed(
                        TypedFilter::creature()
                            .subtype(descriptor.to_string())
                            .controller(ControllerRef::You),
                    )
                } else {
                    return None;
                };
            let typed_filter = if is_other {
                add_another_filter(typed_filter)
            } else {
                typed_filter
            };
            return parse_continuous_gets_has(after_prefix, typed_filter);
        }
    }

    // Try "Xs you control get/have" (e.g. "Zombies you control get +1/+1")
    if let Some(yc_pos) = lower.find(" you control ") {
        let descriptor = text[..yc_pos].trim();
        if !descriptor.is_empty() {
            let after_prefix = &text[yc_pos + 13..];
            let full_subject = text[..yc_pos + " you control".len()].trim();
            let typed_filter =
                if let Some(filter) = parse_modified_creature_subject_filter(full_subject) {
                    filter
                } else if let Some(color) = parse_named_color(descriptor) {
                    TargetFilter::Typed(
                        TypedFilter::creature()
                            .controller(ControllerRef::You)
                            .properties(vec![FilterProp::HasColor {
                                color: color.to_string(),
                            }]),
                    )
                } else if is_capitalized_words(descriptor) {
                    // Strip trailing 's' for the subtype name (Zombies -> Zombie)
                    let subtype_name = descriptor.trim_end_matches('s').to_string();
                    TargetFilter::Typed(
                        TypedFilter::creature()
                            .subtype(subtype_name)
                            .controller(ControllerRef::You),
                    )
                } else {
                    return None;
                };
            let typed_filter = if is_other {
                add_another_filter(typed_filter)
            } else {
                typed_filter
            };
            return parse_continuous_gets_has(after_prefix, typed_filter);
        }
    }

    None
}

fn parse_subject_rule_static(text: &str) -> Option<StaticDefinition> {
    let lower = text.to_lowercase();
    let (affected, predicate_text) = strip_rule_static_subject(text, &lower)?;
    let predicate = parse_rule_static_predicate(predicate_text)?;
    Some(lower_rule_static(predicate, affected, text))
}

fn parse_subject_continuous_static(text: &str) -> Option<StaticDefinition> {
    let lower = text.to_lowercase();

    let subject_end = find_continuous_predicate_start(&lower)?;
    let subject = text[..subject_end].trim();
    let predicate = text[subject_end + 1..].trim();
    if parse_rule_static_predicate(predicate).is_some() {
        return None;
    }
    let affected = parse_continuous_subject_filter(subject)?;
    let modifications = parse_continuous_modifications(predicate);
    if !modifications.is_empty() {
        return Some(
            StaticDefinition::continuous()
                .affected(affected)
                .modifications(modifications)
                .description(text.to_string()),
        );
    }

    None
}

fn parse_conditional_static(text: &str) -> Option<StaticDefinition> {
    let conditional = text.strip_prefix("As long as ")?;
    let (condition_text, remainder) = conditional.split_once(", ")?;
    let condition = parse_static_condition(condition_text)?;
    let mut def = parse_static_line(remainder.trim())?;
    if def.condition.is_some() {
        return None;
    }
    def.condition = Some(condition);
    def.description = Some(text.to_string());
    Some(def)
}

fn parse_static_condition(text: &str) -> Option<StaticCondition> {
    let lower = text.to_lowercase();
    let amount_text = lower
        .strip_prefix("you have at least ")?
        .strip_suffix(" life more than your starting life total")?;
    let (amount, rest) = parse_number(amount_text)?;
    if !rest.trim().is_empty() {
        return None;
    }
    Some(StaticCondition::LifeMoreThanStartingBy {
        amount: amount as i32,
    })
}

fn find_continuous_predicate_start(lower: &str) -> Option<usize> {
    [
        " gets ", " get ", " gains ", " gain ", " has ", " have ", " loses ", " lose ",
    ]
    .into_iter()
    .filter_map(|marker| lower.find(marker))
    .min()
}

fn parse_continuous_subject_filter(subject: &str) -> Option<TargetFilter> {
    let trimmed = subject.trim();
    let lower = trimmed.to_lowercase();

    if lower.starts_with("other ") {
        let original_rest = trimmed[6..].trim();
        return parse_continuous_subject_filter(original_rest).map(add_another_filter);
    }

    if let Some(filter) = parse_modified_creature_subject_filter(trimmed) {
        return Some(filter);
    }

    if let Some(filter) = parse_creature_subject_filter(trimmed) {
        return Some(filter);
    }

    parse_rule_static_subject_filter(trimmed)
}

fn parse_modified_creature_subject_filter(subject: &str) -> Option<TargetFilter> {
    let lower = subject.to_lowercase();
    if lower == "equipped creature" {
        return Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::EquippedBy]),
        ));
    }

    let controlled_patterns = [
        ("tapped creatures you control", FilterProp::Tapped),
        ("attacking creatures you control", FilterProp::Attacking),
        ("equipped creatures you control", FilterProp::EquippedBy),
    ];

    for (pattern, property) in controlled_patterns {
        if lower == pattern {
            return Some(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![property]),
            ));
        }
    }

    if lower == "attacking creatures" {
        return Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::Attacking]),
        ));
    }

    None
}

fn parse_creature_subject_filter(subject: &str) -> Option<TargetFilter> {
    let trimmed = subject.trim();
    let lower = trimmed.to_lowercase();

    let descriptor = if let Some(prefix) = trimmed.strip_suffix(" creatures") {
        prefix.trim()
    } else if !trimmed.contains(' ') && lower.ends_with('s') {
        trimmed.trim_end_matches('s').trim()
    } else {
        return None;
    };

    if descriptor.is_empty() {
        return None;
    }

    if let Some(color) = parse_named_color(descriptor) {
        return Some(TargetFilter::Typed(TypedFilter::creature().properties(
            vec![FilterProp::HasColor {
                color: color.to_string(),
            }],
        )));
    }

    if is_capitalized_words(descriptor) {
        let subtype = descriptor.to_string();
        return Some(TargetFilter::Typed(
            TypedFilter::creature().subtype(subtype),
        ));
    }

    None
}

fn add_another_filter(filter: TargetFilter) -> TargetFilter {
    match filter {
        TargetFilter::Typed(mut typed) => {
            typed.properties.push(FilterProp::Another);
            TargetFilter::Typed(typed)
        }
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters.into_iter().map(add_another_filter).collect(),
        },
        other => TargetFilter::And {
            filters: vec![
                other,
                TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Another])),
            ],
        },
    }
}

fn strip_rule_static_subject<'a>(text: &'a str, lower: &str) -> Option<(TargetFilter, &'a str)> {
    for marker in [
        " doesn't untap during ",
        " doesn’t untap during ",
        " don't untap during ",
        " don’t untap during ",
        " attacks each combat if able",
        " can block only creatures with flying",
        " has shroud",
        " have shroud",
        " has no maximum hand size",
        " have no maximum hand size",
        " may play an additional land",
        " may play up to ",
        " may look at the top card of your library",
        " loses all abilities",
        " lose all abilities",
    ] {
        let Some(subject_end) = lower.find(marker) else {
            continue;
        };
        let subject = text[..subject_end].trim();
        let predicate = text[subject_end + 1..].trim();
        let affected = parse_rule_static_subject_filter(subject)?;
        return Some((affected, predicate));
    }

    None
}

fn parse_rule_static_subject_filter(subject: &str) -> Option<TargetFilter> {
    let lower = subject.to_lowercase();

    if matches!(
        lower.as_str(),
        "~" | "this"
            | "it"
            | "this card"
            | "this creature"
            | "this permanent"
            | "this artifact"
            | "this land"
    ) {
        return Some(TargetFilter::SelfRef);
    }

    if lower == "you" {
        return Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::You),
        ));
    }

    if matches!(lower.as_str(), "players" | "each player") {
        return Some(TargetFilter::Player);
    }

    if lower == "enchanted creature" {
        return Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
        ));
    }

    if lower == "enchanted permanent" {
        return Some(TargetFilter::Typed(
            TypedFilter::permanent().properties(vec![FilterProp::EnchantedBy]),
        ));
    }

    if lower == "equipped creature" {
        return Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::EquippedBy]),
        ));
    }

    let (filter, rest) = parse_type_phrase(subject);
    if rest.trim().is_empty() {
        return Some(filter);
    }

    None
}

fn parse_rule_static_predicate(text: &str) -> Option<RuleStaticPredicate> {
    let lower = text.to_lowercase();

    if lower.starts_with("doesn't untap during")
        || lower.starts_with("doesn\u{2019}t untap during")
        || lower.starts_with("don't untap during")
        || lower.starts_with("don\u{2019}t untap during")
    {
        return Some(RuleStaticPredicate::CantUntap);
    }

    if matches!(
        lower.as_str(),
        "attacks each combat if able" | "attacks each combat if able."
    ) {
        return Some(RuleStaticPredicate::MustAttack);
    }

    if matches!(
        lower.as_str(),
        "can block only creatures with flying" | "can block only creatures with flying."
    ) {
        return Some(RuleStaticPredicate::BlockOnlyCreaturesWithFlying);
    }

    if matches!(
        lower.as_str(),
        "has shroud" | "has shroud." | "have shroud" | "have shroud."
    ) {
        return Some(RuleStaticPredicate::Shroud);
    }

    if lower.starts_with("may look at the top card of your library") {
        return Some(RuleStaticPredicate::MayLookAtTopOfLibrary);
    }

    if matches!(
        lower.as_str(),
        "lose all abilities"
            | "lose all abilities."
            | "loses all abilities"
            | "loses all abilities."
    ) {
        return Some(RuleStaticPredicate::LoseAllAbilities);
    }

    if matches!(
        lower.as_str(),
        "has no maximum hand size"
            | "has no maximum hand size."
            | "have no maximum hand size"
            | "have no maximum hand size."
    ) {
        return Some(RuleStaticPredicate::NoMaximumHandSize);
    }

    if lower.starts_with("may play an additional land")
        || (lower.starts_with("may play up to ") && lower.contains("additional land"))
    {
        return Some(RuleStaticPredicate::MayPlayAdditionalLand);
    }

    None
}

fn lower_rule_static(
    predicate: RuleStaticPredicate,
    affected: TargetFilter,
    description: &str,
) -> StaticDefinition {
    match predicate {
        RuleStaticPredicate::CantUntap => {
            StaticDefinition::new(StaticMode::Other("CantUntap".to_string()))
                .affected(affected)
                .description(description.to_string())
        }
        RuleStaticPredicate::MustAttack => StaticDefinition::new(StaticMode::MustAttack)
            .affected(affected)
            .description(description.to_string()),
        RuleStaticPredicate::BlockOnlyCreaturesWithFlying => {
            StaticDefinition::new(StaticMode::Other("BlockRestriction".to_string()))
                .affected(affected)
                .description(description.to_string())
        }
        RuleStaticPredicate::Shroud => {
            StaticDefinition::new(StaticMode::Other("Shroud".to_string()))
                .affected(affected)
                .description(description.to_string())
        }
        RuleStaticPredicate::MayLookAtTopOfLibrary => {
            StaticDefinition::new(StaticMode::Other("MayLookAtTopOfLibrary".to_string()))
                .affected(affected)
                .description(description.to_string())
        }
        RuleStaticPredicate::LoseAllAbilities => StaticDefinition::continuous()
            .affected(affected)
            .modifications(vec![ContinuousModification::RemoveAllAbilities])
            .description(description.to_string()),
        RuleStaticPredicate::NoMaximumHandSize => {
            StaticDefinition::new(StaticMode::Other("NoMaximumHandSize".to_string()))
                .affected(affected)
                .description(description.to_string())
        }
        RuleStaticPredicate::MayPlayAdditionalLand => {
            StaticDefinition::new(StaticMode::Other("MayPlayAdditionalLand".to_string()))
                .affected(affected)
                .description(description.to_string())
        }
    }
}

fn parse_named_color(text: &str) -> Option<&'static str> {
    match text.trim().to_ascii_lowercase().as_str() {
        "white" => Some("White"),
        "blue" => Some("Blue"),
        "black" => Some("Black"),
        "red" => Some("Red"),
        "green" => Some("Green"),
        _ => None,
    }
}

/// Check that a string is one or more capitalized words.
fn is_capitalized_words(s: &str) -> bool {
    let trimmed = s.trim();
    !trimmed.is_empty()
        && trimmed
            .split_whitespace()
            .all(|w| w.chars().next().is_some_and(|c| c.is_uppercase()))
}

/// Parse "gets +N/+M [and has {keyword}]" after the subject.
fn parse_continuous_gets_has(text: &str, affected: TargetFilter) -> Option<StaticDefinition> {
    let modifications = parse_continuous_modifications(text);

    if modifications.is_empty() {
        return None;
    }

    Some(
        StaticDefinition::continuous()
            .affected(affected)
            .modifications(modifications),
    )
}

pub(crate) fn parse_continuous_modifications(text: &str) -> Vec<ContinuousModification> {
    let lower = text.to_lowercase();
    let mut modifications = Vec::new();

    if lower.contains("lose all abilities") {
        modifications.push(ContinuousModification::RemoveAllAbilities);
    }

    if lower.starts_with("gets ") || lower.starts_with("get ") {
        let offset = if lower.starts_with("gets ") { 5 } else { 4 };
        let after = &text[offset..].trim();
        if let Some((p, t)) = parse_pt_mod(after) {
            modifications.push(ContinuousModification::AddPower { value: p });
            modifications.push(ContinuousModification::AddToughness { value: t });
        }
    }

    if let Some((power, toughness)) = parse_base_pt_mod(text) {
        modifications.push(ContinuousModification::SetPower { value: power });
        modifications.push(ContinuousModification::SetToughness { value: toughness });
    }
    if let Some(power) = parse_base_power_mod(text) {
        modifications.push(ContinuousModification::SetPower { value: power });
    }
    if let Some(toughness) = parse_base_toughness_mod(text) {
        modifications.push(ContinuousModification::SetToughness { value: toughness });
    }

    for definition in parse_quoted_abilities(text) {
        modifications.push(ContinuousModification::GrantAbility { definition: Box::new(definition) });
    }

    if let Some(keyword_text) = extract_keyword_clause(text) {
        for part in split_keyword_list(keyword_text.trim().trim_end_matches('.')) {
            if let Some(kw) = map_keyword(part.trim().trim_end_matches('.')) {
                modifications.push(ContinuousModification::AddKeyword { keyword: kw });
            }
        }
    }

    modifications
}

fn parse_base_pt_mod(text: &str) -> Option<(i32, i32)> {
    let lower = text.to_lowercase();
    let pos = lower.find("base power and toughness ")?;
    let pt_text = text[pos + "base power and toughness ".len()..].trim();
    parse_pt_mod(pt_text)
}

fn parse_base_power_mod(text: &str) -> Option<i32> {
    let lower = text.to_lowercase();
    if lower.contains("base power and toughness ") {
        return None;
    }
    let pos = lower.find("base power ")?;
    let power_text = text[pos + "base power ".len()..].trim();
    parse_single_pt_value(power_text)
}

fn parse_base_toughness_mod(text: &str) -> Option<i32> {
    let lower = text.to_lowercase();
    if lower.contains("base power and toughness ") {
        return None;
    }
    let pos = lower.find("base toughness ")?;
    let toughness_text = text[pos + "base toughness ".len()..].trim();
    parse_single_pt_value(toughness_text)
}

fn parse_single_pt_value(text: &str) -> Option<i32> {
    let value = text
        .split(|c: char| c.is_whitespace() || matches!(c, '.' | ','))
        .next()?;
    value.replace('+', "").parse::<i32>().ok()
}

/// Extract quoted ability text from Oracle text and parse each into a typed AbilityDefinition.
///
/// Quoted abilities like `"{T}: Add two mana of any one color."` are parsed by splitting
/// at the cost separator (`:` after mana/tap symbols) and reusing `parse_oracle_cost` +
/// `parse_effect_chain`. Non-activated quoted text is parsed as a spell-like effect chain.
fn parse_quoted_abilities(text: &str) -> Vec<AbilityDefinition> {
    let mut definitions = Vec::new();
    let mut start = None;

    for (idx, ch) in text.char_indices() {
        if ch == '"' {
            if let Some(open) = start.take() {
                let ability_text = text[open + 1..idx].trim();
                if !ability_text.is_empty() {
                    definitions.push(parse_quoted_ability(ability_text));
                }
            } else {
                start = Some(idx);
            }
        }
    }

    definitions
}

/// Parse a single quoted ability string into a typed AbilityDefinition.
///
/// If the text contains a cost separator (e.g., `{T}: ...`), it's treated as an
/// activated ability with the cost parsed separately. Otherwise it's treated as
/// a spell-like effect.
fn parse_quoted_ability(text: &str) -> AbilityDefinition {
    // Find the cost/effect separator — look for ": " after a cost-like prefix
    // (mana symbols, {T}, loyalty, etc.)
    if let Some(colon_pos) = find_cost_separator(text) {
        let cost_text = text[..colon_pos].trim();
        let effect_text = text[colon_pos + 1..].trim();
        let cost = parse_oracle_cost(cost_text);
        let mut def = parse_effect_chain(effect_text, AbilityKind::Activated);
        def.cost = Some(cost);
        def.description = Some(text.to_string());
        def
    } else {
        // No cost separator — treat as spell-like ability text
        let mut def = parse_effect_chain(text, AbilityKind::Spell);
        def.description = Some(text.to_string());
        def
    }
}

/// Find the position of the cost/effect separator colon in ability text.
///
/// Looks for `: ` or `:\n` that appears after cost-like content (mana symbols,
/// {T}, numeric loyalty). Returns the byte offset of the colon, or None.
fn find_cost_separator(text: &str) -> Option<usize> {
    // Walk through looking for ':' that follows a closing brace or known cost prefix
    for (idx, ch) in text.char_indices() {
        if ch == ':' && idx > 0 {
            let prefix = &text[..idx];
            // Must have cost-like content before the colon
            let has_cost = prefix.contains('{')
                || prefix.trim().parse::<i32>().is_ok()
                || prefix.trim().starts_with('+')
                || prefix.trim().starts_with('\u{2212}'); // minus sign for loyalty
            if has_cost {
                return Some(idx);
            }
        }
    }
    None
}

/// Split a keyword list like "flying and trample" or "flying, trample, and haste".
fn split_keyword_list(text: &str) -> Vec<&str> {
    let text = text.trim().trim_end_matches('.');
    // Split on ", and ", " and ", or ", "
    let mut parts: Vec<&str> = Vec::new();
    for chunk in text.split(", and ") {
        for sub in chunk.split(" and ") {
            for item in sub.split(", ") {
                let trimmed = item.trim();
                if !trimmed.is_empty() {
                    parts.push(trimmed);
                }
            }
        }
    }
    parts
}

fn extract_keyword_clause(text: &str) -> Option<&str> {
    let lower = text.to_lowercase();

    for needle in [
        " and gains ",
        " and gain ",
        " and has ",
        " and have ",
        " gains ",
        " gain ",
        " has ",
        " have ",
    ] {
        if let Some(pos) = lower.find(needle) {
            return Some(&text[pos + needle.len()..]);
        }
    }

    for prefix in ["gains ", "gain ", "has ", "have "] {
        if lower.starts_with(prefix) {
            return Some(&text[prefix.len()..]);
        }
    }

    None
}

fn parse_pt_mod(text: &str) -> Option<(i32, i32)> {
    let text = text.trim();
    let slash = text.find('/')?;
    let p_str = &text[..slash];
    let rest = &text[slash + 1..];
    let t_end = rest
        .find(|c: char| c.is_whitespace() || c == '.' || c == ',')
        .unwrap_or(rest.len());
    let t_str = &rest[..t_end];
    let p = p_str.replace('+', "").parse::<i32>().ok()?;
    let t = t_str.replace('+', "").parse::<i32>().ok()?;
    Some((p, t))
}

/// Map a keyword text to a Keyword enum variant using the FromStr impl.
/// Returns None only for `Keyword::Unknown`.
fn map_keyword(text: &str) -> Option<Keyword> {
    let word = text.trim().trim_end_matches('.').trim();
    if word.is_empty() {
        return None;
    }
    if let Some(keyword) = parse_landwalk_keyword(word) {
        return Some(keyword);
    }
    match Keyword::from_str(word) {
        Ok(Keyword::Unknown(_)) => None,
        Ok(kw) => Some(kw),
        Err(_) => None, // Infallible, but satisfy the compiler
    }
}

fn parse_landwalk_keyword(text: &str) -> Option<Keyword> {
    match text.trim().to_ascii_lowercase().as_str() {
        "plainswalk" => Some(Keyword::Landwalk("Plains".to_string())),
        "islandwalk" => Some(Keyword::Landwalk("Island".to_string())),
        "swampwalk" => Some(Keyword::Landwalk("Swamp".to_string())),
        "mountainwalk" => Some(Keyword::Landwalk("Mountain".to_string())),
        "forestwalk" => Some(Keyword::Landwalk("Forest".to_string())),
        _ => None,
    }
}

/// Parse CDA power/toughness equality patterns like:
/// - "~'s power is equal to the number of card types among cards in all graveyards
///   and its toughness is equal to that number plus 1."
fn parse_cda_pt_equality(lower: &str, text: &str) -> Option<StaticDefinition> {
    // Match "power is equal to the number of card types among cards in all graveyards"
    if !lower.contains("power is equal to")
        && !lower.contains("power and toughness are each equal to")
    {
        return None;
    }

    if lower.contains("card types among cards in all graveyards") {
        let mut modifications = vec![ContinuousModification::SetDynamicPower {
            value: DynamicPTValue::CardTypesInAllGraveyards { offset: 0 },
        }];

        // Check for "and its toughness is equal to that number plus N"
        if let Some(plus_pos) = lower.find("that number plus ") {
            let after_plus = &lower[plus_pos + 17..];
            let n_str = after_plus
                .split(|c: char| !c.is_ascii_digit())
                .next()
                .unwrap_or("0");
            let offset = n_str.parse::<i32>().unwrap_or(0);
            modifications.push(ContinuousModification::SetDynamicToughness {
                value: DynamicPTValue::CardTypesInAllGraveyards { offset },
            });
        } else if lower.contains("power and toughness are each equal to") {
            // Same value for both
            modifications.push(ContinuousModification::SetDynamicToughness {
                value: DynamicPTValue::CardTypesInAllGraveyards { offset: 0 },
            });
        }

        return Some(
            StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .modifications(modifications)
                .cda()
                .description(text.to_string()),
        );
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::TypeFilter;

    #[test]
    fn static_bonesplitter() {
        let def = parse_static_line("Equipped creature gets +2/+0.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddPower { value: 2 }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddToughness { value: 0 }));
    }

    #[test]
    fn static_rancor() {
        let def = parse_static_line("Enchanted creature gets +2/+0 and has trample.").unwrap();
        assert!(def.modifications.len() >= 3); // +2, +0, trample
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Trample
            }));
    }

    #[test]
    fn static_cant_be_blocked() {
        let def =
            parse_static_line("Questing Beast can't be blocked by creatures with power 2 or less.")
                .unwrap();
        assert_eq!(def.mode, StaticMode::Other("CantBeBlocked".to_string()));
    }

    #[test]
    fn static_creatures_you_control() {
        let def = parse_static_line("Creatures you control get +1/+1.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(matches!(
            def.affected,
            Some(TargetFilter::Typed(TypedFilter {
                controller: Some(ControllerRef::You),
                ..
            }))
        ));
    }

    // --- New pattern tests ---

    #[test]
    fn static_self_referential_has_keyword() {
        let def = parse_static_line("Phage the Untouchable has deathtouch.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Deathtouch,
            }));
    }

    #[test]
    fn static_enchanted_permanent() {
        let def = parse_static_line("Enchanted permanent has hexproof.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(matches!(
            def.affected,
            Some(TargetFilter::Typed(TypedFilter {
                card_type: Some(TypeFilter::Permanent),
                ..
            }))
        ));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Hexproof,
            }));
    }

    #[test]
    fn static_all_creatures() {
        let def = parse_static_line("All creatures get +1/+1.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(matches!(
            def.affected,
            Some(TargetFilter::Typed(TypedFilter {
                card_type: Some(TypeFilter::Creature),
                controller: None,
                ..
            }))
        ));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddPower { value: 1 }));
    }

    #[test]
    fn static_subtype_creatures_you_control() {
        let def = parse_static_line("Elf creatures you control get +1/+1.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(matches!(
            def.affected,
            Some(TargetFilter::Typed(TypedFilter {
                card_type: Some(TypeFilter::Creature),
                subtype: Some(ref s),
                controller: Some(ControllerRef::You),
                ..
            })) if s == "Elf"
        ));
    }

    #[test]
    fn static_color_creatures_you_control() {
        let def = parse_static_line("White creatures you control get +1/+1.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(matches!(
            def.affected,
            Some(TargetFilter::Typed(TypedFilter {
                card_type: Some(TypeFilter::Creature),
                subtype: None,
                controller: Some(ControllerRef::You),
                properties,
            })) if properties == vec![FilterProp::HasColor {
                color: "White".to_string(),
            }]
        ));
    }

    #[test]
    fn static_other_subtype_you_control() {
        let def = parse_static_line("Other Zombies you control get +1/+1.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
    }

    #[test]
    fn static_cant_block() {
        let def = parse_static_line("Ragavan can't block.").unwrap();
        assert_eq!(def.mode, StaticMode::CantBlock);
        assert!(def.modifications.is_empty());
        assert!(def.description.is_some());
    }

    #[test]
    fn static_doesnt_untap() {
        let def =
            parse_static_line("Darksteel Sentinel doesn't untap during your untap step.").unwrap();
        assert_eq!(def.mode, StaticMode::Other("CantUntap".to_string()));
        assert!(def.description.is_some());
    }

    #[test]
    fn static_cant_be_countered() {
        let def = parse_static_line("Carnage Tyrant can't be countered.").unwrap();
        assert_eq!(def.mode, StaticMode::CantBeCast);
        assert!(def.description.is_some());
    }

    #[test]
    fn static_spells_cost_less() {
        let def = parse_static_line("Spells you cast cost {1} less to cast.").unwrap();
        assert_eq!(def.mode, StaticMode::ReduceCost);
    }

    #[test]
    fn static_opponent_spells_cost_more() {
        let def = parse_static_line("Spells your opponents cast cost {1} more to cast.").unwrap();
        assert_eq!(def.mode, StaticMode::RaiseCost);
    }

    // NOTE: static_enters_with_counters test moved to oracle_replacement tests —
    // "enters with counters" is now parsed as a Moved replacement effect.

    #[test]
    fn static_as_long_as() {
        let def = parse_static_line(
            "As long as you control a creature with power 4 or greater, Elemental Bond has hexproof.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(matches!(
            def.condition,
            Some(StaticCondition::CheckSVar { .. })
        ));
    }

    #[test]
    fn static_has_keyword_as_long_as() {
        let def =
            parse_static_line("Tarmogoyf has trample as long as a land card is in a graveyard.")
                .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Trample,
            }));
        assert!(matches!(
            def.condition,
            Some(StaticCondition::CheckSVar { .. })
        ));
    }

    #[test]
    fn static_life_more_than_starting_conditional() {
        let def = parse_static_line(
            "As long as you have at least 7 life more than your starting life total, creatures you control get +2/+2.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(matches!(
            def.affected,
            Some(TargetFilter::Typed(TypedFilter {
                card_type: Some(TypeFilter::Creature),
                controller: Some(ControllerRef::You),
                ..
            }))
        ));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddPower { value: 2 }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddToughness { value: 2 }));
        assert_eq!(
            def.condition,
            Some(StaticCondition::LifeMoreThanStartingBy { amount: 7 })
        );
    }

    #[test]
    fn static_lands_you_control_have() {
        let def = parse_static_line("Lands you control have 'Forests'.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddSubtype {
                subtype: "Forests".to_string(),
            }));
    }

    #[test]
    fn static_cant_be_the_target() {
        let def = parse_static_line(
            "Sphinx of the Final Word can't be the target of spells or abilities your opponents control.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::CantBeTargeted);
    }

    #[test]
    fn static_cant_be_sacrificed() {
        let def = parse_static_line("Sigarda, Host of Herons can't be sacrificed.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(def.description.is_some());
    }

    #[test]
    fn map_keyword_uses_fromstr() {
        // Test that map_keyword handles all standard keywords via FromStr
        assert_eq!(map_keyword("flying"), Some(Keyword::Flying));
        assert_eq!(map_keyword("first strike"), Some(Keyword::FirstStrike));
        assert_eq!(map_keyword("double strike"), Some(Keyword::DoubleStrike));
        assert_eq!(map_keyword("trample"), Some(Keyword::Trample));
        assert_eq!(map_keyword("deathtouch"), Some(Keyword::Deathtouch));
        assert_eq!(map_keyword("lifelink"), Some(Keyword::Lifelink));
        assert_eq!(map_keyword("vigilance"), Some(Keyword::Vigilance));
        assert_eq!(map_keyword("haste"), Some(Keyword::Haste));
        assert_eq!(map_keyword("reach"), Some(Keyword::Reach));
        assert_eq!(map_keyword("menace"), Some(Keyword::Menace));
        assert_eq!(map_keyword("hexproof"), Some(Keyword::Hexproof));
        assert_eq!(map_keyword("indestructible"), Some(Keyword::Indestructible));
        assert_eq!(map_keyword("defender"), Some(Keyword::Defender));
        assert_eq!(map_keyword("shroud"), Some(Keyword::Shroud));
        assert_eq!(map_keyword("flash"), Some(Keyword::Flash));
        assert_eq!(map_keyword("prowess"), Some(Keyword::Prowess));
        assert_eq!(map_keyword("fear"), Some(Keyword::Fear));
        assert_eq!(map_keyword("intimidate"), Some(Keyword::Intimidate));
        assert_eq!(map_keyword("wither"), Some(Keyword::Wither));
        assert_eq!(map_keyword("infect"), Some(Keyword::Infect));
        // Unknown returns None
        assert_eq!(map_keyword("notakeyword"), None);
    }

    #[test]
    fn static_multiple_keywords() {
        let def = parse_static_line("Enchanted creature has flying, trample, and haste.").unwrap();
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Flying,
            }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Trample,
            }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Haste,
            }));
    }

    #[test]
    fn static_self_gets_pt() {
        let def = parse_static_line("Tarmogoyf gets +1/+2.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddPower { value: 1 }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddToughness { value: 2 }));
    }

    #[test]
    fn static_have_keyword() {
        let def = parse_static_line("Creatures you control have vigilance.").unwrap();
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Vigilance,
            }));
    }

    #[test]
    fn during_your_turn_has_lifelink() {
        let def = parse_static_line("During your turn, this creature has lifelink.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
        assert_eq!(def.condition, Some(StaticCondition::DuringYourTurn));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Lifelink,
            }));
    }

    #[test]
    fn this_land_is_the_chosen_type() {
        let def = parse_static_line("This land is the chosen type.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
        assert_eq!(
            def.modifications,
            vec![ContinuousModification::AddChosenSubtype {
                kind: ChosenSubtypeKind::BasicLandType,
            }]
        );
    }

    #[test]
    fn this_creature_is_the_chosen_type() {
        let def =
            parse_static_line("This creature is the chosen type in addition to its other types.")
                .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
        assert_eq!(
            def.modifications,
            vec![ContinuousModification::AddChosenSubtype {
                kind: ChosenSubtypeKind::CreatureType,
            }]
        );
    }

    #[test]
    fn static_tarmogoyf_cda() {
        let def = parse_static_line(
            "Tarmogoyf's power is equal to the number of card types among cards in all graveyards and its toughness is equal to that number plus 1.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
        assert!(def.characteristic_defining);
        assert!(def
            .modifications
            .contains(&ContinuousModification::SetDynamicPower {
                value: DynamicPTValue::CardTypesInAllGraveyards { offset: 0 },
            }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::SetDynamicToughness {
                value: DynamicPTValue::CardTypesInAllGraveyards { offset: 1 },
            }));
    }

    #[test]
    fn static_enchanted_creature_doesnt_untap() {
        let def = parse_static_line(
            "Enchanted creature doesn't untap during its controller's untap step.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Other("CantUntap".to_string()));
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
            ))
        );
    }

    #[test]
    fn static_creatures_with_counters_dont_untap() {
        let def = parse_static_line(
            "Creatures with ice counters on them don't untap during their controllers' untap steps.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Other("CantUntap".to_string()));
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(TypedFilter::creature().properties(
                vec![FilterProp::CountersGE {
                    counter_type: "ice".to_string(),
                    count: 1,
                },]
            )))
        );
    }

    #[test]
    fn static_this_creature_attacks_each_combat_if_able() {
        let def = parse_static_line("This creature attacks each combat if able.").unwrap();
        assert_eq!(def.mode, StaticMode::MustAttack);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn static_enchanted_creature_attacks_each_combat_if_able() {
        let def = parse_static_line("Enchanted creature attacks each combat if able.").unwrap();
        assert_eq!(def.mode, StaticMode::MustAttack);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
            ))
        );
    }

    #[test]
    fn static_this_creature_can_block_only_creatures_with_flying() {
        let def = parse_static_line("This creature can block only creatures with flying.").unwrap();
        assert_eq!(def.mode, StaticMode::Other("BlockRestriction".to_string()));
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn static_you_have_shroud() {
        let def = parse_static_line("You have shroud.").unwrap();
        assert_eq!(def.mode, StaticMode::Other("Shroud".to_string()));
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            ))
        );
    }

    #[test]
    fn static_you_have_no_maximum_hand_size() {
        let def = parse_static_line("You have no maximum hand size.").unwrap();
        assert_eq!(def.mode, StaticMode::Other("NoMaximumHandSize".to_string()));
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            ))
        );
    }

    #[test]
    fn static_each_player_may_play_an_additional_land() {
        let def =
            parse_static_line("Each player may play an additional land on each of their turns.")
                .unwrap();
        assert_eq!(
            def.mode,
            StaticMode::Other("MayPlayAdditionalLand".to_string())
        );
        assert_eq!(def.affected, Some(TargetFilter::Player));
    }

    #[test]
    fn static_you_may_choose_not_to_untap_self() {
        let def =
            parse_static_line("You may choose not to untap this creature during your untap step.")
                .unwrap();
        assert_eq!(
            def.mode,
            StaticMode::Other("MayChooseNotToUntap".to_string())
        );
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn static_you_may_look_at_top_card_of_library() {
        let def =
            parse_static_line("You may look at the top card of your library any time.").unwrap();
        assert_eq!(
            def.mode,
            StaticMode::Other("MayLookAtTopOfLibrary".to_string())
        );
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            ))
        );
    }

    #[test]
    fn static_cards_in_graveyards_lose_all_abilities() {
        let def = parse_static_line("Cards in graveyards lose all abilities.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(TypedFilter::card().properties(vec![
                FilterProp::InZone {
                    zone: crate::types::zones::Zone::Graveyard,
                },
            ])))
        );
        assert_eq!(
            def.modifications,
            vec![ContinuousModification::RemoveAllAbilities]
        );
    }

    #[test]
    fn static_black_creatures_get_plus_one_plus_one() {
        let def = parse_static_line("Black creatures get +1/+1.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(TypedFilter::creature().properties(
                vec![FilterProp::HasColor {
                    color: "Black".to_string(),
                },]
            )))
        );
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddPower { value: 1 }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddToughness { value: 1 }));
    }

    #[test]
    fn static_creatures_you_control_with_mana_value_filter() {
        let def = parse_static_line("Creatures you control with mana value 3 or less get +1/+0.")
            .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::CmcLE { value: 3 }]),
            ))
        );
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddPower { value: 1 }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddToughness { value: 0 }));
    }

    #[test]
    fn static_creatures_you_control_with_flying_filter() {
        let def = parse_static_line("Creatures you control with flying get +1/+1.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::WithKeyword {
                        value: "flying".to_string(),
                    }]),
            ))
        );
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddPower { value: 1 }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddToughness { value: 1 }));
    }

    #[test]
    fn static_other_zombie_creatures_have_swampwalk() {
        let def = parse_static_line("Other Zombie creatures have swampwalk.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature()
                    .subtype("Zombie".to_string())
                    .properties(vec![FilterProp::Another]),
            ))
        );
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Landwalk("Swamp".to_string()),
            }));
    }

    #[test]
    fn static_creature_tokens_you_control_lose_all_abilities_and_have_base_pt() {
        let def = parse_static_line(
            "Creature tokens you control lose all abilities and have base power and toughness 3/3.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::Token]),
            ))
        );
        assert!(def
            .modifications
            .contains(&ContinuousModification::RemoveAllAbilities));
        assert!(def
            .modifications
            .contains(&ContinuousModification::SetPower { value: 3 }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::SetToughness { value: 3 }));
    }

    #[test]
    fn static_target_subject_can_set_base_power_without_toughness() {
        let modifications = parse_continuous_modifications("has base power 3 until end of turn");
        assert_eq!(
            modifications,
            vec![ContinuousModification::SetPower { value: 3 }]
        );
    }

    #[test]
    fn static_enchanted_land_has_quoted_ability() {
        let def = parse_static_line("Enchanted land has \"{T}: Add two mana of any one color.\"")
            .unwrap();
        // Should produce a GrantAbility with a typed activated AbilityDefinition
        let grant = def.modifications.iter().find(|m| {
            matches!(m, ContinuousModification::GrantAbility { .. })
        });
        assert!(grant.is_some(), "should contain a GrantAbility modification");
        if let ContinuousModification::GrantAbility { definition } = grant.unwrap() {
            assert_eq!(definition.kind, AbilityKind::Activated);
            assert!(definition.cost.is_some());
        }
    }

    #[test]
    fn static_other_tapped_creatures_you_control_have_indestructible() {
        let def =
            parse_static_line("Other tapped creatures you control have indestructible.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::Tapped, FilterProp::Another]),
            ))
        );
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Indestructible,
            }));
    }

    #[test]
    fn static_attacking_creatures_you_control_have_double_strike() {
        let def = parse_static_line("Attacking creatures you control have double strike.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::Attacking]),
            ))
        );
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::DoubleStrike,
            }));
    }

    #[test]
    fn static_during_your_turn_creatures_you_control_have_hexproof() {
        let def =
            parse_static_line("During your turn, creatures you control have hexproof.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(def.condition, Some(StaticCondition::DuringYourTurn));
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::You),
            ))
        );
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Hexproof,
            }));
    }

    #[test]
    fn static_during_your_turn_equipped_creatures_you_control_have_double_strike() {
        let def = parse_static_line(
            "During your turn, equipped creatures you control have double strike and haste.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(def.condition, Some(StaticCondition::DuringYourTurn));
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::EquippedBy]),
            ))
        );
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::DoubleStrike,
            }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Haste,
            }));
    }
}
