// CR 613.1f (Layer 6) — keyword-grant static abilities (ability-adding effects).

#[allow(unused_imports)]
use super::prelude::*;
#[allow(unused_imports)]
use super::support::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuleStaticPredicate {
    CantUntap,
    CantAttack,
    CantBlock,
    CantAttackOrBlock,
    CantCrew,
    CantBeActivated,
    CantBeSacrificed,
    MustAttack,
    MustBlock,
    MustBeBlocked,
    Goaded,
    BlockOnlyCreaturesWithFlying,
    Shroud,
    Hexproof,
    MayLookAtTopOfLibrary,
    LoseAllAbilities,
    NoMaximumHandSize,
    MayPlayAdditionalLand,
}

/// CR 702.34a / CR 702.138a / CR 702.187b / CR 702.97 / CR 702.141: maps the
/// leading keyword token of a graveyard-cast-keyword grant ("flashback",
/// "escape", "mayhem", "scavenge", "encore") to its `GraveyardGrantedKeywordKind`.
/// Single authority for the keyword-word → kind dispatch, shared by the static
/// "each ... has <kw>" clause below and the targeted/imperative grant front door
/// in `oracle_effect` so both forms recognize the same keyword set.
pub(crate) fn parse_graveyard_granted_keyword_kind(
    input: &str,
) -> OracleResult<'_, GraveyardGrantedKeywordKind> {
    alt((
        value(GraveyardGrantedKeywordKind::Flashback, tag("flashback")),
        value(GraveyardGrantedKeywordKind::Escape, tag("escape")),
        value(GraveyardGrantedKeywordKind::Mayhem, tag("mayhem")),
        // CR 702.97 / CR 702.141: Varolz, Young Deathclaws (scavenge);
        // Wire Surgeons (encore) grant activated graveyard keywords.
        value(GraveyardGrantedKeywordKind::Scavenge, tag("scavenge")),
        value(GraveyardGrantedKeywordKind::Encore, tag("encore")),
    ))
    .parse(input)
}

pub(crate) fn try_parse_graveyard_keyword_grant_clause(
    text: &str,
) -> Option<(TargetFilter, GraveyardGrantedKeywordKind, String)> {
    let stripped = strip_reminder_text(text);
    let lower = stripped.to_lowercase();
    let rest = nom_tag_lower(&stripped, &lower, "each ")?;
    let rest_lower = rest.to_lowercase();
    let (subject, keyword_text) =
        super::oracle_nom::bridge::split_once_on_lower(rest, &rest_lower, " has ").or_else(
            || super::oracle_nom::bridge::split_once_on_lower(rest, &rest_lower, " have "),
        )?;
    let subject = subject.trim();
    let keyword_text = keyword_text.trim().trim_end_matches('.').to_string();

    let kind = nom_on_lower(
        &keyword_text,
        &keyword_text.to_lowercase(),
        parse_graveyard_granted_keyword_kind,
    )?
    .0;

    let (filter, remainder) = parse_type_phrase(subject);
    if !remainder.trim().is_empty() || !target_filter_is_your_graveyard(&filter) {
        return None;
    }

    Some((filter, kind, keyword_text))
}

/// CR 702.97a / CR 702.141a: Resolve the keyword phrase on a graveyard grant
/// line — fixed costs ("encore {5}"), inline variable costs ("encore {X}, where
/// X is its mana value" → `SelfManaValue`), or bare keyword tokens when the cost
/// arrives in a separate continuation sentence (handled upstream).
fn parse_graveyard_granted_keyword_phrase(
    keyword_text: &str,
    kind: GraveyardGrantedKeywordKind,
) -> Option<Keyword> {
    if let Some((keyword, where_x)) = parse_keyword_with_where_x(keyword_text) {
        return normalize_graveyard_granted_keyword(keyword, where_x, kind);
    }
    let keyword = super::oracle_keyword::parse_keyword_from_oracle(keyword_text.trim())?;
    normalize_graveyard_granted_keyword(keyword, None, kind)
}

/// CR 702.97a / CR 702.141a: When a graveyard grant binds X to the recipient
/// card's mana value, lower to `ManaCost::SelfManaValue` so runtime synthesis
/// concretizes the activated ability's mana sub-cost as generic mana.
fn binds_recipient_mana_value(where_x: &Option<QuantityRef>) -> bool {
    matches!(
        where_x,
        Some(QuantityRef::SelfManaValue)
            | Some(QuantityRef::ObjectManaValue {
                scope: ObjectScope::Recipient,
            })
    )
}

fn graveyard_granted_kind_for_keyword(keyword: &Keyword) -> Option<GraveyardGrantedKeywordKind> {
    [
        GraveyardGrantedKeywordKind::Flashback,
        GraveyardGrantedKeywordKind::Escape,
        GraveyardGrantedKeywordKind::Mayhem,
        GraveyardGrantedKeywordKind::Scavenge,
        GraveyardGrantedKeywordKind::Encore,
    ]
    .into_iter()
    .find(|kind| kind.matches_keyword(keyword))
}

fn finalize_graveyard_zone_grant_keyword(
    keyword: Keyword,
    where_x: Option<QuantityRef>,
) -> Keyword {
    let Some(kind) = graveyard_granted_kind_for_keyword(&keyword) else {
        return keyword;
    };
    normalize_graveyard_granted_keyword(keyword.clone(), where_x, kind).unwrap_or(keyword)
}

fn normalize_graveyard_granted_keyword(
    keyword: Keyword,
    where_x: Option<QuantityRef>,
    kind: GraveyardGrantedKeywordKind,
) -> Option<Keyword> {
    if !kind.matches_keyword(&keyword) {
        return None;
    }
    match (keyword, &where_x) {
        (Keyword::Encore(_), where_x) if binds_recipient_mana_value(where_x) => {
            Some(Keyword::Encore(ManaCost::SelfManaValue))
        }
        (Keyword::Scavenge(_), where_x) if binds_recipient_mana_value(where_x) => {
            Some(Keyword::Scavenge(ManaCost::SelfManaValue))
        }
        (keyword, None) => Some(keyword),
        _ => None,
    }
}

/// CR 702.97 / CR 702.141: Parse a single-sentence graveyard keyword grant whose
/// keyword (and optional inline "where X is its mana value" binding) lives on
/// the same line — Sliver Gravemother's "encore {X}, where X is its mana value".
/// Continuation-sentence grants (Wire Surgeons / Varolz) return `None`.
pub(crate) fn try_parse_graveyard_keyword_grant_static(line: &str) -> Option<StaticDefinition> {
    let stripped = strip_reminder_text(line);
    let lower = stripped.to_lowercase();
    // Same period boundary as `try_parse_graveyard_keyword_static_with_continuation`
    // in oracle.rs — if a continuation sentence is present, the inline path must
    // not parse a bare keyword off the first sentence and drop the cost clause.
    if super::oracle_nom::bridge::split_once_on_lower(&stripped, &lower, ". ").is_some() {
        return None;
    }

    let (turn_condition, grant_prefix) = nom_on_lower(&stripped, &lower, |input| {
        value(StaticCondition::DuringYourTurn, tag("during your turn, ")).parse(input)
    })
    .map_or((None, stripped.as_str()), |(condition, rest)| {
        (Some(condition), rest)
    });

    let (affected, kind, keyword_text) = try_parse_graveyard_keyword_grant_clause(grant_prefix)?;
    let keyword = parse_graveyard_granted_keyword_phrase(&keyword_text, kind)?;

    let mut def = StaticDefinition::continuous()
        .affected(affected)
        .modifications(vec![ContinuousModification::AddKeyword { keyword }])
        .description(line.to_string());
    if let Some(condition) = turn_condition {
        def = def.condition(condition);
    }
    Some(def)
}

pub(crate) fn parse_keyword_with_where_x(input: &str) -> Option<(Keyword, Option<QuantityRef>)> {
    type VE<'a> = OracleError<'a>;

    let input = input.trim().trim_end_matches('.');
    let (rest, keyword_text) = nom::bytes::complete::take_till::<_, _, VE<'_>>(|c| c == ',')
        .parse(input)
        .ok()?;
    let keyword = super::oracle_keyword::parse_keyword_from_oracle(keyword_text.trim())?;
    let rest = rest.trim();
    if rest.is_empty() {
        return Some((keyword, None));
    }

    let (_, qty_text) = preceded(
        tag_no_case::<_, _, VE<'_>>(", where x is "),
        nom::combinator::rest,
    )
    .parse(rest)
    .ok()?;
    let (_, qty) =
        super::oracle_nom::quantity::parse_quantity_ref_complete(qty_text.trim()).ok()?;
    Some((keyword, Some(qty)))
}

#[cfg(test)]
pub(crate) fn parse_spells_have_keyword_for_test(text: &str) -> Option<StaticDefinition> {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    parse_spells_have_keyword(&tp, text)
}

/// Parse "[Type] spells you cast [from zone] have [keyword]" patterns.
/// CR 702.51a: Grants a keyword (typically convoke) to spells matching a filter during casting.
/// Also handles "Creature cards you own that aren't on the battlefield have flash."
pub(crate) fn parse_spells_have_keyword(tp: &TextPair<'_>, text: &str) -> Option<StaticDefinition> {
    type VE<'a> = OracleError<'a>;

    let scoped_tp = nom_tag_tp(tp, "during your turn, ");
    let condition = scoped_tp.as_ref().map(|_| StaticCondition::DuringYourTurn);
    let tp = scoped_tp.as_ref().unwrap_or(tp);

    // CR 702.74a: keyword-grant lines that read "... gain <keyword> as you cast
    // them" (Ashling, the Limitless) carry a trailing " as you cast them" after
    // the keyword text. Strip it structurally (period then suffix) BEFORE the
    // separator split so the keyword residue is "evoke {4}" rather than
    // "evoke {4} as you cast them" — `parse_keyword_with_where_x` takes up to the
    // first comma as keyword_text, and `parse_keyword_from_oracle` would reject
    // the trailing clause. Mirror the existing trailing-period handling.
    let trimmed_tp = tp.trim_end_matches('.');
    let trimmed_tp = trimmed_tp
        // allow-noncombinator: structural trailing-clause cleanup on the pre-delimited grant phrase, not parsing dispatch (mirrors the trim_end_matches period strip above).
        .strip_suffix(" as you cast them")
        .unwrap_or(trimmed_tp);
    let tp = &trimmed_tp;

    // Pattern 1: "[type] spell(s) you cast [from zone] have/has/gain/gains [keyword]."
    // Find the predicate separator to split subject from keyword.
    // CR 702.74a: "... spells you cast ... gain <keyword>" (Ashling) uses "gain"/
    // "gains" as the grant verb instead of "have"/"has". The grant verb is tried
    // in the fixed priority order have → has → gain → gains (first verb in this
    // list that appears anywhere wins); the real card class carries exactly one
    // grant verb, so the order only disambiguates hypothetical mixed-verb text.
    let (have_pos, have_len) = tp
        .lower
        .match_indices(" have ")
        .next()
        .map(|(pos, sep)| (pos, sep.len()))
        .or_else(|| {
            tp.lower
                .match_indices(" has ")
                .next()
                .map(|(pos, sep)| (pos, sep.len()))
        })
        .or_else(|| {
            tp.lower
                .match_indices(" gain ")
                .next()
                .map(|(pos, sep)| (pos, sep.len()))
        })
        .or_else(|| {
            tp.lower
                .match_indices(" gains ")
                .next()
                .map(|(pos, sep)| (pos, sep.len()))
        })?;
    let subject = &tp.lower[..have_pos];
    let keyword_str = tp.lower[have_pos + have_len..].trim();

    // Parse the keyword — must be a valid keyword. A trailing "where X is …"
    // clause binds an earlier variable-X mana-value qualifier on the subject.
    let (keyword, where_x) = parse_keyword_with_where_x(keyword_str)?;

    // CR 611.2f: "The first <qualifier> spell you cast [from <zone>] <timing> has
    // [keyword]" — a once-per-turn keyword grant gated on the first qualifying
    // spell of the turn (Peri Brown, The Twelfth Doctor, Maelstrom Nexus,
    // Wild-Magic Sorcerer, Current Curriculum). Reuses the same
    // `parse_first_qualified_spell_filter` grammar + `first_qualified_spell_condition`
    // gate as the paired cost-modifier consumer, so the qualifying spell filter,
    // cast-origin restriction, and `SpellsCastThisTurn == 0` gate are all preserved
    // instead of collapsing to "every spell you cast".
    match parse_first_qualified_spell_filter(subject) {
        // Not a first-qualified-spell line — fall through to the ordinary
        // "[type] spells you cast [from zone] have [keyword]" patterns below.
        FirstQualifiedSpell::NotApplicable => {}
        // The shape is present but the qualifier/timing isn't representable. Fall
        // through (NOT a `return None`) so the existing gateless static is
        // preserved for not-yet-representable qualifiers — no regression.
        FirstQualifiedSpell::UnsupportedQualifier => {}
        FirstQualifiedSpell::Supported(filter, timing) => {
            // CR 601.2f: trailing-residue guard. `parse_first_qualified_spell_filter`
            // discards any text after the timing phrase; if that region is
            // non-empty an unrepresentable qualifier was dropped (Rain of Riches'
            // "that mana from a Treasure was spent to cast"; TARDIS Bay's
            // post-timing "with mana value 2 or greater"). Decline rather than emit
            // a residue-blind gate — fall through to the existing gateless static.
            if first_qualified_spell_subject_fully_consumed(subject) {
                // CR 601.2a: scope `ControllerRef::You` to every leaf (And/Not
                // recursion) so an opponent's qualifying spell never qualifies.
                let affected =
                    apply_spell_keyword_subject_constraints(filter.clone(), None, None, Vec::new());
                // CR 611.2f: `SpellsCastThisTurn(filter) == 0` first-spell gate,
                // plus `DuringYourTurn` for the controller-turn timing (a clean
                // "during each of your turns" subject with no post-timing residue;
                // TARDIS Bay itself declines above because its MV qualifier follows
                // the timing). When a leading "during your turn," scope was already
                // stripped, combine both rather than dropping either.
                let first_qualified = first_qualified_spell_condition(&filter, &timing);
                let combined_condition = match condition.clone() {
                    Some(leading) => StaticCondition::And {
                        conditions: vec![leading, first_qualified],
                    },
                    None => first_qualified,
                };
                return Some(
                    StaticDefinition::new(StaticMode::CastWithKeyword { keyword })
                        .affected(affected)
                        .condition(combined_condition)
                        .description(text.to_string())
                        .active_zones(vec![Zone::Battlefield]),
                );
            }
        }
    }

    // Find "spells you cast" in the subject — may be preceded by a type descriptor
    let spell_marker = subject
        .match_indices("spells you cast")
        .next()
        .map(|(pos, matched)| (pos, matched.len()))
        .or_else(|| {
            subject
                .match_indices("spell you cast")
                .next()
                .map(|(pos, matched)| (pos, matched.len()))
        });
    if let Some((marker_pos, marker_len)) = spell_marker {
        let raw_type_part = subject[..marker_pos].trim();
        let type_part = tag::<_, _, VE<'_>>("each ")
            .parse(raw_type_part)
            .map_or(raw_type_part, |(rest, _)| rest.trim());
        let after_spells = subject[marker_pos + marker_len..].trim();

        // Walk a cursor through optional qualifiers — zone first, then MV —
        // so combinations like "from exile with mana value 4 or greater" parse
        // correctly. Each qualifier consumes its own bytes.
        let mut cursor = after_spells;

        // Parse optional zone qualifier: "from exile", "from your graveyard"
        let zone_filter = if let Ok((rest, zone)) = alt((
            value(Zone::Exile, tag::<_, _, VE<'_>>("from exile")),
            value(Zone::Hand, tag("from your hand")),
        ))
        .parse(cursor)
        {
            cursor = rest.trim_start();
            Some(FilterProp::InZone { zone })
        } else {
            None
        };

        // CR 202.3: Optional "with mana value N or greater/less" qualifier
        // (Imoti, Celebrant of Bounty: "Spells you cast with mana value 6 or
        // greater have cascade."). Variable-X thresholds may be bound by the
        // keyword clause's trailing "where X is …" quantity (Abaddon class).
        let mv_filter = parse_mana_value_suffix(cursor, &mut ParseContext::default()).and_then(
            |(prop, consumed)| {
                let FilterProp::Cmc { comparator, value } = prop else {
                    return None;
                };
                let value = match where_x.as_ref() {
                    Some(qty) => bind_where_x_in_quantity_expr(value, qty)?,
                    None => match value {
                        QuantityExpr::Fixed { .. } => value,
                        _ => return None,
                    },
                };
                cursor = cursor[consumed..].trim_start();
                Some(FilterProp::Cmc { comparator, value })
            },
        );
        // CR 105.2: trailing "that's one or more colors"/"that's exactly N colors" relative clause → ColorCount.
        let color_props = if let Some((props, consumed)) =
            crate::parser::oracle_target::parse_that_clause_suffix(cursor, None)
        {
            cursor = cursor[consumed..].trim_start();
            props
        } else {
            Vec::new()
        };
        let _ = cursor; // qualifiers are optional; remaining slice is unused

        let mut supertype_props: Vec<FilterProp> = Vec::new();
        let base_filter = if type_part.is_empty() {
            // "Spells you cast" (no type prefix) — applies to all spells
            TargetFilter::Typed(TypedFilter::card())
        } else {
            // CR 205.4a: peel leading supertype word(s) BEFORE parse_type_phrase, which only
            // emits HasSupertype for a supertype prefixed before a type word (requires a trailing
            // space); a bare "legendary" would otherwise be dropped, and an un-peeled prefix would
            // double-emit. Peel here (emit once) and pass only the remainder to parse_type_phrase.
            let type_prefix_original = tp.original[..marker_pos].trim();
            let lower_prefix = type_prefix_original.to_lowercase();
            let prefix_tp = TextPair::new(type_prefix_original, &lower_prefix);
            let prefix_tp = nom_tag_tp(&prefix_tp, "each ").unwrap_or(prefix_tp);
            let mut peel_lower = prefix_tp.lower;
            let mut peel_offset = 0usize;
            while let Ok((rest, supertype)) = nom_target::parse_supertype_word(peel_lower) {
                // CR 205.4a: parse_supertype_word consumes no boundary by contract, so the
                // caller must require a word boundary (space, punctuation, or end-of-string)
                // after the supertype — otherwise a longer word with a supertype prefix
                // ("snow" in "snowman") would be mis-peeled. A bare trailing supertype
                // ("legendary") legitimately ends at end-of-string.
                let at_boundary = rest
                    .chars()
                    .next()
                    .is_none_or(|c| !c.is_alphanumeric() && c != '_');
                if !at_boundary {
                    break;
                }
                supertype_props.push(FilterProp::HasSupertype { value: supertype });
                // Consume the supertype word plus its trailing whitespace boundary
                // via nom (space0 — a bare trailing supertype has no following space).
                let rest = space0::<_, VE<'_>>
                    .parse(rest)
                    .map_or(rest, |(after, _)| after);
                peel_offset += peel_lower.len() - rest.len();
                peel_lower = rest;
            }
            let type_remainder = prefix_tp.original[peel_offset..].trim();
            if type_remainder.is_empty() {
                TargetFilter::Typed(TypedFilter::card())
            } else {
                parse_type_phrase(type_remainder).0
            }
        };
        let mut extra_props = supertype_props;
        extra_props.extend(color_props);
        // CR-correct affected scope: `apply_spell_keyword_subject_constraints`
        // recurses into `TargetFilter::Or` so compound type prefixes ("instant
        // and sorcery spells you cast have affinity for creatures") preserve
        // each branch instead of collapsing to all spells.
        let affected = apply_spell_keyword_subject_constraints(
            base_filter,
            zone_filter,
            mv_filter,
            extra_props,
        );

        let mut def = StaticDefinition::new(StaticMode::CastWithKeyword { keyword })
            .affected(affected)
            .description(text.to_string())
            .active_zones(vec![Zone::Battlefield]);
        if let Some(condition) = condition.clone() {
            def = def.condition(condition);
        }
        return Some(def);
    }

    // Pattern 2: "Creature cards you own that aren't on the battlefield have flash"
    // This grants flash to cards in non-battlefield zones.
    if nom_primitives::scan_contains(subject, "cards you own that aren't on the battlefield") {
        let (prefix, _) = nom_primitives::scan_split_at_phrase(subject, |i| tag("cards").parse(i))?;
        let type_end = prefix.len();
        let type_part = &tp.original[..type_end];
        let (base_filter, _) = parse_type_phrase(type_part);
        let affected = match base_filter {
            TargetFilter::Typed(mut typed) => {
                typed = typed.controller(ControllerRef::You);
                // "aren't on the battlefield" means any zone except battlefield
                typed.properties.push(FilterProp::InAnyZone {
                    zones: vec![Zone::Hand, Zone::Graveyard, Zone::Exile, Zone::Command],
                });
                TargetFilter::Typed(typed)
            }
            _ => base_filter,
        };
        let mut def = StaticDefinition::new(StaticMode::CastWithKeyword { keyword })
            .affected(affected)
            .description(text.to_string())
            .active_zones(vec![Zone::Battlefield]);
        if let Some(condition) = condition.clone() {
            def = def.condition(condition);
        }
        return Some(def);
    }
    // Pattern 3: "[type] cards in/from [your zone] have [keyword]"
    // CR 702.81a (Retrace): Grants a casting keyword to cards in a specific zone.
    // Six grants retrace to nonland permanent cards in your graveyard.
    // Emits a Continuous static with AddKeyword so the off-zone keyword-grant
    // path (`effective_off_zone_keywords`) sees the grant and the card becomes
    // castable from the graveyard.
    {
        let (base_filter, rest) = parse_type_phrase(subject);
        if rest.trim().is_empty() && target_filter_is_your_graveyard(&base_filter) {
            let keyword = finalize_graveyard_zone_grant_keyword(keyword, where_x.clone());
            let mut def = StaticDefinition::continuous()
                .affected(base_filter)
                .modifications(vec![ContinuousModification::AddKeyword { keyword }])
                .description(text.to_string());
            if let Some(condition) = condition.clone() {
                def = def.condition(condition);
            }
            return Some(def);
        }
    }
    None
}

/// Parse the static permission "You may cast [type] spells as though they had
/// flash." (Leyline of Anticipation, Vedalken Orrery, Vivien, Champion of the
/// Wilds' first ability).
///
/// CR 601.3b: An effect that lets a player cast a spell "as though it had flash"
/// lets that player begin to cast it at instant speed. CR 702.8a: flash means
/// the spell may be cast any time its controller could cast an instant.
///
/// This must emit `StaticMode::CastWithKeyword { keyword: Flash }` with the
/// spell-type filter in `affected` — that is the ONLY static mode the
/// flash-timing path (`granted_spell_keywords` in casting.rs) actually reads.
/// The legacy `StaticMode::CastWithFlash` carries no spell filter and is never
/// consumed by that path, so it silently dropped both the timing grant and the
/// "creature spells" restriction (issue #1957). Mirrors the activated/triggered
/// `try_parse_cast_as_though_flash_permission` (oracle_effect) so the static and
/// duration-scoped forms share one filter-construction contract.
pub(crate) fn parse_cast_as_though_flash_static(
    tp: &TextPair<'_>,
    text: &str,
) -> Option<StaticDefinition> {
    let (type_text, all_players) = nom_on_lower(tp.original, tp.lower, |i| {
        let (i, all_players) = alt((
            value(false, tag::<_, _, OracleError<'_>>("you may ")),
            value(true, tag("players may ")),
            value(true, tag("any player may ")),
            value(false, tag("")),
        ))
        .parse(i)?;
        let (i, _) = tag("cast ").parse(i)?;
        // "[type] spells as though they had flash" — the bare "spells" form
        // (no type prefix) grants flash to every spell (Leyline of Anticipation).
        let (i, type_part) = alt((
            value("", tag("spells as though they had flash")),
            map(
                terminated(
                    take_until(" spells as though they had flash"),
                    tag(" spells as though they had flash"),
                ),
                str::trim,
            ),
        ))
        .parse(i)?;
        let (i, _) = opt(tag(".")).parse(i)?;
        let (i, _) = eof.parse(i)?;
        Ok((i, (type_part.to_string(), all_players)))
    })?
    .0;

    // CR 601.3b: scope the grant to the spell class. A bare "spells" grant
    // applies to every spell the controller casts; a typed grant ("creature
    // spells") constrains to that type. "Players may" / "Any player may" forms
    // intentionally remain unscoped, while "you may" forms recurse through
    // `TargetFilter::Or` via `apply_spell_keyword_subject_constraints`.
    let base_filter = if type_text.is_empty() {
        TargetFilter::Typed(TypedFilter::card())
    } else {
        let phrase = format!("{type_text} spells");
        parse_type_phrase(&phrase).0
    };
    let affected = if all_players {
        base_filter
    } else {
        apply_spell_keyword_subject_constraints(base_filter, None, None, Vec::new())
    };

    Some(
        StaticDefinition::new(StaticMode::CastWithKeyword {
            keyword: Keyword::Flash,
        })
        .affected(affected)
        .description(text.to_string())
        .active_zones(vec![Zone::Battlefield]),
    )
}

pub(crate) fn apply_spell_keyword_subject_constraints(
    filter: TargetFilter,
    zone_filter: Option<FilterProp>,
    mv_filter: Option<FilterProp>,
    extra_props: Vec<FilterProp>,
) -> TargetFilter {
    match filter {
        TargetFilter::Typed(mut typed) => {
            typed = typed.controller(ControllerRef::You);
            if let Some(prop) = zone_filter {
                typed.properties.push(prop);
            }
            if let Some(prop) = mv_filter {
                typed.properties.push(prop);
            }
            typed.properties.extend(extra_props);
            TargetFilter::Typed(typed)
        }
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters
                .into_iter()
                .map(|filter| {
                    apply_spell_keyword_subject_constraints(
                        filter,
                        zone_filter.clone(),
                        mv_filter.clone(),
                        extra_props.clone(),
                    )
                })
                .collect(),
        },
        // CR 601.2a: compound subjects ("<type> spell you cast from <zone>") lower
        // to And/Not; recurse so `ControllerRef::You` reaches every leaf, otherwise
        // the grant would scope to opponents' qualifying spells too. The And arm is
        // live on the first-qualified-spell keyword-grant path (type + origin-zone
        // both present); the Not arm is defensive.
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters
                .into_iter()
                .map(|filter| {
                    apply_spell_keyword_subject_constraints(
                        filter,
                        zone_filter.clone(),
                        mv_filter.clone(),
                        extra_props.clone(),
                    )
                })
                .collect(),
        },
        TargetFilter::Not { filter } => TargetFilter::Not {
            filter: Box::new(apply_spell_keyword_subject_constraints(
                *filter,
                zone_filter.clone(),
                mv_filter.clone(),
                extra_props.clone(),
            )),
        },
        other => other,
    }
}

/// Parse creature subject phrases containing "of the chosen color/type" qualifiers.
/// Handles patterns like:
/// - "Creatures you control of the chosen color"
/// - "Creatures of the chosen color"
/// - "Creatures of the chosen type your opponents control"
/// - "creature you control of the chosen type other than this Vehicle"
/// - "creatures of that color" (CR 608.2c anaphor form after a `Choose a color`)
/// - "creatures of that type" (CR 608.2c anaphor form after a `Choose a creature type`)
///
/// CR 105.4: "of the chosen color" / "of that color" → `FilterProp::IsChosenColor`
/// CR 205.3m: "of the chosen type" / "of that type" → `FilterProp::IsChosenCreatureType`
///
/// Issue #327: the "of that color" / "of that type" anaphor forms are
/// equivalent to "of the chosen color" / "of the chosen type" — same typed
/// reference, same runtime resolution. They differ only orthographically
/// (CR 608.2c anaphor vs CR 113.6 explicit chosen-attribute reference).
pub(crate) fn parse_chosen_qualifier_subject(tp: &TextPair<'_>) -> Option<TargetFilter> {
    type VE<'a> = OracleError<'a>;

    // Must start with "creature" or "creatures"
    let rest = if let Ok((r, _)) = tag::<_, _, VE<'_>>("creatures ")(tp.lower) {
        r
    } else if let Ok((r, _)) = tag::<_, _, VE<'_>>("creature ")(tp.lower) {
        r
    } else {
        return None;
    };

    // Try to find "of the chosen color" / "of that color" / "of the chosen
    // type" / "of that type" somewhere in the rest. Same typed reference for
    // both anaphor forms — see fn doc.
    let chosen_prop: FilterProp;
    let before_chosen: &str;
    let after_chosen: &str;

    let color_split = nom_primitives::split_once_on(rest, "of the chosen color")
        .or_else(|_| nom_primitives::split_once_on(rest, "of that color"));
    let type_split = nom_primitives::split_once_on(rest, "of the chosen type")
        .or_else(|_| nom_primitives::split_once_on(rest, "of that type"));

    if let Ok((_, (before, after))) = color_split {
        chosen_prop = FilterProp::IsChosenColor;
        before_chosen = before.trim();
        after_chosen = after.trim();
    } else if let Ok((_, (before, after))) = type_split {
        chosen_prop = FilterProp::IsChosenCreatureType;
        before_chosen = before.trim();
        after_chosen = after.trim();
    } else {
        return None;
    };

    // Parse controller from before or after the chosen qualifier
    let mut controller = None;
    let mut extra_props = vec![chosen_prop];

    // Check "you control" before the qualifier
    if before_chosen == "you control" {
        controller = Some(ControllerRef::You);
    } else if !before_chosen.is_empty() {
        return None;
    }

    // Check controller/qualifiers after the qualifier
    let remaining = after_chosen;
    if nom_tag_lower(remaining, remaining, "your opponents control").is_some() {
        controller = Some(ControllerRef::Opponent);
    } else if nom_tag_lower(remaining, remaining, "you control").is_some() {
        controller = Some(ControllerRef::You);
    }

    // Check for "other than" suffix (e.g., "other than this Vehicle")
    if nom_primitives::scan_contains(remaining, "other than") {
        extra_props.push(FilterProp::Another);
    }

    let mut typed = TypedFilter::creature().properties(extra_props);
    if let Some(ctrl) = controller {
        typed = typed.controller(ctrl);
    }
    Some(TargetFilter::Typed(typed))
}

/// CR 602.1 + CR 603.1: The set of ability categories a "[~ has] all
/// [activated|triggered|activated and triggered] abilities of [source]" grant
/// donates. Activated and triggered abilities land in different stores via
/// different continuous modifications (`GrantAllActivatedAbilitiesOf` →
/// `obj.abilities`; `GrantAllTriggeredAbilitiesOf` → `obj.trigger_definitions`),
/// so the parser captures which categories the phrase named.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GrantedAbilityKinds {
    Activated,
    Triggered,
    ActivatedAndTriggered,
}

/// CR 602.1 + CR 603.1: The grant-phrase category axis. The conjunction form
/// ("activated and triggered" / "triggered and activated", order-insensitive) is
/// tried before the single-category leaves so the longer phrase wins. The plural
/// and the singular-distributive activated form ("each activated ability of",
/// Locus of Enlightenment) map to the same activated set.
fn parse_granted_ability_kinds(input: &str) -> OracleResult<'_, GrantedAbilityKinds> {
    alt((
        value(
            GrantedAbilityKinds::ActivatedAndTriggered,
            alt((
                tag("all activated and triggered abilities of "),
                tag("all triggered and activated abilities of "),
            )),
        ),
        value(
            GrantedAbilityKinds::Triggered,
            tag("all triggered abilities of "),
        ),
        value(
            GrantedAbilityKinds::Activated,
            alt((
                tag("all activated abilities of "),
                tag("each activated ability of "),
            )),
        ),
    ))
    .parse(input)
}

/// CR 613.1f + CR 113.3: Recognize "[~ has] all [category] abilities of [source]"
/// and return the donated category set plus the provider `source` filter.
///
/// Composed from nom combinators along three independent axes: the optional
/// leading verb (`has`/`have`), the category axis ([`parse_granted_ability_kinds`]),
/// and the source-set noun phrase ([`grant_source_noun_phrase`]) —
/// `ExiledBySource` (Myr Welder / Agatha), the same-name exclusion (Marvin),
/// `ChosenCard` ("the last chosen card", Koh), etc.
///
/// Returns `None` for forms still needing extra infrastructure (counter-gated
/// exile sets) so they stay a loud gap rather than over-granting.
fn parse_grant_all_abilities_clause(
    lower: &str,
) -> Option<(GrantedAbilityKinds, crate::types::ability::TargetFilter)> {
    let p = lower.trim().trim_end_matches('.').trim();
    all_consuming((
        opt(alt((tag::<_, _, OracleError<'_>>("has "), tag("have ")))),
        parse_granted_ability_kinds,
        grant_source_noun_phrase,
    ))
    .parse(p)
    .ok()
    .map(|(_, (_, kinds, source))| (kinds, source))
}

/// CR 602.5b + CR 602.5c: The "you may activate each of those abilities only once
/// each turn" use-restriction rider that follows an ability-grant sentence, mapped
/// to the typed `ActivationRestriction::OnlyOnceEachTurn`. Decomposed into its
/// grammatical axes — permission (`you may activate`), the granted-set anaphor
/// (`each of those abilities`), and the frequency cap (`only once each turn`, the
/// semantic key) — so the cap is a *meaningfully parsed* restriction, not a
/// verbatim sentence consumed and discarded.
fn parse_activate_once_each_turn_rider(input: &str) -> OracleResult<'_, ActivationRestriction> {
    value(
        ActivationRestriction::OnlyOnceEachTurn,
        (
            tag("you may activate "),
            tag("each of those abilities"),
            tag(" only once each turn"),
        ),
    )
    .parse(input)
}

/// CR 602.5b + CR 602.5c: Fold an "activate ... only once each turn" use-restriction
/// rider (a trailing sentence that yields no standalone static) into the `cap` of
/// the most recently emitted `GrantAllActivatedAbilitiesOf` modification, returning
/// `true` when the fold lands. Returns `false` when `segment` is not the rider, or
/// when there is no still-uncapped ability grant preceding it to attach to.
///
/// This is the SHARED grant-rider primitive: it composes the once-per-turn cap with
/// the STANDARD grant parse (sentence 1 → `GrantAllActivatedAbilitiesOf` via the
/// ordinary continuous-clause dispatch) during normal sentence splitting
/// (`parse_multi_sentence_statics`), so any "<grant activated abilities>. You may
/// activate each of those abilities only once each turn." card is capped — over any
/// grant source, with no card-specific whole-line hook. The restriction travels
/// with the granted abilities (CR 602.5c — a use-restriction acquired with an
/// ability applies to that acquired ability), which the layer-6 expansion injects
/// and `game/restrictions.rs` enforces per `(recipient, ability_index)`.
pub(super) fn fold_grant_cap_rider(segment: &str, defs: &mut [StaticDefinition]) -> bool {
    let lower = segment.trim().trim_end_matches('.').trim().to_lowercase();
    let Ok((_, restriction)) =
        all_consuming(parse_activate_once_each_turn_rider).parse(lower.as_str())
    else {
        return false;
    };
    // Attach to the most recent grant modification that is still uncapped.
    for def in defs.iter_mut().rev() {
        for modification in def.modifications.iter_mut().rev() {
            if let ContinuousModification::GrantAllActivatedAbilitiesOf { cap, .. } = modification {
                if cap.is_none() {
                    *cap = Some(restriction);
                    return true;
                }
            }
        }
    }
    false
}

/// CR 613.1f + CR 607.2a + CR 201.2: The source-set noun phrase of an
/// ability-grant-by-reference static. Each arm is a leaf of the source-set axis;
/// adding a new referenced set is one more `alt` arm here, never a new variant.
fn grant_source_noun_phrase(input: &str) -> OracleResult<'_, crate::types::ability::TargetFilter> {
    use crate::types::counter::CounterType;
    alt((
        // CR 607.2a: cards exiled with the host. Optional card-type qualifier
        // narrows the granted set (Agatha grants creature cards only).
        grant_exiled_source,
        // CR 201.2: "creatures you control that don't have the same name as
        // it/~" (Marvin) — battlefield creatures you control, excluding ones
        // sharing the recipient's name.
        value(
            TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::Not {
                        prop: Box::new(FilterProp::SameName),
                    }]),
            ),
            (
                tag("creatures you control that don't have the same name as "),
                alt((tag("it"), tag("~"))),
            ),
        ),
        // CR 613.1f: "each other creature with a +1/+1 counter on it"
        // (Experiment Kraj) — all creatures except self with at least one
        // +1/+1 counter.
        value(
            TargetFilter::And {
                filters: vec![
                    TargetFilter::Typed(TypedFilter::creature().properties(vec![
                        FilterProp::Counters {
                            counters: CounterMatch::OfType(CounterType::Plus1Plus1),
                            comparator: Comparator::GE,
                            count: QuantityExpr::Fixed { value: 1 },
                        },
                    ])),
                    TargetFilter::Not {
                        filter: Box::new(TargetFilter::SelfRef),
                    },
                ],
            },
            (
                tag("each other creature with a +1/+1 counter on "),
                alt((tag("it"), tag("them"))),
            ),
        ),
        // CR 613.1f: "all creatures your opponents control" (Drana and Linvala)
        // — battlefield permanents; scope to InZone { Battlefield } so dead
        // or exiled creatures of theirs do not donate abilities.
        value(
            TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::Opponent)
                    .properties(vec![FilterProp::InZone {
                        zone: Zone::Battlefield,
                    }]),
            ),
            tag("all creatures your opponents control"),
        ),
        // CR 613.1f: "all creature cards in all graveyards" (Necrotic Ooze)
        value(
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::InZone {
                zone: Zone::Graveyard,
            }])),
            tag("all creature cards in all graveyards"),
        ),
        // CR 613.1f: "all land cards in all graveyards"
        value(
            TargetFilter::Typed(TypedFilter::land().properties(vec![FilterProp::InZone {
                zone: Zone::Graveyard,
            }])),
            tag("all land cards in all graveyards"),
        ),
        // CR 613.1f: "all lands on the battlefield" (Manascape Refractor)
        // — zone is explicit in the phrase; encode it so graveyard/hand land
        // cards are excluded from the runtime provider scan.
        value(
            TargetFilter::Typed(TypedFilter::land().properties(vec![FilterProp::InZone {
                zone: Zone::Battlefield,
            }])),
            tag("all lands on the battlefield"),
        ),
        // CR 613.1f: "all legendary creatures you control" (Robaran Mercenaries)
        // — battlefield permanents; scope to InZone { Battlefield } so
        // legendary creature cards in hand/graveyard do not donate abilities.
        value(
            TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![
                        FilterProp::HasSupertype {
                            value: Supertype::Legendary,
                        },
                        FilterProp::InZone {
                            zone: Zone::Battlefield,
                        },
                    ]),
            ),
            tag("all legendary creatures you control"),
        ),
        // CR 613.1f: "all artifact cards in your graveyard"
        // CR 108.3: Graveyard cards are "yours" by ownership, not control —
        // use FilterProp::Owned rather than TypedFilter::controller here.
        value(
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact).properties(vec![
                FilterProp::Owned {
                    controller: ControllerRef::You,
                },
                FilterProp::InZone {
                    zone: Zone::Graveyard,
                },
            ])),
            tag("all artifact cards in your graveyard"),
        ),
        // CR 613.1f + CR 611.2c: "the last chosen card" (Koh, the Face Stealer) —
        // the single card most recently recorded on the host via
        // `Effect::RememberCard` (`ChosenAttribute::Card`). Resolved live each
        // layer pass by `TargetFilter::ChosenCard`.
        value(TargetFilter::ChosenCard, tag("the last chosen card")),
    ))
    .parse(input)
}

/// CR 607.2a: "[the exiled card] | all [<card type>] cards exiled with it/~".
/// The optional card-type qualifier intersects `ExiledBySource` with a typed
/// filter so the grant tracks only matching exiled cards.
fn grant_exiled_source(input: &str) -> OracleResult<'_, crate::types::ability::TargetFilter> {
    alt((
        // CR 702.167c: "the exiled card[s] used to craft it/~" — the craft pile
        // (cards exiled to pay the craft cost that returned this permanent). The
        // craft materials are linked to the host by `ExileLinkKind::CraftMaterial`,
        // which `ExiledBySource` reads kind-agnostically. Tried before the bare
        // "the exiled card" arm so the longer craft phrase wins (Locus of
        // Enlightenment).
        value(
            TargetFilter::ExiledBySource,
            (
                tag("the exiled card"),
                opt(tag("s")),
                tag(" used to craft "),
                alt((tag("it"), tag("~"))),
            ),
        ),
        value(TargetFilter::ExiledBySource, tag("the exiled card")),
        // "all [creature] cards exiled with it/~". The optional "creature"
        // qualifier intersects `ExiledBySource` with the Creature type filter
        // (CR 205.3 — a creature card is type Creature in exile) so Agatha grants
        // only creature cards' abilities; the untyped form (Myr Welder, Territory
        // Forge) stays a bare `ExiledBySource`.
        (
            tag("all "),
            opt(tag("creature ")),
            tag("cards exiled with "),
            alt((tag("it"), tag("~"))),
        )
            .map(|(_, creature_qualifier, _, _)| match creature_qualifier {
                Some(_) => TargetFilter::And {
                    filters: vec![
                        TargetFilter::Typed(TypedFilter::creature()),
                        TargetFilter::ExiledBySource,
                    ],
                },
                None => TargetFilter::ExiledBySource,
            }),
    ))
    .parse(input)
}

/// CR 613.1d: Parse a layer-4 type-removal predicate `"isn't a/an <core type>"`
/// (e.g. `"isn't a creature"`). Scans at word boundaries so the predicate can
/// appear anywhere in the modification text (the subject and any trailing
/// duration are stripped by the caller). Returns the removed [`CoreType`], or
/// `None` when no such predicate is present.
fn parse_isnt_a_core_type(lower: &str) -> Option<CoreType> {
    fn core_type_word(input: &str) -> OracleResult<'_, CoreType> {
        alt((
            value(CoreType::Artifact, tag("artifact")),
            value(CoreType::Battle, tag("battle")),
            value(CoreType::Creature, tag("creature")),
            value(CoreType::Enchantment, tag("enchantment")),
            value(CoreType::Land, tag("land")),
            value(CoreType::Planeswalker, tag("planeswalker")),
        ))
        .parse(input)
    }
    fn predicate(input: &str) -> OracleResult<'_, CoreType> {
        preceded(alt((tag("isn't an "), tag("isn't a "))), core_type_word).parse(input)
    }
    nom_primitives::scan_split_at_phrase(lower, |i| predicate(i))
        .and_then(|(_, clause)| predicate(clause).ok().map(|(_, ct)| ct))
}

/// CR 305.6 + CR 305.7 + CR 205.3i: Recognize a "gain all basic land types" /
/// "gain all land types" predicate (and the `has`/`have`/`are`/`is` copula
/// variants) and map it to the matching all-land-type continuous modification.
///
/// `AddAllBasicLandTypes` adds the five basic land subtypes (Plains, Island,
/// Swamp, Mountain, Forest — CR 305.6) in addition to a land's existing types;
/// each grants its intrinsic mana ability per CR 305.6 / CR 305.7.
/// `AddAllLandTypes` adds every one of the 17 land subtypes (CR 205.3i). Built
/// for the whole "[lands you control] gain all basic land types until <duration>"
/// class (Energybending), not a single card. The verb/copula is matched but
/// otherwise discarded — the affected filter and duration are owned by the
/// caller (`build_continuous_clause` / the static/anthem parsers).
fn parse_all_land_types_modification(text: &str) -> Option<ContinuousModification> {
    let lower = text.trim().trim_end_matches('.').trim().to_lowercase();
    super::oracle_nom::bridge::nom_parse_lower(&lower, |i| {
        let (i, _) = opt(alt((
            tag::<_, _, OracleError<'_>>("gains "),
            tag("gain "),
            tag("has "),
            tag("have "),
            tag("are "),
            tag("is "),
        )))
        .parse(i)?;
        // Longer phrase first so "all basic land types" wins over "all land types".
        all_consuming(alt((
            value(
                ContinuousModification::AddAllBasicLandTypes,
                tag("all basic land types"),
            ),
            value(
                ContinuousModification::AddAllLandTypes,
                tag("all land types"),
            ),
        )))
        .parse(i)
    })
}

pub(crate) fn parse_continuous_modifications(text: &str) -> Vec<ContinuousModification> {
    // Strip "where X is [quantity]" before parsing modifications,
    // but only if the text doesn't contain quoted abilities (which have their
    // own "where X is" handling inside the quote).
    let text_lower = text.to_lowercase();
    let text_tp = TextPair::new(text, &text_lower);
    let (stripped_tp, where_x_expression) = if text.contains('"') {
        (text_tp, None)
    } else {
        super::oracle_effect::strip_trailing_where_x(text_tp)
    };
    let tp = nom_tag_tp(&stripped_tp, "also ").unwrap_or(stripped_tp);
    let text_stripped = tp.original;
    let unquoted_text = strip_quoted_segments(text_stripped);
    let unquoted_lower = unquoted_text.to_lowercase();
    let unquoted_tp = TextPair::new(&unquoted_text, &unquoted_lower);

    // CR 613.1f + CR 113.3: "all [activated|triggered|activated and triggered]
    // abilities of [the exiled card | all cards exiled with it | the last chosen
    // card | …]" — grant the host the named ability categories of the source set
    // (Myr Welder / Agatha activated; Koh, the Face Stealer activated AND
    // triggered, source = the last chosen card). Typed/counter-gated sources stay
    // a gap (follow-ups).
    if let Some((kinds, source)) = parse_grant_all_abilities_clause(unquoted_tp.lower) {
        // CR 602.1 + CR 603.1: emit one continuous modification per donated
        // category. Activated and triggered land in different stores, so a
        // conjunction grant produces BOTH mods over the same `source`.
        // CR 602.5b: the grant sentence itself carries no use-restriction — the
        // once-per-turn cap (Locus) is folded in separately by `fold_grant_cap_rider`
        // when the trailing rider sentence is present, so the activated grant stays
        // uncapped; triggered abilities take no cap.
        let mut mods = Vec::new();
        if matches!(
            kinds,
            GrantedAbilityKinds::Activated | GrantedAbilityKinds::ActivatedAndTriggered
        ) {
            mods.push(ContinuousModification::GrantAllActivatedAbilitiesOf {
                source: source.clone(),
                cap: None,
            });
        }
        if matches!(
            kinds,
            GrantedAbilityKinds::Triggered | GrantedAbilityKinds::ActivatedAndTriggered
        ) {
            mods.push(ContinuousModification::GrantAllTriggeredAbilitiesOf { source });
        }
        return mods;
    }

    // CR 305.6 + CR 305.7 + CR 205.3i: "gain all basic land types" / "gain all
    // land types" (and the copula variants) — the whole predicate maps to a
    // single all-land-type modification. Checked early so the trailing "types"
    // noun is never mistaken for a P/T or keyword token by the parsers below.
    if let Some(modification) = parse_all_land_types_modification(unquoted_tp.original) {
        return vec![modification];
    }

    let mut modifications = Vec::new();

    // CR 205.1a + CR 613.1d/f: "loses all [other] abilities, card types, and
    // creature types" — a comma-and enumeration parsed with nom. Each member
    // maps to one modification. `CardTypes` requires the granted core-type
    // list, which only the "is a [type]" caller (`parse_enchanted_is_type`)
    // owns — in the standalone path it has no type set and is a no-op (such
    // text does not occur outside the "is a [type]" frame).
    for member in scan_loss_enumeration(unquoted_tp.lower) {
        match member {
            LossMember::Abilities => {
                modifications.push(ContinuousModification::RemoveAllAbilities);
            }
            LossMember::CreatureTypes => {
                modifications.push(ContinuousModification::RemoveAllSubtypes {
                    set: crate::types::card_type::SubtypeSet::Creature,
                });
            }
            LossMember::CardTypes => {}
        }
    }

    if let Some(dynamic_mods) = parse_dynamic_for_each_pt_modifications(&unquoted_text) {
        modifications.extend(dynamic_mods);
    } else if let Some(rest_tp) =
        nom_tag_tp(&unquoted_tp, "gets ").or_else(|| nom_tag_tp(&unquoted_tp, "get "))
    {
        let after = rest_tp.original.trim();
        if let Some((p, t)) = parse_pt_mod(after) {
            modifications.push(ContinuousModification::AddPower { value: p });
            modifications.push(ContinuousModification::AddToughness { value: t });
        }
    } else if let Some((p, t)) = parse_fixed_pt_in_text(unquoted_tp.lower) {
        modifications.push(ContinuousModification::AddPower { value: p });
        modifications.push(ContinuousModification::AddToughness { value: t });
    }

    if parse_legendary_supertype_grant(unquoted_tp.lower).is_some() {
        modifications.push(ContinuousModification::AddSupertype {
            supertype: Supertype::Legendary,
        });
    }

    // CR 613.1d: Layer 4 type removal — "isn't a/an <core type>" (e.g. Blink's
    // Alien Angel token: "this token isn't a creature until end of turn"). The
    // duration is already stripped by the caller. This is the building-block
    // analogue of the static-dispatch "~ isn't a <type> as long as <cond>" arm,
    // so the same removal works as a one-shot continuous effect on a token.
    if let Some(core_type) = parse_isnt_a_core_type(unquoted_tp.lower) {
        modifications.push(ContinuousModification::RemoveType { core_type });
    }

    // CR 510.1c: Aura/Equipment-style compound statics can attach the
    // toughness-combat-damage rule to the same affected object as a P/T
    // modification ("Enchanted creature gets +0/+2 and assigns…"). The same
    // predicate also rides one-shot duration-bound continuous effects whose
    // subject is plural ("creatures you control … assign combat damage equal to
    // their toughness rather than their power" — The Kingpin of Crime), so this
    // accepts both the singular ("its…its") and plural ("their…their") surface
    // forms via the shared predicate combinator.
    if nom_primitives::scan_at_word_boundaries(
        unquoted_lower.as_str(),
        super::evasion::parse_assigns_damage_from_toughness_predicate,
    )
    .is_some()
    {
        modifications.push(ContinuousModification::AssignDamageFromToughness);
    }

    // CR 701.15b: Positive goaded designation on token anaphors and compound
    // statics ("The tokens are goaded for the rest of the game", Life of the
    // Party; "Enchanted creature … is goaded").
    if nom_primitives::scan_contains(unquoted_lower.as_str(), "is goaded")
        || nom_primitives::scan_contains(unquoted_lower.as_str(), "are goaded")
    {
        modifications.push(ContinuousModification::AddStaticMode {
            mode: StaticMode::Goaded,
        });
    }

    // CR 701.60a + CR 701.60d: "can't become suspected" prohibition riding on a
    // compound static (Airtight Alibi: "Enchanted creature gets +2/+2 and can't
    // become suspected"). Confers a `CantBecomeSuspected` static onto the
    // affected creature; the suspect resolver gates on it. Mirrors the goaded
    // designation rider above.
    if nom_primitives::scan_contains(unquoted_lower.as_str(), "can't become suspected")
        || nom_primitives::scan_contains(unquoted_lower.as_str(), "cant become suspected")
    {
        modifications.push(ContinuousModification::AddStaticMode {
            mode: StaticMode::CantBecomeSuspected,
        });
    }

    // CR 702.73a + CR 205.3 + CR 613.1d: Conjunctive "is/are every creature
    // type" predicate — the Changeling-class type grant when it appears as
    // one conjunct in an Aura/Equipment compound static ("Enchanted creature
    // gets +2/+2, has reach, and is every creature type", "Equipped creature
    // gets +1/+1 and is every creature type"). The top-level grant form
    // ("Creatures you control are every creature type", "~ is every creature
    // type") is owned by `parse_all_creature_types_grant` and never reaches
    // this helper. Both copulas are scanned because subject number drives
    // verb agreement at the outer parser layer.
    if nom_primitives::scan_contains(unquoted_lower.as_str(), "is every creature type")
        || nom_primitives::scan_contains(unquoted_lower.as_str(), "are every creature type")
    {
        modifications.push(ContinuousModification::AddAllCreatureTypes);
    }

    // CR 613.4c: Scan for "get +X/+X" / "gets +X/+X" anywhere in the text
    // for dynamic P/T modification (e.g., Craterhoof Behemoth)
    if let Some(dynamic_mods) =
        parse_dynamic_pt_in_text(&unquoted_lower, where_x_expression.as_deref())
    {
        modifications.extend(dynamic_mods);
    }

    // CR 613.4b + CR 107.3m: "have base power and toughness X/X" — dynamic set
    // at layer 7b. Checked before the fixed-literal parser so X-bearing patterns
    // are not mis-parsed as literal integers.
    if let Some((power, toughness)) =
        parse_base_pt_dynamic(&unquoted_text, where_x_expression.as_deref())
    {
        modifications.push(ContinuousModification::SetPowerDynamic { value: power });
        modifications.push(ContinuousModification::SetToughnessDynamic { value: toughness });
    } else if !push_base_pt_mana_value_dynamic_modifications(&mut modifications, &unquoted_lower) {
        if let Some((power, toughness)) = parse_base_pt_mod(&unquoted_text) {
            modifications.push(ContinuousModification::SetPower { value: power });
            modifications.push(ContinuousModification::SetToughness { value: toughness });
        }
    }
    if let Some(power) = parse_base_power_mod(&unquoted_text) {
        modifications.push(ContinuousModification::SetPower { value: power });
    }
    if let Some(toughness) = parse_base_toughness_mod(&unquoted_text) {
        modifications.push(ContinuousModification::SetToughness { value: toughness });
    }

    // CR 509.1b + CR 613.4b: "can't be blocked [this turn] and has/have base power
    // and toughness N/N" — the restriction conjunct precedes the base-PT conjunct.
    // `extract_keyword_clause` only recovers the trailing conjunct, so scan the
    // leading restriction explicitly (Atomic Microsizer).
    if let Some((restriction_text, _)) =
        nom_primitives::scan_split_at_phrase(&unquoted_lower, |i| {
            (
                tag("and "),
                alt((tag("has"), tag("have"))),
                tag(" base power"),
            )
                .parse(i)
        })
    {
        let restriction_text = restriction_text.trim();
        if let Some(modes) = parse_restriction_modes(restriction_text) {
            for mode in modes {
                if static_mode_needs_grant_propagation(&mode)
                    && !modifications.iter().any(|existing| {
                        matches!(
                            existing,
                            ContinuousModification::AddStaticMode { mode: existing_mode }
                                if *existing_mode == mode
                        )
                    })
                {
                    modifications.push(ContinuousModification::AddStaticMode { mode });
                }
            }
        }
    }

    // CR 508.1d + CR 509.1a + CR 205.1b: A one-shot combat trick that leads with
    // a movement restriction and then a type/stat change — "can't block this
    // turn and becomes a Coward in addition to its other types" (Coward); the
    // generalized class is "can't <restriction> [this turn] and <continuous
    // mod>". The trailing change conjunct (becomes/gets/gains/has …) is already
    // recovered by the dedicated scans above and below; recover the LEADING
    // restriction conjunct here so it is not silently dropped. Anchored on
    // " and <change-verb>" so a "can't attack, block, or crew" restriction list
    // (separated by ", or "/", "/" or ") is never split mid-list.
    // `parse_restriction_modes` itself gates on the "can't"/"cannot" prefix and
    // is `all_consuming`, so a non-restriction prefix yields `None`. The
    // grant-propagation dedup mirrors the base-PT restriction block above.
    if let Some((restriction_prefix, _)) =
        nom_primitives::scan_split_at_phrase(&unquoted_lower, |i| {
            (
                tag("and "),
                alt((
                    tag("becomes "),
                    tag("become "),
                    tag("gets "),
                    tag("get "),
                    tag("gains "),
                    tag("gain "),
                    tag("has "),
                    tag("have "),
                )),
            )
                .parse(i)
        })
    {
        // Strip the embedded " this turn" duration off the restriction chunk
        // ("can't block this turn" → "can't block") via the shared combinator
        // duration grammar before delegating dispatch to
        // `parse_restriction_modes`; the bare CantBlock/CantAttack atoms do not
        // themselves consume a trailing " this turn" (unlike "be blocked").
        let (restriction_prefix, _) = strip_trailing_duration(restriction_prefix.trim());
        if let Some(modes) = parse_restriction_modes(restriction_prefix) {
            for mode in modes {
                if static_mode_needs_grant_propagation(&mode)
                    && !modifications.iter().any(|existing| {
                        matches!(
                            existing,
                            ContinuousModification::AddStaticMode { mode: existing_mode }
                                if *existing_mode == mode
                        )
                    })
                {
                    modifications.push(ContinuousModification::AddStaticMode { mode });
                }
            }
        }
    }

    for modification in parse_quoted_ability_modifications(text_stripped) {
        modifications.push(modification);
    }

    if let Some(additive_modifications) = parse_additive_type_clause_modifications(&unquoted_text) {
        modifications.extend(additive_modifications);
    }

    // CR 702: Guard "can't have or gain [keyword]" from extract_keyword_clause —
    // "have" inside "can't have" must NOT produce AddKeyword.
    if nom_primitives::scan_contains(&unquoted_lower, "can't have")
        || nom_primitives::scan_contains(&unquoted_lower, "can't have or gain")
    {
        // Parse the keyword from "can't have or gain [keyword]" / "can't have [keyword]"
        // allow-noncombinator: punctuation cleanup after parser dispatch, not dispatch itself.
        let stripped_lower = unquoted_lower.strip_suffix('.').unwrap_or(&unquoted_lower); // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        let cant_text = if let Ok((_, (_, after))) =
            nom_primitives::split_once_on(stripped_lower, "can't have or gain ")
        {
            Some(after)
        } else if let Ok((_, (_, after))) =
            nom_primitives::split_once_on(stripped_lower, "can't have ")
        {
            Some(after)
        } else {
            None
        };
        if let Some(kw_text) = cant_text {
            if let Some(kw) = map_keyword(kw_text.trim().trim_end_matches('.')) {
                modifications.push(ContinuousModification::RemoveKeyword {
                    keyword: kw.clone(),
                });
                // Note: CantHaveKeyword is a StaticMode variant, not a ContinuousModification.
                // It will be handled at the static definition level.
            }
        }
    } else if let Some(keyword_text) = extract_keyword_clause(&unquoted_text) {
        for part in split_keyword_list(keyword_text.trim().trim_end_matches('.')) {
            push_grant_clause_modifications(
                &mut modifications,
                part.as_ref(),
                where_x_expression.as_deref(),
            );
        }
    }

    // CR 613.1f: Pre-quote keyword recovery for compound lines like Swashbuckler's
    // Whip: 'has reach, "{2}, {T}: ...," and "{8}, {T}: ...".' Stripping the quoted
    // segments can mangle the boundary between the leading bare keyword and the
    // first quote, so the keyword clause above may miss "reach". Scan the slice
    // BEFORE the first quote independently. GUARD: only run when the post-strip
    // path produced no AddKeyword (prevents double-adding a keyword).
    if !modifications
        .iter()
        .any(|m| matches!(m, ContinuousModification::AddKeyword { .. }))
    {
        if let Ok((_, pre_quote)) = take_until::<_, _, OracleError<'_>>("\"").parse(text_stripped) {
            if let Some(keyword_text) = extract_keyword_clause(pre_quote) {
                for part in split_keyword_list(keyword_text.trim().trim_end_matches(',').trim()) {
                    push_grant_clause_modifications(&mut modifications, part.as_ref(), None);
                }
            }
        }
    }

    // CR 702: "lose [keyword]" / "loses [keyword]" — keyword removal.
    if let Some(keyword_text) = extract_lose_keyword_clause(&unquoted_text) {
        for part in split_keyword_list(keyword_text.trim().trim_end_matches('.')) {
            if let Some(kw) = map_keyword(part.trim().trim_end_matches('.')) {
                modifications.push(ContinuousModification::RemoveKeyword { keyword: kw });
            }
        }
    }

    // CR 205.1a + CR 205.2 + CR 205.3 + CR 613.1c: "becomes a [subtype]*
    // [core-type]+ in addition to its other types" — delegates to the shared
    // animation type-sequence combinator so one CR-205 type-line decomposes
    // into one AddType/AddSubtype modification per token (not a single
    // whole-phrase AddSubtype string).
    modifications.extend(parse_becomes_type_addition_modifications(&unquoted_tp));
    modifications.extend(parse_bare_becomes_type_replacement_modifications(
        &unquoted_tp,
    ));

    modifications
}

pub(crate) fn push_grant_clause_modifications(
    modifications: &mut Vec<ContinuousModification>,
    part: &str,
    where_x_expression: Option<&str>,
) {
    // CR 702.16n / 702.16p: a keyword-grant clause reaching this fn is a single
    // BARE (unquoted) keyword token — granted activated/triggered abilities are
    // quoted and stripped to a separate path (strip_quoted_segments at :645 +
    // parse_quoted_ability_modifications at :798) before extract_keyword_clause
    // runs, so any ". " here can only introduce a trailing inert prose sentence
    // (e.g. Benevolent Blessing's SBA-exemption "This effect doesn't remove ...").
    // Drop it so the keyword sentence reaches map_keyword clean.
    let part =
        match super::oracle_nom::bridge::split_once_on_lower(part, &part.to_lowercase(), ". ") {
            Some((first, _)) => first,
            None => part,
        };

    let part_trimmed = part.trim().trim_end_matches('.');
    let (part_without_duration, _) = strip_trailing_duration(part_trimmed);
    let part_trimmed = part_without_duration.trim().trim_end_matches('.');
    let part_lower = part_trimmed.to_lowercase();

    // CR 702: Check for dynamic "keyword X" with "where X is [qty]"
    if let Some(where_expr) = where_x_expression {
        if let Ok((_, kw_name)) = terminated(
            alpha1::<_, OracleError<'_>>,
            preceded(space1, tag_no_case("x")),
        )
        .parse(part_lower.as_str())
        {
            if let Some(kind) = crate::types::keywords::DynamicKeywordKind::from_name(kw_name) {
                if let Ok((_, qty_ref)) =
                    super::oracle_nom::quantity::parse_quantity_ref_complete(where_expr)
                {
                    modifications.push(ContinuousModification::AddDynamicKeyword {
                        kind,
                        value: QuantityExpr::Ref { qty: qty_ref },
                    });
                    return;
                }
            }
        }
    }

    // CR 608.2d + CR 613.1f: chosen-keyword anaphor — "that ability" / "the
    // chosen ability" / "the chosen keyword" refers back to a preceding
    // `Effect::Choose { ChoiceType::Keyword, persist }` clause (Angelic
    // Skirmisher: "choose first strike, vigilance, or lifelink. Creatures you
    // control gain that ability ..."; Linvala, Shield of Sea Gate: "choose
    // hexproof or indestructible. Creatures you control gain that ability
    // ..."). The plural forms — "each of the chosen abilities" / "the chosen
    // abilities" — refer back to a multi-keyword choice (Greymond, Avacyn's
    // Stalwart: "choose two abilities ... Humans you control have each of the
    // chosen abilities"); the same `AddChosenKeyword` reads ALL persisted
    // `ChosenAttribute::Keyword` entries at layer evaluation. Emits
    // `AddChosenKeyword`, the additive mirror of `RemoveChosenKeyword` (Urborg /
    // Walking Sponge). Checked before `map_keyword` so the anaphor is never
    // mis-classified as an unknown keyword. Builds for the whole "gain the
    // chosen keyword(s)" class.
    if alt((
        tag::<_, _, OracleError<'_>>("that ability"),
        tag("each of the chosen abilities"),
        tag("the chosen abilities"),
        tag("the chosen ability"),
        tag("the chosen keyword"),
    ))
    .parse(part_lower.as_str())
    .is_ok()
    {
        modifications.push(ContinuousModification::AddChosenKeyword);
        return;
    }

    // CR 702.6a: bare "equip {N}" in a keyword list ("has indestructible and
    // equip {0}") is the equip activated ability — not an inert AddKeyword.
    // Mirrors `classify_quoted_inner`'s pre-keyword equip dispatch.
    if nom_tag_lower(&part_lower, &part_lower, "equip").is_some() {
        if let Some(ability) = super::oracle::try_parse_equip(part_trimmed) {
            modifications.push(ContinuousModification::GrantAbility {
                definition: Box::new(ability),
            });
            return;
        }
    }

    if let Some(kw) = map_keyword(part_trimmed) {
        modifications.push(ContinuousModification::AddKeyword { keyword: kw });
        return;
    }

    // CR 702.18a / 702.11a: a descriptive "can't be the target [of ...]" grant is
    // Shroud (blanket) or Hexproof (opponents only). Emit the keyword so the
    // existing targeting checks apply the correct controller scope, rather than a
    // scope-less rule static.
    if let Some(scope) =
        crate::parser::oracle_keyword::classify_cant_be_targeted(part_lower.as_str())
    {
        let keyword = match scope {
            crate::parser::oracle_keyword::CantBeTargetedScope::AnyPlayer => Keyword::Shroud,
            crate::parser::oracle_keyword::CantBeTargetedScope::OpponentsOnly => Keyword::Hexproof,
        };
        modifications.push(ContinuousModification::AddKeyword { keyword });
        return;
    }

    if let Some(modes) = parse_restriction_modes(part_lower.as_str()) {
        for mode in modes {
            if static_mode_needs_grant_propagation(&mode) {
                modifications.push(ContinuousModification::AddStaticMode { mode });
            }
        }
    }
}

/// Extract quoted ability text from Oracle text and parse each into a typed AbilityDefinition.
///
/// Quoted abilities like `"{T}: Add two mana of any one color."` are parsed by splitting
/// at the cost separator (`:` after mana/tap symbols) and reusing `parse_oracle_cost` +
/// `parse_effect_chain`. Non-activated quoted text is parsed as a spell-like effect chain.
/// Parse quoted abilities and return the appropriate ContinuousModification.
/// CR 604.1: Trigger-prefix quoted text (when/whenever/at the beginning) becomes
/// GrantTrigger to preserve trigger metadata; all others become GrantAbility.
pub(crate) fn parse_quoted_ability_modifications(text: &str) -> Vec<ContinuousModification> {
    let mut modifications = Vec::new();
    let mut start = None;

    for (idx, ch) in text.char_indices() {
        if ch == '"' {
            if let Some(open) = start.take() {
                let ability_text = text[open + 1..idx].trim();
                modifications.extend(classify_quoted_inner(ability_text));
            } else {
                start = Some(idx);
            }
        }
    }

    modifications
}

/// CR 604.1: Classify already-stripped inner-quote text into the appropriate
/// `ContinuousModification` variant. Extracted from
/// `parse_quoted_ability_modifications` so callers that already have the
/// inner-quote slice (e.g., `parser::oracle_nom::return_as_aura::try_parse`)
/// can dispatch directly without re-walking for `"..."` pairs.
///
/// Dispatch ladder (single authority — DO NOT duplicate elsewhere):
///   1. CR 603.1: trigger prefix ("when "/"whenever "/"at the beginning of "/
///      "at the end of ") → `ContinuousModification::GrantTrigger`.
///   2. CR 702: keyword text ("flying", "ward—pay 2 life", etc.) →
///      `ContinuousModification::AddKeyword`.
///   3. CR 113.3d + CR 604.1: static-line text ("enchanted creature gets +N/+M",
///      "creatures you control have ...") → one or more
///      `ContinuousModification::GrantStaticAbility` / `AddStaticMode`.
///   4. CR 113 / CR 117 (fallback): spell/activated text → `GrantAbility`
///      wrapping the parsed `AbilityDefinition`.
///
/// Visibility: `pub(crate)` so external crate-local callers can reuse the
/// canonical inner classifier without exposing the private
/// `parse_quoted_ability` / `parse_quoted_rule_static_modifications` helpers.
pub(crate) fn classify_quoted_inner(ability_text: &str) -> Vec<ContinuousModification> {
    let ability_text = ability_text.trim();
    if ability_text.is_empty() {
        return Vec::new();
    }

    // CR 207.2c: A granted ability's text may carry an italicized ability-word
    // prefix ("Landfall — Whenever a land you control enters, ..."). Ability
    // words have no rules meaning, so the body parses through ordinary
    // trigger/keyword/static machinery. Strip a recognized ability-word prefix
    // and re-classify the remainder so the inner trigger/static is detected
    // (otherwise the ability-word prefix masks the trigger keyword and the line
    // falls through to the GrantAbility catch-all as an unimplemented effect).
    // Gated on a known ability word so a legitimate em-dash body is untouched.
    if let Some((aw_name, body)) = super::oracle_modal::strip_ability_word_with_name(ability_text) {
        if super::oracle_modal::is_known_ability_word(&aw_name) {
            return classify_quoted_inner(&body);
        }
    }

    let lower = ability_text.to_lowercase();

    // CR 603.1: Detect trigger prefixes to route to GrantTrigger.
    if nom_tag_lower(&lower, &lower, "when ").is_some()
        || nom_tag_lower(&lower, &lower, "whenever ").is_some()
        || nom_tag_lower(&lower, &lower, "at the beginning of ").is_some()
        || nom_tag_lower(&lower, &lower, "at the end of ").is_some()
    {
        return super::oracle_trigger::parse_trigger_lines(ability_text, "~")
            .into_iter()
            .map(|trigger| ContinuousModification::GrantTrigger {
                trigger: Box::new(trigger),
            })
            .collect();
    }

    // CR 702.6a: a standalone "Equip {N}" line is the equip activated ability —
    // detect it BEFORE keyword extraction. An MTGJSON keyword name can match the
    // printed equip cost, so parse_keyword_from_oracle("equip {2}") would otherwise
    // land an inert AddKeyword{Equip}; but equip is an activated ability that needs
    // its Effect::Attach body. Mirrors oracle.rs's "Pre-keyword activated ability"
    // ordering. `try_parse_equip` assumes its caller has already confirmed the
    // "equip" prefix (it strips the first 5 bytes unconditionally), so the
    // `starts_with` guard is required — without it any quoted line would be
    // mis-parsed as equip.
    if nom_tag_lower(&lower, &lower, "equip").is_some() {
        if let Some(ability) = super::oracle::try_parse_equip(ability_text) {
            return vec![ContinuousModification::GrantAbility {
                definition: Box::new(ability),
            }];
        }
    }

    // CR 702: Quoted text that is a keyword (e.g. "Ward—Pay 2 life") should be
    // granted as AddKeyword, not wrapped in an AbilityDefinition.
    if let Some(keyword) = super::oracle_keyword::parse_keyword_from_oracle(&lower) {
        return vec![ContinuousModification::AddKeyword { keyword }];
    }

    // CR 113.3d + CR 604.1: Static-line text → GrantStaticAbility / AddStaticMode.
    if let Some(static_modifications) = parse_quoted_rule_static_modifications(ability_text) {
        return static_modifications;
    }

    // CR 113 / CR 117 fallback: spell/activated text → GrantAbility.
    vec![ContinuousModification::GrantAbility {
        definition: Box::new(parse_quoted_ability(ability_text)),
    }]
}

/// CR 702: Split a keyword list like "flying and first strike" into individual keywords.
pub(crate) fn split_keyword_list(text: &str) -> Vec<Cow<'_, str>> {
    // Strip both trailing periods and trailing commas. A comma tail arises when
    // `strip_quoted_segments` removes `and "Whenever..."` from the end of a list
    // like "has first strike, trample, haste, and \"Whenever...\""— the connector
    // `, and` is dropped but the comma after the last bare keyword remains.
    let text = text.trim().trim_end_matches(['.', ',']).trim();
    // Split on ", and/or ", ", and ", " and ", or ", " — longest-match-first
    // ordering prevents ", and " from consuming the prefix of ", and/or ".
    let mut parts: Vec<&str> = Vec::new();
    for chunk in text.split(", and/or ") {
        for sub_chunk in chunk.split(", and ") {
            for sub in sub_chunk.split(" and ") {
                for item in sub.split(", ") {
                    let trimmed = item.trim();
                    if !trimmed.is_empty() {
                        parts.push(trimmed);
                    }
                }
            }
        }
    }
    // CR 702.16: Expand "protection from X and from Y" into separate entries.
    // Reuses the building block from oracle_keyword.rs which handles inline,
    // comma-continuation, and Oxford comma protection patterns.
    super::oracle_keyword::expand_protection_parts(&parts)
}
