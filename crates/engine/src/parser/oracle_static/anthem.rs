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
            // CR 611.3: "X creatures you control and Y ..." — "you control" here
            // ends only the FIRST conjunct of a compound subject, not the whole
            // subject. A well-formed single-subject predicate always starts with
            // a verb (get/gets/has/have/gain/gains) right after "you control ",
            // never the conjunction "and" — so a leading "and " means this
            // positional split guessed wrong and the real subject is compound
            // (Dune Chanter: "Lands you control and land cards you own that
            // aren't on the battlefield are Deserts..."). Decline so dispatch
            // falls through to a handler that resolves the whole compound
            // subject correctly (`parse_contextual_continuous_subject_static` for
            // get/has predicates, `parse_subject_additive_type_static` for
            // are/is predicates) instead of treating the second conjunct as
            // unparsed predicate noise that `parse_continuous_gets_has`'s lenient
            // `parse_additive_type_clause_modifications` fallback can scan past
            // and silently drop.
            if tag::<_, _, OracleError<'_>>("and ")
                .parse(after_prefix.trim_start())
                .is_ok()
            {
                return None;
            }
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
                // CR 105.1 + CR 205.4a: a compound color/supertype descriptor
                // ("Black legendary", "Legendary black", ...) — the Legends
                // banding-land cycle (Unholy Citadel, Seafarer's Quay,
                // Adventurers' Guildhouse, Cathedral of Serra, Mountain
                // Stronghold): "<Color> legendary creatures you control have
                // \"bands with other legendary creatures.\"" (issue #6332).
                // None of the bespoke arms above recognize a compound
                // descriptor, so delegate the full subject to
                // `parse_type_phrase` — the general subject-filter grammar
                // that already composes a color prefix and a supertype prefix
                // in either order (see its leading and post-negation
                // supertype/color passes in `oracle_target.rs`) — rather than
                // growing a second bespoke color+supertype combinator here.
                //
                // Accept ONLY when the fully-consumed result carries BOTH a
                // `HasColor` and a `HasSupertype` property — i.e. genuinely a
                // color+supertype compound, not merely "some descriptor
                // `parse_type_phrase` happens to accept." A full-consumption
                // check alone is not narrow enough: descriptors this function
                // has no OTHER arm for (e.g. Saryth, the Viper's Fang / Augusta,
                // Dean of Order's "Other tapped creatures you control .../Other
                // untapped creatures you control ...") also fully consume
                // through `parse_type_phrase`, and unconditionally accepting
                // them here would silently reroute cards that are unrelated to
                // this fix onto a different (and untested, for them) filter
                // path. Requiring both properties scopes acceptance to exactly
                // the class this fix targets.
                } else {
                    let subject_and_type = tp.original[..creatures_pos + " creatures".len()].trim();
                    let (compound_filter, remainder) = parse_type_phrase(subject_and_type);
                    match compound_filter {
                        TargetFilter::Typed(typed)
                            if remainder.trim().is_empty()
                                && typed
                                    .properties
                                    .iter()
                                    .any(|p| matches!(p, FilterProp::HasColor { .. }))
                                && typed
                                    .properties
                                    .iter()
                                    .any(|p| matches!(p, FilterProp::HasSupertype { .. })) =>
                        {
                            TargetFilter::Typed(typed.controller(ControllerRef::You))
                        }
                        _ => return None,
                    }
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
            // CR 611.3: same compound-subject guard as the "creatures you
            // control" branch above — see its comment for the full rationale.
            if tag::<_, _, OracleError<'_>>("and ")
                .parse(after_prefix.trim_start())
                .is_ok()
            {
                return None;
            }
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
                // CR 111.1 + CR 111.6 + CR 109.5: "[Creature ]tokens you control" —
                // token-ness is an object property (CR 111.1), not a subtype, and a
                // token can be any card type (CR 111.6), so this must span
                // Treasure/Clue/Food tokens as well as creature tokens. Precedes the
                // capitalized-subtype fallback, which would otherwise mis-synthesize a
                // bogus `Subtype("Token")` creature-only filter (Jaheira, Friend of the
                // Forest).
                } else if let Some(filter) =
                    parse_token_you_control_descriptor(&TextPair::new(descriptor, &desc_lower))
                {
                    filter
                // CR 205.2a + CR 110.1: "Permanents you control" (and the bare
                // "Creature" card-type word) name a type, not a subtype — resolve
                // to the all-permanents base before the capitalized-subtype
                // fallback fabricates a zero-match `Subtype("Permanent")`.
                } else if let Some(base) = bulk_type_subject_base(descriptor) {
                    TargetFilter::Typed(base.controller(ControllerRef::You))
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
            } else if let Some(base) = bulk_type_subject_base(desc_remaining) {
                // CR 205.2a + CR 110.1: bulk permanent/creature noun after a
                // combat-status prefix ("Untapped permanents you control") — base
                // type, not a subtype.
                TargetFilter::Typed(base.controller(ControllerRef::You).properties(extra_props))
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

/// CR 611.3a + CR 109.5 + CR 301.5a: Peel a leading "During your turn, as long
/// as &lt;condition&gt;, " prefix and attach it — as an intrinsic conditional
/// gate (CR 611.3a, re-evaluated each layer recompute) — to the
/// recursively-parsed remainder static. Cloud, Planet's Champion:
/// "During your turn, as long as ~ is equipped, it has double strike and
/// indestructible" → the double-strike/indestructible static gains
/// `condition: And { [DuringYourTurn, SourceIsEquipped] }`.
///
/// Both prefixes are REQUIRED. Bare "As long as X, Y" statics are already owned
/// by `parse_conditional_static` (later in dispatch), and plain "During your
/// turn, Y" statics by the dedicated during-your-turn handler; requiring the
/// full compound keeps both untouched (no shadowing of either class). When the
/// remainder does not parse to a clean subject static (the counter-animation
/// "…, it's a P/T and has …" form), returns `None` so the dispatcher's
/// `parse_compound_turn_counter_animation` still claims it.
fn parse_leading_condition_peel(tp: &TextPair) -> Option<StaticDefinition> {
    let after_turn = nom_tag_tp(tp, "during your turn, ")?;
    let after_gate = nom_tag_tp(&after_turn, "as long as ")?;
    // First-comma split: "<condition>, <remainder>".
    let (body_tp, remainder_tp) = after_gate.split_around(", ")?;
    // CR 109.5: "during your turn" binds to the source object's controller.
    let condition = parse_static_condition(body_tp.original.trim())?;
    let leading = StaticCondition::And {
        conditions: vec![StaticCondition::DuringYourTurn, condition],
    };

    // Recurse on the remainder; on failure return None so specialized parsers run.
    let mut def = parse_subject_continuous_static(remainder_tp.original.trim())?;
    // CR 611.3a: compose with any condition the remainder itself carried rather
    // than dropping one (mirrors `parse_conditional_static`).
    def.condition = Some(match def.condition.take() {
        Some(existing) => StaticCondition::And {
            conditions: vec![leading, existing],
        },
        None => leading,
    });
    def.description = Some(tp.original.to_string());
    Some(def)
}

pub(crate) fn parse_subject_continuous_static(text: &str) -> Option<StaticDefinition> {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);

    // CR 611.3a + CR 109.5 + CR 301.5a: peel a leading "During your turn, as long
    // as <cond>, " condition prefix onto the recursively-parsed remainder static
    // (Cloud, Planet's Champion). Runs first so the intrinsic conditional gate is
    // attached; the counter-animation remainder form returns None and falls
    // through to `parse_compound_turn_counter_animation`.
    if let Some(def) = parse_leading_condition_peel(&tp) {
        return Some(def);
    }

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

    // CR 604.1: Strip suffix turn conditions from the ORIGINAL-case predicate
    // (not `pred_lower`) — "has first strike during your turn" → "has first
    // strike" + DuringYourTurn. The condition phrase is lowercase in the original
    // too, so `strip_suffix_turn_condition` still matches, and the retained
    // predicate keeps its printed case: a granted ability's serialized,
    // user-visible `description` must read "{T}: Add {G}.", not "{t}: add {g}."
    // (issue #5599, Brightcap Badger).
    let (effective_predicate, suffix_condition) = strip_suffix_turn_condition(predicate);

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
    // CR 613.1: a distributive "each" trailing a multi-subject list ("this
    // creature and enchanted creature each get +1/+1 …", Eidolon of Countless
    // Battles) shares one predicate across every named subject. Strip the marker
    // from both views so the compound subject parses; the `Or` union below
    // already applies the predicate to each branch. Stripping each view with the
    // same combinator keeps them aligned without slicing one by the other's byte
    // length (safe when case-folding changes width).
    let subject_lower = strip_trailing_distributive_each(subject_lower);
    let subject_original = strip_trailing_distributive_each(subject_original);

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
    // CR 702.95: Soulbond. The paired reminder-text grant — "As long as ~ is
    // paired with another creature, each of those creatures <predicate>." — is a
    // CR 613.1f layer-6 ability-adding effect applied to BOTH paired creatures
    // (SourceOrPaired). Split the pairing frame off the granted predicate with
    // TextPair so the predicate keeps its ORIGINAL case: a quoted granted
    // ability's mana symbols (`{1}{U}`) must reach the cost parser un-lowercased.
    // allow-noncombinator: TextPair dual-string structural strip preserving original case
    let after = tp.strip_prefix("as long as ")?;
    let (condition, predicate) = after
        .split_around(", each of those creatures ")
        .or_else(|| after.split_around(", both creatures "))?;
    if !matches_soulbond_paired_condition(condition.lower) {
        return None;
    }
    let predicate = strip_granted_predicate_period(&predicate);
    let mut def = parse_continuous_gets_has(
        predicate.original,
        TargetFilter::SourceOrPaired,
        description,
    )?;
    def.condition = Some(StaticCondition::SourceIsPaired);
    Some(def)
}

/// Trim a granted predicate's sentence-ending period, but leave a quoted ability
/// intact. A quoted activated (CR 602.1) or triggered (CR 603.1) granted ability
/// terminates with its period INSIDE the closing quote (`has "{1}{U}: ... your
/// control."`), so a predicate ending in `"` has no outside period to strip —
/// only the bare keyword/P-T forms (`has flying.`, `gets +1/+1.`) carry an outer
/// period. A period-terminated `take_until(".")` would instead sever the quote
/// at that inner period and drop the whole granted ability.
fn strip_granted_predicate_period<'a>(predicate: &TextPair<'a>) -> TextPair<'a> {
    let predicate = predicate.trim_end();
    // allow-noncombinator: punctuation inspection on a pre-tokenized chunk, not parse dispatch
    if predicate.ends_with("\"") {
        predicate
    } else {
        predicate.trim_end_matches('.')
    }
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
///    combat-state siblings "attacking"/"blocking"/"blocked" and the compound
///    "attacking or blocking" (which `~ is …` lowers to
///    `Or([SourceIsAttacking, SourceIsBlocking])`)
///    (CR 508.1k / 509.1g / 509.1h). "it" is otherwise overloaded: "it's your
///    turn" is impersonal (a turn reference, not the source); "it's a Wall" /
///    "it's red" / "it's legendary" are type/characteristic gates with their own
///    parse paths. Rewriting those would break or mis-bind them, so they are
///    left untouched. The match is EXACT, so "it's attacking alone" keeps its
///    trailing word and falls through to `SourceAttackingAlone` rather than
///    collapsing to `SourceIsAttacking`.
///
/// Returns the condition unchanged when neither guard matches.
pub(crate) fn rewrite_self_pronoun_subject(condition: &str) -> String {
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
                | "attacking or blocking"
                | "modified"
                | "equipped"
                | "enchanted"
        ) {
            return format!("~ is {}", rest.trim());
        }
    }
    // CR 400.7: the non-contraction "it <verb>" self-state form — "it entered
    // this turn" / "it entered the battlefield this turn" (Crew Captain's
    // indestructible gate, Drownyard Behemoth's / Thrasta's / Zurgo and
    // Ojutai's hexproof gate). Strip the bound-pronoun subject and re-emit the
    // canonical "~ entered …" templating the context-free grammar resolves to
    // SourceEnteredThisTurn. Exact match on the tail; only reached on the
    // SelfRef path, so the attached-subject "it" stays an honest gap.
    if let Some(rest) = nom_tag_lower(&lower, &lower, "it entered ") {
        if matches!(rest.trim(), "this turn" | "the battlefield this turn") {
            return format!("~ entered {}", rest.trim());
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

    // CR 611.3a: Split a trailing " unless [condition]" gate, mirroring the
    // " as long as " form above. An "unless <cond>" rider grants the modification
    // precisely when <cond> is FALSE, so the parsed condition is wrapped in `Not`
    // (Tadeas, Juniper Ascendant: "has hexproof unless it's attacking" → AddKeyword
    // gated on Not(SourceIsAttacking)). Only peel when the split sits OUTSIDE a
    // quoted granted ability — a granted ability's own inner "unless" (e.g. "gains
    // 'counter target spell unless its controller pays {1}'") must stay with the
    // quoted text; balanced double quotes in the body signal the split is outside
    // any "...". As with the " as long as " form, the self-pronoun condition
    // subject ("it's attacking"/"it's tapped") is resolved to the source only for
    // SelfRef grants — an attached-subject "it" keeps its enchanted/equipped
    // binding and stays an honest gap.
    if let Some((before_cond, after_cond)) = tp
        .split_around(" unless ")
        .filter(|(body, _)| body.original.chars().filter(|&c| c == '"').count() % 2 == 0)
    {
        let continuous_text = before_cond.original;
        let condition_text = after_cond.original.trim().trim_end_matches('.');
        if let Some(mut def) =
            parse_continuous_gets_has(continuous_text, affected.clone(), description)
        {
            let typed = if matches!(affected, TargetFilter::SelfRef) {
                parse_static_condition(&rewrite_self_pronoun_subject(condition_text))
            } else {
                parse_static_condition(condition_text)
            };
            let condition = match typed {
                Some(inner) => StaticCondition::Not {
                    condition: Box::new(inner),
                },
                None => StaticCondition::Not {
                    condition: Box::new(StaticCondition::Unrecognized {
                        text: format!("unless {condition_text}"),
                    }),
                },
            };
            def.condition = Some(condition);
            return Some(def);
        }
    }

    // CR 613.4c: Handle repeated dynamic pump terms — "gets +N/+M for each X and
    // +P/+Q for each Y" (Eidolon of Countless Battles) — where each term scales
    // by its own count. Try this before the single-"for each" path so the second
    // term isn't silently dropped and the pump collapsed to a fixed value.
    if let Some(modifications) = parse_repeated_for_each_pt_modifications(text) {
        return Some(
            StaticDefinition::continuous()
                .affected(affected)
                .modifications(modifications)
                .description(description.to_string()),
        );
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
                    // CR 205.1b + CR 604.1: also recover a trailing type-addition
                    // ("and is an Assassin in addition to its other types",
                    // Reaper's Scythe) or a trailing quoted-ability grant ("and has
                    // \"{T}, Sacrifice a creature: ...\"", Rakdos Riteknife) after the
                    // dynamic pump — the keyword path above only recovers trailing
                    // keywords. Both scanners no-op when their pattern is absent.
                    if let Some(type_mods) = parse_additive_type_clause_modifications(description) {
                        modifications.extend(type_mods);
                    }
                    modifications.extend(parse_quoted_ability_modifications(description));
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

/// CR 613.4c: A compound of repeated dynamic pump terms — "gets +N/+M for each X
/// and +P/+Q for each Y" (Eidolon of Countless Battles) — where each term scales
/// by its own count. Splits on the " and " that introduces another "+n/+m for
/// each" pump term (so a single term's embedded "for each A and B" count-list
/// stays with the single-term path) and accumulates every term's dynamic
/// modifications. Returns `None` unless at least two whole pump terms parse, so
/// the single-term path keeps ownership of every non-repeated case.
fn parse_repeated_for_each_pt_modifications(text: &str) -> Option<Vec<ContinuousModification>> {
    let lower = text.to_lowercase();
    let mut terms: Vec<String> = Vec::new();
    let mut current = String::new();
    for (index, segment) in split_on_and(&lower).into_iter().enumerate() {
        if index > 0 && segment_starts_pump_term(segment) {
            terms.push(std::mem::take(&mut current));
            current.push_str(segment);
        } else {
            if !current.is_empty() {
                current.push_str(" and ");
            }
            current.push_str(segment);
        }
    }
    if !current.is_empty() {
        terms.push(current);
    }
    if terms.len() < 2 {
        return None;
    }

    let mut modifications = Vec::new();
    for term in &terms {
        // `parse_dynamic_for_each_pt_modifications` expects the "gets"/"get" verb;
        // only the first term carries it once the predicate is split.
        let owned;
        let term = if segment_has_gets_verb(term) {
            term.as_str()
        } else {
            owned = format!("gets {term}");
            owned.as_str()
        };
        modifications.extend(parse_dynamic_for_each_pt_modifications(term)?);
    }
    (!modifications.is_empty()).then_some(modifications)
}

/// Split `s` into its " and "-delimited segments via a forward `take_until`
/// scan (the combinator form of `str::split(" and ")`).
fn split_on_and(s: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut remaining = s;
    while let Ok((rest, before)) =
        terminated(take_until::<_, _, OracleError<'_>>(" and "), tag(" and ")).parse(remaining)
    {
        segments.push(before);
        remaining = rest;
    }
    segments.push(remaining);
    segments
}

/// True iff `segment` (already lowercased) opens with a "gets"/"get" verb.
fn segment_has_gets_verb(segment: &str) -> bool {
    alt((tag::<_, _, OracleError<'_>>("gets "), tag("get ")))
        .parse(segment)
        .is_ok()
}

/// True iff `segment` (already lowercased) begins a "+N/+M for each …" pump term:
/// an optional "gets"/"get" verb, a P/T modifier, then " for each ". Used to tell
/// a repeated-pump term boundary apart from an " and " inside a count clause.
fn segment_starts_pump_term(segment: &str) -> bool {
    preceded(
        opt(alt((tag::<_, _, OracleError<'_>>("gets "), tag("get ")))),
        preceded(nom_primitives::parse_pt_modifier, tag(" for each ")),
    )
    .parse(segment.trim_start())
    .is_ok()
}

/// Strip a trailing distributive " each" ("this creature and enchanted creature
/// each") so a multi-subject list parses. Only strips when " each" is the final
/// token, and returns `s` unchanged otherwise, so applying it to both the lower
/// and original views of a subject keeps them aligned without length slicing.
fn strip_trailing_distributive_each(s: &str) -> &str {
    match terminated(take_until::<_, _, OracleError<'_>>(" each"), tag(" each")).parse(s) {
        Ok(("", before)) => before,
        _ => s,
    }
}

/// Split a compound "+X/+Y" pump binding clause `<A>, and Y is <B>` into the
/// X-axis expression (`A`) and Y-axis expression (`B`) when the two axes bind to
/// different quantities (Aspect of Wolf: "X is half the number of Forests you
/// control, rounded down, and Y is half the number of Forests you control,
/// rounded up"). Returns `None` for the common single-quantity clause. `wx`
/// reaches here in original case (printed "and Y is"), so it is lowercased
/// before locating the boundary via the `split_once_on` combinator. The
/// separator `", and y is "` consumes the joining comma, and the sentence period
/// is already stripped upstream by `strip_trailing_where_x`, so each half feeds
/// the case-insensitive `parse_cda_quantity` after a plain whitespace trim.
fn split_x_and_y_where_clause(wx: &str) -> Option<(String, String)> {
    let lower = wx.to_lowercase();
    let (_, (x_expr, y_expr)) = nom_primitives::split_once_on(&lower, ", and y is ").ok()?;
    Some((x_expr.trim().to_string(), y_expr.trim().to_string()))
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

    // CR 613.4c: Parse the variable P/T pattern. Each axis is a fixed magnitude,
    // the variable X, or (toughness only, in a distinct "+X/+Y" pump) the
    // variable Y.
    let (_, (p_sign, p_mag, t_sign, t_mag)) = parse_variable_pt_pattern(after_verb).ok()?;
    let p_is_dynamic = matches!(p_mag, PtAxisMag::VarX | PtAxisMag::VarY);
    let t_is_dynamic = matches!(t_mag, PtAxisMag::VarX | PtAxisMag::VarY);

    if !p_is_dynamic && !t_is_dynamic {
        return None; // No variable axis — not a dynamic P/T pattern
    }

    // A distinct-letter "+X/+Y" pump (X on power, Y on toughness) is supported
    // ONLY when a paired "where X is <A>, and Y is <B>" binding was structurally
    // parsed — its two axes carry independent bindings. Without one the pattern
    // stays UNSUPPORTED rather than synthesizing from cost-X: Snowblind's
    // "gets -X/-Y" (X/Y defined by later conditional sentences, no `{X}` cost)
    // must not emit a bogus `-CostXPaid/-CostXPaid` static.
    let is_distinct_xy = p_mag == PtAxisMag::VarX && t_mag == PtAxisMag::VarY;
    // `Y` is not a generic cost variable in this grammar. The only supported
    // Y-bearing form is Aspect of Wolf's ordered `+X/+Y` pair, whose distinct
    // bindings are carried by the structured where-clause below. Reject every
    // other placement so `+Y/+X` or `+Y/+Y` cannot silently borrow `CostXPaid`
    // or an X-only binding.
    if (matches!(p_mag, PtAxisMag::VarY) || matches!(t_mag, PtAxisMag::VarY)) && !is_distinct_xy {
        return None;
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
    // Intensity and the other derived quantities live in the shared
    // `parse_quantity_ref` combinator (oracle_nom/quantity.rs), which
    // `parse_cda_quantity` delegates to. Most pumps bind X to a single quantity
    // applied to both axes; a "+X/+Y" pump whose clause reads "where X is <A> and
    // Y is <B>" (Aspect of Wolf) binds each axis to its own quantity, so the
    // clause is split on " and y is " and each half is parsed independently.
    let resolve_quantity =
        |wx: &str| parse_cda_quantity(wx).or_else(|| parse_event_context_quantity(wx));
    let (p_quantity, t_quantity) = if is_distinct_xy {
        // Require the paired binding; a "+X/+Y" without it is unsupported.
        let (x_expr, y_expr) = split_x_and_y_where_clause(where_x_expression?)?;
        (resolve_quantity(&x_expr)?, resolve_quantity(&y_expr)?)
    } else {
        match where_x_expression {
            Some(wx) => {
                let q = resolve_quantity(wx)?;
                (q.clone(), q)
            }
            // CR 107.3a + CR 107.3i: no binding clause → X is the value chosen as
            // the ability's cost-X was paid (Kessig Wolf Run).
            None => {
                let q = QuantityExpr::Ref {
                    qty: QuantityRef::CostXPaid,
                };
                (q.clone(), q)
            }
        }
    };

    let mut mods = Vec::new();
    // CR 613.4c layer 7c: the dynamic axis grants an X-valued modification; a
    // fixed nonzero axis grants a constant modification alongside it (the mixed
    // "+X/+1" case). A fixed `0` axis contributes nothing.
    match p_mag {
        PtAxisMag::VarX | PtAxisMag::VarY => {
            let value = if p_sign < 0 {
                QuantityExpr::Multiply {
                    factor: -1,
                    inner: Box::new(p_quantity),
                }
            } else {
                p_quantity
            };
            mods.push(ContinuousModification::AddDynamicPower { value });
        }
        PtAxisMag::Fixed(n) if n != 0 => {
            mods.push(ContinuousModification::AddPower { value: p_sign * n })
        }
        PtAxisMag::Fixed(_) => {}
    }
    match t_mag {
        PtAxisMag::VarX | PtAxisMag::VarY => {
            let value = if t_sign < 0 {
                QuantityExpr::Multiply {
                    factor: -1,
                    inner: Box::new(t_quantity),
                }
            } else {
                t_quantity
            };
            mods.push(ContinuousModification::AddDynamicToughness { value });
        }
        PtAxisMag::Fixed(n) if n != 0 => {
            mods.push(ContinuousModification::AddToughness { value: t_sign * n })
        }
        PtAxisMag::Fixed(_) => {}
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

#[cfg(test)]
mod l02_bb5_leading_condition_peel_tests {
    use super::*;

    /// Positive: Cloud, Planet's Champion — the "During your turn, as long as ~
    /// is equipped, ..." compound peels into an intrinsic conditional static
    /// gated on `And { [DuringYourTurn, SourceIsEquipped] }` (was fully
    /// swallowed pre-fix: `statics: null`). REVERT-PROBE: dropping the peel
    /// leaves `condition == None` / the whole static unparsed.
    #[test]
    fn cloud_leading_condition_peel_attaches_compound_condition() {
        let def = parse_subject_continuous_static(
            "During your turn, as long as ~ is equipped, it has double strike and indestructible",
        )
        .expect("Cloud's double-strike/indestructible static must parse");
        assert_eq!(
            def.condition,
            Some(StaticCondition::And {
                conditions: vec![
                    StaticCondition::DuringYourTurn,
                    StaticCondition::SourceIsEquipped,
                ],
            }),
            "peel must attach And{{DuringYourTurn, SourceIsEquipped}}"
        );
        assert!(
            !def.modifications.is_empty(),
            "the recursed remainder must still grant the keywords"
        );
        assert_eq!(
            def.description.as_deref(),
            Some("During your turn, as long as ~ is equipped, it has double strike and indestructible"),
            "full description must be restored after the peel"
        );
    }

    /// F1 non-vacuity (team-lead mandate, `parser-coverage-regression-ci-only`):
    /// every live static whose Oracle text starts "During your turn," and
    /// contains get/has/gain but has NO "as long as" must NOT be claimed by the
    /// leading-condition peel — those belong to the dedicated during-your-turn
    /// dispatch handler downstream, unchanged (byte-identical shape). `as long
    /// as ` is MANDATORY in the peel; these lines lack it, so the peel returns
    /// None.
    ///
    /// REVERT-PROBE (this is the F1 discriminator): making `as long as `
    /// OPTIONAL — the reviewed-out bug where a bare "during your turn, " alone
    /// fires the peel — makes the peel recurse on the anthem remainder and
    /// return `Some` for the anthem-shaped lines (e.g. "creatures you control
    /// get +2/+0"), so these per-card asserts flip to fail. Aggregate
    /// REGRESSED=0 would not catch a lose-N-here / gain-N-elsewhere swap; the
    /// per-card `is_none()` does. Enumerated via jq over `data/card-data.json`
    /// (50 cards at impl time).
    #[test]
    fn during_your_turn_without_as_long_as_is_not_peeled() {
        const SHADOWED: &[&str] = &[
            "During your turn, this creature has first strike.",
            "During your turn, commanders you control have indestructible.",
            "During your turn, outlaws you control have first strike.",
            "During your turn, Avatar Kyoshi has hexproof.",
            "During your turn, each creature assigns combat damage equal to its toughness rather than its power.",
            "During your turn, creatures you control have hexproof.",
            "During your turn, each non-Equipment artifact and non-Aura enchantment you control with mana value 4 or greater is a 4/4 Elemental creature in addition to its other types and has indestructible, haste, and \"Whenever this creature deals combat damage to a player, draw a card.\"",
            "During your turn, equipped creature has hexproof and can't be blocked.",
            "During your turn, this creature has lifelink.",
            "During your turn, Cait has indestructible.",
            "During your turn, Colossus has indestructible.",
            "During your turn, this creature has flying.",
            "During your turn, Gideon Blackblade is a 4/4 Human Soldier creature with indestructible that's still a planeswalker.",
            "During your turn, creatures you control get +2/+0.",
            "During your turn, this creature gets +0/+2.",
            "During your turn, Kefka has indestructible.",
            "During your turn, equipped creature gets +1/+0 and has first strike.",
            "During your turn, this creature has lifelink.",
            "During your turn, other creatures you control get +1/+0.",
            "During your turn, Mounts and Vehicles you control have hexproof.",
            "During your turn, creatures you control have first strike and equip abilities you activate cost {1} less to activate.",
            "During your turn, this creature has hexproof.",
            "During your turn, creature tokens you control get +1/+4.",
            "During your turn, this creature gets +2/+0 and has first strike.",
            "During your turn, equipped creature gets +2/+0 and has first strike.",
            "During your turn, Radha has first strike.",
            "During your turn, attacking creatures get +1/+0.",
            "During your turn, each instant and sorcery card in your graveyard has flashback. Its flashback cost is equal to its mana cost.",
            "During your turn, Shocker has first strike.",
            "During your turn, this creature gets +2/+0.",
            "During your turn, Allies you control have double strike and lifelink.",
            "During your turn, creatures and planeswalkers you control have lifelink.",
            "During your turn, Spider-Girl has flying.",
            "During your turn, this creature gets +0/+2.",
            "During your turn, creatures you control get +1/+0 and have trample.",
            "During your turn, Sun-Spider has flying.",
            "During your turn, The River Warlock has flying, lifelink, and indestructible.",
            "During your turn, this creature gets +2/+2.",
            "During your turn, Yuna and enchantment creatures you control have trample, lifelink, and ward {2}.",
        ];
        for line in SHADOWED {
            let lower = line.to_lowercase();
            let tp = TextPair::new(line, &lower);
            assert!(
                parse_leading_condition_peel(&tp).is_none(),
                "leading-condition peel must NOT claim a plain during-your-turn static (no 'as long as'): {line:?}"
            );
        }
    }

    /// Regression (plan Risk #1 / test #7): the counter-animation form
    /// ("During your turn, as long as ~ has … counters …, it's a P/T … and has
    /// …") must NOT be claimed by `parse_subject_continuous_static` — its "'s a"
    /// remainder has no clean subject, so the peel recurses to None and returns
    /// None, letting `parse_compound_turn_counter_animation` (later in dispatch)
    /// claim it. Kaito, Bane of Nightmares is the live case
    /// (`parse_compound_static_kaito_animation` in `tests.rs` guards the
    /// downstream animation shape). REVERT-PROBE: if the peel wrongly claimed
    /// the remainder, this returns `Some` and the assert flips.
    #[test]
    fn counter_animation_falls_through_the_peel() {
        let kaito = "During your turn, as long as ~ has one or more loyalty counters on him, he's a 3/4 Ninja creature and has hexproof.";
        assert!(
            parse_subject_continuous_static(kaito).is_none(),
            "counter-animation must fall through to parse_compound_turn_counter_animation"
        );
        // The specialized parser still claims it (guards the fallthrough target).
        assert!(
            parse_compound_turn_counter_animation(&kaito.to_lowercase(), kaito).is_some(),
            "parse_compound_turn_counter_animation must still handle Kaito's animation"
        );
    }
}
