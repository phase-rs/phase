// CR 613.3g (Layer 7) — P/T anthem static abilities.

#[allow(unused_imports)]
use super::prelude::*;
#[allow(unused_imports)]
use super::support::*;

/// Try to parse "[Subtype] creatures you control get/have ..." patterns.
/// `text` is the original-case text starting at the subtype word.
/// `lower` is the lowercased version of `text`.
/// `is_other` indicates whether this was preceded by "Other ".
pub(crate) fn parse_typed_you_control(
    text: &str,
    lower: &str,
    is_other: bool,
) -> Option<StaticDefinition> {
    let tp = TextPair::new(text, lower);
    // Try "X creatures you control get/have" first
    // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
    if let Some(creatures_pos) = tp.find(" creatures you control ") {
        // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        let (before, after) = tp.split_at(creatures_pos);
        let descriptor = before.original.trim();
        if !descriptor.is_empty() {
            let after_prefix = &after.original[" creatures you control ".len()..];
            let full_subject = tp.original[..creatures_pos + " creatures you control".len()].trim();
            // CR 509.1h: Strip combat-status prefixes ("Attacking Ninja" → props=[Attacking], subtype="Ninja")
            let mut extra_props = Vec::new();
            let mut desc_remaining = descriptor;
            let mut desc_lower = descriptor.to_lowercase();
            while let Some((prop, consumed)) = parse_combat_status_prefix(&desc_lower) {
                extra_props.push(prop);
                desc_remaining = desc_remaining[consumed..].trim_start();
                desc_lower = desc_remaining.to_lowercase();
            }
            // CR 105.2c / CR 205.4a: Property-descriptor recognition for colorless,
            // multicolored, and snow creatures before subtype parsing.
            if let Some(prop_filter) =
                parse_property_descriptor(&desc_lower, desc_remaining, &extra_props, is_other)
            {
                let (prop_filter, after_prefix) =
                    if let Some((prop, rest)) = strip_counter_condition_prefix(after_prefix) {
                        (add_property(prop_filter, prop), rest)
                    } else {
                        (prop_filter, after_prefix)
                    };
                return parse_continuous_gets_has(after_prefix, prop_filter, text);
            }
            // CR 205.3m: Try compound subtypes first ("Ninja and Rogue", "Elf or Warrior")
            // The helper bakes in extra_props and is_other, so skip add_another_filter below.
            if let Some(compound_filter) =
                try_parse_compound_subtypes(desc_remaining, &extra_props, is_other)
            {
                // CR 613.7: Check for counter condition before returning
                let (compound_filter, after_prefix) =
                    if let Some((prop, rest)) = strip_counter_condition_prefix(after_prefix) {
                        (add_property(compound_filter, prop), rest)
                    } else {
                        (compound_filter, after_prefix)
                    };
                return parse_continuous_gets_has(after_prefix, compound_filter, text);
            }
            let typed_filter = if extra_props.is_empty() {
                // No combat-status prefix — use original dispatch path
                if let Some(filter) = parse_modified_creature_subject_filter(full_subject) {
                    filter
                } else if let Some(filter) =
                    parse_attachment_creatures_you_control_descriptor(descriptor)
                {
                    filter
                } else if let Some(color) = parse_named_color(descriptor) {
                    TargetFilter::Typed(
                        TypedFilter::creature()
                            .controller(ControllerRef::You)
                            .properties(vec![FilterProp::HasColor { color }]),
                    )
                // CR 205.2a: "artifact creatures" = Creature + Artifact conjunctive type filter
                } else if let Some(core_tf) =
                    try_parse_core_type_descriptor(&descriptor.to_lowercase())
                {
                    TargetFilter::Typed(
                        TypedFilter::creature()
                            .with_type(core_tf)
                            .controller(ControllerRef::You),
                    )
                // CR 903.3d: "Commander creatures you control" — bare "Commander"
                // descriptor on a creature subject is the commander designation,
                // not an MTG subtype. Constrain to creatures + IsCommander.
                } else if descriptor.eq_ignore_ascii_case("commander") {
                    TargetFilter::Typed(
                        TypedFilter::creature()
                            .controller(ControllerRef::You)
                            .properties(vec![FilterProp::IsCommander]),
                    )
                // CR 111.1 / CR 205.3 / CR 205.4a: A `non`/`non-` negation
                // descriptor ("Nontoken creatures you control") or supertype
                // descriptor ("Legendary creatures you control") is NOT a
                // subtype. Bail so dispatch falls through to the subject parser,
                // which routes the full phrase through `parse_type_phrase`.
                } else if descriptor_is_negation(descriptor) || descriptor_is_supertype(descriptor)
                {
                    return None;
                } else if is_capitalized_words(descriptor) {
                    TargetFilter::Typed(
                        typed_filter_for_subtype(descriptor).controller(ControllerRef::You),
                    )
                } else {
                    return None;
                }
            } else if desc_remaining.eq_ignore_ascii_case("commander") {
                // CR 903.3d: Combat-status prefix + "Commander creature" — same
                // designation guard as the no-prefix branch above.
                TargetFilter::Typed(
                    TypedFilter::creature()
                        .controller(ControllerRef::You)
                        .properties({
                            let mut p = extra_props.clone();
                            p.push(FilterProp::IsCommander);
                            p
                        }),
                )
            } else if descriptor_is_negation(desc_remaining)
                || descriptor_is_supertype(desc_remaining)
            {
                // CR 111.1 / CR 205.3 / CR 205.4a: negation/supertype descriptor
                // after a combat-status prefix — not a subtype; fall through to
                // full subject parsing.
                return None;
            } else if is_capitalized_words(desc_remaining) {
                // Combat-status prefix found + remaining is a subtype
                TargetFilter::Typed(
                    typed_filter_for_subtype(desc_remaining)
                        .controller(ControllerRef::You)
                        .properties(extra_props),
                )
            } else {
                return None;
            };
            // CR 613.7: Check for "with [counter] on it/them" condition between
            // "you control" and the predicate (e.g., "Elf creatures you control
            // with a +1/+1 counter on it has trample").
            let (typed_filter, after_prefix) =
                if let Some((prop, rest)) = strip_counter_condition_prefix(after_prefix) {
                    (add_property(typed_filter, prop), rest)
                } else {
                    (typed_filter, after_prefix)
                };
            let typed_filter = if is_other {
                add_another_filter(typed_filter)
            } else {
                typed_filter
            };
            return parse_continuous_gets_has(after_prefix, typed_filter, text);
        }
    }

    // Try "Xs you control get/have" (e.g. "Zombies you control get +1/+1")
    // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
    if let Some(yc_pos) = tp.find(" you control ") {
        // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        let (before, after) = tp.split_at(yc_pos);
        let descriptor = before.original.trim();
        if !descriptor.is_empty() {
            let after_prefix = &after.original[" you control ".len()..];
            let full_subject = tp.original[..yc_pos + " you control".len()].trim();
            // CR 509.1h: Strip combat-status prefixes
            let mut extra_props = Vec::new();
            let mut desc_remaining = descriptor;
            let mut desc_lower = descriptor.to_lowercase();
            while let Some((prop, consumed)) = parse_combat_status_prefix(&desc_lower) {
                extra_props.push(prop);
                desc_remaining = desc_remaining[consumed..].trim_start();
                desc_lower = desc_remaining.to_lowercase();
            }
            // CR 205.3m: Try compound subtypes first ("Ninja and Rogue", "Elf or Warrior")
            if let Some(compound_filter) =
                try_parse_compound_subtypes(desc_remaining, &extra_props, is_other)
            {
                // CR 613.7: Check for counter condition before returning
                let (compound_filter, after_prefix) =
                    if let Some((prop, rest)) = strip_counter_condition_prefix(after_prefix) {
                        (add_property(compound_filter, prop), rest)
                    } else {
                        (compound_filter, after_prefix)
                    };
                return parse_continuous_gets_has(after_prefix, compound_filter, text);
            }
            let typed_filter = if extra_props.is_empty() {
                if let Some(filter) = parse_modified_creature_subject_filter(full_subject) {
                    filter
                } else if let Some(color) = parse_named_color(descriptor) {
                    TargetFilter::Typed(
                        TypedFilter::creature()
                            .controller(ControllerRef::You)
                            .properties(vec![FilterProp::HasColor { color }]),
                    )
                // CR 205.2a: "Artifacts you control" — standalone core type as permanent filter
                } else if let Some(core_tf) =
                    try_parse_core_type_descriptor(&descriptor.to_lowercase())
                {
                    TargetFilter::Typed(TypedFilter::new(core_tf).controller(ControllerRef::You))
                // CR 903.3d: "Commander(s) you control" — commander designation is
                // NOT an MTG subtype (CR 903.3); route to FilterProp::IsCommander
                // before the capitalized-subtype fallback would synthesize a
                // bogus `Subtype("Commander")`.
                } else if matches!(
                    descriptor.to_lowercase().as_str(),
                    "commander" | "commanders"
                ) {
                    TargetFilter::Typed(
                        TypedFilter::permanent()
                            .controller(ControllerRef::You)
                            .properties(vec![FilterProp::IsCommander]),
                    )
                } else if is_capitalized_words(descriptor) {
                    // CR 205.3m: Normalize plural subtypes to canonical singular form
                    let subtype_name = parse_subtype(descriptor)
                        .map(|(canonical, _)| canonical)
                        .unwrap_or_else(|| descriptor.trim_end_matches('s').to_string());
                    TargetFilter::Typed(
                        typed_filter_for_subtype(&subtype_name).controller(ControllerRef::You),
                    )
                } else {
                    return None;
                }
            } else if is_capitalized_words(desc_remaining) {
                // CR 205.3m: Normalize plural subtypes to canonical singular form
                let subtype_name = parse_subtype(desc_remaining)
                    .map(|(canonical, _)| canonical)
                    .unwrap_or_else(|| desc_remaining.trim_end_matches('s').to_string());
                TargetFilter::Typed(
                    typed_filter_for_subtype(&subtype_name)
                        .controller(ControllerRef::You)
                        .properties(extra_props),
                )
            } else {
                return None;
            };
            // CR 613.7: Check for "with [counter] on it/them" condition
            let (typed_filter, after_prefix) =
                if let Some((prop, rest)) = strip_counter_condition_prefix(after_prefix) {
                    (add_property(typed_filter, prop), rest)
                } else {
                    (typed_filter, after_prefix)
                };
            let typed_filter = if is_other {
                add_another_filter(typed_filter)
            } else {
                typed_filter
            };
            return parse_continuous_gets_has(after_prefix, typed_filter, text);
        }
    }

    None
}

pub(crate) fn parse_subject_continuous_static(text: &str) -> Option<StaticDefinition> {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);

    // Additive-type clauses do not use any of the get/has/have/lose verbs that
    // `find_continuous_predicate_start` scans for. They split on "are"/"is"
    // instead and may embed a " have " inside a granted-ability quote that
    // would otherwise confuse the verb scanner. Route them to their own
    // extractor before falling through to the general predicate parser.
    if let Some(def) = parse_subject_additive_type_static(text) {
        return Some(def);
    }

    let subject_end = find_continuous_predicate_start(tp.lower)?;
    let subject = tp.original[..subject_end].trim();
    let predicate = tp.original[subject_end + 1..].trim();
    if parse_rule_static_predicate(predicate).is_some() {
        return None;
    }
    let affected = parse_continuous_subject_filter(subject)?;

    // CR 613.4c / CR 611.3a: Route "for each" and "as long as" predicates through
    // parse_continuous_gets_has which handles dynamic P/T and condition splitting.
    let pred_lower = predicate.to_lowercase();
    if nom_primitives::scan_contains(&pred_lower, "for each")
        || nom_primitives::scan_contains(&pred_lower, "as long as")
    {
        return parse_continuous_gets_has(predicate, affected, text);
    }

    // CR 604.1: Strip suffix turn conditions from predicate —
    // "has first strike during your turn" → "has first strike" + DuringYourTurn
    let (effective_predicate, suffix_condition) = strip_suffix_turn_condition(&pred_lower);

    let modifications = parse_continuous_modifications(&effective_predicate);
    if !modifications.is_empty() {
        let mut def = StaticDefinition::continuous()
            .affected(affected)
            .modifications(modifications)
            .description(text.to_string());
        if let Some(cond) = suffix_condition {
            def.condition = Some(cond);
        }
        return Some(def);
    }

    None
}

/// CR 205.1 / CR 205.3a: Top-level dispatcher for additive-type-only statics
/// whose predicate begins with `"are"` / `"is"` — e.g.
/// `"Other creatures are Food artifacts in addition to their other types and
/// have \"…\""`. These do not contain a get/has/have/lose verb at the
/// grammatical top level, so `parse_subject_continuous_static`'s main path
/// would mis-split on a " have " buried inside the granted-ability quote.
///
/// Compound predicates (P/T + additive type, e.g. Kudo:
/// `"have base power and toughness 2/2 and are Bears in addition to their
/// other types"`) go through the main path instead and reach the same
/// extractor via `parse_continuous_modifications`.
pub(crate) fn parse_subject_additive_type_static(text: &str) -> Option<StaticDefinition> {
    type VE<'a> = OracleError<'a>;
    let lower = text.to_lowercase();
    let (subject_lower, predicate_lower) = nom_primitives::scan_split_at_phrase(&lower, |i| {
        alt((tag::<_, _, VE>("are "), tag::<_, _, VE>("is "))).parse(i)
    })?;
    let subject = text[..subject_lower.len()].trim();
    let predicate = &text[text.len() - predicate_lower.len()..];
    let affected = parse_continuous_subject_filter(subject)?;

    let predicate_tp = TextPair::new(predicate, predicate_lower);
    if let Some((before_cond, after_cond)) = predicate_tp.split_around(" as long as ") {
        let modifications = parse_additive_type_clause_modifications(before_cond.original)?;
        let condition_text = after_cond.original.trim().trim_end_matches('.');
        let condition =
            parse_static_condition(condition_text).unwrap_or(StaticCondition::Unrecognized {
                text: condition_text.to_string(),
            });
        return Some(
            StaticDefinition::continuous()
                .affected(affected)
                .modifications(modifications)
                .condition(condition)
                .description(text.to_string()),
        );
    }

    let modifications = parse_additive_type_clause_modifications(predicate)?;
    Some(
        StaticDefinition::continuous()
            .affected(affected)
            .modifications(modifications)
            .description(text.to_string()),
    )
}

/// Parse compound condition + animation pattern:
/// "During your turn, as long as ~ has one or more [counter] counters on [pronoun],
///  [pronoun]'s a [P/T] [types] and has [keyword]"
///
/// Produces `StaticCondition::And { DuringYourTurn, HasCounters { .. } }` with
/// `ContinuousModification` list for type/subtype/P-T/keyword changes.
pub(crate) fn parse_compound_turn_counter_animation(
    lower: &str,
    text: &str,
) -> Option<StaticDefinition> {
    // Strip "during your turn, " prefix via nom tag
    let (rest, _) = tag::<_, _, OracleError<'_>>("during your turn, ")(lower).ok()?;

    // Strip "as long as " prefix from the remainder
    let (rest, _) = tag::<_, _, OracleError<'_>>("as long as ")(rest).ok()?;

    // Parse "~ has one or more [type] counters on [pronoun], "
    let (rest, _) = tag::<_, _, OracleError<'_>>("~ has ")(rest).ok()?;

    // Parse the counter count requirement: "one or more" / "N or more" / "a"
    let (minimum, rest) = parse_counter_minimum(rest)?;

    // Parse "[type] counters on [pronoun], "
    let rest = rest.trim_start();
    let counters_pos = rest.find(" counter")?; // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
    let counter_type_text = rest[..counters_pos].trim();
    // CR 122.1: bare "a counter on it" with no type word → Any; typed "a [type]
    // counter on it" → OfType(ct). Routes through the shared mapping in
    // `types::counter::parse_counter_type` to keep the canonical set in one place.
    let counters = if counter_type_text.is_empty() {
        CounterMatch::Any
    } else {
        CounterMatch::OfType(parse_counter_type(counter_type_text))
    };

    // Skip past "counters on [pronoun], " to get the modification text
    let rest = &rest[counters_pos..];
    let modification_text = strip_after(rest, ", ")?.trim();

    let modifications = parse_animation_modifications(modification_text.trim_end_matches('.'));
    if modifications.is_empty() {
        return None;
    }

    Some(
        StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .condition(StaticCondition::And {
                conditions: vec![
                    StaticCondition::DuringYourTurn,
                    StaticCondition::HasCounters {
                        counters,
                        minimum,
                        maximum: None,
                    },
                ],
            })
            .modifications(modifications)
            .description(text.to_string()),
    )
}

/// Parse "one or more" / "N or more" / "a" into a counter minimum count.
/// Returns (minimum, remaining text).
pub(crate) fn parse_counter_minimum(text: &str) -> Option<(u32, &str)> {
    if let Some(rest) = nom_tag_lower(text, text, "one or more ") {
        return Some((1, rest));
    }
    if let Some(rest) = nom_tag_lower(text, text, "a ") {
        return Some((1, rest));
    }
    // "N or more" pattern
    if let Some((n, rest)) = parse_number(text) {
        let rest = rest.trim_start();
        if let Some(rest) = nom_tag_lower(rest, rest, "or more ") {
            return Some((n, rest));
        }
    }
    None
}

/// Parse "[pronoun]'s a [P/T] [types] and has [keyword]" into modifications.
///
/// Handles patterns like:
/// - "he's a 3/4 ninja creature and has hexproof"
/// - "it's a 3/4 ninja creature with hexproof"
pub(crate) fn parse_animation_modifications(text: &str) -> Vec<ContinuousModification> {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    let mut modifications = Vec::new();

    // Strip pronoun prefix via nom tag: "he's a", "she's a", "it's a", "~'s a"
    let body = nom_tag_lower(tp.original, tp.lower, "he's a ")
        .or_else(|| nom_tag_lower(tp.original, tp.lower, "she's a "))
        .or_else(|| nom_tag_lower(tp.original, tp.lower, "it's a "))
        .or_else(|| nom_tag_lower(tp.original, tp.lower, "~'s a "));

    let body = match body {
        Some(b) => b.trim_start(),
        None => return modifications,
    };

    // Split on " and has " or " with " to separate type/PT from keywords
    let body_lower = body.to_lowercase();
    // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
    let (type_pt_part, keyword_part) = if let Some(pos) = body_lower.find(" and has ") {
        // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        (&body[..pos], Some(&body[pos + 9..]))
    // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
    } else if let Some(pos) = body_lower.find(" with ") {
        // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        (&body[..pos], Some(&body[pos + 6..]))
    } else {
        (body, None)
    };

    // Parse P/T from the beginning: "3/4 ninja creature"
    let remaining = if let Some((p, t)) = parse_pt_mod(type_pt_part) {
        modifications.push(ContinuousModification::SetPower { value: p });
        modifications.push(ContinuousModification::SetToughness { value: t });
        // Skip past the P/T value
        let slash = type_pt_part.find('/').unwrap();
        let rest = &type_pt_part[slash + 1..];
        let pt_end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
        rest[pt_end..].trim()
    } else {
        type_pt_part
    };

    // Parse types and subtypes from remaining: "ninja creature", "human ninja creature"
    for word in remaining.split_whitespace() {
        let word = word.trim_end_matches('.').trim_end_matches(',');
        if word.is_empty() {
            continue;
        }
        let mut chars = word.chars();
        let Some(first) = chars.next() else {
            continue;
        };
        let capitalized = format!("{}{}", first.to_uppercase(), chars.as_str());
        if let Ok(core_type) = crate::types::card_type::CoreType::from_str(&capitalized) {
            modifications.push(ContinuousModification::AddType { core_type });
        } else {
            modifications.push(ContinuousModification::AddSubtype {
                subtype: capitalized,
            });
        }
    }

    // Parse keywords from keyword part
    if let Some(kw_text) = keyword_part {
        for part in split_keyword_list(kw_text.trim().trim_end_matches('.')) {
            if let Some(kw) = map_keyword(part.trim().trim_end_matches('.')) {
                modifications.push(ContinuousModification::AddKeyword { keyword: kw });
            }
        }
    }

    modifications
}

pub(crate) fn parse_conditional_static(text: &str) -> Option<StaticDefinition> {
    let conditional = text.strip_prefix("As long as ")?; // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
    let (condition_text, remainder) = conditional.split_once(", ")?; // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.

    let condition =
        parse_static_condition(condition_text).unwrap_or(StaticCondition::Unrecognized {
            text: condition_text.to_string(),
        });

    let mut def = parse_static_line(remainder.trim())?;
    // CR 611.3a + CR 118.12a: When the inner static already carries a typed
    // condition (e.g. combat-tax `UnlessPay` for "creatures can't attack you
    // unless their controller pays {1}"), compose both conditions via
    // `StaticCondition::And` rather than dropping one. This is the only correct
    // way to model lines like "As long as ~ is untapped, creatures can't attack
    // you unless their controller pays {1}..." (Archangel of Tithes) — the
    // outer `Not(SourceIsTapped)` gates whether the tax is active, the inner
    // `UnlessPay` carries the tax cost. Both must survive to runtime.
    def.condition = Some(match def.condition.take() {
        Some(existing) => StaticCondition::And {
            conditions: vec![condition, existing],
        },
        None => condition,
    });
    def.description = Some(text.to_string());
    Some(def)
}

pub(crate) fn parse_contextual_continuous_subject_static(
    tp: &TextPair<'_>,
    description: &str,
) -> Option<StaticDefinition> {
    let (subject, verb_prefix, rest_lower) = continuous_subject_verb(tp.lower)?;
    let subject_original = tp.original[..subject.len()].trim();
    let after = &tp.original[tp.original.len() - rest_lower.len()..];
    let predicate = format!("{verb_prefix}{after}");
    let condition = predicate_condition(&predicate);
    let affected =
        contextual_continuous_subject_filter(subject, subject_original, condition.as_ref())?;
    parse_continuous_gets_has(&predicate, affected, description)
}

pub(crate) fn continuous_subject_verb(lower: &str) -> Option<(&str, &'static str, &str)> {
    let (subject, verb_prefix, rest) = nom_primitives::scan_preceded(lower, |input| {
        alt((
            value("gets ", tag::<_, _, OracleError<'_>>("gets ")),
            value("gets ", tag("get ")),
            value("has ", tag("has ")),
            value("has ", tag("have ")),
        ))
        .parse(input)
    })?;
    Some((subject.trim(), verb_prefix, rest))
}

pub(crate) fn predicate_condition(predicate: &str) -> Option<StaticCondition> {
    let lower = predicate.to_lowercase();
    let tp = TextPair::new(predicate, &lower);
    let (_, condition_tp) = tp.split_around(" as long as ")?;
    let condition_text = condition_tp.original.trim().trim_end_matches('.');
    parse_static_condition(condition_text)
}

pub(crate) fn contextual_continuous_subject_filter(
    subject_lower: &str,
    subject_original: &str,
    condition: Option<&StaticCondition>,
) -> Option<TargetFilter> {
    if subject_lower == "that creature" {
        return condition
            .and_then(exactly_one_creature_you_control_filter)
            .cloned();
    }

    let subject_tp = TextPair::new(subject_original, subject_lower);
    if let Some(filter) = parse_controlled_compound_continuous_subject_filter(&subject_tp) {
        return Some(filter);
    }

    let group_subject_tp = nom_tag_tp(&subject_tp, "~ and ")
        .or_else(|| nom_tag_tp(&subject_tp, "this creature and "))?;
    let group_filter = parse_continuous_subject_filter(group_subject_tp.original)?;
    Some(TargetFilter::Or {
        filters: vec![TargetFilter::SelfRef, group_filter],
    })
}

/// CR 613.1: A single continuous static may name multiple controlled subjects
/// before one shared predicate ("Skeletons you control and other Zombies you
/// control get ..."). Parse each complete subject phrase and union them rather
/// than letting the first subject consume the whole predicate.
pub(crate) fn parse_controlled_compound_continuous_subject_filter(
    subject: &TextPair<'_>,
) -> Option<TargetFilter> {
    let (left_lower, _, right_lower) = nom_primitives::scan_preceded(subject.lower, |input| {
        value((), tag::<_, _, OracleError<'_>>("and ")).parse(input)
    })?;
    let right_start = subject.lower.len() - right_lower.len();
    let left_original = subject.original[..left_lower.len()].trim();
    let right_original = &subject.original[right_start..];

    let left_filter = parse_continuous_subject_filter(left_original)?;
    let right_filter = if let Some(filter) = parse_controlled_compound_continuous_subject_filter(
        &TextPair::new(right_original, right_lower),
    ) {
        filter
    } else {
        parse_continuous_subject_filter(right_original)?
    };

    if !filter_has_source_or_controller_anchor(&left_filter)
        || !filter_has_source_or_controller_anchor(&right_filter)
    {
        return None;
    }

    let mut filters = Vec::new();
    push_or_filter_branch(&mut filters, left_filter);
    push_or_filter_branch(&mut filters, right_filter);
    Some(TargetFilter::Or { filters })
}

pub(crate) fn parse_soulbond_paired_static(
    tp: &TextPair<'_>,
    description: &str,
) -> Option<StaticDefinition> {
    let parser = preceded(
        tag("as long as "),
        preceded(
            terminated(parse_soulbond_paired_condition_nom, tag(", ")),
            preceded(
                alt((tag("each of those creatures "), tag("both creatures "))),
                alt((terminated(take_until("."), tag(".")), rest)),
            ),
        ),
    );
    let (_, predicate) = all_consuming(parser).parse(tp.lower).ok()?;
    let mut def = parse_continuous_gets_has(predicate, TargetFilter::SourceOrPaired, description)?;
    def.condition = Some(StaticCondition::SourceIsPaired);
    Some(def)
}

pub(crate) fn bind_where_x_in_quantity_expr(
    value: QuantityExpr,
    where_x: &QuantityRef,
) -> Option<QuantityExpr> {
    match value {
        QuantityExpr::Fixed { .. } => Some(value),
        QuantityExpr::Ref {
            qty: QuantityRef::Variable { name },
        } if name == "X" => Some(QuantityExpr::Ref {
            qty: where_x.clone(),
        }),
        _ => None,
    }
}

/// CR 109.5: In a static ability, "you" and "your" refer to the current
/// controller of the object with that ability.
pub(crate) fn parse_typed_you_control_subject_filter(
    subject: &TextPair<'_>,
) -> Option<TargetFilter> {
    if let Some(descriptor) = parse_subject_suffix(subject, " creatures you control") {
        let descriptor = descriptor.trim_end();
        if descriptor.is_empty() {
            return Some(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::You),
            ));
        }
        return typed_you_control_descriptor_filter(descriptor, true);
    }

    let descriptor = parse_subject_suffix(subject, " you control")?.trim_end();
    if descriptor.is_empty() {
        return None;
    }
    typed_you_control_descriptor_filter(descriptor, false)
}

/// Parse "gets +N/+M [and has {keyword}]" after the subject.
/// Also handles "gets +N/+M for each [clause]" dynamic P/T patterns.
/// CR 611.3a: In a self-referential static the pronoun "it" co-refers with the
/// source permanent, so rewrite a leading "it's "/"it is " subject to the
/// canonical "~ is " before the condition is typed (e.g. Giant Tortoise's
/// "as long as it's untapped").
///
/// Two guards keep this safe:
/// 1. Callers MUST only apply it when the affected subject is `SelfRef` — for
///    attached-subject statics (an Aura/Equipment whose "it" refers to the
///    enchanted/equipped creature) the pronoun is not the source.
/// 2. Only the bare source-STATE predicates that `~ is …` already resolves to a
///    typed condition are rewritten — the tapped/untapped pair plus their
///    combat-state siblings "attacking"/"blocking"/"blocked"
///    (CR 508.1k / 509.1g / 509.1h). "it" is otherwise overloaded: "it's your
///    turn" is impersonal (a turn reference, not the source); "it's a Wall" /
///    "it's red" / "it's legendary" are type/characteristic gates with their own
///    parse paths. Rewriting those would break or mis-bind them, so they are
///    left untouched. The match is EXACT, so "it's attacking alone" keeps its
///    trailing word and falls through to `SourceAttackingAlone` rather than
///    collapsing to `SourceIsAttacking`.
///
/// Returns the condition unchanged when neither guard matches.
fn rewrite_self_pronoun_subject(condition: &str) -> String {
    let lower = condition.to_lowercase();
    if let Some(rest) =
        nom_tag_lower(&lower, &lower, "it's ").or_else(|| nom_tag_lower(&lower, &lower, "it is "))
    {
        // CR 508.1k / CR 509.1g / CR 509.1h: combat-state pronoun siblings of the
        // tapped/untapped rewrite. CR 700.9: "modified" is the self-state sibling
        // for "it's modified" (Obstinate Gargoyle, Skyward Spider). CR 301.5a:
        // "equipped"; CR 303.4: "enchanted" — self-state predicates for SelfRef
        // statics (Merry "as long as it's equipped"; Fledgling Osprey "as long as
        // it's enchanted"). Exact-match only — "attacking alone" keeps its trailing
        // word and is left for SourceAttackingAlone; "modified creature" and
        // "enchanted by N Auras" keep their trailing words and never hit this arm.
        if matches!(
            rest.trim(),
            "tapped"
                | "untapped"
                | "attacking"
                | "blocking"
                | "blocked"
                | "modified"
                | "equipped"
                | "enchanted"
        ) {
            return format!("~ is {}", rest.trim());
        }
    }
    condition.to_string()
}

pub(crate) fn parse_continuous_gets_has(
    text: &str,
    affected: TargetFilter,
    description: &str,
) -> Option<StaticDefinition> {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);

    // CR 611.3a: Split "as long as [condition]" BEFORE "for each" — the condition applies
    // to the entire static, not to a quantity count. Mirrors parse_enchanted_equipped_predicate.
    if let Some((before_cond, after_cond)) = tp.split_around(" as long as ") {
        let continuous_text = before_cond.original;
        let condition_text = after_cond.original.trim().trim_end_matches('.');
        // Recursively parse the continuous part without the condition
        if let Some(mut def) =
            parse_continuous_gets_has(continuous_text, affected.clone(), description)
        {
            // CR 611.3a: only resolve the self-pronoun "it" to the source when the
            // static modifies itself; attached-subject statics keep "it" bound to
            // the enchanted/equipped creature and stay an honest gap.
            let typed = if matches!(affected, TargetFilter::SelfRef) {
                parse_static_condition(&rewrite_self_pronoun_subject(condition_text))
            } else {
                parse_static_condition(condition_text)
            };
            let condition = typed.unwrap_or(StaticCondition::Unrecognized {
                text: condition_text.to_string(),
            });
            def.condition = Some(condition);
            return Some(def);
        }
    }

    // CR 613.4c: Handle "gets +N/+M for each [clause]" — dynamic P/T via ObjectCount.
    if let Some((before_for_each, after_for_each)) = tp.split_around("for each ") {
        let pt_text = before_for_each.original.trim();
        let raw_for_each = after_for_each.lower.trim_end_matches('.');
        // Strip a trailing keyword clause (" and has flying", " and gains haste",
        // etc.) so the for-each filter parser sees only its own clause. The
        // trailing keywords are picked up separately via `extract_keyword_clause`
        // on `description` below.
        let for_each_clause = strip_trailing_keyword_clause(raw_for_each);

        let pt_lower = pt_text.to_lowercase();
        // CR 613.4c: the "gets +N/+M" verb may sit AFTER a leading keyword clause
        // ("equipped creature has first strike and gets +1/+0 for each ...",
        // Glamdring), not only at the head of the clause. Scan word boundaries for
        // the verb with `nom_tag_lower` (the multi-position phrase-scan idiom, cf.
        // `scan_timing_restrictions`) so the dynamic P/T is still extracted; the
        // leading keyword is recovered separately via `extract_keyword_clause`.
        let mut pt_scan: &str = &pt_lower;
        let pt_source = loop {
            if let Some(rest) = nom_tag_lower(pt_scan, pt_scan, "gets ")
                .or_else(|| nom_tag_lower(pt_scan, pt_scan, "get "))
            {
                break rest;
            }
            match pt_scan.find(' ') {
                Some(idx) => pt_scan = pt_scan[idx + 1..].trim_start(),
                None => break pt_lower.as_str(),
            }
        };

        if let Some((p, t)) = parse_pt_mod(pt_source) {
            if let Some(quantity) =
                super::oracle_quantity::parse_for_each_clause_expr(for_each_clause)
            {
                let mut modifications = Vec::new();
                push_dynamic_pt_modifications(&mut modifications, p, t, quantity);
                if !modifications.is_empty() {
                    // Check for trailing "and has [keyword]" after the for-each clause
                    // e.g., "gets +1/+0 for each Mountain you control and has first strike"
                    if let Some(keyword_text) = extract_keyword_clause(description) {
                        for part in split_keyword_list(keyword_text.trim().trim_end_matches('.')) {
                            push_grant_clause_modifications(
                                &mut modifications,
                                part.as_ref(),
                                None,
                            );
                        }
                    }
                    return Some(
                        StaticDefinition::continuous()
                            .affected(affected)
                            .modifications(modifications)
                            .description(description.to_string()),
                    );
                }
            }
        }
    }

    let modifications = parse_continuous_modifications(text);

    if modifications.is_empty() {
        return None;
    }

    Some(
        StaticDefinition::continuous()
            .affected(affected)
            .modifications(modifications)
            .description(description.to_string()),
    )
}

pub(crate) fn parse_dynamic_for_each_pt_modifications(
    text: &str,
) -> Option<Vec<ContinuousModification>> {
    let lower = text.to_lowercase();
    let (for_each_with_marker, pt_text) = take_until::<_, _, OracleError<'_>>("for each ")
        .parse(lower.as_str())
        .ok()?;
    let (for_each_clause, _) = tag::<_, _, OracleError<'_>>("for each ")
        .parse(for_each_with_marker)
        .ok()?;
    let pt_text = pt_text.trim();
    let pt_source = nom_tag_lower(pt_text, pt_text, "gets ")
        .or_else(|| nom_tag_lower(pt_text, pt_text, "get "))?;
    let (power, toughness) = parse_pt_mod(pt_source)?;
    let quantity = super::oracle_quantity::parse_for_each_clause_expr(
        strip_trailing_keyword_clause(for_each_clause.trim_end_matches('.')),
    )?;

    let mut modifications = Vec::new();
    push_dynamic_pt_modifications(&mut modifications, power, toughness, quantity);
    (!modifications.is_empty()).then_some(modifications)
}

pub(crate) fn parse_dynamic_pt_in_text(
    lower: &str,
    where_x_expression: Option<&str>,
) -> Option<Vec<ContinuousModification>> {
    // Find "get " or "gets " followed by a variable P/T pattern via nom combinator
    let gets_pos = lower.find("gets ").or_else(|| lower.find("get "))?; // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
    let after_gets = &lower[gets_pos..];
    let after_verb = nom_tag_lower(after_gets, after_gets, "gets ")
        .or_else(|| nom_tag_lower(after_gets, after_gets, "get "))?;

    // CR 613.4c: Parse variable P/T pattern via nom combinator
    let (_, (p_sign, p_is_x, t_sign, t_is_x)) = parse_variable_pt_pattern(after_verb).ok()?;

    if !p_is_x && !t_is_x {
        return None; // No X variable — not a dynamic P/T pattern
    }

    // CR 706.2 + CR 706.3b: "where X is the result" binds X to the preceding
    // die roll's result. `parse_cda_quantity` has no "the result" arm; fall
    // through to `parse_event_context_quantity`, which maps it to
    // `EventContextAmount` (the same channel "that much"/"the result" use).
    //
    // CR 107.3a + CR 107.3i: When no "where X is …" clause is present and the
    // containing activated ability has an {X} (or X) in its cost, X in the
    // effect refers to the value chosen as the ability was activated
    // (CR 107.3a) and every instance of X on the object shares that value
    // (CR 107.3i). The engine models this as `QuantityRef::CostXPaid`,
    // mirroring `parse_cost_x_become_pt_prefix` in
    // `oracle_effect/animation.rs` for the "becomes an X/X creature" animation
    // case. This unblocks +X/+0 and +X/+X pump activations like Kessig Wolf
    // Run whose effect text has no binding clause — the X is bound to the
    // cost, not to a derived quantity.
    let quantity = match where_x_expression {
        Some(wx) => parse_cda_quantity(wx).or_else(|| parse_event_context_quantity(wx))?,
        None => QuantityExpr::Ref {
            qty: QuantityRef::CostXPaid,
        },
    };

    let mut mods = Vec::new();
    if p_is_x {
        let qty = if p_sign < 0 {
            QuantityExpr::Multiply {
                factor: -1,
                inner: Box::new(quantity.clone()),
            }
        } else {
            quantity.clone()
        };
        mods.push(ContinuousModification::AddDynamicPower { value: qty });
    }
    if t_is_x {
        let qty = if t_sign < 0 {
            QuantityExpr::Multiply {
                factor: -1,
                inner: Box::new(quantity),
            }
        } else {
            quantity
        };
        mods.push(ContinuousModification::AddDynamicToughness { value: qty });
    }

    Some(mods)
}

pub(crate) fn parse_base_pt_mod(text: &str) -> Option<(i32, i32)> {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    let pt_text = tp.strip_after("base power and toughness ")?.original.trim();
    parse_pt_mod(pt_text)
}

/// CR 613.4b + CR 208.1: Parse the dynamic base-P/T set value in a
/// "[base] power and [base] toughness [are] each equal to <quantity>" static
/// grant (layer 7b). The grammar is factored per the nom mandate into
/// orthogonal axes — the optional "base " on each characteristic, the optional
/// "are " copula, and the shared " each equal to " suffix — so the arm count is
/// the SUM of per-axis choices, not their product. The trailing quantity is
/// dispatched: "its mana value" reads the recipient's mana value (Animate
/// Artifact class); any other tail routes through the shared CDA quantity
/// grammar so the same building block covers "the number of creatures you
/// control" (Porcelain Gallery) and every other recognized count/aggregate
/// phrase — not just the one card.
pub(crate) fn parse_base_pt_each_equal_dynamic(lower: &str) -> Option<QuantityExpr> {
    type VE<'a> = OracleError<'a>;
    let (_, _, tail) = nom_primitives::scan_preceded(lower, |input| {
        // CR 208.1: which characteristics are set. "base power and base
        // toughness", "base power and toughness", and "power and toughness" are
        // the three observed surface forms; the "base " on the first noun is the
        // only one that varies in the corpus.
        let (input, _) = alt((tag::<_, _, VE<'_>>("base power"), tag("power"))).parse(input)?;
        let (input, _) = tag(" and ").parse(input)?;
        let (input, _) = opt(tag("base ")).parse(input)?;
        let (input, _) = tag("toughness ").parse(input)?;
        // Optional "are " copula ("… are each equal to …").
        let (input, _) = opt(tag("are ")).parse(input)?;
        tag("each equal to ").parse(input)
    })?;
    let tail = tail.trim().trim_end_matches('.').trim();

    // CR 202.3: "its mana value" reads the recipient's own mana value (the
    // animation-grant idiom). Kept as a dedicated arm because its `ObjectScope`
    // is `Recipient` (the granted-to object), which the general CDA grammar does
    // not produce for a bare "its mana value".
    if tail == "its mana value" {
        return Some(QuantityExpr::Ref {
            qty: QuantityRef::ObjectManaValue {
                scope: ObjectScope::Recipient,
            },
        });
    }

    // Everything else routes through the shared CDA quantity grammar so the
    // dynamic base-P/T set composes over every count/aggregate phrase the
    // engine already recognizes (e.g. "the number of creatures you control").
    parse_cda_quantity(tail)
}

/// Back-compat shim: the historical name used by callers that only need the
/// "its mana value" recipient form. Delegates to the generalized parser so the
/// CDA-quantity tail is also accepted at every existing call site.
pub(crate) fn parse_base_pt_mana_value_dynamic(lower: &str) -> Option<QuantityExpr> {
    parse_base_pt_each_equal_dynamic(lower)
}

pub(crate) fn parse_base_pt_side(input: &str) -> nom::IResult<&str, BasePtSide, OracleError<'_>> {
    let (rest, sign) = opt(alt((value(-1i32, tag("-")), value(1i32, tag("+"))))).parse(input)?;
    let sign = sign.unwrap_or(1);
    if let Ok((rest2, _)) = tag::<_, _, OracleError<'_>>("x")(rest) {
        return Ok((rest2, BasePtSide::Dynamic { sign }));
    }
    let (rest, n) = nom_primitives::parse_number.parse(rest)?;
    Ok((
        rest,
        BasePtSide::Fixed {
            value: sign * (n as i32),
        },
    ))
}

/// CR 613.4b + CR 107.3: Parse "base power and toughness X/X" (dynamic form).
/// Returns a `(power_expr, toughness_expr)` pair when the P/T token contains X
/// on either side; otherwise returns `None` (literal N/N is handled by
/// `parse_base_pt_mod`). The X-ref is resolved via the provided
/// `where_x_expression` (for patterns like "base power and toughness X/X,
/// where X is the number of …"), falling back to `CostXPaid` for spell-cast
/// contexts where X is the cost X (e.g., Biomass Mutation).
pub(crate) fn parse_base_pt_dynamic(
    text: &str,
    where_x_expression: Option<&str>,
) -> Option<(QuantityExpr, QuantityExpr)> {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    let pt_tp = tp.strip_after("base power and toughness ")?;
    let (_, (p, _, t)) = (parse_base_pt_side, tag("/"), parse_base_pt_side)
        .parse(pt_tp.lower)
        .ok()?;
    match (p, t) {
        (BasePtSide::Fixed { .. }, BasePtSide::Fixed { .. }) => None,
        (p_side, t_side) => {
            let x_ref = resolve_base_pt_x_ref(where_x_expression)?;
            Some((
                base_pt_side_to_expr(p_side, &x_ref),
                base_pt_side_to_expr(t_side, &x_ref),
            ))
        }
    }
}
