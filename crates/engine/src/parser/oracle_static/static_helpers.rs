// CR 604 / CR 613 - cross-category static parser helpers.

#[allow(unused_imports)]
use super::prelude::*;
#[allow(unused_imports)]
use super::support::*;
use nom::character::complete::multispace0;

/// CR 113.6 + CR 201.2: Recognize the "sources with the chosen name" / "cards with
/// the chosen name" subject phrase and map it to `TargetFilter::HasChosenName`.
/// Shared by the chosen-name name-picker classes — the `CantBeActivated`
/// prohibition (Pithing Needle / Phyrexian Revoker / Sorcerous Spyglass) and the
/// directional activated-ability cost modifier (Skyseer's Chariot). Returns
/// `None` for any other subject so callers fall back to `parse_type_phrase`.
pub(crate) fn parse_chosen_name_source_filter(subject_lower: &str) -> Option<TargetFilter> {
    let trimmed = subject_lower.trim();
    value(
        TargetFilter::HasChosenName,
        all_consuming(alt((
            tag::<_, _, OracleError<'_>>("sources with the chosen name"),
            tag("cards with the chosen name"),
        ))),
    )
    .parse(trimmed)
    .ok()
    .map(|(_, filter)| filter)
}

/// CR 601.2f: Parse cost modification statics from Oracle text.
/// Handles all four sub-patterns:
/// 1. Type-filtered: "Creature spells you cast cost {1} less to cast"
/// 2. Color-filtered: "White spells your opponents cast cost {1} more to cast"
/// 3. Global taxing: "Noncreature spells cost {1} more to cast" (Thalia)
/// 4. Broad: "Spells you cast cost {1} less to cast"
/// 5. Self-spell: "This spell costs {N} less to cast for each ..." (Tolarian Terror)
///    — emitted with `affected = SelfRef`, `active_zones = self_spell_cost_mod_active_zones()`.
///
/// CR 601.2f: Parse the spell-type prefix of a cost-modification line before
/// `"cost"`. Handles compound subjects such as Goblin Anarchomancer's
/// "Each spell you cast that's red or green" via `parse_that_clause_suffix`.
/// CR 205.4a: A bare supertype spell subject ("Legendary spells you cast cost
/// {1} less", Kethis, the Hidden Hand) — `parse_type_phrase` doesn't consume a
/// lone supertype word (it requires a following type noun), so the restriction
/// would otherwise drop and reduce the cost of EVERY spell. Emit a `HasSupertype`
/// card filter instead.
fn parse_bare_supertype_spell_filter(base: &str) -> Option<TargetFilter> {
    let lower = base.to_lowercase();
    let (_, supertype) = all_consuming(nom_target::parse_supertype_word)
        .parse(lower.as_str())
        .ok()?;
    Some(TargetFilter::Typed(TypedFilter::card().properties(vec![
        FilterProp::HasSupertype { value: supertype },
    ])))
}

/// CR 105.2 + CR 700.6 + CR 205.4a + CR 601.2f: Resolve a BARE-word spell-subject
/// filter for a cost modifier — a color or color-CATEGORY ("white", "colorless",
/// "monocolored", "multicolored"), "historic", or a supertype ("legendary").
/// `parse_type_phrase` declines all of these because they carry no trailing type
/// noun (it needs "white creature", not a lone "white"), so without this the
/// whole restriction dropped and the cost modifier (mis)applied to EVERY spell.
///
/// The color word routes through the single [`nom_filter::parse_color_property`]
/// authority, so the color-CATEGORY axis resolves identically to the noun-bearing
/// path: colorless → `ColorCount { EQ, 0 }`, monocolored → `{ EQ, 1 }`,
/// multicolored → `{ GE, 2 }`, a named color → `HasColor`. The prior fallback
/// hand-rolled only the five named colors via `parse_named_color`, so it dropped
/// the category axis (Herald of Kozilek, Ugin, Urza's Filter, It That Heralds the
/// End) and "historic" (Jhoira's Familiar) — all of which then cheapened every spell.
fn parse_bare_spell_subject_filter(base: &str) -> Option<TargetFilter> {
    let lower = base.trim().to_ascii_lowercase();
    // CR 105.2: color / color-category, via the single color-property authority.
    if let Ok((_, prop)) = all_consuming(nom_filter::parse_color_property).parse(lower.as_str()) {
        return Some(TargetFilter::Typed(
            TypedFilter::card().properties(vec![prop]),
        ));
    }
    // CR 700.6: "historic" = legendary supertype, artifact card type, or Saga subtype.
    if all_consuming(tag::<_, _, OracleError<'_>>("historic"))
        .parse(lower.as_str())
        .is_ok()
    {
        return Some(TargetFilter::Typed(
            TypedFilter::card().properties(vec![FilterProp::Historic]),
        ));
    }
    // CR 205.4a: a lone supertype word ("legendary", "snow", ...).
    parse_bare_supertype_spell_filter(&lower)
}

/// CR 202.3: Nom parse of a trailing "with/of mana value N or greater/less"
/// (also "or more"/"or fewer") qualifier — returns the prefix BEFORE the
/// qualifier plus the `FilterProp::Cmc` it selects. Fails (nom `Err`) when no
/// mana-value qualifier is present.
fn parse_cost_mod_mana_value_qualifier(prefix: &str) -> OracleResult<'_, (&str, FilterProp)> {
    let (rest, before) = alt((
        take_until::<_, _, OracleError<'_>>(" with mana value "),
        take_until(" of mana value "),
    ))
    .parse(prefix)?;
    let (rest, _) = alt((tag(" with mana value "), tag(" of mana value "))).parse(rest)?;
    let (rest, mv) = nom_primitives::parse_number(rest)?;
    let (rest, comparator) = alt((
        value(Comparator::GE, alt((tag(" or greater"), tag(" or more")))),
        value(Comparator::LE, alt((tag(" or less"), tag(" or fewer")))),
    ))
    .parse(rest)?;
    Ok((
        rest,
        (
            before,
            FilterProp::Cmc {
                comparator,
                value: QuantityExpr::Fixed { value: mv as i32 },
            },
        ),
    ))
}

/// CR 202.3 + CR 601.2f: Peel a trailing "with mana value N or greater/less"
/// spell-selection qualifier off a cost-modifier subject prefix, returning the
/// prefix WITHOUT the qualifier plus the parsed `FilterProp::Cmc`. The mana-value
/// gate sits AFTER the "spells you cast" infix ("Instant and sorcery spells you
/// cast with mana value 4 or greater cost {X} less" — The Scarlet Witch, #5606),
/// so the caller's "you cast"/"spells" end-trims cannot reach the type words
/// until this qualifier is peeled. Covers the whole MV-gated cost-modifier class,
/// not one card.
fn strip_cost_mod_mana_value_qualifier(prefix: &str) -> (&str, Option<FilterProp>) {
    match parse_cost_mod_mana_value_qualifier(prefix) {
        Ok((_, (before, prop))) => (before, Some(prop)),
        Err(_) => (prefix, None),
    }
}

/// Compose an optional mana-value `FilterProp::Cmc` (from
/// `strip_cost_mod_mana_value_qualifier`) into the cost-modifier spell filter.
/// A typed filter absorbs the prop directly; an `Or` (or any non-`Typed`)
/// filter is `And`-wrapped with a card+prop leaf; a bare mana-value gate with no
/// type restriction ("spells you cast with mana value 4 or greater") becomes a
/// card filter carrying the prop.
fn compose_cost_mod_mana_value(
    filter: Option<TargetFilter>,
    prop: Option<FilterProp>,
) -> Option<TargetFilter> {
    let Some(prop) = prop else {
        return filter;
    };
    match filter {
        Some(TargetFilter::Typed(mut tf)) => {
            tf.properties.push(prop);
            Some(TargetFilter::Typed(tf))
        }
        Some(other) => Some(TargetFilter::And {
            filters: vec![
                other,
                TargetFilter::Typed(TypedFilter::card().properties(vec![prop])),
            ],
        }),
        None => Some(TargetFilter::Typed(
            TypedFilter::card().properties(vec![prop]),
        )),
    }
}

fn parse_cost_mod_spell_type_prefix(type_desc: &str) -> Option<TargetFilter> {
    let base = type_desc.trim();
    let base = tag::<_, _, OracleError<'_>>("each ")
        .parse(base)
        .map_or(base, |(rest, _)| rest);

    // CR 105.1 + CR 601.2f: Compound BARE-color subject — "<color> spells and
    // <color> spells" (the Prophecy Familiar cycle: Nightscape / Stormscape /
    // Sunscape / Thornscape / Thunderscape Familiar). The single-subject path
    // below maps a lone bare color via `parse_named_color`, and
    // `parse_type_phrase` decomposes compounds whose operands carry a type noun
    // ("Angel spells and Human spells", "red creature spells and green creature
    // spells"). A two-BARE-color compound falls through both and yields
    // `None` — which silently drops the color restriction and reduces EVERY
    // spell. Recognize it here and emit the same `Or` of `HasColor` typed
    // filters the noun-bearing compounds already produce.
    if let Some(filter) = parse_cost_mod_compound_color_subject(base) {
        return Some(filter);
    }

    let that_split: Result<(&str, (&str, &str)), nom::Err<OracleError<'_>>> = all_consuming(alt((
        (
            take_until(" that's "),
            recognize((tag::<_, _, OracleError<'_>>(" that's "), rest)),
        ),
        (
            take_until(" that is "),
            recognize((tag::<_, _, OracleError<'_>>(" that is "), rest)),
        ),
        (
            take_until(" that are "),
            recognize((tag::<_, _, OracleError<'_>>(" that are "), rest)),
        ),
        (
            take_until(" that "),
            recognize((tag::<_, _, OracleError<'_>>(" that "), rest)),
        ),
    )))
    .parse(base);

    let (base_part, qual_props) = if let Ok((_, (before, suffix))) = that_split {
        let suffix = suffix.trim_start();
        let (props, consumed) =
            crate::parser::oracle_target::parse_that_clause_suffix(suffix, None)?;
        if !suffix[consumed..].trim().is_empty() {
            return None;
        }
        (before.trim(), props)
    } else {
        (base, Vec::new())
    };

    let base_part = strip_cost_mod_cast_scope_suffix(base_part);
    let base_part = strip_cost_mod_spell_noun_suffix(base_part);

    let typed_filter = if base_part.is_empty() {
        None
    } else {
        let (filter, remainder) = parse_type_phrase(base_part);
        let remainder = remainder.trim();
        match &filter {
            TargetFilter::Typed(tf)
                if (!tf.type_filters.is_empty() || !tf.properties.is_empty())
                    && remainder.is_empty() =>
            {
                Some(filter)
            }
            TargetFilter::Or { filters } if !filters.is_empty() && remainder.is_empty() => {
                Some(filter)
            }
            // Bare color/color-category words ("white", "colorless",
            // "multicolored"), "historic", and bare supertype words ("legendary")
            // are not consumed by parse_type_phrase, which requires a trailing type
            // noun ("white creature", "legendary permanent"). Route them through
            // the single bare-subject authority so the color-category axis is not
            // dropped (CR 105.2 + CR 700.6 + CR 205.4a).
            _ if remainder.is_empty() || remainder.eq_ignore_ascii_case(base_part) => {
                parse_bare_spell_subject_filter(base_part)
            }
            _ => None,
        }
    };

    let filter = match (typed_filter, qual_props.is_empty()) {
        (filter, true) => filter,
        (Some(TargetFilter::Typed(mut tf)), false) => {
            tf.properties.extend(qual_props);
            Some(TargetFilter::Typed(tf))
        }
        (Some(other), false) => Some(TargetFilter::And {
            filters: vec![
                other,
                TargetFilter::Typed(TypedFilter::card().properties(qual_props)),
            ],
        }),
        (None, false) => Some(TargetFilter::Typed(
            TypedFilter::card().properties(qual_props),
        )),
    };
    filter.map(remap_cost_mod_imprint_exile_reference)
}

/// CR 105.1 + CR 601.2f: Decompose a compound BARE-color cost-mod subject —
/// "<color>[ spells] and <color>[ spells]" — into an `Or` of `HasColor` typed
/// filters. Each operand is a color name (`nom_primitives::parse_color`)
/// optionally trailed by the spell noun; operands are joined by " and ".
///
/// The Prophecy Familiar cycle (Nightscape Familiar "Blue spells and red
/// spells you cast cost {1} less to cast", plus Stormscape / Sunscape /
/// Thornscape / Thunderscape Familiar) is the exemplar class. Requires two or
/// more colors and full consumption, so a lone bare color ("Red spells …") and
/// a noun-bearing operand ("red creature spells and …") both decline here and
/// fall through to the single-subject path and `parse_type_phrase` respectively.
fn parse_cost_mod_compound_color_subject(base: &str) -> Option<TargetFilter> {
    // Operand: a bare color name, optionally followed by the spell noun. The
    // trailing " spell[s]" is present on every operand except the last (the
    // caller strips one trailing " spells" before this runs).
    fn color_operand(input: &str) -> OracleResult<'_, ManaColor> {
        let (input, color) = nom_primitives::parse_color(input)?;
        let (input, _) = opt(alt((tag(" spells"), tag(" spell")))).parse(input)?;
        Ok((input, color))
    }

    let (rest, colors) = separated_list1(tag::<_, _, OracleError<'_>>(" and "), color_operand)
        .parse(base.trim())
        .ok()?;
    if !rest.trim().is_empty() || colors.len() < 2 {
        return None;
    }

    let filters = colors
        .into_iter()
        .map(|color| {
            TargetFilter::Typed(
                TypedFilter::card().properties(vec![FilterProp::HasColor { color }]),
            )
        })
        .collect();
    Some(TargetFilter::Or { filters })
}

/// CR 607.2a + CR 607.3: Cost-mod lines such as Semblance Anvil reference
/// "the exiled card" as the imprinted card exiled by the source permanent. The
/// shared-quality nom parser emits `TrackedSet` for that phrase; remap to
/// `ExiledBySource` so live `exile_links` resolve the reference at cast time.
fn remap_cost_mod_imprint_exile_reference(filter: TargetFilter) -> TargetFilter {
    match filter {
        TargetFilter::Typed(mut tf) => {
            for prop in &mut tf.properties {
                remap_shares_quality_imprint_reference(prop);
            }
            TargetFilter::Typed(tf)
        }
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters
                .into_iter()
                .map(remap_cost_mod_imprint_exile_reference)
                .collect(),
        },
        other => other,
    }
}

fn remap_shares_quality_imprint_reference(prop: &mut FilterProp) {
    if let FilterProp::SharesQuality { reference, .. } = prop {
        if matches!(reference.as_deref(), Some(TargetFilter::TrackedSet { .. })) {
            *reference = Some(Box::new(TargetFilter::ExiledBySource));
        }
    }
}

fn strip_cost_mod_cast_scope_suffix(input: &str) -> &str {
    let (_, stripped) = all_consuming(alt((
        terminated(
            take_until(" your opponents cast"),
            (tag::<_, _, OracleError<'_>>(" your opponents cast"), eof),
        ),
        terminated(take_until(" opponents cast"), (tag(" opponents cast"), eof)),
        terminated(take_until(" you cast"), (tag(" you cast"), eof)),
        rest,
    )))
    .parse(input)
    .expect("rest fallback makes cast-scope suffix stripping infallible");
    stripped.trim()
}

/// CR 604.1: Parse a LEADING "during your turn, " / "during turns other than
/// yours, " turn-scope clause into its `StaticCondition`. Shares the negated-turn
/// vocabulary the CDA / dispatch / type-change static parsers already recognize
/// (`nom_tag_tp("during turns other than yours, ")`), and the affirmative form
/// mirrors the trailing suffix arm below. `peel_leading_cost_modifier_condition`
/// consumes this before self-spell and first-qualified dispatch, so every
/// cost-modifier branch retains the scope.
fn parse_leading_turn_scope(text: &str) -> OracleResult<'_, StaticCondition> {
    alt((
        value(
            StaticCondition::Not {
                condition: Box::new(StaticCondition::DuringYourTurn),
            },
            tag("during turns other than yours, "),
        ),
        value(StaticCondition::DuringYourTurn, tag("during your turn, ")),
    ))
    .parse(text)
}

/// CR 604.1 + CR 601.2f: Strip an inline "during your turn" timing clause from
/// a cost-modification subject before type parsing. Paladin Class: "Spells your
/// opponents cast during your turn cost {1} more to cast." The LEADING clause
/// (affirmative + negated) is handled once for every branch before dispatch by
/// `peel_leading_cost_modifier_condition`.
fn strip_cost_mod_during_your_turn_scope(text: &str) -> (&str, Option<StaticCondition>) {
    if let Ok((_, prefix)) = terminated(
        take_until(" during your turn"),
        (
            tag::<_, _, OracleError<'_>>(" during your turn"),
            nom::combinator::eof,
        ),
    )
    .parse(text)
    {
        return (prefix.trim(), Some(StaticCondition::DuringYourTurn));
    }
    if let Some((before, _, _)) = nom_primitives::scan_preceded(text, |i| {
        all_consuming(tag::<_, _, OracleError<'_>>(" during your turn")).parse(i)
    }) {
        return (before, Some(StaticCondition::DuringYourTurn));
    }
    (text, None)
}

pub(super) fn strip_cost_mod_spell_noun_suffix(input: &str) -> &str {
    let (_, stripped) = all_consuming(alt((
        value("", terminated(tag::<_, _, OracleError<'_>>("spells"), eof)),
        value("", terminated(tag("spell"), eof)),
        terminated(take_until(" spells"), (tag(" spells"), eof)),
        terminated(take_until(" spell"), (tag(" spell"), eof)),
        rest,
    )))
    .parse(input)
    .expect("rest fallback makes spell-noun suffix stripping infallible");
    stripped.trim()
}

/// CR 601.2f + CR 118.8: Parse static-imposed additional non-mana costs such as
/// Terror of the Peaks ("cost an additional 3 life to cast").
pub(crate) fn try_parse_impose_additional_cost(
    text: &str,
    lower: &str,
) -> Option<StaticDefinition> {
    type VE<'a> = OracleError<'a>;

    let (_prefix, (life_amount, action), _) = nom_primitives::scan_preceded(lower, |i| {
        let (i, _) = tag::<_, _, VE>("cost an additional ").parse(i)?;
        let (i, life_amount) = alt((
            map(nom_primitives::parse_number, |n| QuantityExpr::Fixed {
                value: n as i32,
            }),
            value(
                QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
                tag("x"),
            ),
            value(
                QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
                tag("{x}"),
            ),
        ))
        .parse(i)?;
        let (i, action) = value(AdditionalCostTaxAction::Cast, tag(" life to cast")).parse(i)?;
        Ok((i, (life_amount, action)))
    })?;

    let cost = AbilityCost::PayLife {
        amount: life_amount,
    };

    let controller = if nom_primitives::scan_contains(lower, "your opponents cast")
        || nom_primitives::scan_contains(lower, "opponents cast")
        || nom_primitives::scan_contains(lower, "each opponent casts")
    {
        Some(ControllerRef::Opponent)
    } else if nom_primitives::scan_contains(lower, "you cast")
        || nom_primitives::scan_contains(lower, " you activate")
        || nom_primitives::scan_contains(lower, " you may activate")
    {
        Some(ControllerRef::You)
    } else {
        None
    };

    let target_cost_filter = parse_cost_modifier_target_filter(lower)?;
    let spell_filter = Some(target_cost_filter);

    let is_self_scoped = nom_primitives::scan_contains(lower, "of this land")
        || nom_primitives::scan_contains(lower, "of this creature")
        || nom_primitives::scan_contains(lower, "of this permanent")
        || nom_primitives::scan_contains(lower, "of ~");

    let affected = if is_self_scoped {
        TargetFilter::SelfRef
    } else {
        match controller {
            Some(ControllerRef::You) => {
                TargetFilter::Typed(TypedFilter::card().controller(ControllerRef::You))
            }
            Some(ControllerRef::Opponent) => {
                TargetFilter::Typed(TypedFilter::card().controller(ControllerRef::Opponent))
            }
            Some(ControllerRef::ScopedPlayer) => TargetFilter::Typed(TypedFilter::card()),
            Some(ControllerRef::TargetPlayer) => TargetFilter::Typed(TypedFilter::card()),
            // CR 109.4: TargetOpponent, like TargetPlayer, has no cost-static
            // semantics — fall back to an untyped card filter.
            Some(ControllerRef::TargetOpponent) => TargetFilter::Typed(TypedFilter::card()),
            Some(ControllerRef::ParentTargetController) => TargetFilter::Typed(TypedFilter::card()),
            Some(ControllerRef::ParentTargetOwner) => TargetFilter::Typed(TypedFilter::card()),
            Some(ControllerRef::DefendingPlayer) => TargetFilter::Typed(TypedFilter::card()),
            Some(ControllerRef::SourceChosenPlayer) => TargetFilter::Typed(TypedFilter::card()),
            Some(ControllerRef::ChosenPlayer { .. }) => TargetFilter::Typed(TypedFilter::card()),
            Some(ControllerRef::TriggeringPlayer) => TargetFilter::Typed(TypedFilter::card()),
            // CR 303.4b: Enchanted-player scope is not supported for cost statics;
            // fall back to untyped filter (same as TriggeringPlayer).
            Some(ControllerRef::EnchantedPlayer) => TargetFilter::Typed(TypedFilter::card()),
            // CR 102.1: active-player scope is not emitted for cost statics;
            // fall back to an untyped card filter (same as TriggeringPlayer).
            Some(ControllerRef::ActivePlayer) => TargetFilter::Typed(TypedFilter::card()),
            Some(ControllerRef::SpecificPlayer { .. }) => TargetFilter::Typed(TypedFilter::card()),
            None => TargetFilter::Typed(TypedFilter::card()),
        }
    };

    Some(
        StaticDefinition::new(StaticMode::ImposeAdditionalCost {
            cost,
            spell_filter,
            action,
        })
        .affected(affected)
        .description(text.to_string()),
    )
}

/// Dynamic "for each" counts are extracted when present.
pub(crate) fn try_parse_cost_modification(
    text: &str,
    lower: &str,
    casting_as_variant: Option<crate::types::game_state::CastingVariant>,
) -> Option<StaticDefinition> {
    let original_text = text;
    let (cost_text, leading_condition) =
        peel_leading_cost_modifier_condition(TextPair::new(text, lower));
    let text = cost_text.original;
    let lower = cost_text.lower;

    let is_raise = nom_primitives::scan_contains(lower, "more to cast")
        || nom_primitives::scan_contains(lower, "more to activate");
    let is_reduce = nom_primitives::scan_contains(lower, "less to cast")
        || nom_primitives::scan_contains(lower, "less to activate");
    if !is_raise && !is_reduce {
        return None;
    }

    // CR 601.2f: Detect self-spell cost reduction ("this spell costs {N} less ...").
    // Distinct from battlefield cost modification (e.g., "creature spells you cast cost {1} less")
    // because the static must apply to the card while it is in hand (or on the stack during
    // casting), not once it has entered the battlefield. The caller wires this into
    // `active_zones = self_spell_cost_mod_active_zones()` with `affected =
    // SelfRef` so the casting-time scanner finds it on the spell being cast.
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
        || nom_primitives::scan_contains(lower, "each opponent casts")
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

    let nth_qualified_spell = match parse_nth_qualified_spell_filter(lower) {
        // CR 601.2f: A recognized "the first … spell <timing> costs …" subject
        // whose qualifier/timing can't be lowered to a filter + once-per-turn
        // gate (e.g. "the first kicked spell you cast each turn costs {1} less").
        // Declining here is mandatory — falling through to the generic
        // cost-modifier path would emit a filterless, conditionless reducer that
        // drops both the printed "first … each turn" restriction and the
        // qualifier, reducing every spell the controller casts.
        NthQualifiedSpell::UnsupportedQualifier => return None,
        NthQualifiedSpell::NotApplicable => None,
        NthQualifiedSpell::Supported {
            filter,
            timing,
            ordinal,
        } => Some((filter, timing, ordinal)),
    };
    let nth_qualified_spell_filter = nth_qualified_spell.as_ref().map(|(filter, _, _)| filter);
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
    let mut during_your_turn_scope = None;
    let spell_filter = if is_self_spell {
        parse_self_spell_target_cost_filter(lower)
    } else if let Some(filter) = nth_qualified_spell_filter.cloned() {
        Some(filter)
    // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
    } else if let Some(cost_idx) = lower.find(" cost") {
        // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        let prefix = &lower[..cost_idx];
        let (prefix, turn_scope) = strip_cost_mod_during_your_turn_scope(prefix);
        during_your_turn_scope = turn_scope;
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
        // CR 202.3 + CR 601.2f: Peel a trailing "with mana value N or greater/less"
        // gate BEFORE the "you cast"/"spells" end-trims. The qualifier sits after
        // the "spells you cast" infix, so without peeling it the trims never reach
        // the type words and the whole type+MV restriction is dropped (The Scarlet
        // Witch reduced EVERY spell, not just instants/sorceries — #5606).
        let (without_chosen, mana_value_prop) = strip_cost_mod_mana_value_qualifier(without_chosen);
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
            parse_cost_mod_spell_type_prefix(type_desc)
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
        let base_filter = match (typed_filter, has_chosen_name) {
            (Some(tf), true) => Some(TargetFilter::And {
                filters: vec![tf, TargetFilter::HasChosenName],
            }),
            (None, true) => Some(TargetFilter::HasChosenName),
            (tf, false) => tf,
        };
        // CR 202.3: fold the peeled mana-value gate back into the spell filter.
        compose_cost_mod_mana_value(base_filter, mana_value_prop)
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

    // CR 601.2f: {X}-flavored self-spell reductions must bind X to a quantity
    // (devotion, object counts, etc.). Emitting ModifyCost with multiplier 1
    // and no dynamic_count silently under-reduces by {1} (Drag to the Underworld
    // class when the where-X clause fails to lower).
    if amount_is_variable_x && dynamic_count.is_none() {
        return None;
    }

    let amount = if amount_is_variable_x {
        ManaCost::generic(1)
    } else {
        amount
    };

    let mode = StaticMode::ModifyCost {
        mode: if is_raise {
            CostModifyMode::Raise
        } else {
            CostModifyMode::Reduce
        },
        amount,
        spell_filter: spell_filter.clone(),
        dynamic_count: dynamic_count.clone(),
    };

    // Build the affected filter for the static definition.
    // This controls which objects are "affected" — for cost modification statics,
    // this is the source permanent's controller scope (used by the registry).
    // CR 601.2f: Self-spell cost reduction ("This spell costs {N} less ...") uses
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
            // CR 109.4: TargetOpponent, like TargetPlayer, has no cost-static
            // semantics — fall back to an untyped card filter.
            Some(ControllerRef::TargetOpponent) => TargetFilter::Typed(TypedFilter::card()),
            Some(ControllerRef::ParentTargetController) => TargetFilter::Typed(TypedFilter::card()),
            Some(ControllerRef::ParentTargetOwner) => TargetFilter::Typed(TypedFilter::card()),
            Some(ControllerRef::DefendingPlayer) => TargetFilter::Typed(TypedFilter::card()),
            // CR 613.1: chosen-player scope is not emitted for cost statics.
            Some(ControllerRef::SourceChosenPlayer) => TargetFilter::Typed(TypedFilter::card()),
            // CR 109.4: Chosen-player scope is not emitted for cost statics.
            Some(ControllerRef::ChosenPlayer { .. }) => TargetFilter::Typed(TypedFilter::card()),
            // CR 603.2 + CR 109.4: Triggering-player scope is not emitted for
            // cost statics. Fall back to an untyped filter.
            Some(ControllerRef::TriggeringPlayer) => TargetFilter::Typed(TypedFilter::card()),
            // CR 303.4b: Enchanted-player scope is not supported for cost statics;
            // fall back to untyped filter (same as TriggeringPlayer).
            Some(ControllerRef::EnchantedPlayer) => TargetFilter::Typed(TypedFilter::card()),
            // CR 102.1: active-player scope is not emitted for cost statics;
            // fall back to an untyped card filter (same as TriggeringPlayer).
            Some(ControllerRef::ActivePlayer) => TargetFilter::Typed(TypedFilter::card()),
            Some(ControllerRef::SpecificPlayer { .. }) => TargetFilter::Typed(TypedFilter::card()),
            None => TargetFilter::Typed(TypedFilter::card()),
        }
    };

    let mut definition = StaticDefinition::new(mode)
        .affected(affected)
        .description(original_text.to_string());

    // CR 601.2f: A self-spell cost reduction must apply while the
    // card is in hand (pre-cast affordability checks), in the command zone
    // (commander casting), in the graveyard or exile (alternative-zone casting),
    // and on the stack (final cost determination during casting). Without opting
    // in via `active_zones`, layer collection would ignore the static outside
    // the battlefield, and the card would never reduce its own cost.
    if is_self_spell {
        definition.active_zones = crate::types::zones::self_spell_cost_mod_active_zones();
    }
    let branch_condition = if let Some((filter, timing, ordinal)) = nth_qualified_spell.as_ref() {
        Some(nth_qualified_spell_condition(filter, timing, *ordinal))
    } else {
        during_your_turn_scope
    };
    definition.condition = match (leading_condition, branch_condition) {
        (Some(leading), Some(branch)) => Some(StaticCondition::And {
            conditions: vec![leading, branch],
        }),
        (Some(leading), None) => Some(leading),
        (None, Some(branch)) => Some(branch),
        (None, None) => None,
    };

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
            // CR 601.2f + CR 611.3a: try the cost-specific predicates first, then
            // fall back to the shared static-condition grammar so board-state
            // gates ("if there are ten or more nonland permanents on the
            // battlefield", Hour of Revelation) attach instead of being swallowed.
            if let Some(sc) = parse_cost_modifier_condition(cond_text)
                .or_else(|| parse_static_condition(cond_text))
            {
                definition.condition = Some(sc);
            } else if let Ok((rest, sc)) = nom_condition::parse_inner_condition(cond_text) {
                if rest.trim().is_empty() || rest.trim() == "." {
                    definition.condition = Some(sc);
                }
            } else if is_nested_stack_target_condition(cond_text) {
                let filter = parse_it_targets_that_targets_spell_filter(cond_text)?;
                // CR 115.9b: "if it targets a spell or ability that targets a [type]
                // you control [with power N or greater]" — wire the parsed nested
                // target filter into spell_filter so the reduction only applies when
                // NOOW's target is a qualifying stack object (e.g. "Not of This World").
                if let StaticMode::ModifyCost { spell_filter, .. } = &mut definition.mode {
                    *spell_filter = Some(filter);
                }
            }
        }
    }

    // CR 601.2f: Leading-condition form — "If [condition], this spell costs
    // {N} less to cast." The trailing scan above misses this because the "if"
    // is at the start of the line (no preceding space), so `rfind(" if ")`
    // never matches it. Consume the condition with the shared combinator and
    // accept it only when followed by the comma separating it from the
    // already-parsed cost clause. The Avatar cycle (Avatar of
    // Fury/Hope/Might/Will/Woe) and "If you weren't the starting player, this
    // spell costs {1} less" cards use this form.
    if definition.condition.is_none() {
        if let Ok((_rest, sc)) = preceded(
            tag("if "),
            terminated(
                nom_condition::parse_inner_condition,
                (multispace0, tag(",")),
            ),
        )
        .parse(lower)
        {
            definition.condition = Some(sc);
        }
    }

    // CR 601.2f + CR 702.34a: Caller-proven casting variant (e.g. Flashback from
    // the compound-line parser) gates self-spell cost modifiers — never inferred
    // from generic "cast this way" wording alone.
    if let Some(variant) = casting_as_variant {
        definition.condition = Some(match definition.condition.take() {
            Some(existing) => StaticCondition::And {
                conditions: vec![existing, StaticCondition::CastingAsVariant { variant }],
            },
            None => StaticCondition::CastingAsVariant { variant },
        });
    }

    Some(definition)
}

fn peel_leading_cost_modifier_condition<'a>(
    pair: TextPair<'a>,
) -> (TextPair<'a>, Option<StaticCondition>) {
    let trimmed = pair.trim_start();
    if let Ok((remainder, condition)) = parse_leading_turn_scope(trimmed.lower) {
        let consumed = trimmed.lower.len() - remainder.len();
        let cost_clause = trimmed.slice(consumed, trimmed.lower.len()).trim_start();
        if nom_primitives::scan_contains(cost_clause.lower, "less to cast")
            || nom_primitives::scan_contains(cost_clause.lower, "more to cast")
            || nom_primitives::scan_contains(cost_clause.lower, "less to activate")
            || nom_primitives::scan_contains(cost_clause.lower, "more to activate")
        {
            return (cost_clause, Some(condition));
        }
    }

    let Ok((after_if, _)) = tag::<_, _, OracleError<'_>>("if ").parse(trimmed.lower) else {
        return (pair, None);
    };
    let rest = trimmed.slice(trimmed.lower.len() - after_if.len(), trimmed.lower.len());
    let Some((condition, cost_clause)) = rest.split_around(", ") else {
        return (pair, None);
    };
    if !(nom_primitives::scan_contains(cost_clause.lower, "less to cast")
        || nom_primitives::scan_contains(cost_clause.lower, "more to cast")
        || nom_primitives::scan_contains(cost_clause.lower, "less to activate")
        || nom_primitives::scan_contains(cost_clause.lower, "more to activate"))
    {
        return (pair, None);
    }

    let cond_text = condition.lower.trim().trim_end_matches('.');
    let parsed = parse_cost_modifier_condition(cond_text).or_else(|| {
        let (rest, sc) = nom_condition::parse_inner_condition(cond_text).ok()?;
        (rest.trim().is_empty() || rest.trim() == ".").then_some(sc)
    });

    match parsed {
        Some(condition) => (cost_clause.trim_start(), Some(condition)),
        None => (pair, None),
    }
}

/// Structural pre-check for the "it targets [stack object] that targets […]"
/// cost-reduction condition (CR 601.2f + CR 115.9b).
///
/// This gate must never be NARROWER than the grammar it guards
/// (`parse_it_targets_that_targets_spell_filter`): a phrase the gate rejects
/// silently skips the whole nested-target branch, leaving `spell_filter` unset
/// and the cost reduction UNCONDITIONAL. So the ability alternative delegates
/// the kind spelling to `nom_target::parse_ability_kind` — the same single
/// authority the filter builder uses — rather than keeping yet another private
/// list. CR 113.3b / CR 113.3c: "a triggered ability that targets …" is an
/// ability phrase just as much as the bare noun is.
///
/// The gate stays a separate cheap pre-check rather than becoming
/// `parse_it_targets_that_targets_spell_filter(..).is_some()`, because the
/// branch it guards deliberately FAILS CLOSED (returns `None` for the whole
/// static line) when the shape matches but the details do not parse.
fn is_nested_stack_target_condition(cond_text: &str) -> bool {
    preceded(
        tag::<_, _, OracleError<'_>>("it targets "),
        preceded(
            opt(alt((tag("a "), tag("an "), tag("one or more ")))),
            alt((
                value((), tag("spell or ability that targets ")),
                value((), tag("spell that targets ")),
                // CR 113.3b / CR 113.3c: the ability noun may carry a kind
                // qualifier ("a triggered ability that targets …"). Shared axis
                // authority, tried ahead of the bare noun.
                value((), (nom_target::parse_ability_kind, tag(" that targets "))),
                value((), tag("ability that targets ")),
            )),
        ),
    )
    .parse(cond_text)
    .is_ok()
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

/// CR 601.2f + CR 115.9b: Parse "it targets [spell/ability] that targets [creature filter]"
/// as a `spell_filter` for `StaticMode::ModifyCost`.
///
/// Returns a `TargetFilter` expressing the two-level targeting constraint:
/// the self-spell must be targeting something (a spell or ability) that itself
/// targets a creature matching the parsed filter. Used by cards like Not of This
/// World whose cost reduction is conditioned on which stack entry they target.
///
/// `cond_text` is already lowercase.
fn parse_it_targets_that_targets_spell_filter(cond_text: &str) -> Option<TargetFilter> {
    // Consume "it targets "
    let (i, _) = tag::<_, _, OracleError<'_>>("it targets ")
        .parse(cond_text)
        .ok()?;

    // Parse what the self-spell targets — a spell and/or ability on the stack.
    let (i, intermediate_filter) = alt((
        value(
            TargetFilter::Or {
                filters: vec![
                    TargetFilter::StackSpell,
                    TargetFilter::StackAbility {
                        controller: None,
                        tag: None,
                        kind: None,
                    },
                ],
            },
            tag::<_, _, OracleError<'_>>("a spell or ability"),
        ),
        value(
            TargetFilter::StackSpell,
            tag::<_, _, OracleError<'_>>("a spell"),
        ),
        // CR 113.3b / CR 113.3c + CR 601.2f: the ability-kind spelling narrows
        // which stack abilities satisfy the cost-reduction condition. Article +
        // the shared axis authority — never a private spelling list. (No printed
        // card reaches the narrowing arms today; Not of This World uses
        // "a spell or ability", handled above. Delegating closes the axis
        // prospectively at zero corpus risk.)
        map(
            preceded(
                alt((
                    tag::<_, _, OracleError<'_>>("an "),
                    tag::<_, _, OracleError<'_>>("a "),
                )),
                nom_target::parse_ability_kind,
            ),
            |kind| TargetFilter::StackAbility {
                controller: None,
                tag: None,
                kind,
            },
        ),
        // CR 113.3b / CR 113.3c: bare "an ability" names NO kind, so both kinds
        // are legal and `kind: None` is the correct reading — this is the same
        // category as the "a spell or ability" arm above, not a dropped axis.
        // It is a LOCAL literal on purpose: `parse_ability_kind` must not grow a
        // bare-"ability" arm, because it also feeds the disjunction driver where
        // a kindless leg would over-match (see its doc comment).
        // Ordered AFTER the delegation. The two are prefix-disjoint
        // ("an ability" cannot be confused with "an activated ability"), so this
        // is for determinism and for keeping the alt readable longest-first.
        value(
            TargetFilter::StackAbility {
                controller: None,
                tag: None,
                kind: None,
            },
            tag::<_, _, OracleError<'_>>("an ability"),
        ),
    ))
    .parse(i)
    .ok()?;

    // "that targets "
    let (i, _) = tag::<_, _, OracleError<'_>>(" that targets ")
        .parse(i)
        .ok()?;

    // Article
    let (i, _) = alt((tag::<_, _, OracleError<'_>>("a "), tag("an ")))
        .parse(i)
        .ok()?;

    // Type of the final target (creature is the canonical case for this pattern)
    let (i, type_filter) = alt((
        value(
            TypeFilter::Creature,
            tag::<_, _, OracleError<'_>>("creature"),
        ),
        value(TypeFilter::Permanent, tag("permanent")),
    ))
    .parse(i)
    .ok()?;

    // Optional controller suffix
    let (i, controller) = opt(alt((
        value(
            ControllerRef::You,
            tag::<_, _, OracleError<'_>>(" you control"),
        ),
        value(ControllerRef::Opponent, tag(" an opponent controls")),
    )))
    .parse(i)
    .ok()?;

    // Optional P/T comparison ("with power 7 or greater" etc.).
    // parse_pt_comparison handles the "with " prefix itself; trim leading whitespace first.
    let trimmed = i.trim_start();
    let power_prop = if trimmed.is_empty() {
        None
    } else {
        let (rest, prop) = nom_filter::parse_pt_comparison(trimmed).ok()?;
        if !rest.trim().is_empty() {
            return None;
        }
        Some(prop)
    };

    // Build the innermost creature/permanent filter
    let mut creature_typed = TypedFilter::new(type_filter);
    if let Some(ctrl) = controller {
        creature_typed = creature_typed.controller(ctrl);
    }
    if let Some(prop) = power_prop {
        creature_typed = creature_typed.properties(vec![prop]);
    }
    let creature_filter = TargetFilter::Typed(creature_typed);

    // The intermediate (spell/ability on the stack) must itself target the creature.
    // CR 115.9b: "targets" is satisfied when ANY of its targets match.
    let inner_filter = TargetFilter::And {
        filters: vec![
            intermediate_filter,
            TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Targets {
                filter: Box::new(creature_filter),
            }])),
        ],
    };

    // The self-spell must target something matching inner_filter.
    Some(TargetFilter::Typed(TypedFilter::default().properties(
        vec![FilterProp::Targets {
            filter: Box::new(inner_filter),
        }],
    )))
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

/// CR 611.3a: Classification of a trailing parenthetical on a static line.
/// Must be evaluated on the **raw** Oracle line before `strip_reminder_text`
/// removes parenthetical spans — rules-bearing gates like Alhammarret's
/// `(as long as this creature is on the battlefield)` share the same surface
/// syntax as reminder prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParentheticalGateExtract<'a> {
    /// No trailing parenthetical on the line.
    Absent,
    /// Trailing parenthetical is reminder prose, not a rules-bearing gate.
    Benign,
    /// `(as long as/if <condition>)` with a parseable `StaticCondition`.
    Recognized(&'a str),
    /// Gate-shaped parenthetical whose condition is not recognized — caller must decline.
    Unrecognized,
}

fn parse_parenthetical_gate_condition_body(i: &str) -> OracleResult<'_, &str> {
    preceded(
        alt((tag::<_, _, OracleError<'_>>("as long as "), tag("if "))),
        rest,
    )
    .parse(i)
}

fn parse_trailing_parenthetical_pieces(i: &str) -> OracleResult<'_, (&str, &str)> {
    let (i, body) = take_until::<_, _, OracleError<'_>>(" (").parse(i)?;
    let (i, inner) = preceded(tag(" ("), terminated(take_until(")"), tag(")"))).parse(i)?;
    Ok((i, (body.trim(), inner.trim())))
}

/// CR 611.3a: Peel a trailing parenthetical gate from the raw (pre-reminder-strip)
/// lowercase line. `as long as` is tried before `if` inside the parenthetical.
/// Unrecognized gate conditions return `Unrecognized` so callers decline rather
/// than enforce the restriction unconditionally.
pub(crate) fn extract_trailing_parenthetical_gate_condition(
    lower: &str,
) -> ParentheticalGateExtract<'_> {
    let input = lower.trim().trim_end_matches('.');
    let Ok((rest, (body, inner))) = parse_trailing_parenthetical_pieces(input) else {
        return ParentheticalGateExtract::Absent;
    };
    if !rest.is_empty() || body.is_empty() {
        return ParentheticalGateExtract::Absent;
    }
    if let Ok(("", condition_text)) =
        all_consuming(parse_parenthetical_gate_condition_body).parse(inner)
    {
        return if parse_static_condition(condition_text).is_some() {
            ParentheticalGateExtract::Recognized(condition_text)
        } else {
            ParentheticalGateExtract::Unrecognized
        };
    }
    ParentheticalGateExtract::Benign
}

/// CR 611.3a: Oracle dispatch strips reminder parentheticals before the general
/// static parser runs. Re-attach cant-cast gate conditions from the raw line
/// without feeding benign parentheticals through unrelated static parsers
/// (Varolz / Underworld Breach graveyard-keyword grants, etc.).
pub(crate) fn apply_raw_parenthetical_cant_cast_gate(
    defs: Vec<StaticDefinition>,
    raw_line: &str,
    card_name: &str,
) -> Vec<StaticDefinition> {
    use crate::parser::oracle_special::normalize_self_refs_for_static;
    use crate::types::statics::StaticMode;

    let normalized_raw = normalize_self_refs_for_static(raw_line, card_name);
    match extract_trailing_parenthetical_gate_condition(&normalized_raw.to_lowercase()) {
        ParentheticalGateExtract::Unrecognized => defs
            .into_iter()
            .filter(|def| !matches!(def.mode, StaticMode::CantBeCast { .. }))
            .collect(),
        ParentheticalGateExtract::Recognized(condition_text) => {
            let Some(condition) = parse_static_condition(condition_text) else {
                return defs
                    .into_iter()
                    .filter(|def| !matches!(def.mode, StaticMode::CantBeCast { .. }))
                    .collect();
            };
            defs.into_iter()
                .map(|mut def| {
                    if matches!(def.mode, StaticMode::CantBeCast { .. }) && def.condition.is_none()
                    {
                        def.condition = Some(condition.clone());
                    }
                    def
                })
                .collect()
        }
        ParentheticalGateExtract::Absent | ParentheticalGateExtract::Benign => defs,
    }
}

/// CR 611.3a: Attach an optional parsed static gate to a prohibition static.
/// When `gate_condition_text` is present but `parse_static_condition` declines,
/// return `None` so the caller does not enforce the restriction unconditionally.
///
/// CR 118.12a: a condition this static's ENFORCEMENT POINT can never satisfy
/// declines the same way — `parse_static_condition` accepts a bare
/// `"you pay {N}"` as `UnlessPay`, and the prohibition modes this helper serves
/// have no payment prompt (see [`accept_enforceable_condition`]).
pub(crate) fn attach_parsed_static_gate(
    def: StaticDefinition,
    gate_condition_text: Option<&str>,
) -> Option<StaticDefinition> {
    match gate_condition_text {
        None => Some(def),
        Some(text) => {
            let condition = parse_static_condition(text)?;
            let condition = accept_enforceable_condition(&def.mode, condition)?;
            Some(def.condition(condition))
        }
    }
}

/// CR 502.3: Extract a trailing condition from a "doesn't untap during [untap step]" clause.
/// Handles patterns like:
/// - "doesn't untap during your untap step as long as [condition]"
/// - "doesn't untap during your untap step if [condition]"
/// - "doesn't untap during your untap step unless [condition]" ("unless" is a
///   negative-polarity conditional: the restriction applies precisely when the
///   trailing condition is false, so the "unless" body is negated — Bombur,
///   Gentle Dreamer: "doesn't untap ... unless you have an enduring story" only
///   withholds the untap while the story is absent. CR 611.3a: because this is
///   a continuous effect generated by a static ability, it isn't "locked in" —
///   the condition is re-evaluated dynamically at every untap step rather than
///   fixed once at parse time).
pub(crate) fn extract_cant_untap_condition(lower: &str) -> Option<StaticCondition> {
    // Find the end of the "untap step" phrase
    let untap_phrases = [
        "its controller's untap step",
        "its controller\u{2019}s untap step",
        "their controllers' untap steps",
        "their controllers\u{2019} untap steps",
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
    // "unless" is negative polarity: delegate to the shared unless-condition
    // combinator so the UnlessPay raw-passthrough rule (avoid double-negating an
    // already-negative "unless you pay [cost]" cost condition) is applied
    // identically to every other "unless" gate in the parser. Whatever comes
    // back is then run through the one untap-step enforceability gate below,
    // exactly like the positive "as long as …"/"if …" tail.
    if let Some(unless_text) = nom_tag_lower(remaining, remaining, "unless ") {
        return Some(match nom_condition::parse_unless_condition(unless_text) {
            Ok((rest, condition)) if rest.trim().is_empty() => {
                gate_cant_untap_condition(condition, unless_text)
            }
            _ => unparsed_gate_condition(unless_text, ConditionGatePolarity::Negative),
        });
    }
    // Strip "as long as" or "if" prefix
    let condition_text = nom_tag_lower(remaining, remaining, "as long as ")
        .or_else(|| nom_tag_lower(remaining, remaining, "if "))?;
    Some(match parse_static_condition(condition_text) {
        Some(condition) => gate_cant_untap_condition(condition, condition_text),
        None => unparsed_gate_condition(condition_text, ConditionGatePolarity::Positive),
    })
}

/// CR 604.1 + CR 611.3a: the polarity a static's gate condition is STORED with,
/// and therefore the shape its UNPARSED-clause fallback must take.
///
/// `active_static_definitions` reads a static's `condition` as "static ACTIVE
/// when TRUE", so a positive `"as long as X"` / `"if X"` tail stores `X` while a
/// negative `"unless X"` tail stores `Not(X)`. When the clause parser produced
/// NOTHING at all, the fallback has to match that storage sense or the gap
/// marker would flip the static's sense relative to what the same route already
/// did before any gate existed. A typed axis rather than a raw bool so the two
/// readings stay self-documenting at every call site.
///
/// This axis governs [`unparsed_gate_condition`] ONLY. It deliberately does NOT
/// reach [`unenforceable_gate_marker`], the remedy for a clause that DID parse
/// but cannot be enforced at the static's enforcement point: that marker is
/// unconditionally inert, because a gate the engine cannot evaluate must never
/// switch a static ON. See [`unenforceable_gate_marker`] for the full argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConditionGatePolarity {
    /// `"… as long as X"` / `"… if X"`.
    Positive,
    /// `"… unless X"`.
    Negative,
}

/// CR 611.3a: the honest, ALWAYS-INERT marker for a gate clause the engine
/// parsed but whose enforcement point can never evaluate
/// ([`StaticCondition::is_unenforceable_on`]).
///
/// `Not(Unrecognized { text })` is the single shape, for both grammatical
/// polarities, and it is chosen for two properties that must hold together:
///
/// 1. **Inert at runtime.** `game::layers::evaluate_condition` reads
///    `Unrecognized` as `true`, so the wrapping `Not` pins the gate to `false`
///    forever and `active_static_definitions` never switches the static on.
/// 2. **Visible to coverage.** `StaticCondition::contains_unrecognized` walks
///    through `Not` (see its doc), so every coverage-honesty gate still reports
///    the clause as an unsupported gap. Declining the static outright would
///    satisfy (1) and silently lose (2).
///
/// WHY POLARITY IS NOT AN INPUT HERE. It used to be, and that was a defect:
/// the positive branch returned a BARE `Unrecognized`, so a `"… as long as
/// <unenforceable clause>"` static became permanently ON. That is fail-OPEN in
/// the one direction the gate exists to prevent. Two independent arguments
/// close it:
///
/// - **Don't invent behavior.** The gate fires precisely because the engine
///   cannot decide the clause. Applying the static anyway asserts a P/T bonus,
///   keyword grant, evasion, or untap lock that the printed card confers only
///   conditionally. `"Enchanted creature gets +2/+2 as long as you pay {1}"`
///   became an unconditional +2/+2 — a rules-wrong buff, not a conservative
///   approximation. Leaving it off is the only outcome that adds nothing the
///   card does not unconditionally grant.
/// - **Truth conservation.** Every leaf `is_unenforceable_on` rejects already
///   evaluates `false` at this enforcement point: `game::layers` hard-codes
///   `UnlessPay` (CR 118.12a) and `CastingAsVariant` (CR 702.34a) to `false`,
///   `AdditionalCostPaid` (CR 601.2f) is `false` with no in-flight cast, and
///   `evaluate_condition` rejects a scoped-player designation anchor (CR 725.1)
///   at its entry boundary. A marker that reads `true` therefore INVERTS the
///   very truth value that justified rejecting the leaf, turning a
///   coverage-honesty deferral into a rules-behavior change. The inert marker
///   preserves it.
///
/// The failure mode this closes is worst on grant-shaped modes
/// (`StaticMode::Continuous`, and the keyword companions it feeds), where a
/// permanently-true gate manufactures a buff out of nothing. It is real but
/// less alarming on restriction-shaped modes (`CantUntap`, `CantBeBlocked`,
/// `CantAttack`, `BlockRestriction`), where it manufactures an unconditional
/// restriction instead. Both are wrong; the marker is uniform so no future
/// route has to re-litigate which shape it is.
///
/// This reverses the `Positive` half of the fallback that PR #8012 shipped —
/// deliberately, and with its pinned test updated rather than deleted. #8012's
/// load-bearing property was that EVERY unenforceable leaf, at any nesting
/// depth, is replaced by a coverage-visible marker; that property is unchanged
/// and still asserted. Only the marker's runtime truth value moved, and it moved
/// toward the value the rejected leaf already had.
///
/// NOTE for the layer cache: `Unrecognized` is opaque to
/// `static_condition_uses_object_population`,
/// `static_condition_characteristic_reads`, and
/// `entered_object_perturbs_static_condition`, so a gated static is
/// re-evaluated more eagerly than a fully-typed one. That was already true of
/// the negative branch; over-invalidation cannot produce a wrong board state,
/// only redundant work.
pub(crate) fn unenforceable_gate_marker(gap_text: &str) -> StaticCondition {
    StaticCondition::Not {
        condition: Box::new(StaticCondition::Unrecognized {
            text: gap_text.to_string(),
        }),
    }
}

/// Engine limitation, not CR-mandated: the honest "we could not parse this gate
/// clause at all" condition, in the polarity the branch STORES its condition in.
///
/// Distinct from [`unenforceable_gate_marker`], and the distinction is the whole
/// point: this fallback is for a clause the parser could not decompose, where
/// the engine has learned NOTHING about the gate and the only defensible
/// behavior is the one the route already had before any gate existed. That
/// behavior is polarity-shaped, so this function is too:
///
/// - `Negative` yields `Not(Unrecognized)` → `false` forever → the static is
///   permanently INERT. The `"unless X"` escape clause we could not model might
///   always hold, so leaving the restriction off never invents a restriction the
///   card doesn't have.
/// - `Positive` yields a bare `Unrecognized` → `true` forever → the static is
///   permanently ON, matching what the `"as long as"` / `"if"` branches have
///   always done for an unparsed tail.
///
/// The asymmetry is inherited, not designed, and it is preserved ONLY here.
/// Each branch reproduces exactly what its call site did before, so adding the
/// enforceability gate to a route did not move any card's runtime behavior on
/// the unparsed path. Narrowing the positive branch to inert would silently
/// disable statics that apply today across the whole unparsed-tail corpus —
/// a far broader change than the enforcement-point defect, and one that needs
/// its own parse-diff evidence.
///
/// It is NOT a no-op for the layer CACHE. Three classifiers in `game::layers` —
/// `static_condition_uses_object_population`,
/// `static_condition_characteristic_reads`, and
/// `entered_object_perturbs_static_condition` — group the typed leaves with the
/// cheap, inert conditions (`false` / `EMPTY` / `false`) but must treat
/// `Unrecognized` as opaque (`true` / `ALL` / `true`). A gated card therefore
/// stops being cached as population-independent and gets re-evaluated more
/// eagerly. That direction is the safe one (over-invalidation cannot produce a
/// wrong board state, only redundant work).
///
/// The load-bearing property either way is that
/// `StaticCondition::contains_unrecognized` — which every coverage-honesty gate
/// consults — sees the gap instead of a fully-typed condition.
pub(crate) fn unparsed_gate_condition(
    gap_text: &str,
    polarity: ConditionGatePolarity,
) -> StaticCondition {
    match polarity {
        ConditionGatePolarity::Positive => StaticCondition::Unrecognized {
            text: gap_text.to_string(),
        },
        // Same SHAPE as the enforcement-point remedy, reached for a different
        // reason. Built from that one constructor so the two can never drift
        // into differently-nested markers that coverage tooling has to special-
        // case; `contains_unrecognized` and `evaluate_condition` see one shape.
        ConditionGatePolarity::Negative => unenforceable_gate_marker(gap_text),
    }
}

/// Engine limitation, not CR-mandated (these are runtime binding gaps, not rules
/// restrictions): the SINGLE authority for whether a parsed condition may gate a
/// static of the given `mode`.
///
/// The acceptance boundary is a property of the mode's ENFORCEMENT POINT, not of
/// the condition alone — the same CR 118.12a `UnlessPay` leaf is exactly right
/// on `CantAttack` (Ghostly Prison: `WaitingFor::CombatTaxPayment` prompts for
/// it at declaration) and unsatisfiable on `CantBeBlocked` (Awesome Presence) or
/// `BlockRestriction` (Hipparion), where no prompt exists. Two classes of leaf
/// are therefore rejected, each against the mode that would carry it:
///
/// 1. [`StaticCondition::requires_unavailable_continuation`] — a leaf whose
///    truth is established by a payment round-trip (CR 118.12a `UnlessPay`) or
///    by an in-flight cast (CR 601.2f `AdditionalCostPaid` / `CastingAsVariant`)
///    that THIS mode's enforcement point never runs
///    ([`StaticMode::provides_continuation`]). The layer pipeline just returns
///    the hard-coded `false`, so no player can ever satisfy the gate.
/// 2. [`StaticCondition::has_unbindable_designation_anchor`] — a scoped-player
///    designation ("that player is the monarch") on a mode whose enforcement
///    point cannot bind the scope ([`StaticMode::binds_scoped_player_anchor`]).
///    CR 502.3's untap step is the audited such point: a turn-based action with
///    no triggering event or combat context, so
///    `game::layers::evaluate_condition` rejects the whole condition outright.
///
/// Everything else — including the combat-scoped leaves, which are computed
/// correctly and are legitimately `false` outside combat — is enforceable and
/// passes through unchanged. Bombur, Gentle Dreamer's controller-scoped
/// `Not(HasEnduringStory)` is in that supported set.
///
/// Call sites are listed on [`attach_gated_condition`], which is how nearly all
/// of them reach this.
///
/// The remedy is [`unenforceable_gate_marker`] — inert at runtime, visible to
/// coverage — and it takes NO polarity. A clause that reached this function
/// parsed successfully; the engine knows exactly which enforcement point cannot
/// run it and that the leaf itself already evaluates `false` there, so the
/// marker must not read `true` in either grammatical direction. That is a
/// deliberate departure from [`unparsed_gate_condition`], whose input carries no
/// such information; see [`unenforceable_gate_marker`] for the argument.
pub(crate) fn gate_static_condition(
    mode: &StaticMode,
    condition: StaticCondition,
    gap_text: &str,
) -> StaticCondition {
    if condition.is_unenforceable_on(mode) {
        return unenforceable_gate_marker(gap_text);
    }
    condition
}

/// CR 611.3a: the fail-CLOSED sibling of [`gate_static_condition`], for sites
/// whose honesty policy is to DECLINE the whole static rather than keep it
/// behind a gap marker.
///
/// Same acceptance predicate ([`StaticCondition::is_unenforceable_on`]),
/// different remedy. These sites already return `None` when
/// `parse_static_condition` declines, and they keep that shape for the
/// unenforceable case too, so the two failure modes stay indistinguishable at
/// the call site. `evasion::parse_forced_attack_defender_static` is the existing
/// precedent: gate first, decline if a gap resulted.
///
/// [`gate_static_condition`]'s marker is now inert in both grammatical
/// directions ([`unenforceable_gate_marker`]), so substituting it here would no
/// longer enforce the restriction unconditionally — the hazard that originally
/// forced these sites apart is closed. The split survives because the two
/// remedies still differ in what they REPORT: this one drops the static (and
/// with it the clause), the marker keeps it visible to
/// `contains_unrecognized`. Prefer the marker for any new site; use this one
/// only where the surrounding parser genuinely has no definition to hang a
/// condition on.
///
/// Call sites (all fail-closed prohibition/permission gates):
///
/// - [`attach_parsed_static_gate`] (this module) — the shared trailing-gate
///   attach for prohibition statics.
/// - `oracle_static::dispatch`'s `"can't play lands … as long as X"` arm.
/// - `oracle_static::dispatch`'s `"can block an additional creature … as long
///   as X"` arm.
pub(crate) fn accept_enforceable_condition(
    mode: &StaticMode,
    condition: StaticCondition,
) -> Option<StaticCondition> {
    (!condition.is_unenforceable_on(mode)).then_some(condition)
}

/// Attach `condition` to `def`, deferring it to the honest gap marker when
/// `def.mode`'s enforcement point cannot satisfy it
/// ([`gate_static_condition`]).
///
/// Reading the mode off the definition is what makes this the practical single
/// entry point: a call site cannot pass a mode that disagrees with the
/// definition it is building. Every site that attaches a parsed `"unless"` /
/// `"as long as"` / `"if"` gate to a `StaticDefinition` routes through here or
/// through [`gate_static_condition`] directly:
///
/// Sites that call [`gate_static_condition`] DIRECTLY, because they hold a bare
/// condition rather than a definition:
///
/// - `extract_cant_untap_condition` (this module) — the `"doesn't untap …"`
///   tail, both polarities, via the [`gate_cant_untap_condition`] specialization.
/// - `oracle_static::grammar::parse_enchanted_equipped_predicate`'s
///   `" as long as "` split for `CantUntap`.
/// - `oracle_static::evasion::try_split_and_doesnt_untap`'s inherited `CantUntap`
///   companion gate (the companion is assembled before its condition is known).
/// - `oracle_static::evasion::parse_forced_attack_defender_static`, which gates
///   first and then DECLINES the whole static if a gap resulted.
///
/// Sites that go through this function:
///
/// - `oracle_static::evasion::parse_subject_rule_static` — both its
///   `cant_be_blocked_mode` tail conditions and its `" unless you control … "`
///   split (which reaches `RuleStaticPredicate::CantUntap` BEFORE the dedicated
///   untap arm, and every other rule-static predicate besides).
/// - `oracle_static::evasion::parse_subject_combat_rule_static` — the trailing
///   `"unless"` gate on `CantBlock` / `BlockRestriction` / `CantBeBlockedBy`
///   (Hipparion).
/// - `oracle_static::evasion::parse_combat_tax_static` — the dedicated
///   combat-tax form (Archangel of Tithes, War Cadence). Always ACCEPTED today,
///   because that parser only yields the three taxed modes; routed through the
///   gate anyway so this list is exhaustive by construction rather than by
///   assertion.
/// - `oracle_static::dispatch`'s `"~ can't block"`, `"~ can't attack"` (both via
///   [`parse_trailing_gate_condition`]) and `"activated abilities can't be
///   activated"` arms.
/// - `oracle_static::dispatch`'s `"<subject> can't be blocked …"` FALLBACK arm —
///   the second authority that catches lines `parse_subject_rule_static`
///   declined (flavor-word subject prefixes, untyped gates). It builds the same
///   `CantBeBlocked` mode as the evasion route, so it clears the same bar.
/// - `oracle_static::grammar::parse_enchanted_equipped_predicate`'s three
///   `"can't be blocked …"` branches, whose gated condition is also what the
///   granted-keyword companion inherits (Awesome Presence).
/// - `oracle_static::grammar::parse_enchanted_equipped_predicate`'s conditional
///   `"gets +N/+M as long as X"` / `"has <keyword> as long as X"` grant branch,
///   and the whole-predicate `Continuous` default beneath it that carries the
///   trailing `"unless"` rider (Heroic Defiance).
pub(crate) fn attach_gated_condition(
    def: &mut StaticDefinition,
    condition: StaticCondition,
    gap_text: &str,
) {
    // No `def.mode.clone()`: `StaticMode` carries `TargetFilter`/`PlayerFilter`
    // trees, and the assignment's RHS is evaluated (ending the immutable borrow
    // of `def.mode`) before the `def.condition` place is written.
    def.condition = Some(gate_static_condition(&def.mode, condition, gap_text));
}

/// CR 604.1 + CR 611.3a: parse a static line's trailing gate clause — `"unless
/// X"`, `"as long as X"`, or `"if X"` — in the one precedence order every
/// trailing-gate site must share.
///
/// The three shared clause parsers are otherwise indistinguishable at the call
/// site, and `"as long as"` must be tried before `"if"` to match
/// `split_trailing_gate_condition`'s precedence; ordering them once here means
/// no call site has to re-derive it.
///
/// Callers hand the result straight to [`attach_gated_condition`], which no
/// longer needs to know WHICH branch fired: the enforcement-point remedy
/// ([`unenforceable_gate_marker`]) is inert in both directions. Only the
/// unparsed-clause fallback still cares, and its call sites know their own
/// grammar locally — see [`ConditionGatePolarity`].
///
/// `affected` is the host static's affected set, threaded unchanged to all three
/// clause parsers so the gate's anaphoric "it" binds to the source only for a
/// SelfRef static (CR 611.3a). Centralizing the precedence must not centralize
/// away that binding: a gate parsed without the host's affected set would resolve
/// "it" against the wrong object, so the parameter travels with the text.
/// Call sites pass `def.affected.as_ref()` — the same definition the resulting
/// condition is attached to by [`attach_gated_condition`].
pub(crate) fn parse_trailing_gate_condition(
    tp: &TextPair<'_>,
    affected: Option<&TargetFilter>,
) -> Option<StaticCondition> {
    super::shared::parse_unless_static_condition(tp, affected).or_else(|| {
        super::shared::parse_as_long_as_static_condition(tp, affected)
            .or_else(|| super::shared::parse_if_static_condition(tp, affected))
    })
}

/// [`gate_static_condition`] specialized to `StaticMode::CantUntap` (CR 502.3).
///
/// CR 502.3 makes untapping a TURN-BASED ACTION: the active player determines
/// which permanents untap, then untaps them. That gives the untap loop
/// (`game::turns`) a game state, the source, and the affected permanent as
/// recipient — and nothing else, so it runs NEITHER continuation and binds no
/// scoped-player anchor. It is the strictest enforcement point in the engine and
/// the one the untap-tail parsers hand bare conditions to.
pub(crate) fn gate_cant_untap_condition(
    condition: StaticCondition,
    gap_text: &str,
) -> StaticCondition {
    gate_static_condition(&StaticMode::CantUntap, condition, gap_text)
}

/// CR 611.3: Peel a `"all <X> … and all <Y> …"` phrase into per-conjunct strings
/// on the `" and all "` seam. Each conjunct after the first is re-prefixed with
/// `"all "` because the seam consumes the quantifier. Returns `None` when fewer
/// than two conjuncts are found — single-subject lines fall through to dedicated
/// handlers.
///
/// Shared by compound-subject static parsers (`parse_compound_all_subjects_filter`
/// in `type_change.rs`, sibling land/animation handlers) and the effect-layer
/// compound-quantified become handler (`try_parse_compound_all_subjects_become_clause`
/// in `oracle_effect/subject.rs`). The mandatory second `all` quantifier is what
/// distinguishes this compound form from an incidental `" and "` inside a lone
/// subject phrase.
pub(crate) fn peel_compound_all_quantified_conjuncts(text: &str) -> Option<Vec<String>> {
    let trimmed = text.trim().trim_end_matches('.').to_string();
    if trimmed.is_empty() {
        return None;
    }
    let mut conjuncts = Vec::new();
    let mut current_original = trimmed;
    loop {
        let current_lower = current_original.to_lowercase();
        let tp = TextPair::new(&current_original, &current_lower);
        match tp.split_around(" and all ") {
            Some((before, after)) => {
                conjuncts.push(before.original.trim().to_string());
                current_original = format!("all {}", after.original.trim());
            }
            None => {
                conjuncts.push(current_original.trim().to_string());
                break;
            }
        }
    }
    if conjuncts.len() < 2 {
        return None;
    }
    Some(conjuncts)
}
