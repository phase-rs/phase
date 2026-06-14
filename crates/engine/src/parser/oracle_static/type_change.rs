// CR 613.3d (Layer 4) — type-changing static abilities.

#[allow(unused_imports)]
use super::prelude::*;
#[allow(unused_imports)]
use super::support::*;

/// CR 607.2d: Parse a self-chosen type static ability line.
pub(crate) fn parse_self_chosen_type_static(input: &str) -> OracleResult<'_, ChosenSubtypeKind> {
    let (input, kind) = alt((
        value(ChosenSubtypeKind::BasicLandType, tag("~ is")),
        value(ChosenSubtypeKind::CreatureType, tag("this creature is")),
        value(ChosenSubtypeKind::BasicLandType, tag("this land is")),
        value(ChosenSubtypeKind::BasicLandType, tag("this permanent is")),
    ))
    .parse(input)?;
    let (input, _) = tag(" the chosen type").parse(input)?;
    let (input, _) = opt(preceded(
        tag(" in addition to "),
        terminated(alt((tag("its"), tag("their"))), tag(" other types")),
    ))
    .parse(input)?;
    let (input, _) = opt(tag(".")).parse(input)?;
    eof.parse(input)?;
    Ok((input, kind))
}

pub(crate) fn parse_enchanted_land_chosen_type_static(
    tp: &TextPair<'_>,
    description: &str,
) -> Option<StaticDefinition> {
    let ((), _) = nom_on_lower(
        tp.original,
        tp.lower,
        parse_enchanted_land_chosen_type_static_sentence,
    )?;

    Some(
        StaticDefinition::continuous()
            .affected(TargetFilter::Typed(
                TypedFilter::land().properties(vec![FilterProp::EnchantedBy]),
            ))
            .modifications(vec![ContinuousModification::SetChosenBasicLandType])
            .description(description.to_string()),
    )
}

pub(crate) fn parse_enchanted_land_chosen_type_static_sentence(
    input: &str,
) -> OracleResult<'_, ()> {
    let (input, _) = tag("enchanted land is the chosen type").parse(input)?;
    let (input, _) = opt(alt((
        tag(" and loses its other land types"),
        tag(" and loses its other types"),
    )))
    .parse(input)?;
    let (input, _) = opt(tag(".")).parse(input)?;
    eof.parse(input)?;
    Ok((input, ()))
}

/// CR 607.2d: Subject scope for "<[scope]> are/is the chosen [creature] type in
/// addition to [their/its] other types" statics (Arcane Adaptation, Lifecraft
/// Engine, Xenograft, …).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChosenCreatureTypeStaticScope {
    Creatures,
    EachCreature,
    VehicleCreatures,
}

impl ChosenCreatureTypeStaticScope {
    fn target_filter(self) -> TargetFilter {
        match self {
            Self::Creatures | Self::EachCreature => {
                TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You))
            }
            // CR 301.7 + CR 607.2d: Lifecraft Engine grants a creature subtype to
            // Vehicle permanents you control — not the Creature card type.
            Self::VehicleCreatures => TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Subtype("Vehicle".to_string()))
                    .controller(ControllerRef::You),
            ),
        }
    }
}

pub(crate) fn parse_arcane_adaptation_chosen_type_static(
    tp: &TextPair<'_>,
    description: &str,
) -> Option<StaticDefinition> {
    let (scope, _) = nom_on_lower(
        tp.original,
        tp.lower,
        parse_chosen_creature_type_static_sentence_with_scope,
    )?;

    Some(
        StaticDefinition::continuous()
            .affected(scope.target_filter())
            .modifications(vec![ContinuousModification::AddChosenSubtype {
                kind: ChosenSubtypeKind::CreatureType,
            }])
            .description(description.to_string()),
    )
}

fn parse_chosen_creature_type_static_sentence_with_scope(
    input: &str,
) -> OracleResult<'_, ChosenCreatureTypeStaticScope> {
    let (input, scope) = parse_chosen_creature_type_static_scope_body(input)?;
    Ok((input, scope))
}

pub(crate) fn parse_chosen_creature_type_static_prefix(input: &str) -> OracleResult<'_, ()> {
    let (input, _) = parse_chosen_creature_type_static_scope_body(input)?;
    Ok((input, ()))
}

fn parse_chosen_creature_type_static_scope_body(
    input: &str,
) -> OracleResult<'_, ChosenCreatureTypeStaticScope> {
    let (input, (pronoun, scope)) = parse_chosen_creature_type_static_subject(input)?;
    let (input, _) = alt((
        tag(" the chosen type in addition to "),
        tag(" the chosen creature type in addition to "),
    ))
    .parse(input)?;
    let (input, _) = tag(pronoun).parse(input)?;
    let (input, _) = tag(" other types").parse(input)?;
    let (input, _) = opt(tag(".")).parse(input)?;
    Ok((input, scope))
}

pub(crate) fn parse_chosen_creature_type_static_subject(
    input: &str,
) -> OracleResult<'_, (&'static str, ChosenCreatureTypeStaticScope)> {
    alt((
        value(
            ("their", ChosenCreatureTypeStaticScope::Creatures),
            tag("creatures you control are"),
        ),
        value(
            ("its", ChosenCreatureTypeStaticScope::EachCreature),
            tag("each creature you control is"),
        ),
        value(
            ("their", ChosenCreatureTypeStaticScope::VehicleCreatures),
            tag("vehicle creatures you control are"),
        ),
    ))
    .parse(input)
}

// CR 613.1d + CR 205.3m: "<creatures you control are> every creature type" —
// Layer 4 type-changing effect that adds every creature type (CR 205.3m) to each
// creature the controller has on the battlefield. Maskwood Nexus is the
// canonical printing; the static is the class of "<your creatures> are every
// creature type" effects, paralleling `parse_arcane_adaptation_chosen_type_static`
// for "the chosen type". Maskwood's "The same is true for creature spells you
// control and creature cards you own that aren't on the battlefield" tail is
// stripped upstream by `oracle.rs` (it's reported as `Unimplemented` because
// continuous effects on non-battlefield zones aren't currently modeled).
pub(crate) fn parse_every_creature_type_static(
    tp: &TextPair<'_>,
    description: &str,
) -> Option<StaticDefinition> {
    let ((), _) = nom_on_lower(
        tp.original,
        tp.lower,
        parse_every_creature_type_static_sentence,
    )?;

    Some(
        StaticDefinition::continuous()
            .affected(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::You),
            ))
            .modifications(vec![ContinuousModification::AddAllCreatureTypes])
            .description(description.to_string()),
    )
}

pub(crate) fn parse_every_creature_type_static_sentence(input: &str) -> OracleResult<'_, ()> {
    let (input, _) = parse_every_creature_type_static_prefix(input)?;
    let (input, _) = eof.parse(input)?;
    Ok((input, ()))
}

pub(crate) fn parse_every_creature_type_static_prefix(input: &str) -> OracleResult<'_, ()> {
    let (input, _) = parse_chosen_creature_type_static_subject(input)?;
    let (input, _) = tag(" every creature type").parse(input)?;
    let (input, _) = opt(tag(".")).parse(input)?;
    Ok((input, ()))
}

pub(crate) fn parse_collection_counter_play_permission_static(
    tp: &TextPair<'_>,
    description: &str,
) -> Option<StaticDefinition> {
    let ((), _) = nom_on_lower(tp.original, tp.lower, |input| {
        let (input, _) = tag("once each turn, you may play a card from exile with a collection counter on it if it was exiled by an ability you controlled").parse(input)?;
        let (input, _) = alt((
            tag(", and mana of any type can be spent to cast that spell"),
            tag(", and you may spend mana as though it were mana of any color to cast it"),
        ))
        .parse(input)?;
        let (input, _) = opt(tag(".")).parse(input)?;
        let (input, _) = eof.parse(input)?;
        Ok((input, ()))
    })?;

    Some(
        StaticDefinition::new(StaticMode::LinkedCollectionCounterPlayPermission)
            .description(description.to_string()),
    )
}

/// CR 205.1 / CR 205.3a: Extract additive-type modifications from a predicate
/// like `"are Food artifacts in addition to their other types"` or its
/// compound/granted-ability variants. Used both as the body of
/// `parse_subject_additive_type_static` (pure additive predicates) and as a
/// fallback inside `parse_continuous_modifications` (compound predicates
/// whose leading `have …` clause is already consumed upstream).
///
/// Returns `None` when:
/// * the clause does not contain an additive-type phrase,
/// * the type-word region is a placeholder handled by another specialized
///   extractor (`every basic land type`, `the chosen type`), or
/// * no valid type or subtype was recognized (unknown words are dropped —
///   the curated `SUBTYPES` list is authoritative).
pub(crate) fn parse_additive_type_clause_modifications(
    text: &str,
) -> Option<Vec<ContinuousModification>> {
    type VE<'a> = OracleError<'a>;

    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower)
        .trim_start()
        .trim_end()
        .trim_end_matches('.');
    let (_, clause_lower) = nom_primitives::scan_split_at_phrase(tp.lower, |i| {
        alt((
            tag::<_, _, VE>("are "),
            tag::<_, _, VE>("is "),
            tag::<_, _, VE>("and are "),
            tag::<_, _, VE>("and is "),
        ))
        .parse(i)
    })?;
    let clause_original = &tp.original[tp.original.len() - clause_lower.len()..];
    let (after_verb_lower, _) = alt((
        tag::<_, _, VE>("are "),
        tag::<_, _, VE>("is "),
        tag::<_, _, VE>("and are "),
        tag::<_, _, VE>("and is "),
    ))
    .parse(clause_lower)
    .ok()?;
    let after_verb_original = &clause_original[clause_original.len() - after_verb_lower.len()..];
    let (after_suffix_lower, type_words_lower) = terminated(
        take_until::<_, _, VE>(" in addition to "),
        (
            tag::<_, _, VE>(" in addition to "),
            alt((tag::<_, _, VE>("its"), tag::<_, _, VE>("their"))),
            tag::<_, _, VE>(" other "),
            // CR 105.2 + CR 205.1a: "colors and types" forms add a color (CR
            // 613.1e, layer 5) alongside the type/subtype additions. Longer
            // phrases first so "colors and types" wins over bare "types".
            alt((
                tag::<_, _, VE>("colors and creature types"),
                tag::<_, _, VE>("colors and types"),
                tag::<_, _, VE>("creature types"),
                tag::<_, _, VE>("land types"),
                tag::<_, _, VE>("types"),
                tag::<_, _, VE>("colors"),
            )),
        ),
    )
    .parse(after_verb_lower)
    .ok()?;
    let type_words = &after_verb_original[..type_words_lower.len()];
    let normalized_type_words = type_words_lower.trim();
    // Placeholders owned by other specialized extractors (basic-land-type copies,
    // chosen-type statics). Let those branches produce the correct modification.
    if matches!(
        normalized_type_words,
        "every basic land type" | "the chosen type" | "the chosen creature type"
    ) {
        return None;
    }
    // CR 205.3i + CR 305.7: "is every land type in addition to its other types"
    // grants all 17 land subtypes additively (Omo, Queen of Vesuva). The token
    // is already combinator-extracted above; gate it against the fixed CR
    // phrase here (mirrors the adjacent placeholder `matches!`).
    if normalized_type_words == "every land type" {
        return Some(vec![ContinuousModification::AddAllLandTypes]);
    }
    let granted_lower = opt(preceded(
        alt((tag::<_, _, VE>(" and have "), tag::<_, _, VE>(" and has "))),
        rest::<_, VE>,
    ))
    .parse(after_suffix_lower)
    .ok()?
    .1;
    let granted_original = granted_lower
        .map(|granted| &clause_original[clause_original.len() - granted.len()..])
        .map(str::trim);
    let granted_modifications = granted_original
        .map(parse_quoted_ability_modifications)
        .unwrap_or_default();

    let mut modifications = Vec::new();
    for raw_word in type_words.split_whitespace() {
        let word = raw_word.trim_matches(|c: char| c == ',' || c == '.');
        if word.is_empty() {
            continue;
        }
        let lower_word = word.to_lowercase();
        // CR 105.2 + CR 613.1e: a color word ("black", "white", …) adds that
        // color (layer 5), e.g. Rise from the Grave's "black Zombie".
        // `all_consuming` asserts the whole token is the color word, matching
        // the sibling guard idiom rather than a manual `rest.is_empty()` check.
        if let Ok((_, color)) =
            all_consuming(nom_primitives::parse_color).parse(lower_word.as_str())
        {
            modifications.push(ContinuousModification::AddColor { color });
            continue;
        }
        if let Some(core_type) = core_type_from_additive_word(lower_word.as_str()) {
            modifications.push(ContinuousModification::AddType { core_type });
            continue;
        }
        // CR 205.3a: Only canonical subtypes from the curated list may be
        // added. Unrecognized words are silently dropped rather than
        // fabricated — a heuristic capitalize-and-strip-s would synthesize
        // non-MTG subtypes from noise tokens.
        if let Some((canonical, _)) = parse_subtype(lower_word.as_str()) {
            modifications.push(ContinuousModification::AddSubtype { subtype: canonical });
        }
    }

    modifications.extend(granted_modifications);
    if let Some(granted) = granted_original {
        push_base_pt_mana_value_dynamic_modifications(&mut modifications, &granted.to_lowercase());
    }
    (!modifications.is_empty()).then_some(modifications)
}

/// CR 205.1: Map a bare type word (singular or plural) to its `CoreType`.
pub(crate) fn core_type_from_additive_word(word: &str) -> Option<CoreType> {
    match word {
        "artifact" | "artifacts" => Some(CoreType::Artifact),
        "creature" | "creatures" => Some(CoreType::Creature),
        "enchantment" | "enchantments" => Some(CoreType::Enchantment),
        "land" | "lands" => Some(CoreType::Land),
        "planeswalker" | "planeswalkers" => Some(CoreType::Planeswalker),
        "battle" | "battles" => Some(CoreType::Battle),
        _ => None,
    }
}

/// CR 205.3 + CR 700.8: Parse a self-static of the form
/// `~ is also a <subtype>(, <subtype>)*[, [and|or] <subtype>]` into a vec of
/// `AddSubtype` modifications. The anchor `~` (set by `normalize_self_refs_for_static`)
/// scopes the match to source-self type grants — attached-object additive grants
/// ("Enchanted land is also a Plains") route through `parse_subject_additive_type_static`
/// instead. Returns `None` if the anchor doesn't match or any trailing text
/// remains after the subtype list, so other arms remain free to try the line.
///
/// CR 205.3d: An object can't gain a subtype that doesn't correspond to one of
/// its types. The pithy "X is also a Y" phrasing is exclusively used by
/// creature-subtype grants (party tribal: Cleric/Rogue/Warrior/Wizard, plus
/// scattered self-typegrant creatures); land/artifact/enchantment subtype
/// additions use the "in addition to its other types" phrasing handled by
/// `parse_subject_additive_type_static`. We therefore reject any token whose
/// canonical subtype maps to a non-creature core type so a stray Forest /
/// Equipment / Aura is not silently added to a creature.
pub(crate) fn try_parse_self_is_also_subtypes(
    tp: &TextPair<'_>,
) -> Option<Vec<ContinuousModification>> {
    type VE<'a> = OracleError<'a>;

    let (after_anchor, _): (&str, &str) = alt((
        tag::<_, _, VE>("~ is also a "),
        tag::<_, _, VE>("~ is also an "),
    ))
    .parse(tp.lower)
    .ok()?;

    fn parse_one(input: &str) -> nom::IResult<&str, String, OracleError<'_>> {
        match parse_subtype(input) {
            Some((canonical, len)) if infer_core_type_for_subtype(&canonical).is_none() => {
                Ok((&input[len..], canonical))
            }
            _ => Err(nom::Err::Error(nom::error::Error::new(
                input,
                nom::error::ErrorKind::Fail,
            ))),
        }
    }

    // Decomposes the separator into independent axes — connective phrase
    // (`,` optionally followed by `and`/`or`/`and/or`, or space-led
    // `and`/`or`/`and/or`) × mandatory trailing space × optional indefinite
    // article (`a `/`an `). Each axis is one `alt()`; the ≤14-form cartesian
    // product is composed, not enumerated, per the "compose combinators by
    // dimension" rule.
    fn parse_connective(input: &str) -> nom::IResult<&str, &str, OracleError<'_>> {
        // Order long-first within each branch so `, and/or` wins over the
        // bare `,` prefix in nom's left-to-right `alt` evaluation.
        alt((
            recognize((
                tag::<_, _, OracleError<'_>>(","),
                opt(preceded(
                    tag(" "),
                    alt((tag("and/or"), tag("and"), tag("or"))),
                )),
            )),
            recognize(preceded(
                tag(" "),
                alt((tag("and/or"), tag("and"), tag("or"))),
            )),
        ))
        .parse(input)
    }
    fn parse_sep(input: &str) -> nom::IResult<&str, (), OracleError<'_>> {
        let (input, _) = parse_connective(input)?;
        let (input, _) = tag(" ").parse(input)?;
        let (input, _) = opt(alt((tag("a "), tag("an ")))).parse(input)?;
        Ok((input, ()))
    }

    // `all_consuming` + `terminated` asserts the entire `after_anchor` slice
    // parses as `<subtype list><optional period><optional trailing space>` —
    // replaces the prior manual `.trim().is_empty()` trailing-text check with
    // an idiomatic nom assertion.
    let (_, names) = all_consuming(terminated(
        separated_list1(parse_sep, parse_one),
        (opt(tag::<_, _, VE>(".")), space0),
    ))
    .parse(after_anchor)
    .ok()?;

    if names.is_empty() {
        return None;
    }

    Some(
        names
            .into_iter()
            .map(|subtype| ContinuousModification::AddSubtype { subtype })
            .collect(),
    )
}

/// CR 613.1d + CR 205.1a: "Enchanted [permanent-type] is a/an [type] [with base P/T N/N]
/// [in addition to its other types]"
///
/// Handles type-changing aura effects like Ensoul Artifact, Imprisoned in the Moon,
/// and Darksteel Mutation. Reuses nom type-word and P/T combinators.
pub(crate) fn parse_enchanted_is_type(
    tp: &TextPair,
    description: &str,
) -> Option<StaticDefinition> {
    // Match "enchanted " prefix
    let rest_tp = nom_tag_tp(tp, "enchanted ")?;

    // Parse the enchanted permanent type using nom type-word combinator
    let (after_type, perm_tf) = nom_target::parse_type_filter_word(rest_tp.lower).ok()?;
    let after_type_lower = after_type.trim_start();

    // Must have " is a " or " is an " or " loses all abilities and is a "
    let mut modifications = Vec::new();
    type VE<'a> = OracleError<'a>;

    let is_rest_lower = if let Ok((r, _)) = alt((
        tag::<_, _, VE>("loses all abilities and is a "),
        tag::<_, _, VE>("loses all abilities and is an "),
    ))
    .parse(after_type_lower)
    {
        modifications.push(ContinuousModification::RemoveAllAbilities);
        r
    } else if let Ok((r, _)) =
        alt((tag::<_, _, VE>("is a "), tag::<_, _, VE>("is an "))).parse(after_type_lower)
    {
        r
    } else {
        return None;
    };

    let is_rest_lower = is_rest_lower.trim_end_matches('.');

    // Check for "in addition to its other types" suffix.
    // CR 205.1b: "in addition to its other types" retains all prior card types
    // (additive). Its absence means CR 205.1a applies: the new card type(s)
    // replace the existing ones.
    let (type_part, is_additive) =
        // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        if let Some(before) = is_rest_lower.strip_suffix(" in addition to its other types") {
            // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
            (before.trim(), true)
        } else {
            (is_rest_lower, false)
        };

    // Try to parse "base power and toughness N/N" suffix.
    //
    // `pt_part` is everything after the " with base power and toughness "
    // token, e.g. for Darksteel Mutation: "0/1 and has indestructible, and it
    // loses all other abilities, card types, and creature types". `parse_pt_mod`
    // consumes only the leading "N/N" — the unconsumed remainder (the
    // "and has <kw> ... and it loses all ..." clause) is captured and fed to
    // `parse_continuous_modifications` below so it is not silently dropped.
    let (type_part, base_pt, trailing_clause) =
        if let Some((before_pt, pt_part)) =
            // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
            type_part.rsplit_once(" with base power and toughness ")
        // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        {
            if let Some((p, t)) = parse_pt_mod(pt_part) {
                // Locate the end of the "N/N" token to capture the remainder.
                let slash_pos = pt_part.find('/').unwrap_or(0);
                let after_slash = &pt_part[slash_pos + 1..];
                let t_end = after_slash
                    .find(|c: char| c.is_whitespace() || c == '.' || c == ',')
                    .unwrap_or(after_slash.len());
                let remainder = after_slash[t_end..].trim();
                let clause = (!remainder.is_empty()).then_some(remainder);
                (before_pt.trim(), Some((p, t)), clause)
            } else {
                (type_part, None, None)
            }
        } else {
            (type_part, None, None)
        };

    // Parse "N/N [color] [type] [subtype]" patterns for Darksteel Mutation style
    // e.g., "0/1 green Insect creature"
    let (type_part, inline_pt) = if let Some((p, t)) = parse_pt_mod(type_part) {
        // parse_pt_mod trims and finds the slash — get remainder after P/T
        let slash_pos = type_part.find('/').unwrap_or(0);
        let after_slash = &type_part[slash_pos + 1..];
        let t_end = after_slash
            .find(|c: char| c.is_whitespace() || c == '.' || c == ',')
            .unwrap_or(after_slash.len());
        let rest = after_slash[t_end..].trim();
        (rest, Some((p, t)))
    } else {
        (type_part, None)
    };

    // Parse optional color
    let (type_part, opt_color) = if let Ok((rest, color)) = nom_primitives::parse_color(type_part) {
        (rest.trim(), Some(color))
    } else if let Ok((rest, _)) = tag::<_, _, VE>("colorless ").parse(type_part) {
        // "colorless" removes all colors — handled via SetColor([])
        (rest.trim(), None)
    } else {
        (type_part, None)
    };
    let is_colorless = nom_primitives::scan_contains(is_rest_lower, "colorless");

    // Parse the target type(s) — use parse_type_filter_word for the main type.
    // Handle "[Subtype] [type]" patterns (e.g., "insect creature") by trying the
    // first word as a subtype and the second as a type if direct parse fails.
    use crate::types::card_type::CoreType;

    let (parsed_type, subtype_word, remainder) =
        if let Ok((remainder, target_tf)) = nom_target::parse_type_filter_word(type_part) {
            (Some(target_tf), None, remainder.trim())
        } else if let Some(space_pos) = type_part.find(' ') {
            // First word might be a subtype — try the rest as a type
            let maybe_subtype = &type_part[..space_pos];
            let after_subtype = type_part[space_pos..].trim();
            if let Ok((remainder, target_tf)) = nom_target::parse_type_filter_word(after_subtype) {
                // Capitalize the subtype for canonical form
                let capitalized = {
                    let mut chars = maybe_subtype.chars();
                    match chars.next() {
                        Some(first) => {
                            let mut s = first.to_uppercase().collect::<String>();
                            s.push_str(chars.as_str());
                            s
                        }
                        None => maybe_subtype.to_string(),
                    }
                };
                (Some(target_tf), Some(capitalized), remainder.trim())
            } else {
                (None, None, type_part)
            }
        } else {
            (None, None, type_part)
        };

    if let Some(target_tf) = parsed_type {
        // Collect the granted core types and subtypes separately so the
        // trailing-clause loss modifications can be inserted in the correct
        // written order: any `RemoveAllSubtypes` must precede `AddSubtype`
        // (the new creature type must survive the subtype wipe — CR 205.1b).
        let mut granted_core_types: Vec<CoreType> = Vec::new();
        let mut granted_subtypes: Vec<String> = Vec::new();

        // Route a parsed TypeFilter to the granted core-type list or the
        // granted-subtype list. `TypeFilter::Subtype` (e.g. "Insect") must be
        // emitted as `AddSubtype`, not dropped — CR 205.1b: a "[creature type]
        // artifact creature" replaces the creature type with that subtype.
        let classify_type =
            |tf: &TypeFilter, cores: &mut Vec<CoreType>, subs: &mut Vec<String>| match tf {
                TypeFilter::Creature => cores.push(CoreType::Creature),
                TypeFilter::Artifact => cores.push(CoreType::Artifact),
                TypeFilter::Enchantment => cores.push(CoreType::Enchantment),
                TypeFilter::Land => cores.push(CoreType::Land),
                TypeFilter::Planeswalker => cores.push(CoreType::Planeswalker),
                TypeFilter::Subtype(sub) => subs.push(sub.clone()),
                _ => {}
            };

        // Leading type word.
        classify_type(&target_tf, &mut granted_core_types, &mut granted_subtypes);

        // Subtype parsed from the "[Subtype] [type]" two-word branch.
        if let Some(sub) = subtype_word {
            granted_subtypes.push(sub);
        }

        // Parse any additional type words or subtypes from remainder
        // Handles "Insect artifact creature" where remainder = "creature" after parsing "artifact"
        let mut extra = remainder;
        while !extra.is_empty() {
            if let Ok((rest, extra_tf)) = nom_target::parse_type_filter_word(extra) {
                classify_type(&extra_tf, &mut granted_core_types, &mut granted_subtypes);
                extra = rest.trim();
            } else if is_capitalized_words(extra) {
                granted_subtypes.push(extra.to_string());
                break;
            } else {
                break;
            }
        }

        // This branch handles type-*changing* auras that grant at least one
        // core card type ("is an Insect artifact creature ..."). A bare
        // "is a [land subtype]" ("Enchanted land is a Mountain") grants no
        // core type and is a basic-land-type change — defer to the dedicated
        // SetBasicLandType parser by returning None here.
        if granted_core_types.is_empty() {
            return None;
        }

        // Parse the trailing "and has <kw> ... and it loses all other ..."
        // clause that the " with base power and toughness " split would
        // otherwise discard. `parse_continuous_modifications` turns "and has
        // <kw>" into `AddKeyword` and "loses all [other] abilities/creature
        // types" into `RemoveAllAbilities` / `RemoveAllSubtypes`.
        let mut clause_mods: Vec<ContinuousModification> = Vec::new();
        let mut loss_replaces_card_types = false;
        if let Some(clause) = trailing_clause {
            clause_mods = parse_continuous_modifications(clause);
            // CR 205.1b: an explicit "loses all other card types" makes the
            // type-set replacement exact — emit a single `SetCardTypes`
            // carrying the granted core types instead of additive `AddType`s.
            loss_replaces_card_types = scan_loss_enumeration(&clause.to_lowercase())
                .iter()
                .any(|m| matches!(m, LossMember::CardTypes));
        }

        // CR 205.1a + CR 613.1d (Layer 4): Two independent conditions each require
        // SetCardTypes (replacing) rather than AddType (additive):
        //   (A) loss_replaces_card_types: trailing clause explicitly says "loses
        //       all other card types" (Darksteel Mutation path — already working).
        //   (B) !is_additive: "in addition to its other types" is absent, so "is a
        //       [type]" replaces existing card types (Frogify, Lignify, etc.).
        // These are documented separately and combined into a single bool to avoid
        // emitting two SetCardTypes pushes.
        let needs_set_card_types = loss_replaces_card_types || !is_additive;

        // --- Assemble modifications in written (mod_index) order ---
        // 1. Core types: replacement (SetCardTypes) when CR 205.1a applies (no
        //    "in addition" suffix) or the clause says "loses all other card
        //    types"; else additive AddType (CR 205.1b "in addition").
        if needs_set_card_types {
            modifications.push(ContinuousModification::SetCardTypes {
                core_types: granted_core_types.clone(),
            });
        } else {
            for ct in &granted_core_types {
                modifications.push(ContinuousModification::AddType { core_type: *ct });
            }
        }

        // 2. Color
        // CR 105.3 + CR 613.1e (Layer 5): a new color replaces all previous
        // colors unless the effect is "in addition"; additive "in addition to
        // its other types" appends via AddColor.
        if let Some(color) = opt_color {
            if is_additive {
                modifications.push(ContinuousModification::AddColor { color });
            } else {
                modifications.push(ContinuousModification::SetColor {
                    colors: vec![color],
                });
            }
        } else if is_colorless {
            modifications.push(ContinuousModification::SetColor { colors: vec![] });
        }

        // 3. Base P/T from explicit "with base power and toughness" or inline "N/N"
        if let Some((p, t)) = base_pt.or(inline_pt) {
            modifications.push(ContinuousModification::SetPower { value: p });
            modifications.push(ContinuousModification::SetToughness { value: t });
        }

        // CR 205.1a + CR 613.1d (Layer 4): Non-additive "is a [subtype] creature"
        // sets a new creature subtype, which replaces existing creature subtypes.
        // Auto-inject RemoveAllSubtypes{Creature} unless the trailing clause
        // already provides it (Darksteel Mutation explicitly says "loses all
        // other creature types" and its clause_mods contains the wipe).
        if !is_additive
            && granted_core_types.contains(&CoreType::Creature)
            && !granted_subtypes.is_empty()
            && !modifications
                .iter()
                .chain(clause_mods.iter())
                .any(|m| matches!(m, ContinuousModification::RemoveAllSubtypes { .. }))
        {
            modifications.push(ContinuousModification::RemoveAllSubtypes {
                set: crate::types::card_type::SubtypeSet::Creature,
            });
        }

        // 4. Trailing-clause mods (AddKeyword, RemoveAllAbilities,
        //    RemoveAllSubtypes) — RemoveAllSubtypes here must precede the
        //    AddSubtype emissions below so the granted creature type survives.
        modifications.extend(clause_mods);

        // 5. Granted subtypes (e.g. AddSubtype(Insect)) — after any
        //    RemoveAllSubtypes wipe.
        for sub in granted_subtypes {
            modifications.push(ContinuousModification::AddSubtype { subtype: sub });
        }

        if modifications.is_empty() {
            return None;
        }

        let affected = TargetFilter::Typed(
            TypedFilter::new(perm_tf).properties(vec![FilterProp::EnchantedBy]),
        );

        return Some(
            StaticDefinition::continuous()
                .affected(affected)
                .modifications(modifications)
                .description(description.to_string()),
        );
    }

    None
}

/// CR 205.1a + CR 205.2 + CR 205.3 + CR 613.1c: Scan text for a "becomes a
/// [subtype]* [core-type]+ in addition to its other types" descriptor and
/// decompose it into typed `ContinuousModification`s.
///
/// Uses nom combinators (`tag`, `alt`, `take_until`) to locate the descriptor
/// slice on the lowered text, then hands the original-cased slice to
/// [`super::oracle_effect::animation::parse_becomes_type_modifications`] which
/// reuses the existing animation type-sequence combinator for CR-205
/// token-by-token classification. One `AddType` per CR 205.2 core type and
/// one `AddSubtype` per CR 205.3 subtype are emitted; CR 205.4 supertypes are
/// recognized-and-discarded (animations don't grant supertypes).
pub(crate) fn parse_becomes_type_addition_modifications(
    tp: &TextPair<'_>,
) -> Vec<ContinuousModification> {
    type VE<'a> = OracleError<'a>;

    // Scan for the "becomes a"/"becomes an" phrase anywhere in the lowered
    // text, then locate the terminating "in addition to its other types"
    // clause. `scan_split_at_phrase` returns the lowered slice beginning at
    // the matched phrase.
    let Some((_, tail_lower)) = nom_primitives::scan_split_at_phrase(tp.lower, |i| {
        alt((
            tag::<_, _, VE>("becomes a "),
            tag::<_, _, VE>("becomes an "),
        ))
        .parse(i)
    }) else {
        return Vec::new();
    };
    let Ok::<_, nom::Err<VE<'_>>>((after_article_lower, _consumed)) =
        alt((tag("becomes a "), tag("becomes an "))).parse(tail_lower)
    else {
        return Vec::new();
    };

    // Extract the descriptor up to the first " in addition to" clause.
    let Ok::<_, nom::Err<VE<'_>>>((_, descriptor_lower)) =
        take_until(" in addition to")(after_article_lower)
    else {
        return Vec::new();
    };

    // Map the lowered descriptor back onto the original-cased text so the CR
    // 205.3 subtype grammar (which requires capitalized proper nouns) sees the
    // correct case.
    let start = tp.lower.len() - after_article_lower.len();
    let end = start + descriptor_lower.len();
    let descriptor_original = &tp.original[start..end];

    super::oracle_effect::animation::parse_becomes_type_modifications(descriptor_original)
}

/// CR 205.1a-b + CR 613.1d: bare "becomes a/an <descriptor>" type-changing
/// effects are replacement-form changes. Setting core card types replaces the
/// previous card-type set except for CR 205.1b's artifact-creature exception;
/// setting creature subtypes replaces the object's previous creature types.
pub(crate) fn parse_bare_becomes_type_replacement_modifications(
    tp: &TextPair<'_>,
) -> Vec<ContinuousModification> {
    type VE<'a> = OracleError<'a>;

    let Some((_, tail_lower)) = nom_primitives::scan_split_at_phrase(tp.lower, |i| {
        alt((
            tag::<_, _, VE>("becomes a "),
            tag::<_, _, VE>("becomes an "),
        ))
        .parse(i)
    }) else {
        return Vec::new();
    };
    let Ok::<_, nom::Err<VE<'_>>>((after_article_lower, _)) =
        alt((tag("becomes a "), tag("becomes an "))).parse(tail_lower)
    else {
        return Vec::new();
    };
    let (descriptor_lower, retained_core_type) =
        if let Some((descriptor_lower, retained_core_type)) =
            split_type_retention_clause(after_article_lower)
        {
            (descriptor_lower, Some(retained_core_type))
        } else {
            (after_article_lower, None)
        };
    if retained_core_type.is_none()
        && take_until::<_, _, VE>(" in addition to")
            .parse(descriptor_lower)
            .is_ok()
    {
        return Vec::new();
    }

    let Ok::<_, nom::Err<VE<'_>>>((_, descriptor_lower)) =
        parse_clause_before_optional_period(descriptor_lower)
    else {
        return Vec::new();
    };
    let (descriptor_lower, _) = strip_trailing_duration(descriptor_lower.trim());
    let descriptor_lower = descriptor_lower.trim();
    if descriptor_lower.is_empty() {
        return Vec::new();
    }

    let start = tp.lower.len() - after_article_lower.len();
    let end = start + descriptor_lower.len();
    let descriptor_original = &tp.original[start..end];
    let Some(spec) = super::oracle_effect::animation::parse_animation_spec(
        descriptor_original,
        &mut ParseContext::default(),
    ) else {
        return Vec::new();
    };
    let animation_modifications = super::oracle_effect::animation::animation_modifications(&spec);
    if let Some(core_type) = retained_core_type {
        let mut modifications = animation_modifications;
        if !modifications.contains(&ContinuousModification::AddType { core_type }) {
            modifications.push(ContinuousModification::AddType { core_type });
        }
        return modifications;
    }

    let core_types: Vec<CoreType> = animation_modifications
        .iter()
        .filter_map(|modification| match modification {
            ContinuousModification::AddType { core_type } => Some(*core_type),
            _ => None,
        })
        .collect();
    let keep_additive_core_types = core_types.len() == 2
        && core_types.contains(&CoreType::Artifact)
        && core_types.contains(&CoreType::Creature);

    let mut modifications = Vec::new();
    let mut set_core_types = false;
    let mut removed_subtype_sets = Vec::new();
    for modification in animation_modifications {
        if matches!(modification, ContinuousModification::AddType { .. }) {
            if core_types.is_empty() || keep_additive_core_types {
                modifications.push(modification);
            } else if !set_core_types {
                modifications.push(ContinuousModification::SetCardTypes {
                    core_types: core_types.clone(),
                });
                set_core_types = true;
            }
            continue;
        }

        if let ContinuousModification::AddSubtype { subtype } = &modification {
            let set = noncreature_subtype_set(subtype).unwrap_or(SubtypeSet::Creature);
            if !removed_subtype_sets.contains(&set) {
                modifications.push(ContinuousModification::RemoveAllSubtypes { set });
                removed_subtype_sets.push(set);
            }
        }
        modifications.push(modification);
    }
    modifications
}

/// CR 613.1d + CR 613.1g: "[pronoun]'s a/an <descriptor> [as long as <condition>]"
/// — self-referential conditional animation static. Covers:
///   - Dynamic-P/T-by-mana-value: "it's an artifact creature with power and
///     toughness each equal to its mana value" (Animate Artifact)
///   - Fixed P/T + types + keywords: "he's a 7/7 Dragon God creature with flying
///     and indestructible" (Grand Master of Flowers — CR 613.4b fixed P/T,
///     CR 613.1d type grant, CR 613.1g keyword grant)
///
/// Accepts gender-neutral and gendered pronouns ("it's", "~'s", "they're",
/// "he's", "she's"). Delegates body parsing to
/// `parse_animation_spec` + `animation_modifications` (which handles fixed P/T,
/// dynamic P/T-by-MV, types, subtypes, and keyword tails in one pass), falling
/// back to the prior type-only + MV-dynamic-P/T path if the spec parser returns
/// None.
pub(crate) fn parse_pronoun_becomes_type_static(
    tp: &TextPair<'_>,
    text: &str,
) -> Option<StaticDefinition> {
    // STEP A.0 — peel an optional leading "during your turn, " timing clause.
    // Mirror of `parse_compound_turn_counter_animation` (anthem.rs): the
    // alternate printing convention writes the turn restriction as a leading
    // timing prefix ("During your turn, ~ is a 4/4 ...") rather than a trailing
    // "as long as it's your turn" clause. The peel is Option-returning, so when
    // absent it falls through to `*tp` unchanged and no condition is attached.
    let (tp, turn_condition) = match nom_tag_tp(tp, "during your turn, ") {
        Some(rest) => (rest, Some(StaticCondition::DuringYourTurn)),
        None => (*tp, None),
    };

    // STEP A — peel a trailing " as long as <condition>" FIRST. The canonical
    // inverted-form rewrite produces "<effect> as long as <condition>"; the
    // condition must come off before the effect is parsed, or it leaks into
    // the " with " tail and never becomes a StaticCondition.
    let (effect_tp, condition_tp) = match tp.split_around(" as long as ") {
        Some((before, after)) => (before, Some(after)),
        None => (tp, None),
    };

    // STEP B — pronoun + article prefix. Accept gender-neutral ("it's", "~'s",
    // "they're") and gendered ("he's", "she's") pronouns; planeswalker
    // animation statics use gendered pronouns (Grand Master of Flowers, Kaito,
    // Gideon classes). Also accept the bare "is"-copula form ("~ is a/an ...")
    // produced when an inverted "As long as it's your turn, ~ is a ..." line
    // (Gideon Blackblade, #1155) is split by `parse_conditional_static` and the
    // pronoun-less effect clause "~ is a 4/4 ..." re-enters this parser.
    let body = nom_tag_tp(&effect_tp, "it's a ")
        .or_else(|| nom_tag_tp(&effect_tp, "it's an "))
        .or_else(|| nom_tag_tp(&effect_tp, "~'s a "))
        .or_else(|| nom_tag_tp(&effect_tp, "~'s an "))
        .or_else(|| nom_tag_tp(&effect_tp, "~ is a "))
        .or_else(|| nom_tag_tp(&effect_tp, "~ is an "))
        .or_else(|| nom_tag_tp(&effect_tp, "they're a "))
        .or_else(|| nom_tag_tp(&effect_tp, "they're an "))
        .or_else(|| nom_tag_tp(&effect_tp, "he's a "))
        .or_else(|| nom_tag_tp(&effect_tp, "he's an "))
        .or_else(|| nom_tag_tp(&effect_tp, "she's a "))
        .or_else(|| nom_tag_tp(&effect_tp, "she's an "))?;

    // STEP C — delegate body parsing to parse_animation_spec which handles
    // fixed P/T (CR 613.4b), dynamic P/T-by-mana-value, types (CR 613.1d),
    // subtypes (CR 205.3), and keyword tails (CR 613.1g) in one composable pass.
    let body_text = body.original.trim().trim_end_matches('.');
    let modifications = if let Some(spec) = super::oracle_effect::animation::parse_animation_spec(
        body_text,
        &mut ParseContext::default(),
    ) {
        super::oracle_effect::animation::animation_modifications(&spec)
    } else {
        // Fallback: type-token parse + mana-value dynamic P/T. Handles edge
        // cases where parse_animation_spec returns None (e.g., unusual clause
        // ordering not yet covered by the animation spec parser).
        let mut mods = Vec::new();
        let (type_part, with_tail) = match body.split_around(" with ") {
            Some((before, after)) => (before, Some(after)),
            None => (body, None),
        };
        mods.extend(
            super::oracle_effect::animation::parse_becomes_type_modifications(type_part.original),
        );
        if let Some(tail) = &with_tail {
            push_base_pt_mana_value_dynamic_modifications(&mut mods, tail.lower);
        }
        mods
    };

    if modifications.is_empty() {
        return None;
    }

    // STEP D — attach the condition(s). The leading "during your turn, " timing
    // peel (STEP A.0) and the trailing " as long as <cond>" peel (STEP A) are
    // independent; either, both, or neither may be present.
    // CR 205.1b + CR 613.7: "~ is a [P/T] [types] creature ... that's still a
    // planeswalker" — additive type-change (AddType is non-replacing, so the
    // permanent retains its Planeswalker type while it is also a creature).
    let trailing_condition = condition_tp.map(|cond_tp| {
        let cond_text = cond_tp.original.trim().trim_end_matches('.');
        parse_static_condition(cond_text).unwrap_or(StaticCondition::Unrecognized {
            text: cond_text.to_string(),
        })
    });
    let condition = match (turn_condition, trailing_condition) {
        // CR 611.3a: when both a leading turn restriction and a trailing
        // "as long as" condition are present, compose via `And` rather than
        // dropping one (mirrors `parse_conditional_static` in anthem.rs).
        (Some(turn), Some(inner)) => Some(StaticCondition::And {
            conditions: vec![turn, inner],
        }),
        (Some(turn), None) => Some(turn),
        (None, Some(inner)) => Some(inner),
        (None, None) => None,
    };

    let mut def = StaticDefinition::continuous()
        .affected(TargetFilter::SelfRef)
        .modifications(modifications)
        .description(text.to_string());
    if let Some(condition) = condition {
        def = def.condition(condition);
    }
    Some(def)
}

/// CR 205.2 + CR 613.1d + CR 613.4b + CR 611.3a: "Each noncreature <T> [you control]
/// is a[n] [<T>] creature with power and toughness each equal to its mana value
/// [as long as <condition>]." — March of the Machines class. The affirmative type
/// `<T>` must be artifact or enchantment. The second type token (if present) must
/// agree with `<T>`. Corpus members: March of the Machines, Karn, Silver Golem.
///
/// This is the noncreature-subject sibling of `parse_pronoun_becomes_type_static`
/// (which handles self-referential `it's a/an <types>` animations). Opalescence
/// (`"Each other non-Aura enchantment ..."`) starts with `"Each other"` and is
/// handled by a different parser arm — it is NOT in this class.
///
/// Composition: `nom_tag_tp` peels the subject prefix; `nom_target::parse_type_filter_word`
/// recognizes the affirmative type; `nom_tag_lower` (leading-space-anchored) peels
/// the optional controller clause and the copula; the dynamic-P/T-by-mana-value
/// tail is delegated to `push_base_pt_mana_value_dynamic_modifications`.
pub(crate) fn parse_each_noncreature_subject_is_creature_with_pt_mv(
    tp: &TextPair<'_>,
    description: &str,
) -> Option<StaticDefinition> {
    // STEP A — CR 611.3a: peel a trailing " as long as <condition>" FIRST.
    // The condition must come off before the effect is parsed, or it leaks into
    // the dynamic-P/T tail and never becomes a StaticCondition. Mirrors STEP A
    // of `parse_pronoun_becomes_type_static`.
    let (effect_tp, condition_tp) = match tp.split_around(" as long as ") {
        Some((before, after)) => (before, Some(after)),
        None => (*tp, None),
    };

    // STEP C.1 — strip "each noncreature " subject prefix.
    let rest_tp = nom_tag_tp(&effect_tp, "each noncreature ")?;

    // STEP C.2 — affirmative type word. Direct nom call: (remainder, value) ordering.
    let (after_subject_lower, affirmative_type) =
        nom_target::parse_type_filter_word(rest_tp.lower).ok()?;
    if !matches!(
        affirmative_type,
        TypeFilter::Artifact | TypeFilter::Enchantment
    ) {
        return None;
    }

    // STEP C.3 — optional " you control" (leading-space-anchored).
    // CR 109.5: "you/your" rebinding.
    let (rest_after_controller, controller): (&str, Option<ControllerRef>) =
        match nom_tag_lower(after_subject_lower, after_subject_lower, " you control") {
            Some(rest) => (rest, Some(ControllerRef::You)),
            None => (after_subject_lower, None),
        };

    // STEP C.4 — copula (leading-space-anchored). Try " is an " first (longer match).
    let after_copula = nom_tag_lower(rest_after_controller, rest_after_controller, " is an ")
        .or_else(|| nom_tag_lower(rest_after_controller, rest_after_controller, " is a "))?;

    // STEP D — optional adjective matching affirmative_type, then required "creature".
    // March of the Machines: "is an artifact creature ..." — adjective present.
    // Hypothetical sibling "is a creature ...": adjective absent (fall through).
    let after_adjective = match nom_target::parse_type_filter_word(after_copula) {
        Ok((rest, adj)) if adj == affirmative_type => rest,
        _ => after_copula,
    };
    // When STEP D consumed an adjective, `after_adjective` begins with " creature"
    // (the space between adjective and noun is still pending). When STEP D fell
    // through, `after_adjective == after_copula` already had its leading space
    // consumed by the " is a "/" is an " copula and now begins with "creature"
    // directly (no leading space). Both branches must succeed for the union.
    let after_creature = nom_tag_lower(after_adjective, after_adjective, " creature")
        .or_else(|| nom_tag_lower(after_adjective, after_adjective, "creature"))?;

    // STEP E — emit modifications.
    // CR 205.2 + CR 613.1d: Layer 4 add of the Creature core type.
    // CR 613.4b: Layer 7b set of base power/toughness (delegated).
    let mut modifications = vec![ContinuousModification::AddType {
        core_type: CoreType::Creature,
    }];
    if !push_base_pt_mana_value_dynamic_modifications(&mut modifications, after_creature) {
        return None;
    }

    // STEP F — build the affected-object selector: [<T>, Non(Creature)] + optional controller.
    let mut typed = TypedFilter::new(affirmative_type)
        .with_type(TypeFilter::Non(Box::new(TypeFilter::Creature)));
    if let Some(ctrl) = controller {
        typed = typed.controller(ctrl);
    }
    let affected = TargetFilter::Typed(typed);

    // STEP G — build the continuous static and re-attach the condition peeled
    // in STEP A. S8: description is the ORIGINAL line, not any peeled remainder.
    let mut def = StaticDefinition::continuous()
        .affected(affected)
        .modifications(modifications)
        .description(description.to_string());
    if let Some(cond_tp) = condition_tp {
        let cond_text = cond_tp.original.trim().trim_end_matches('.');
        let condition =
            parse_static_condition(cond_text).unwrap_or(StaticCondition::Unrecognized {
                text: cond_text.to_string(),
            });
        def = def.condition(condition);
    }
    Some(def)
}

/// CR 205.1a: Parse "All permanents are [type] in addition to their other types."
/// Handles global type-addition effects like Mycosynth Lattice ("artifacts") and
/// Enchanted Evening ("enchantments").
pub(crate) fn parse_all_permanents_are_type(
    tp: &TextPair<'_>,
    description: &str,
) -> Option<StaticDefinition> {
    let rest_tp = nom_tag_tp(tp, "all permanents are ")?;
    let rest = rest_tp.lower.trim_end_matches('.');
    let type_part = rest.strip_suffix(" in addition to their other types")?; // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
                                                                             // Map the type word to a CoreType
    let core_type = match type_part.trim() {
        "artifacts" => CoreType::Artifact, // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        "enchantments" => CoreType::Enchantment, // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        "creatures" => CoreType::Creature, // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        "lands" => CoreType::Land, // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        _ => return None,
    };
    Some(
        StaticDefinition::continuous()
            .affected(TargetFilter::Typed(TypedFilter::permanent()))
            .modifications(vec![ContinuousModification::AddType { core_type }])
            .description(description.to_string()),
    )
}

/// CR 613.1e + CR 105.1 / CR 105.2c / CR 105.3: Parse "All [subject] are [color(s)]."
/// — a global color-defining static ability (Layer 5).
///
/// - CR 105.1 enumerates the five colors.
/// - CR 105.2c: "A colorless object has no color." → empty color set.
/// - CR 105.3 authorizes color-changing effects (new color replaces previous
///   colors unless the effect says "in addition").
/// - CR 613.1e places color-changing effects in Layer 5.
///
/// Covers the class of "All X are Y" color-setting statics — Darkest Hour
/// ("All creatures are black."), Thran Lens ("All permanents are colorless."),
/// Ghostflame Sliver ("All Slivers are colorless."), and every future card
/// sharing this shape. Composes existing building blocks rather than writing
/// one-off string dispatch:
///
/// - `nom_target::parse_type_filter_word` recognizes every plural core-type
///   subject (creatures, permanents, lands, artifacts, enchantments,
///   planeswalkers, battles) AND every plural subtype in the shared subtype
///   table (Slivers, Elves, Treasures, Zombies, ...).
/// - `parse_color_predicate` composes a `tag("colorless")` combinator with
///   the shared `parse_color_list` (giving single colors, "X and Y", and
///   "X, Y, and Z" forms for free per CR 105.1).
/// - `typed_filter_for_subtype` routes artifact/land/enchantment subtypes to
///   their correct core type (e.g., Treasure → Artifact, not Creature).
///
/// Dispatch ordering constraints are documented at the call site in
/// `parse_static_line_inner` and pinned by three regression tests below.
pub(crate) fn parse_all_subject_are_color(
    tp: &TextPair<'_>,
    description: &str,
) -> Option<StaticDefinition> {
    let rest_tp = nom_tag_tp(tp, "all ")?;
    // Subject: single shared combinator for both core types and plural subtypes.
    let (after_subject, type_filter) = nom_target::parse_type_filter_word(rest_tp.lower).ok()?;
    // Copula — require " are " with surrounding whitespace so we never eat
    // words like "aren't" or "area".
    let after_verb = nom_tag_lower(after_subject, after_subject, " are ")?;
    // Strip the terminal period (structural cleanup on a post-combinator
    // chunk — the subject and copula have already been consumed), then the
    // predicate must fully parse as a color expression or follow-on clauses
    // route elsewhere.
    let predicate = after_verb.trim().trim_end_matches('.');
    let colors = parse_color_predicate(predicate)?;

    let affected = match type_filter {
        TypeFilter::Subtype(s) => TargetFilter::Typed(typed_filter_for_subtype(&s)),
        other => TargetFilter::Typed(TypedFilter::new(other)),
    };
    Some(
        StaticDefinition::continuous()
            .affected(affected)
            .modifications(vec![ContinuousModification::SetColor { colors }])
            .description(description.to_string()),
    )
}

/// CR 305.7: Parse "[Subject] lands are [type]" land type-changing static abilities.
/// Handles replacement ("Nonbasic lands are Mountains"), additive ("Each land is a
/// Swamp in addition to its other land types"), and all-basic-types ("Lands you control
/// are every basic land type in addition to their other types").
pub(crate) fn parse_land_type_change(tp: &TextPair<'_>, text: &str) -> Option<StaticDefinition> {
    let (subject_tp, rest_tp) = tp
        .split_around(" are ")
        .or_else(|| tp.split_around(" is a "))
        .or_else(|| tp.split_around(" is an "))
        .or_else(|| tp.split_around(" is "))?;
    let subject = subject_tp.original;
    let rest = rest_tp.original.trim().trim_end_matches('.');

    // Only proceed if subject is a land-type-change subject (avoids matching non-land patterns).
    let affected = parse_land_type_change_subject(subject)?;
    let lower_rest = rest.to_lowercase();

    // "every basic land type in addition to their other types"
    if nom_tag_lower(&lower_rest, &lower_rest, "every basic land type").is_some()
        && nom_primitives::scan_contains(&lower_rest, "in addition to")
    {
        return Some(
            StaticDefinition::continuous()
                .affected(affected)
                .modifications(vec![ContinuousModification::AddAllBasicLandTypes])
                .description(text.to_string()),
        );
    }

    // "[Type] in addition to {its/their} other {land }types" → AddSubtype (additive)
    if let Some(type_part) = strip_in_addition_suffix(&lower_rest) {
        let basic_type = parse_basic_land_type_plural(type_part.trim())?;
        return Some(
            StaticDefinition::continuous()
                .affected(affected)
                .modifications(vec![ContinuousModification::AddSubtype {
                    subtype: basic_type.as_subtype_str().to_string(),
                }])
                .description(text.to_string()),
        );
    }

    // CR 305.7: Replacement semantics — "[Type]" or "[Types]" → SetBasicLandType
    // Try multi-type list first: "Mountain, Forest, and Plains"
    if let Some(types) = parse_basic_land_type_list(rest.trim()) {
        if types.len() == 1 {
            return Some(
                StaticDefinition::continuous()
                    .affected(affected)
                    .modifications(vec![ContinuousModification::SetBasicLandType {
                        land_type: types[0],
                    }])
                    .description(text.to_string()),
            );
        }
        // CR 305.7: Multiple types — first SetBasicLandType clears old subtypes,
        // subsequent AddSubtype entries add the remaining types.
        let mut mods = vec![ContinuousModification::SetBasicLandType {
            land_type: types[0],
        }];
        for &lt in &types[1..] {
            mods.push(ContinuousModification::AddSubtype {
                subtype: lt.as_subtype_str().to_string(),
            });
        }
        return Some(
            StaticDefinition::continuous()
                .affected(affected)
                .modifications(mods)
                .description(text.to_string()),
        );
    }

    None
}

/// CR 613.1d (Layer 4) + CR 613.4b (Layer 7b) + CR 205.1b: Parse a continuous
/// static that animates a population of lands into creatures while they remain
/// lands — "All lands are 1/1 creatures that are still lands" (Living Plane,
/// Nature's Revolt), "Lands you control are X/X creatures that are still lands".
///
/// The land subject is shared with [`parse_land_type_change`]; the
/// "[P/T] creature[s] ..." remainder is delegated to the animation building
/// block (`parse_animation_spec` + `animation_modifications`), so power/
/// toughness (Layer 7b), color, keyword, and creature-subtype grants all
/// compose for free. `animation_modifications` adds the creature type
/// additively (CR 613.1d), and card types stay additive, so the land keeps its
/// land type — the "that are still lands" tail (CR 205.1b) merely confirms that
/// reading and is consumed by `split_type_retention_clause`.
///
/// Dispatched before `parse_land_type_change`; the `"creature"` guard makes
/// land *type* lines ("Lands you control are Plains") fall through unclaimed.
pub(crate) fn parse_land_animation(tp: &TextPair<'_>, text: &str) -> Option<StaticDefinition> {
    let (subject_tp, rest_tp) = tp
        .split_around(" are ")
        .or_else(|| tp.split_around(" is a "))
        .or_else(|| tp.split_around(" is an "))?;
    let affected = parse_land_type_change_subject(subject_tp.original)?;

    let rest = rest_tp.original.trim().trim_end_matches('.');
    let lower_rest = rest.to_lowercase();
    // Only claim creature-animation remainders so land type-change lines
    // ("Lands you control are Plains") fall through to parse_land_type_change.
    if !nom_primitives::scan_contains(&lower_rest, "creature") {
        return None;
    }

    // CR 205.1b: strip the "that are still land(s)" / "that's still a land"
    // retention tail. The creature type is added additively, so retention is
    // the default behavior; the clause is confirmatory. Split on the lowercased
    // text and reuse the byte offset on the original to preserve subtype casing.
    let animation_text = match super::grammar::split_type_retention_clause(&lower_rest) {
        Some((descriptor_lower, _retained)) => &rest[..descriptor_lower.len()],
        None => rest,
    }
    .trim();

    let spec = super::oracle_effect::animation::parse_animation_spec(
        animation_text,
        &mut ParseContext::default(),
    )?;
    let modifications = super::oracle_effect::animation::animation_modifications(&spec);
    if modifications.is_empty() {
        return None;
    }
    Some(
        StaticDefinition::continuous()
            .affected(affected)
            .modifications(modifications)
            .description(text.to_string()),
    )
}

/// Parse the subject of a land type-change line into a TargetFilter.
pub(crate) fn parse_land_type_change_subject(subject: &str) -> Option<TargetFilter> {
    match subject.to_lowercase().as_str() {
        // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        "nonbasic lands" => Some(TargetFilter::Typed(TypedFilter::land().properties(vec![
            // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
            FilterProp::NotSupertype {
                value: Supertype::Basic,
            },
        ]))),
        // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        "lands you control" => Some(TargetFilter::Typed(
            // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
            TypedFilter::land().controller(ControllerRef::You),
        )),
        "each land" | "all lands" => Some(TargetFilter::Typed(TypedFilter::land())),
        // CR 305.7: Aura enchantments that change the enchanted land's type.
        // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        "enchanted land" => Some(TargetFilter::Typed(
            // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
            TypedFilter::land().properties(vec![FilterProp::EnchantedBy]),
        )),
        // CR 305.7: "All <basic land type> are <type>" (Conversion, Glaciers:
        // "All Mountains are Plains"). The subject is every permanent with the
        // named basic land subtype; the SetBasicLandType predicate is applied by
        // the caller. Composes over all five basic land types, not one card.
        other => {
            let type_word = opt(tag::<_, _, OracleError<'_>>("all "))
                .parse(other)
                .map(|(rest, _)| rest.trim())
                .unwrap_or(other);
            parse_basic_land_type_plural(type_word).map(|basic| {
                TargetFilter::Typed(TypedFilter::land().subtype(basic.as_subtype_str().to_string()))
            })
        }
    }
}

/// CR 702.73a + CR 205.3 + CR 604.3 + CR 613.1d: Parse "[subject] {is|are}
/// every creature type" — the Changeling-class type grant in static form.
///
/// Self-reference (`~`) becomes a CDA so the grant functions in all zones
/// per CR 604.3 (Mistform Ultimus, Dr. Julius Jumblemorph reminder
/// "even if this card isn't on the battlefield"). Filter subjects produce
/// a normal battlefield-scoped continuous static for the same predicate.
///
/// Most filter-subject cards (e.g. Maskwood Nexus's "Creatures you control
/// are every creature type") are caught upstream by `parse_continuous_gets_has`
/// once `parse_continuous_modifications` recognizes the predicate; this
/// dispatcher catches the residual subject shapes that those code paths
/// don't strip, plus every self-reference grant.
///
/// Returns None when the line's subject doesn't map to a recognized filter.
pub(crate) fn parse_all_creature_types_grant(
    tp: &TextPair<'_>,
    text: &str,
) -> Option<StaticDefinition> {
    let (subject_tp, rest_tp) = tp
        .split_around(" is every creature type")
        .or_else(|| tp.split_around(" are every creature type"))?;
    // The predicate must terminate the line — only punctuation and trailing
    // whitespace may remain. Anything else (e.g., a hypothetical "in addition
    // to ..." extension) is outside the AddAllCreatureTypes contract and
    // should fall through to other parsers rather than be silently dropped.
    let tail = rest_tp.lower.trim().trim_end_matches('.').trim();
    if !tail.is_empty() {
        return None;
    }
    let subject = subject_tp.lower.trim();

    if subject == "~" {
        // CR 604.3 + CR 604.3a: Self-reference type-defining grant. Meets the
        // CDA criteria (defines subtypes, printed on the card it affects,
        // does not affect other objects) and so functions in all zones —
        // mirroring `synthesize_changeling_cda` for the Changeling keyword.
        return Some(
            StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .modifications(vec![ContinuousModification::AddAllCreatureTypes])
                .cda()
                .description(text.to_string()),
        );
    }

    let affected = parse_creature_type_change_subject(subject)?;
    Some(
        StaticDefinition::continuous()
            .affected(affected)
            .modifications(vec![ContinuousModification::AddAllCreatureTypes])
            .description(text.to_string()),
    )
}

/// CR 205.3 + CR 613.1d: Map the subject of an "{is|are} every creature
/// type" static into a TargetFilter restricting which battlefield objects
/// receive the grant. Sibling of `parse_land_type_change_subject` for the
/// CR 702.73a creature-type class. Counter-conditioned subjects ("each nonland
/// creature with an everything counter on it" — Omo, Queen of Vesuva) are
/// handled by `parse_counter_conditioned_nonland_creature_subject`.
pub(crate) fn parse_creature_type_change_subject(subject: &str) -> Option<TargetFilter> {
    // Combinator dispatch — each subject phrase maps to its TypedFilter
    // shape. `all_consuming` requires the whole subject to be matched, so a
    // partial prefix like "creatures" inside "creatures with X" does not
    // false-positive. "creatures" must come last among the bare-creature
    // arms so the longer "creatures you control" prefix wins first.
    all_consuming(alt((
        // CR 205.3 + CR 122.1: "each nonland creature with an everything counter
        // on it" (Omo, Queen of Vesuva). The nonland constraint reuses the
        // existing `TypeFilter::Non` building block; the counter clause is
        // delegated to `parse_counter_suffix`. No new engine surface.
        parse_counter_conditioned_nonland_creature_subject,
        map(
            tag::<_, _, OracleError<'_>>("creatures you control"),
            |_| TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
        ),
        map(tag("enchanted creature"), |_| {
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]))
        }),
        map(tag("equipped creature"), |_| {
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EquippedBy]))
        }),
        map(
            alt((tag("each creature"), tag("all creatures"), tag("creatures"))),
            |_| TargetFilter::Typed(TypedFilter::creature()),
        ),
    )))
    .parse(subject)
    .ok()
    .map(|(_, filter)| filter)
}
