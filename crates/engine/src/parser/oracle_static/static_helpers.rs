// CR 604 / CR 613 - cross-category static parser helpers.

#[allow(unused_imports)]
use super::prelude::*;
#[allow(unused_imports)]
use super::support::*;

/// CR 601.2f: Parse cost modification statics from Oracle text.
/// Handles all four sub-patterns:
/// 1. Type-filtered: "Creature spells you cast cost {1} less to cast"
/// 2. Color-filtered: "White spells your opponents cast cost {1} more to cast"
/// 3. Global taxing: "Noncreature spells cost {1} more to cast" (Thalia)
/// 4. Broad: "Spells you cast cost {1} less to cast"
/// 5. Self-spell: "This spell costs {N} less to cast for each ..." (Tolarian Terror)
///    — emitted with `affected = SelfRef`, `active_zones = [Hand, Stack, Command]`.
///
/// Dynamic "for each" counts are extracted when present.
pub(crate) fn try_parse_cost_modification(text: &str, lower: &str) -> Option<StaticDefinition> {
    let is_raise = nom_primitives::scan_contains(lower, "more to cast")
        || nom_primitives::scan_contains(lower, "more to activate");
    let is_reduce = nom_primitives::scan_contains(lower, "less to cast")
        || nom_primitives::scan_contains(lower, "less to activate");
    if !is_raise && !is_reduce {
        return None;
    }

    // CR 601.2f + CR 117.7: Detect self-spell cost reduction ("this spell costs {N} less ...").
    // Distinct from battlefield cost modification (e.g., "creature spells you cast cost {1} less")
    // because the static must apply to the card while it is in hand (or on the stack during
    // casting), not once it has entered the battlefield. The caller wires this into
    // `active_zones = [Hand, Stack, Command]` with `affected = SelfRef` so
    // the casting-time scanner finds it on the spell being cast from normal
    // hand casting, the cost-determination stack step, and commander casting
    // from the command zone.
    let is_self_spell = parse_self_spell_cost_subject(lower).is_some();

    let amount_is_variable_x = nom_primitives::scan_contains(lower, "{x}");

    // Extract the mana amount from the text (look for {N} pattern)
    let amount = if let Some(brace_start) = text.find('{') {
        let cost_fragment = &text[brace_start..];
        parse_mana_symbols(cost_fragment)
            .map(|(cost, _)| cost)
            .unwrap_or_else(|| ManaCost::generic(1))
    } else {
        ManaCost::generic(1)
    };

    // Determine player scope from "you cast", "your opponents cast", or bare
    let controller = if nom_primitives::scan_contains(lower, "your opponents cast")
        || nom_primitives::scan_contains(lower, "opponents cast")
    {
        Some(ControllerRef::Opponent)
    } else if nom_primitives::scan_contains(lower, "you cast") {
        Some(ControllerRef::You)
    } else {
        // Bare "spells cost more/less" — affects all players' spells.
        // For "Noncreature spells cost {1} more", both players are affected
        // in the casting check — no controller restriction on affected.
        None
    };

    let first_qualified_spell_filter = parse_first_qualified_spell_filter(lower);
    let target_cost_filter = parse_cost_modifier_target_filter(lower);

    // Extract "from [zone(s)]" clause between player scope and "cost".
    // E.g., "cast from graveyards or from exile" → [Graveyard, Exile]
    // This must be extracted before type parsing so it doesn't pollute type_desc.
    let cast_from_zones: Vec<Zone> = {
        let mut zones = Vec::new();
        // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        if let Some(cost_idx) = lower.find(" cost") {
            // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
            let prefix = &lower[..cost_idx];
            // Look for "from <zone> or from <zone>" or "from <zone>" after "cast".
            // Use the first " from " to capture compound patterns like
            // "from graveyards or from exile".
            // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
            if let Some(from_idx) = prefix.find(" from ") {
                // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
                let from_text = &prefix[from_idx..];
                // Skip "from anywhere other than" — that's a negation pattern
                // requiring a Not filter, not a direct zone match.
                if !nom_primitives::scan_contains(from_text, "anywhere other than") {
                    if nom_primitives::scan_contains(from_text, "graveyard") {
                        zones.push(Zone::Graveyard);
                    }
                    if nom_primitives::scan_contains(from_text, "exile") {
                        zones.push(Zone::Exile);
                    }
                    if nom_primitives::scan_contains(from_text, "hand") {
                        zones.push(Zone::Hand);
                    }
                    if nom_primitives::scan_contains(from_text, "command zone") {
                        zones.push(Zone::Command);
                    }
                }
            }
        }
        zones
    };

    // Extract spell type filter from the text before "cost"
    // E.g., "Creature spells you cast" → Creature, "Instant and sorcery spells" → AnyOf(Instant, Sorcery)
    let spell_filter = if is_self_spell {
        parse_self_spell_target_cost_filter(lower)
    } else if let Some(filter) = first_qualified_spell_filter.clone() {
        Some(filter)
    // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
    } else if let Some(cost_idx) = lower.find(" cost") {
        // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        let prefix = &lower[..cost_idx];
        let prefix = strip_cost_modifier_target_clause(prefix);
        // Strip "from [zones]" clause (only if zones were detected), player scope, then "spells"
        let without_from = if !cast_from_zones.is_empty() {
            // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
            if let Some(from_idx) = prefix.find(" from ") {
                // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
                &prefix[..from_idx]
            } else {
                prefix
            }
        } else {
            prefix
        };
        // CR 201.3 / CR 113.6: Strip the trailing "with the chosen name" qualifier
        // (Disruptor Flute: "Spells with the chosen name cost {3} more to cast.")
        // before the standard suffix-trim chain runs. Track it so the spell filter is
        // composed with `HasChosenName` after type parsing — same convention used by
        // `parse_continuous_subject_filter` for object-class chosen-name phrases.
        let (without_chosen, has_chosen_name) =
            match nom_primitives::split_once_on(without_from, " with the chosen name") {
                Ok((_, (before, _))) => (before, true),
                Err(_) => (without_from, false),
            };
        // CR 205.2a + CR 601.2f: Strip the "of the chosen type" / "of that type"
        // qualifier (Cloud Key, Umori, Stenn, Herald's Horn: "Spells you cast of
        // the chosen type cost {1} less"). A "you cast" infix sits between the
        // type word and this qualifier, so the trim chain below can't reach the
        // type word and `parse_type_phrase` never extracts the chosen-type
        // discriminator. Strip it here and re-attach IsChosenCardType /
        // IsChosenCreatureType after the base type is parsed — mirrors the
        // "with the chosen name" handling above.
        let (without_chosen, has_chosen_type) = if let Ok((_, (before, _))) =
            nom_primitives::split_once_on(without_chosen, " of the chosen type")
        {
            (before, true)
        } else if let Ok((_, (before, _))) =
            nom_primitives::split_once_on(without_chosen, " of that type")
        {
            (before, true)
        } else {
            (without_chosen, false)
        };
        let type_desc = without_chosen
            .trim_end_matches(" you cast") // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
            .trim_end_matches(" your opponents cast") // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
            .trim_end_matches(" opponents cast") // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
            .trim_end_matches(" spells") // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
            .trim_end_matches(" spell") // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
            .trim();
        // "spells" alone means no type restriction (bare "Spells you cast cost...")
        let typed_filter = if type_desc.is_empty() || type_desc == "spells" || type_desc == "spell"
        {
            None
        } else {
            // First try parse_type_phrase for standard type patterns
            let (filter, _) = parse_type_phrase(type_desc);
            match &filter {
                // Single type: "creature", "noncreature", "artifact"
                TargetFilter::Typed(tf)
                    if !tf.type_filters.is_empty() || !tf.properties.is_empty() =>
                {
                    Some(filter)
                }
                // Combined types: "instant and sorcery", "artifact or enchantment"
                TargetFilter::Or { filters } if !filters.is_empty() => Some(filter),
                _ => {
                    // Fallback: check for bare color names ("white", "blue", etc.)
                    parse_named_color(type_desc).map(|color| {
                        TargetFilter::Typed(
                            TypedFilter::card().properties(vec![FilterProp::HasColor { color }]),
                        )
                    })
                }
            }
        };
        // CR 205.2a: Re-attach the chosen-type discriminator stripped above. A
        // creature-typed base ("Creature spells ... of the chosen type",
        // Herald's Horn) pairs with a chosen CREATURE type; a bare-spells base
        // ("Spells ... of the chosen type", Cloud Key / Umori / Stenn) pairs
        // with a chosen CARD type. Resolved at cast time against the source
        // permanent's `ChosenAttribute` (CR 601.2f).
        let typed_filter = if has_chosen_type {
            match typed_filter {
                Some(TargetFilter::Typed(mut tf))
                    if tf.type_filters.contains(&TypeFilter::Creature) =>
                {
                    tf.properties.push(FilterProp::IsChosenCreatureType);
                    Some(TargetFilter::Typed(tf))
                }
                Some(TargetFilter::Typed(mut tf)) => {
                    tf.properties.push(FilterProp::IsChosenCardType);
                    Some(TargetFilter::Typed(tf))
                }
                None => Some(TargetFilter::Typed(
                    TypedFilter::card().properties(vec![FilterProp::IsChosenCardType]),
                )),
                other => other,
            }
        } else {
            typed_filter
        };
        // Compose chosen-name constraint with the typed prefix (if any). Bare
        // "Spells with the chosen name" → `HasChosenName` alone; typed
        // "<Type> spells with the chosen name" → `And{Typed, HasChosenName}`.
        match (typed_filter, has_chosen_name) {
            (Some(tf), true) => Some(TargetFilter::And {
                filters: vec![tf, TargetFilter::HasChosenName],
            }),
            (None, true) => Some(TargetFilter::HasChosenName),
            (tf, false) => tf,
        }
    } else {
        None
    };

    let spell_filter = merge_cost_modifier_target_filter(spell_filter, target_cost_filter);

    // Merge cast-from-zone restriction into the spell filter.
    // If zones were extracted, add InZone/InAnyZone to ensure the cost modification
    // only applies when the spell is being cast from the specified zone(s).
    let spell_filter = if !cast_from_zones.is_empty() {
        let zone_prop = if cast_from_zones.len() == 1 {
            FilterProp::InZone {
                zone: cast_from_zones[0],
            }
        } else {
            FilterProp::InAnyZone {
                zones: cast_from_zones,
            }
        };
        match spell_filter {
            Some(TargetFilter::Typed(mut tf)) => {
                tf.properties.push(zone_prop);
                Some(TargetFilter::Typed(tf))
            }
            Some(other) => {
                // Wrap non-Typed filters with an And that adds the zone constraint.
                Some(TargetFilter::And {
                    filters: vec![
                        other,
                        TargetFilter::Typed(TypedFilter::card().properties(vec![zone_prop])),
                    ],
                })
            }
            None => {
                // No type filter, just zone restriction (e.g., "Spells ... cast from exile cost more")
                Some(TargetFilter::Typed(
                    TypedFilter::card().properties(vec![zone_prop]),
                ))
            }
        }
    } else {
        spell_filter
    };

    // Detect dynamic "for each" count pattern
    // "for each artifact you control" → QuantityRef::ObjectCount
    let cost_tp = TextPair::new(text, lower);
    let mut dynamic_count = if let Some((_, after_for_each)) = cost_tp.split_around("for each ") {
        // Strip trailing period/punctuation
        let count_text = after_for_each.original.trim_end_matches('.');
        super::oracle_quantity::parse_for_each_clause(count_text)
            .or_else(|| {
                parse_cda_quantity(count_text).and_then(|expr| match expr {
                    QuantityExpr::Ref { qty } => Some(qty),
                    _ => None,
                })
            })
            .or_else(|| super::oracle_quantity::parse_quantity_ref(count_text))
            .or_else(|| {
                // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
                if let Some(prefixed) = count_text.strip_prefix("the number of ") {
                    // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
                    super::oracle_quantity::parse_quantity_ref(prefixed)
                } else {
                    None
                }
            })
            .or_else(|| {
                let (count_filter, _) = parse_type_phrase(count_text);
                Some(QuantityRef::ObjectCount {
                    filter: count_filter,
                })
            })
    } else {
        None
    };

    if dynamic_count.is_none() && amount_is_variable_x {
        let (_, where_x_text) = super::oracle_effect::strip_trailing_where_x(cost_tp);
        if let Some(expression) = where_x_text {
            if let Some(QuantityExpr::Ref { qty }) = parse_cda_quantity(&expression) {
                dynamic_count = Some(qty);
            }
        }
    }

    let amount = if amount_is_variable_x {
        ManaCost::generic(1)
    } else {
        amount
    };

    let mode = if is_raise {
        StaticMode::RaiseCost {
            amount,
            spell_filter: spell_filter.clone(),
            dynamic_count: dynamic_count.clone(),
        }
    } else {
        StaticMode::ReduceCost {
            amount,
            spell_filter: spell_filter.clone(),
            dynamic_count: dynamic_count.clone(),
        }
    };

    // Build the affected filter for the static definition.
    // This controls which objects are "affected" — for cost modification statics,
    // this is the source permanent's controller scope (used by the registry).
    // CR 117.7: Self-spell cost reduction ("This spell costs {N} less ...") uses
    // SelfRef so the casting-time self-cost scanner matches it on the spell itself.
    let affected = if is_self_spell {
        TargetFilter::SelfRef
    } else {
        match controller {
            Some(ControllerRef::You) => {
                TargetFilter::Typed(TypedFilter::card().controller(ControllerRef::You))
            }
            Some(ControllerRef::Opponent) => {
                TargetFilter::Typed(TypedFilter::card().controller(ControllerRef::Opponent))
            }
            // CR 109.4: TargetPlayer has no defined semantics here (cost-modification
            // static scoping). Fall back to an untyped filter; the parser should not
            // emit this variant for cost statics.
            Some(ControllerRef::ScopedPlayer) => TargetFilter::Typed(TypedFilter::card()),
            Some(ControllerRef::TargetPlayer) => TargetFilter::Typed(TypedFilter::card()),
            Some(ControllerRef::ParentTargetController) => TargetFilter::Typed(TypedFilter::card()),
            Some(ControllerRef::DefendingPlayer) => TargetFilter::Typed(TypedFilter::card()),
            // CR 109.4: Chosen-player scope is not emitted for cost statics.
            Some(ControllerRef::ChosenPlayer { .. }) => TargetFilter::Typed(TypedFilter::card()),
            // CR 603.2 + CR 109.4: Triggering-player scope is not emitted for
            // cost statics. Fall back to an untyped filter.
            Some(ControllerRef::TriggeringPlayer) => TargetFilter::Typed(TypedFilter::card()),
            None => TargetFilter::Typed(TypedFilter::card()),
        }
    };

    let mut definition = StaticDefinition::new(mode)
        .affected(affected)
        .description(text.to_string());

    // CR 117.7 + CR 601.2f: A self-spell cost reduction must apply while the
    // card is in hand (pre-cast affordability checks), in the command zone
    // (commander casting), and on the stack (final cost determination during
    // casting). Without opting in via `active_zones`, layer collection would
    // ignore the static outside the battlefield, and the card would never
    // reduce its own cost.
    if is_self_spell {
        definition.active_zones = vec![Zone::Hand, Zone::Stack, Zone::Command];
    }
    if let Some(filter) = first_qualified_spell_filter.as_ref() {
        definition.condition = Some(first_qualified_spell_condition(filter));
    }

    // Extract trailing "if [condition]" / "as long as [condition]" clause from
    // cost modification lines.
    // Patterns:
    // - "This spell costs {N} less to cast if you control a Wizard."
    // - "Spells you cast cost {1} less to cast as long as there are three or more Lesson cards in your graveyard."
    // Uses the shared nom condition combinator to handle the full class of conditions.
    if definition.condition.is_none() {
        if let Some((cond_pos, marker)) = [" as long as ", " if "]
            .into_iter()
            .filter_map(|marker| lower.rfind(marker).map(|pos| (pos, marker)))
            .max_by_key(|(pos, _)| *pos)
        {
            let cond_text = lower[cond_pos + marker.len()..]
                .trim()
                .trim_end_matches('.');
            if let Some(sc) = parse_cost_modifier_condition(cond_text) {
                definition.condition = Some(sc);
            } else if let Ok((rest, sc)) = nom_condition::parse_inner_condition(cond_text) {
                if rest.trim().is_empty() || rest.trim() == "." {
                    definition.condition = Some(sc);
                }
            }
        }
    }

    Some(definition)
}

pub(crate) fn parse_cost_modifier_condition(cond_text: &str) -> Option<StaticCondition> {
    // CR 702.166a: "This spell costs {N} less to cast if it's bargained" — route the
    // bargained predicate to the cost-determination StaticCondition. Checked ahead of
    // the "another spell" delegation and the parse_inner_condition fallback so the
    // bargained arm wins.
    if let Some(sc) = parse_bargained_condition(cond_text) {
        return Some(sc);
    }
    let (rest, filter) = parse_cost_modifier_another_spell_condition(cond_text).ok()?;
    if !rest.trim().is_empty() {
        return None;
    }
    Some(StaticCondition::QuantityComparison {
        lhs: QuantityExpr::Ref {
            qty: QuantityRef::SpellsCastThisTurn {
                scope: CountScope::Controller,
                filter: if filter == TargetFilter::Any {
                    None
                } else {
                    Some(filter)
                },
            },
        },
        comparator: Comparator::GE,
        rhs: QuantityExpr::Fixed { value: 1 },
    })
}

/// CR 702.166a: Match the bargained predicate of a self-spell cost-reduction line
/// ("This spell costs {N} less to cast if it's bargained"). `cond_text` is already
/// lowercase. Returns `StaticCondition::AdditionalCostPaid` — Bargain's optional
/// sacrifice sets `additional_cost_paid` on the in-flight cast.
pub(crate) fn parse_bargained_condition(cond_text: &str) -> Option<StaticCondition> {
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>("it's bargained"),
        tag("it is bargained"),
        tag("it was bargained"),
        tag("this spell is bargained"),
    ))
    .parse(cond_text)
    .ok()?;
    if !rest.trim().is_empty() {
        return None;
    }
    Some(StaticCondition::AdditionalCostPaid)
}

pub(crate) fn parse_cost_modifier_another_spell_condition(
    input: &str,
) -> OracleResult<'_, TargetFilter> {
    let (rest, _) = alt((tag("you've cast another "), tag("you cast another "))).parse(input)?;
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("spell this turn").parse(rest) {
        return Ok((rest, TargetFilter::Any));
    }
    let (rest, type_text) = take_until(" spell this turn").parse(rest)?;
    let (rest, _) = tag(" spell this turn").parse(rest)?;
    let Some(filter) = nom_condition::parse_spell_history_filter(type_text) else {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    };
    Ok((rest, filter))
}

/// Parse a basic land type name (case-insensitive) to its enum variant.
pub(crate) fn parse_basic_land_type(name: &str) -> Option<BasicLandType> {
    match name.to_ascii_lowercase().as_str() {
        "plains" => Some(BasicLandType::Plains), // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        "island" => Some(BasicLandType::Island), // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        "swamp" => Some(BasicLandType::Swamp), // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        "mountain" => Some(BasicLandType::Mountain), // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        "forest" => Some(BasicLandType::Forest), // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        _ => None,
    }
}

/// Parse a basic land type name, accepting both singular and plural forms.
/// "Mountains" → Mountain, "Islands" → Island. "Plains" is already valid singular.
pub(crate) fn parse_basic_land_type_plural(name: &str) -> Option<BasicLandType> {
    // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
    parse_basic_land_type(name).or_else(|| name.strip_suffix('s').and_then(parse_basic_land_type))
    // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
}

/// CR 305.7: Parse a comma-and-separated list of basic land types.
/// "Mountain, Forest, and Plains" → [Mountain, Forest, Plains].
/// Also handles single types: "Island" → [Island].
pub(crate) fn parse_basic_land_type_list(text: &str) -> Option<Vec<BasicLandType>> {
    // Try single type first (most common case)
    if let Some(single) = parse_basic_land_type_plural(text) {
        return Some(vec![single]);
    }
    // Split on ", " and " and " for multi-type lists
    let mut types = Vec::new();
    for part in text.split(", ") {
        let part = part.trim();
        // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        if let Some(rest) = part.strip_prefix("and ") {
            // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
            types.push(parse_basic_land_type(rest.trim())?);
        // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        } else if let Some((before, after)) = part.split_once(" and ") {
            // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
            types.push(parse_basic_land_type(before.trim())?);
            types.push(parse_basic_land_type(after.trim())?);
        } else {
            types.push(parse_basic_land_type(part)?);
        }
    }
    if types.len() >= 2 {
        Some(types)
    } else {
        None
    }
}

/// CR 105.1 / CR 105.2c: Parse a color expression terminating an
/// "All [subject] are ___" static. Accepts either the literal word "colorless"
/// (→ empty color set, CR 105.2c), "all/every color" (→ WUBRG, CR 105.2), or
/// any color list recognized by `parse_color_list` — single color, "X and Y",
/// or "X, Y, and Z" (CR 105.1).
/// Input must be fully consumed by the combinator path; trailing content
/// returns `None` so the outer dispatcher falls through.
pub(crate) fn parse_color_predicate(text: &str) -> Option<Vec<ManaColor>> {
    // CR 105.2: "all colors" / "every color" means the full WUBRG set.
    if let Ok((rest, _)) = alt((
        tag::<_, _, OracleError<'_>>("all colors"),
        tag("every color"),
    ))
    .parse(text)
    {
        if rest.is_empty() {
            return Some(ManaColor::ALL.to_vec());
        }
    }

    if let Some(rest) = nom_tag_lower(text, text, "colorless") {
        if rest.is_empty() {
            return Some(Vec::new());
        }
    }
    parse_color_list(text)
}

/// CR 604.3 + CR 604.3a + CR 105.2c + CR 613.1e: Parse self-referential
/// "[self subject] is [color expression]." lines into a color CDA.
///
/// This covers the class of card text that defines the source object's own
/// color as a characteristic (Ghostfire-style), not global/class filters
/// handled by `parse_all_subject_are_color`.
pub(crate) fn parse_self_subject_is_color_cda(
    tp: &TextPair<'_>,
    description: &str,
) -> Option<StaticDefinition> {
    let (_, colors) = parse_self_subject_is_color_cda_line(tp.lower).ok()?;

    Some(
        StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(vec![ContinuousModification::SetColor { colors }])
            .active_zones(vec![
                Zone::Library,
                Zone::Hand,
                Zone::Battlefield,
                Zone::Graveyard,
                Zone::Stack,
                Zone::Exile,
                Zone::Command,
            ])
            .cda()
            .description(description.to_string()),
    )
}

pub(crate) fn parse_self_subject_is_color_cda_line(
    input: &str,
) -> OracleResult<'_, Vec<ManaColor>> {
    let (after_subject, _) = parse_self_color_subject(input)?;
    let (after_predicate, predicate_lower) = alt((
        terminated(take_until::<_, _, OracleError<'_>>("."), tag(".")),
        rest,
    ))
    .parse(after_subject)?;
    eof::<_, OracleError<'_>>(after_predicate)?;
    let Some(colors) = parse_color_predicate(predicate_lower) else {
        return Err(nom::Err::Error(nom::error::Error::new(
            predicate_lower,
            nom::error::ErrorKind::Fail,
        )));
    };
    Ok((after_predicate, colors))
}

pub(crate) fn parse_self_color_subject(input: &str) -> OracleResult<'_, ()> {
    let (rest, _) = alt((
        value((), tag::<_, _, OracleError<'_>>("~")),
        value((), tag("this card")),
        value((), tag("this spell")),
        parse_self_ref_type_subject,
    ))
    .parse(input)?;
    let (rest, _) = tag(" is ").parse(rest)?;
    Ok((rest, ()))
}

pub(crate) fn parse_self_ref_type_subject(input: &str) -> OracleResult<'_, ()> {
    for phrase in SELF_REF_TYPE_PHRASES {
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>(*phrase).parse(input) {
            return Ok((rest, ()));
        }
    }
    Err(nom::Err::Error(nom::error::Error::new(
        input,
        nom::error::ErrorKind::Fail,
    )))
}

/// CR 205.3 + CR 122.1: Parse "[each] nonland creature with an everything
/// counter on it" into a creature `TypedFilter` carrying
/// `TypeFilter::Non(Land)` plus the counter `FilterProp` produced by the shared
/// `parse_counter_suffix` combinator. Used by Omo, Queen of Vesuva's
/// "Each nonland creature with an everything counter on it is every creature
/// type" — routes to `AddAllCreatureTypes` via `parse_all_creature_types_grant`.
pub(crate) fn parse_counter_conditioned_nonland_creature_subject(
    input: &str,
) -> OracleResult<'_, TargetFilter> {
    let (input, _) = opt(alt((
        tag::<_, _, OracleError<'_>>("each "),
        tag::<_, _, OracleError<'_>>("all "),
    )))
    .parse(input)?;
    let (input, _) = tag("nonland ").parse(input)?;
    let (input, _) = alt((tag("creatures"), tag("creature"))).parse(input)?;
    let (input, _) = space1.parse(input)?;
    // Delegate the counter clause (e.g. "with an everything counter on it") to
    // the shared building block rather than re-implementing counter recognition.
    let Some((counter_prop, consumed)) = parse_counter_suffix(input) else {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Tag,
        )));
    };
    let filter = TargetFilter::Typed(
        TypedFilter::creature()
            .with_type(TypeFilter::Non(Box::new(TypeFilter::Land)))
            .properties(vec![counter_prop]),
    );
    Ok((&input[consumed..], filter))
}

/// CR 604.1: Strip turn-condition suffixes from predicate text.
///
/// Handles "during your turn" and "during turns other than yours" suffixes
/// on keyword/modification predicates. Returns the stripped predicate and
/// the corresponding `StaticCondition`, or the original text with `None`.
pub(crate) fn strip_suffix_turn_condition(text: &str) -> (String, Option<StaticCondition>) {
    let trimmed = text.trim_end_matches('.');
    // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
    if let Some(rest) = trimmed.strip_suffix(" during your turn") {
        // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        (format!("{rest}."), Some(StaticCondition::DuringYourTurn))
    // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
    } else if let Some(rest) = trimmed.strip_suffix(" during turns other than yours") {
        // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        (
            format!("{rest}."),
            Some(StaticCondition::Not {
                condition: Box::new(StaticCondition::DuringYourTurn),
            }),
        )
    } else {
        (text.to_string(), None)
    }
}

/// Strip "in addition to {its/their} other {land }types" suffix,
/// returning the type name before it.
pub(crate) fn strip_in_addition_suffix(text: &str) -> Option<&str> {
    [
        " in addition to its other land types",
        " in addition to its other types",
        " in addition to their other land types",
        " in addition to their other types",
    ]
    .iter()
    .find_map(|suffix| text.strip_suffix(suffix)) // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
}

/// CR 502.3: Extract a trailing condition from a "doesn't untap during [untap step]" clause.
/// Handles patterns like:
/// - "doesn't untap during your untap step as long as [condition]"
/// - "doesn't untap during your untap step if [condition]"
pub(crate) fn extract_cant_untap_condition(lower: &str) -> Option<StaticCondition> {
    // Find the end of the "untap step" phrase
    let untap_phrases = [
        "its controller's untap step",
        "its controller\u{2019}s untap step",
        "their controllers' untap steps",
        "your untap step",
    ];
    let mut after_untap = None;
    for phrase in &untap_phrases {
        if let Some(pos) = lower.find(phrase) {
            let end = pos + phrase.len();
            after_untap = Some(lower[end..].trim().trim_end_matches('.'));
            break;
        }
    }
    let remaining = after_untap?;
    if remaining.is_empty() {
        return None;
    }
    // Strip "as long as" or "if" prefix
    let condition_text = nom_tag_lower(remaining, remaining, "as long as ")
        .or_else(|| nom_tag_lower(remaining, remaining, "if "))?;
    parse_static_condition(condition_text).or_else(|| {
        Some(StaticCondition::Unrecognized {
            text: condition_text.to_string(),
        })
    })
}
